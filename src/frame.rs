//! Frames and blocks (ports frame.go, block.go and the frame loop of zstd.go).
#![forbid(unsafe_code)]

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bits::BitReader;
use crate::error::Error;
use crate::literals::LitSrc;
use crate::seq::{CANCEL_STRIDE, SeqState};
use crate::xxhash::{le32, le64, xxh64_until};
use crate::{BLOCK_SIZE_MAX, Decoder, Limits, MAX_WINDOW, TABLES, WILDCOPY_SLACK};

const FRAME_MAGIC: u32 = 0xFD2F_B528;
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;
/// Skippable frames between two checks of the cancel flag.
const SKIP_POLL: usize = 1024;
/// The least output per thread of an input of several frames that is decoded on several threads:
/// below it, starting a thread (tens of microseconds) costs about what it saves. See
/// docs/threading.md.
const PAR_MIN: usize = 128 << 10;
/// What `max_alloc` is charged for each helper thread besides its decoder's tables: the slot in
/// `helpers`, and std's per-thread bookkeeping (the packet, the thread handle, the boxed closure),
/// a few hundred bytes in all; rounded up.
const HELPER_SLACK: usize = 4 << 10;

const TRUNCATED: Error = Error::Corrupt("truncated input");
const BAD_MAGIC: Error = Error::Corrupt("unknown magic number");
const BAD_HEADER: Error = Error::Corrupt("bad frame header");
const BAD_BLOCK: Error = Error::Corrupt("bad block");
const BAD_SEQUENCES: Error = Error::Corrupt("bad sequences section");
const BAD_SIZE: Error = Error::Corrupt("content size mismatch");

struct FrameHeader {
    /// Header bytes.
    size: usize,
    fcs: u64,
    has_fcs: bool,
    window: u64,
    checksum: bool,
    dict_id: u32,
}

/// Ports `ZSTD_getFrameHeader_advanced`.
fn parse_frame_header(src: &[u8]) -> Result<FrameHeader, Error> {
    if src.len() < 5 {
        if src.len() >= 4 && le32(src) != FRAME_MAGIC {
            return Err(BAD_MAGIC);
        }
        return Err(TRUNCATED);
    }
    if le32(src) != FRAME_MAGIC {
        return Err(BAD_MAGIC);
    }
    let fhd = src[4];
    let single = fhd & 0x20 != 0;
    let dict_size = [0, 1, 2, 4][usize::from(fhd & 3)];
    let fcs_size = [0, 2, 4, 8][usize::from(fhd >> 6)];
    let mut size = 5 + dict_size + fcs_size;
    if !single || fhd >> 6 == 0 {
        size += 1;
    }
    if src.len() < size {
        return Err(TRUNCATED);
    }
    if fhd & 0x08 != 0 {
        return Err(BAD_HEADER);
    }
    let mut pos = 5;
    let mut window = 0u64;
    if !single {
        let wd = src[pos];
        pos += 1;
        let wlog = u32::from(wd >> 3) + 10;
        if wlog > 31 {
            return Err(Error::WindowTooLarge);
        }
        window = 1u64 << wlog;
        window += (window >> 3) * u64::from(wd & 7);
    }
    let dict_id = match dict_size {
        1 => u32::from(src[pos]),
        2 => u32::from(u16::from_le_bytes([src[pos], src[pos + 1]])),
        4 => le32(&src[pos..]),
        _ => 0,
    };
    pos += dict_size;
    let mut has_fcs = true;
    let mut fcs = 0u64;
    match fhd >> 6 {
        0 => {
            if single {
                fcs = u64::from(src[pos]);
            } else {
                has_fcs = false;
            }
        }
        1 => fcs = u64::from(u16::from_le_bytes([src[pos], src[pos + 1]])) + 256,
        2 => fcs = u64::from(le32(&src[pos..])),
        _ => fcs = le64(&src[pos..]),
    }
    if single {
        window = fcs;
    }
    Ok(FrameHeader {
        size,
        fcs,
        has_fcs,
        window,
        checksum: fhd & 0x04 != 0,
        dict_id,
    })
}

