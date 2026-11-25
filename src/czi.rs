//! CZI subblock payloads compressed with zstd0 (mode 5) or zstd1 (mode 6), as libCZI decodes
//! them (ports czi/czi.go; see zstd-go's docs/format.md).
#![forbid(unsafe_code)]

use std::sync::atomic::AtomicBool;

use crate::error::Error;
use crate::frame::Out;
use crate::interleave::interleave;
use crate::{Limits, content_size};

/// Compression mode zstd0, as stored in the subblock.
pub const ZSTD0: u32 = 5;
/// Compression mode zstd1, as stored in the subblock.
pub const ZSTD1: u32 = 6;

/// Pixel type Gray16, as stored in the subblock. Hi/lo packing needs 16-bit words.
pub const GRAY16: u32 = 1;
/// Pixel type Bgr48, as stored in the subblock.
pub const BGR48: u32 = 4;

const BAD_HEADER: Error = Error::Corrupt("czi: bad zstd1 header");
const BAD_HILO: Error = Error::Corrupt("czi: hi/lo packing needs Gray16 or Bgr48 and an even size");
const NO_SIZE: Error = Error::Corrupt("czi: frame has no content size");
const SIZE_LEFT: Error =
    Error::Corrupt("czi: frames decode to more than the first frame's content size");

/// A reusable decoder.
pub struct Decoder {
    z: crate::Decoder,
    /// Decoded planes. Its length is the writable extent, not the decoded size.
    blob: Vec<u8>,
}

impl Decoder {
    /// A decoder; see [`crate::Decoder::new`] for what `limits` checks.
    ///
    /// # Errors
    ///
    /// [`Error::AllocLimit`] if the tables do not fit `limits.max_alloc`, [`Error::OutOfMemory`]
    /// if the allocator refuses.
    pub fn new(limits: &Limits) -> Result<Self, Error> {
        Ok(Self {
            z: crate::Decoder::new(limits)?,
            blob: Vec::new(),
        })
    }