/// Reports the first frame's content size, `None` if absent. A skippable frame decodes to
/// nothing, so it reports `Some(0)`. It allocates nothing and spawns no thread, so `max_alloc` and
/// `max_threads` do not apply.
///
/// # Errors
///
/// A size over `limits.max_output` is [`Error::OutputLimit`]. A header that is cut short or wrong
/// is [`Error::Corrupt`] (or [`Error::WindowTooLarge`]).
pub fn content_size(src: &[u8], limits: &Limits) -> Result<Option<u64>, Error> {
    if src.len() >= 4 && le32(src) & 0xFFFF_FFF0 == SKIPPABLE_MAGIC {
        if src.len() < 8 {
            return Err(TRUNCATED);
        }
        return Ok(Some(0));
    }
    let h = parse_frame_header(src)?;
    if h.has_fcs && h.fcs > limits.max_output as u64 {
        return Err(Error::OutputLimit);
    }
    Ok(h.has_fcs.then_some(h.fcs))
}

fn skippable_size(src: &[u8]) -> Result<usize, Error> {
    if src.len() < 8 {
        return Err(TRUNCATED);
    }
    let n = u64::from(le32(&src[4..])) + 8;
    if n > src.len() as u64 {
        return Err(TRUNCATED);
    }
    Ok(n as usize)
}

/// The size of the frame at the start of `src` and its content size (`None` for a skippable
/// frame), read from the frame and block headers without decoding: how the parallel path splits
/// the input and the output. `None` for anything the serial loop must meet to give its error or
/// its size: too short for a frame, a frame cut short, a bad header, no content size, a
/// dictionary, a content size the decode calls a lie, a reserved block type.
fn scan_frame(src: &[u8]) -> Option<(usize, Option<usize>)> {
    if src.len() < 5 {
        return None;
    }
    if le32(src) & 0xFFFF_FFF0 == SKIPPABLE_MAGIC {
        return Some((skippable_size(src).ok()?, None));
    }
    let h = parse_frame_header(src).ok()?;
    if !h.has_fcs || h.dict_id != 0 || h.fcs > (src.len() / 4 + 1) as u64 * BLOCK_SIZE_MAX as u64 {
        return None;
    }
    // The block walk of decode_frame: raw blocks hold their size, RLE blocks one byte.
    let mut ip = h.size;
    loop {
        let b = src.get(ip..ip + 3)?;
        let bh = u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16);
        let size = match (bh >> 1) & 3 {
            1 => 1,
            3 => return None,
            _ => (bh >> 3) as usize,
        };
        ip += 3 + size;
        if ip > src.len() {
            return None;
        }
        if bh & 1 != 0 {
            break;
        }
    }
    if h.checksum {
        ip += 4;
        if ip > src.len() {
            return None;
        }
    }
    Some((ip, Some(usize::try_from(h.fcs).ok()?)))
}

/// Where the output goes: a `Vec` that grows as the frames decode, or a buffer that cannot, which
/// is how a caller's buffer is decoded into without zero-filling or copying.
pub(crate) enum Out<'a> {
    /// The `Vec`, the bytes charged to `max_alloc` so far by this decode, and the writable extent.
    /// The `Vec`'s length is the part of the extent that is initialized: it is zero-filled on
    /// demand, as the output needs it, not up front.
    Grow(&'a mut Vec<u8>, usize, usize),
    Fixed(&'a mut [u8]),
    /// The caller's buffer, which spills into the empty `Vec` when the output outgrows it, as Go's
    /// `growOut` grows a slice of the caller's capacity: the buffer is not charged, each growth is,
    /// and the output so far is copied over.
    Spill(&'a mut [u8], &'a mut Vec<u8>),
}

impl Deref for Out<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Out::Grow(v, ..) => v,
            Out::Fixed(s) | Out::Spill(s, _) => s,
        }
    }
}

impl DerefMut for Out<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            Out::Grow(v, ..) => v,
            Out::Fixed(s) | Out::Spill(s, _) => s,
        }
    }
}

impl<'a> Out<'a> {
    /// A `Vec` that is writable (and initialized) up to its length, as czi's scratch buffer is.
    pub(crate) fn grow_vec(v: &'a mut Vec<u8>, charged: usize) -> Self {
        let ext = v.len();
        Out::Grow(v, charged, ext)
    }

    /// The writable extent, in which output may be written once it is initialized.
    fn ext(&self) -> usize {
        match self {
            Out::Grow(_, _, ext) => *ext,
            Out::Fixed(s) | Out::Spill(s, _) => s.len(),
        }
    }

    /// Zero-fills the extent up to `to`, checking the cancel flag every `CANCEL_STRIDE` bytes.
    /// The capacity is already there (`to <= ext`), so nothing is allocated.
    pub(crate) fn init_to(&mut self, to: usize, cancel: &AtomicBool) -> Result<(), Error> {
        if let Out::Grow(v, _, ext) = self {
            let to = to.min(*ext);
            while v.len() < to {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                v.resize(to.min(v.len() + CANCEL_STRIDE), 0);
            }
        }
        Ok(())
    }
}

impl Out<'_> {
    /// Makes room for `room` more bytes after `op` (see `grow_out`). A fixed buffer has none to
    /// give: the output does not fit.
    fn grow(
        &mut self,
        op: usize,
        room: usize,
        limit: usize,
        hint: usize,
        out_max: usize,
        limits: &Limits,
    ) -> Result<usize, Error> {
        match self {
            Out::Grow(v, charged, ext) => {
                grow_out(v, charged, ext, op, room, limit, hint, out_max, limits)
            }
            Out::Fixed(_) => Err(Error::OutputLimit),
            Out::Spill(s, v) => {
                let (mut charged, mut ext) = (0, s.len());
                let n = grow_out(
                    v,
                    &mut charged,
                    &mut ext,
                    op,
                    room,
                    limit,
                    hint,
                    out_max,
                    limits,
                )?;
                v.extend_from_slice(&s[..op]);
                if let Out::Spill(_, v) = std::mem::replace(self, Out::Fixed(&mut [])) {
                    *self = Out::Grow(v, charged, ext);
                }
                Ok(n)
            }
        }
    }
}

impl Decoder {
    /// Decodes the frames of `src`, appending at `out[start..]`. The writable extent of `out` is
    /// only ever grown; the result is where the output ends. `on_block` sees the
    /// output so far after each block.
    pub(crate) fn decode_frames<F: FnMut(&[u8])>(
        &mut self,
        out: &mut Out,
        start: usize,
        mut src: &[u8],
        limits: &Limits,
        cancel: &AtomicBool,
        on_block: &mut F,
    ) -> Result<usize, Error> {
        let out_max = start.saturating_add(limits.max_output);
        self.kept = out.ext();
        // Nothing grows on that path, so `kept` stays the extent, as in the serial loop.
        if let Some(r) = self.decode_par(out, start, src, limits, cancel, out_max) {
            let end = r?;
            on_block(&out[..end]);
            return Ok(end);
        }
        let mut op = start;
        let mut skipped = 0usize;
        while src.len() >= 5 {
            if le32(src) & 0xFFFF_FFF0 == SKIPPABLE_MAGIC {
                src = &src[skippable_size(src)?..];
                // A long run of skippable frames is cancelled. A few are not, as in zstd-go, which
                // checks its context per block only.
                skipped += 1;
                if skipped.is_multiple_of(SKIP_POLL) && cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                continue;
            }
            let (end, n) = self.decode_frame(out, op, src, limits, cancel, out_max, on_block)?;
            op = end;
            self.kept = out.ext();
            src = &src[n..];
        }
        if !src.is_empty() {
            return Err(TRUNCATED);
        }
        Ok(op)
    }