    /// Decodes `src`, a subblock payload of the given compression `mode`, into `dst`.
    /// `dst.len()` must be the bitmap size at minimal stride (width * height * bytes per pixel).
    /// `pixel_type` is only used to check hi/lo packing. As libCZI's default does, a frame content
    /// size smaller than `dst.len()` leaves the rest of `dst` zero, and a larger one is cut to
    /// `dst.len()`. On error the contents of `dst` are undefined.
    ///
    /// `limits.max_alloc` caps what one call allocates for the scratch buffer, which holds the
    /// whole frame. The decoder keeps that buffer for the next call (the growth of the frames
    /// that decoded, as zstd-go keeps it) and does not charge it again, so a call under a smaller
    /// `max_alloc` than an earlier one may hold the earlier one's buffer. A frame that decodes
    /// straight into `dst` allocates nothing; output that outgrows `dst` (a later frame) is moved
    /// to a buffer that grows from `dst.len()` as zstd-go grows `dst`'s capacity, each growth
    /// charged and `dst` not, so that it fails with the error of a later frame, as in Go.
    /// `limits.max_output` caps the frame content size ([`content_size`]). A payload of several
    /// frames that fits `dst` (when it decodes straight into it) or the scratch buffer kept from an
    /// earlier call may decode on up to `limits.max_threads` threads, with the same result (see
    /// [`Limits::max_threads`]).
    /// The cancel flag is checked as [`crate::Decoder::decode_all_into`]'s is.
    ///
    /// # Errors
    ///
    /// [`Error::Mode`] if `mode` is not [`ZSTD0`] or [`ZSTD1`]; [`Error::Corrupt`] for a bad
    /// zstd1 header, hi/lo packing that needs Gray16 or Bgr48 and an even size, a frame without
    /// a content size, frames that decode to more than the first frame's content size, and every
    /// malformed frame; [`Error::OutputLimit`] if the content size passes `limits.max_output`;
    /// [`Error::AllocLimit`] if the scratch buffer would pass `limits.max_alloc`;
    /// [`Error::Cancelled`] if `cancel` is set; [`Error::Checksum`], [`Error::Dictionary`],
    /// [`Error::WindowTooLarge`] and [`Error::OutOfMemory`] as for [`crate::Decoder`].
    pub fn decode(
        &mut self,
        dst: &mut [u8],
        src: &[u8],
        mode: u32,
        pixel_type: u32,
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let mut src = src;
        let mut hilo = false;
        match mode {
            ZSTD0 => {}
            ZSTD1 => {
                let (n, h) = parse_header(src)?;
                // 16-bit pixels make an even bitmap size.
                if h && ((pixel_type != GRAY16 && pixel_type != BGR48)
                    || !dst.len().is_multiple_of(2))
                {
                    return Err(BAD_HILO);
                }
                src = &src[n..];
                hilo = h;
            }
            _ => return Err(Error::Mode),
        }
        let fcs = content_size(src, limits)?.ok_or(NO_SIZE)?;
        let Self { z, blob } = self;
        if hilo {
            // Interleaves each block's words into dst while the block is still in cache.
            let half = usize::try_from(fcs / 2).unwrap_or(usize::MAX >> 2);
            let mut done = 0usize; // words packed so far
            let mut pack = |out: &[u8]| {
                let n = (out.len() as i64 - half as i64)
                    .min(half as i64)
                    .min((dst.len() / 2) as i64);
                if n <= done as i64 {
                    return;
                }
                let n = n as usize;
                interleave(
                    &mut dst[2 * done..2 * n],
                    &out[done..n],
                    &out[half + done..half + n],
                );
                done = n;
            };
            decode_blob(z, blob, src, fcs, limits, cancel, &mut pack)?;
            let words = half.min(dst.len() / 2);
            dst[2 * words..].fill(0);
            return Ok(());
        }
        // As Go does, straight into dst when the content fits. Anything else goes the long way.
        if fcs <= dst.len() as u64 {
            // More output than dst holds spills into a buffer that is charged, as Go grows a slice
            // of dst's capacity (charging only the growth), and then fails: with the error of a
            // later frame, or with a size mismatch, as the output is over `fcs`.
            let mut spill = Vec::new();
            let end = z.decode_frames(
                &mut Out::Spill(dst, &mut spill),
                0,
                src,
                limits,
                cancel,
                &mut |_: &[u8]| {},
            )?;
            if end as u64 != fcs {
                return Err(SIZE_LEFT);
            }
            dst[end..].fill(0);
            return Ok(());
        }
        let n = decode_blob(z, blob, src, fcs, limits, cancel, &mut |_: &[u8]| {})?;
        let k = n.min(dst.len());
        dst[..k].copy_from_slice(&blob[..k]);
        dst[k..].fill(0);
        Ok(())
    }
}

/// Decodes `src` into `blob` and checks the total size against `fcs`. It returns the size.
fn decode_blob<F: FnMut(&[u8])>(
    z: &mut crate::Decoder,
    blob: &mut Vec<u8>,
    src: &[u8],
    fcs: u64,
    limits: &Limits,
    cancel: &AtomicBool,
    on_block: &mut F,
) -> Result<usize, Error> {
    let mut out = Out::grow_vec(&mut *blob, 0);
    let r = z.decode_frames(&mut out, 0, src, limits, cancel, on_block);
    // The buffer's length is its writable extent, which the next call grows from: the decode
    // initialized only what it wrote. On an error, only the extent of the frames that decoded.
    let filled = out.init_to(if r.is_ok() { usize::MAX } else { z.kept }, cancel);
    if r.is_err() {
        // As in Go, where a failed DecodeAll returns the buffer of its last frame that decoded:
        // the growth of a frame that failed is dropped, that of the frames before it is kept.
        blob.truncate(z.kept);
        blob.shrink_to_fit();
    }
    let end = r?;
    filled?;
    if end as u64 != fcs {
        return Err(SIZE_LEFT);
    }
    Ok(end)
}

/// Parses a zstd1 header: `{01}`, or `{03 01 xx}` where bit 0 of `xx` is the hi/lo flag. It
/// reports the header size.
fn parse_header(src: &[u8]) -> Result<(usize, bool), Error> {
    if src.len() > 1 && src[0] == 1 {
        Ok((1, false))
    } else if src.len() > 3 && src[0] == 3 && src[1] == 1 {
        Ok((3, src[2] & 1 != 0))
    } else {
        Err(BAD_HEADER)
    }
}