    /// The frames of `src` decoded on up to `limits.max_threads` threads into `out[start..]`, as
    /// `decode_frames` would decode them: a contiguous run of whole frames per thread, the
    /// caller's thread taking the last, each with a decoder of its own. `None` where the serial
    /// loop must run instead; then what was written here, the serial loop writes again.
    ///
    /// It runs for two frames or more with content sizes (`scan_frame`) and `PAR_MIN` bytes of
    /// output or more per thread, all within `max_output`, when the output fits the extent of `out`: nothing
    /// grows, so `max_alloc` is charged only for the tables of new helpers, and with what the call
    /// has charged they must fit (fewer threads otherwise). Each frame decodes as in the serial
    /// loop, from the same `src` and with `out_max` moved by its start, into a slice of exactly
    /// its content size, which is the extent the serial loop gives it: every check sees the same
    /// numbers, moved by the frame's start, so the error is the serial one of the first frame
    /// that fails.
    fn decode_par(
        &mut self,
        out: &mut Out,
        start: usize,
        src: &[u8],
        limits: &Limits,
        cancel: &AtomicBool,
        out_max: usize,
    ) -> Option<Result<usize, Error>> {
        if limits.max_threads < 2 {
            return None;
        }
        let (mut at, mut frames, mut skipped, mut total) = (0, 0usize, 0usize, 0usize);
        while at < src.len() {
            let (n, fcs) = scan_frame(&src[at..])?;
            at += n;
            match fcs {
                Some(fcs) => {
                    frames += 1;
                    total = total.checked_add(fcs)?;
                }
                // The serial loop checks the cancel flag in a long run of skippable frames.
                None => {
                    skipped += 1;
                    if skipped >= SKIP_POLL {
                        return None;
                    }
                }
            }
        }
        if frames < 2 || total < 2 * PAR_MIN {
            return None;
        }
        let end = start.checked_add(total).filter(|&e| e <= out_max)?;
        let charged = match &*out {
            Out::Grow(_, charged, ext) if *ext >= end => *charged,
            Out::Fixed(s) | Out::Spill(s, _) if s.len() >= end => 0,
            _ => return None,
        };
        // One frame and `PAR_MIN` bytes of output per thread at least. A new helper's tables must
        // fit `max_alloc` with what the call has charged; the helpers are kept, as the decoder's
        // own tables are.
        let w = limits.max_threads.min(frames).min(total / PAR_MIN);
        let need = (w - 1).min(
            self.helpers.len() + limits.max_alloc.saturating_sub(charged) / (TABLES + HELPER_SLACK),
        );
        if self.helpers.len() < need
            && self
                .helpers
                .try_reserve_exact(need - self.helpers.len())
                .is_ok()
        {
            while self.helpers.len() < need {
                let Ok(d) = Decoder::new(limits) else {
                    break;
                };
                self.helpers.push(d);
            }
        }
        let w = w.min(self.helpers.len() + 1);
        if w < 2 {
            return None;
        }
        // As the serial loop zero-fills it block by block (checking the cancel flag).
        if let Err(e) = out.init_to(end, cancel) {
            return Some(Err(e));
        }
        let buf = out.get_mut(start..end)?;
        let mut helpers = std::mem::take(&mut self.helpers);
        let r = std::thread::scope(|s| -> Option<Result<usize, Error>> {
            let mut jobs = Vec::new();
            jobs.try_reserve_exact(w - 1).ok()?;
            let mut hs = helpers.iter_mut();
            let (mut from, mut at, mut seen, mut done, mut acc) = (0, 0, 0, 0, 0);
            let mut rest = buf;
            while at < src.len() {
                let (n, fcs) = scan_frame(&src[at..])?;
                at += n;
                let Some(fcs) = fcs else { continue };
                seen += 1;
                acc += fcs;
                // Runs of about `total / w` bytes of output; the caller's has a frame at least.
                if seen < frames && jobs.len() + 1 < w && acc >= total / w * (jobs.len() + 1) {
                    let d = hs.next()?;
                    let (part, tail) =
                        std::mem::take(&mut rest).split_at_mut_checked(acc - done)?;
                    rest = tail;
                    let (lo, hi, base) = (from, at, start + done);
                    // A thread the OS refuses: the serial loop decodes it all again.
                    let job = std::thread::Builder::new().spawn_scoped(s, move || {
                        d.decode_run(src, lo, hi, part, base, out_max, limits, cancel)
                    });
                    jobs.push(job.ok()?);
                    (from, done) = (at, acc);
                }
            }
            let last = self.decode_run(
                src,
                from,
                src.len(),
                rest,
                start + done,
                out_max,
                limits,
                cancel,
            );
            // The first frame that fails is in the first run that fails.
            let mut r = Ok(());
            for job in jobs {
                match job.join() {
                    Ok(x) => r = r.and(x),
                    // A decode does not panic; if one did, the serial loop would have too.
                    Err(e) => std::panic::resume_unwind(e),
                }
            }
            Some(r.and(last).map(|()| end))
        });
        self.helpers = helpers;
        r
    }

    /// Decodes the frames in `src[from..to]`, a run of `decode_par`'s plan, into `out`, which is
    /// at `at` in the whole output: each into a slice of its content size, from `src` at its
    /// start, with `out_max` moved by its start. It stops at the first error.
    fn decode_run(
        &mut self,
        src: &[u8],
        mut from: usize,
        to: usize,
        mut out: &mut [u8],
        mut at: usize,
        out_max: usize,
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        while from < to {
            let s = &src[from..];
            // The plan has scanned these frames, so neither of these fails.
            let (n, fcs) = scan_frame(s).ok_or(TRUNCATED)?;
            from += n;
            let Some(fcs) = fcs else { continue };
            let (f, rest) = std::mem::take(&mut out)
                .split_at_mut_checked(fcs)
                .ok_or(TRUNCATED)?;
            let (end, read) = self.decode_frame(
                &mut Out::Fixed(f),
                0,
                s,
                limits,
                cancel,
                out_max.saturating_sub(at),
                &mut |_: &[u8]| {},
            )?;
            // decode_frame walks the blocks as scan_frame does and checks the content size.
            debug_assert_eq!((end, read), (fcs, n));
            out = rest;
            at += fcs;
        }
        Ok(())
    }

    /// Decodes one frame from `src` to `out[base..]` and returns where the output ends and the
    /// bytes read.
    fn decode_frame<F: FnMut(&[u8])>(
        &mut self,
        out: &mut Out,
        base: usize,
        src: &[u8],
        limits: &Limits,
        cancel: &AtomicBool,
        out_max: usize,
        on_block: &mut F,
    ) -> Result<(usize, usize), Error> {
        if src.len() < 6 + 3 {
            return Err(TRUNCATED);
        }
        let h = parse_frame_header(src)?;
        if src.len() < h.size + 3 {
            return Err(TRUNCATED);
        }
        if h.dict_id != 0 {
            return Err(Error::Dictionary);
        }
        // Without a content size, follow ZSTD_decompressStream's checks.
        let stream = !h.has_fcs;
        if stream && h.window > MAX_WINDOW {
            return Err(Error::WindowTooLarge);
        }
        let bsm = h.window.min(BLOCK_SIZE_MAX as u64) as usize;
        // Decoded bytes per block.
        let max_out = if stream { bsm } else { usize::MAX };
        let mut ip = h.size;

        // out[base..] is this frame and `avail` the write limit. A frame that does not fit grows
        // as it decodes, so a lying header costs only the output it really produces.
        let mut limit = usize::MAX;
        if h.has_fcs {
            // Each 4 input bytes yield at most one block, so a larger FCS is a lie.
            if h.fcs > (src.len() / 4 + 1) as u64 * BLOCK_SIZE_MAX as u64 {
                return Err(BAD_SIZE);
            }
            let fcs = usize::try_from(h.fcs).map_err(|_| Error::AllocLimit)?;
            limit = base.checked_add(fcs).ok_or(Error::AllocLimit)?;
            if limit > out_max {
                return Err(Error::OutputLimit);
            }
        }
        let mut avail = out.ext().min(limit);
        // ponytail: first growth assumes ratio <= 32; doubling covers the rest.
        let hint = base.saturating_add(src.len().saturating_mul(32));

        self.rep = [1, 4, 8];
        self.lit_ok = false;
        self.fse_ok = false;
        let mut op = base;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if src.len() - ip < 3 {
                return Err(TRUNCATED);
            }
            let bh =
                u32::from(src[ip]) | (u32::from(src[ip + 1]) << 8) | (u32::from(src[ip + 2]) << 16);
            ip += 3;
            let last = bh & 1 != 0;
            let btype = (bh >> 1) & 3;
            let rsize = (bh >> 3) as usize; // regenerated size bound for raw and RLE
            let size = if btype == 1 { 1 } else { rsize };
            // One-shot ZSTD_decompress checks raw and RLE sizes only against the output space.
            if btype == 3 || (btype == 2 && size > bsm) || (stream && rsize > bsm) {
                return Err(BAD_BLOCK);
            }
            if size > src.len() - ip {
                return Err(TRUNCATED);
            }
            if btype != 2 && op.saturating_add(rsize) > out_max {
                return Err(Error::OutputLimit);
            }
            let room = if btype == 2 { bsm } else { rsize }; // a compressed block's size is known only after decoding
            if avail - op < room && avail < limit {
                avail = out.grow(op, room, limit, hint, out_max, limits)?;
            }
            // Initialize what this block can write: all of `avail` for a compressed block of a frame
            // with a content size (its output is not bounded by the block size), else a block and
            // the wildcopy slack.
            let init = match btype {
                2 if !stream => avail,
                2 => avail.min(op + bsm + WILDCOPY_SLACK),
                _ => avail.min(op.saturating_add(rsize)),
            };
            out.init_to(init, cancel)?;
            match btype {
                0 => {
                    if size > avail - op {
                        return Err(BAD_SIZE);
                    }
                    out[op..op + size].copy_from_slice(&src[ip..ip + size]);
                    op += size;
                }
                1 => {
                    if rsize > avail - op {
                        return Err(BAD_SIZE);
                    }
                    out[op..op + rsize].fill(src[ip]);
                    op += rsize;
                }
                _ => {
                    let n = self.decode_block(
                        &mut out[..init],
                        op,
                        base,
                        bsm,
                        max_out,
                        &src[ip..ip + size],
                        cancel,
                    )?;
                    op += n;
                    if op > out_max {
                        return Err(Error::OutputLimit);
                    }
                }
            }
            ip += size;
            on_block(&out[..op]);
            if last {
                break;
            }
        }
        if h.has_fcs && (op - base) as u64 != h.fcs {
            return Err(BAD_SIZE);
        }
        if h.checksum {
            if src.len() - ip < 4 {
                return Err(TRUNCATED);
            }
            let sum = xxh64_until(&out[base..op], || cancel.load(Ordering::Relaxed))
                .ok_or(Error::Cancelled)?;
            if sum as u32 != le32(&src[ip..]) {
                return Err(Error::Checksum);
            }
            ip += 4;
        }
        Ok((op, ip))
    }

    /// Ports `ZSTD_decompressBlock_internal` for a compressed block written to `out[op..]`. `base`
    /// is the start of the frame in `out`, `bsm` the frame's block size limit and `max_out` the
    /// most bytes the block may decode to. It returns the decoded size.
    fn decode_block(
        &mut self,
        out: &mut [u8],
        op: usize,
        base: usize,
        bsm: usize,
        max_out: usize,
        sec: &[u8],
        cancel: &AtomicBool,
    ) -> Result<usize, Error> {
        let (lit_src, lit_size, n) = self.decode_literals(sec, bsm, out.len() - op)?;
        let blk = &sec[n..];
        let (nb_seq, n) = self.decode_seq_headers(blk)?;
        let mut end = op;
        let mut lp = 0;
        if nb_seq > 0 {
            self.fse_ok = true;
        }
        let lits: &[u8] = match lit_src {
            LitSrc::Buf => &self.lit[..],
            LitSrc::Blk(off) => &sec[off..],
        };
        if nb_seq > 0 {
            let t = self.seq.tables();
            let mut br = BitReader::new(&blk[n..]).ok_or(BAD_SEQUENCES)?;
            let ll_s = br.read(t.ll_log) as usize;
            br.reload();
            let of_s = br.read(t.of_log) as usize;
            br.reload();
            let ml_s = br.read(t.ml_log) as usize;
            br.reload();
            let mut s = SeqState {
                br,
                ll_t: t.ll,
                ml_t: t.ml,
                of_t: t.of,
                ll_s,
                ml_s,
                of_s,
                rep: self.rep,
                nb_seq,
                out: &mut *out,
                op,
                base,
                lits,
                lp: 0,
                lit_end: lit_size,
                ll: 0,
                ml: 0,
                off: 0,
                cancel,
                yield_at: op + CANCEL_STRIDE,
            };
            s.run()?;
            if !s.br.finished() {
                return Err(BAD_SEQUENCES);
            }
            self.rep = s.rep;
            end = s.op;
            lp = s.lp;
        }
        let last = lit_size - lp;
        if last > out.len() - end {
            return Err(BAD_SEQUENCES);
        }
        out[end..end + last].copy_from_slice(&lits[lp..lit_size]);
        end += last;
        // Checked once per block, as the streaming libzstd decoder does.
        if end - op > max_out {
            return Err(BAD_BLOCK);
        }
        Ok(end - op)
    }
}

/// Grows `out` so `room` more bytes fit after `op`, up to `limit`, and returns the new writable
/// extent. As zstd-go's `growOut` does, each growth charges its whole new size to `charged`, and
/// the sum may not pass `max_alloc`: the old buffer and the new one are live together while the
/// copy runs. The guess (`hint`, doubling) is cut to `out_max` plus the wildcopy slack, since
/// output past `max_output` is an error (Go has no such limit), but never below `need`.
fn grow_out(
    out: &mut Vec<u8>,
    charged: &mut usize,
    ext: &mut usize,
    op: usize,
    room: usize,
    limit: usize,
    hint: usize,
    out_max: usize,
    limits: &Limits,
) -> Result<usize, Error> {
    let need = op.saturating_add(room).saturating_add(WILDCOPY_SLACK);
    let n = (2 * *ext).max(need).max(64 << 10).max(hint);
    let n = n
        .min(out_max.saturating_add(WILDCOPY_SLACK))
        .max(need)
        .min(limit);
    if n > limits.max_alloc.saturating_sub(*charged) {
        return Err(Error::AllocLimit);
    }
    out.try_reserve_exact(n - out.len())
        .map_err(|_| Error::OutOfMemory)?;
    *charged += n;
    *ext = n;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of one raw block of 100 bytes with a 128 KiB window and no content size.
    fn raw_frame(checksum: bool) -> Vec<u8> {
        let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, if checksum { 4 } else { 0 }, 56];
        f.extend_from_slice(&[0x21, 0x03, 0x00]); // last raw block of 100 bytes
        f.extend_from_slice(&[7; 100]);
        if checksum {
            f.extend_from_slice(&[0; 4]);
        }
        f
    }

    /// The spare capacity of the caller's `Vec` is zero-filled as the output reaches it, and not
    /// before the header and the cancel flag have been checked.
    #[test]
    fn spare_capacity_is_zeroed_lazily() {
        let mut d = Decoder::new(&Limits::default()).unwrap();
        let run = |d: &mut Decoder, v: &mut Vec<u8>, src: &[u8], cancel: bool| {
            let ext = v.capacity();
            d.decode_frames(
                &mut Out::Grow(v, 0, ext),
                0,
                src,
                &Limits::default(),
                &AtomicBool::new(cancel),
                &mut |_: &[u8]| {},
            )
        };
        let mut v = Vec::with_capacity(8 << 20);
        assert_eq!(run(&mut d, &mut v, &raw_frame(false), false), Ok(100));
        assert_eq!(v.len(), 100);
        let mut v = Vec::with_capacity(8 << 20);
        assert_eq!(
            run(&mut d, &mut v, &[0; 20], false),
            Err(Error::Corrupt("unknown magic number"))
        );
        assert_eq!(
            run(&mut d, &mut v, &raw_frame(false), true),
            Err(Error::Cancelled)
        );
        assert!(v.is_empty());
    }

    /// The flag is set after the last block: the checksum sees it.
    #[test]
    fn cancel_in_checksum() {
        let cancel = AtomicBool::new(false);
        let mut v = vec![0; 1 << 10];
        let r = Decoder::new(&Limits::default()).unwrap().decode_frames(
            &mut Out::grow_vec(&mut v, 0),
            0,
            &raw_frame(true),
            &Limits::default(),
            &cancel,
            &mut |_: &[u8]| cancel.store(true, Ordering::Relaxed),
        );
        assert_eq!(r, Err(Error::Cancelled));
    }

    /// A few skippable frames are not cancelled, as in zstd-go; a long run is.
    #[test]
    fn cancel_in_skippable_run() {
        let skip = [0x50, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];
        let run = |n: usize| {
            Decoder::new(&Limits::default()).unwrap().decode_frames(
                &mut Out::Fixed(&mut []),
                0,
                &skip.repeat(n),
                &Limits::default(),
                &AtomicBool::new(true),
                &mut |_: &[u8]| {},
            )
        };
        assert_eq!(run(3), Ok(0));
        assert_eq!(run(SKIP_POLL), Err(Error::Cancelled));
    }

    /// A compressed block of 16 sequences that each copy 1 literal and then 65539 bytes at offset
    /// 1 (all three tables RLE, 16 extra bits of match length, none of the others): 1 MiB from
    /// 55 bytes. A frame with a content size does not bound a block to 128 KiB of output.
    fn big_block() -> Vec<u8> {
        let mut sec = vec![16 << 3]; // 16 raw literals
        sec.extend_from_slice(&[7; 16]);
        sec.extend_from_slice(&[16, 0x54, 1, 0, 52]); // 16 sequences, RLE: ll code 1, of 0, ml 52
        sec.extend_from_slice(&[0; 32]); // 16 x 16 zero bits of match length extra
        sec.push(1); // the end mark of the bitstream
        sec
    }

    #[test]
    fn cancel_inside_block() {
        let sec = big_block();
        let mut out = vec![0; 2 << 20];
        let mut d = Decoder::new(&Limits::default()).unwrap();
        let n = d.decode_block(
            &mut out,
            0,
            0,
            BLOCK_SIZE_MAX,
            usize::MAX,
            &sec,
            &AtomicBool::new(false),
        );
        assert_eq!(n, Ok(16 * 65540));
        let mut d = Decoder::new(&Limits::default()).unwrap();
        let n = d.decode_block(
            &mut out,
            0,
            0,
            BLOCK_SIZE_MAX,
            usize::MAX,
            &sec,
            &AtomicBool::new(true),
        );
        assert_eq!(n, Err(Error::Cancelled));
    }

    /// 3 sequences as in `big_block`, in a 7-byte bitstream: under 16 bytes left, so every
    /// sequence takes the slow path (`decode_careful`), and `run` checks the flag once the output
    /// passes `yield_at`, as it does after the fast loop.
    #[test]
    fn cancel_inside_block_slow_path() {
        let mut sec = vec![3 << 3, 7, 7, 7]; // 3 raw literals
        sec.extend_from_slice(&[3, 0x54, 1, 0, 52]);
        sec.extend_from_slice(&[0; 6]);
        sec.push(1);
        let mut out = vec![0; 1 << 20];
        let mut run = |cancel: bool| {
            Decoder::new(&Limits::default()).unwrap().decode_block(
                &mut out,
                0,
                0,
                BLOCK_SIZE_MAX,
                usize::MAX,
                &sec,
                &AtomicBool::new(cancel),
            )
        };
        assert_eq!(run(false), Ok(3 * 65540));
        assert_eq!(run(true), Err(Error::Cancelled));
    }
}
