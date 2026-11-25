//! The sequences section: FSE tables, their headers and the slow path of the sequence loop (ports
//! seq.go, which ports `ZSTD_decodeSeqHeaders` and `ZSTD_execSequenceEnd`).
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};

use crate::Decoder;
use crate::bits::{BitReader, highbit, read_ncount, spread_symbols};
use crate::error::Error;
use crate::seqloop::seq_loop;

// A table cell is one `ZSTD_seqSymbol`: next_state 0..15 | add_bits 16..23 | nb_bits 24..31 |
// base 32..63.
pub(crate) fn next_state(c: u64) -> usize {
    (c & 0xFFFF) as usize
}
pub(crate) fn add_bits(c: u64) -> usize {
    ((c >> 16) & 0xFF) as usize
}
pub(crate) fn nb_bits(c: u64) -> usize {
    ((c >> 24) & 0xFF) as usize
}
pub(crate) fn base(c: u64) -> u64 {
    c >> 32
}

const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    0x80, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 0x10000,
];
const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 0x83, 0x103, 0x203,
    0x403, 0x803, 0x1003, 0x2003, 0x4003, 0x8003, 0x10003,
];
const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];
const LL_NORM: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
const ML_NORM: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_NORM: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

const OF_BASE: [u32; 32] = {
    let mut b = [0u32; 32];
    let mut i = 0;
    while i < 32 {
        b[i] = (1u32 << i).wrapping_sub(3);
        i += 1;
    }
    b[0] = 0;
    b[1] = 1;
    b
};
const OF_BITS: [u8; 32] = {
    let mut n = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        n[i] = i as u8;
        i += 1;
    }
    n
};

/// Which array a table lives in.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Unset,
    Default,
    Own,
}

/// One of the three sequence tables: a kind and the table log.
#[derive(Clone, Copy)]
struct Tab {
    kind: Kind,
    log: usize,
}

const UNSET: Tab = Tab {
    kind: Kind::Unset,
    log: 0,
};

pub(crate) struct SeqTables {
    ll: [u64; 1 << 9],
    ml: [u64; 1 << 9],
    of: [u64; 1 << 8],
    // The predefined tables fill the start of arrays of the full size, so a table is always a
    // `[u64; 512]` (`[u64; 256]` for offsets) and the loop can index it with a mask.
    ll_def: [u64; 1 << 9],
    ml_def: [u64; 1 << 9],
    of_def: [u64; 1 << 8],
    ll_t: Tab,
    ml_t: Tab,
    of_t: Tab,
}

/// The three tables of a block and their logs.
pub(crate) struct Tables<'a> {
    pub ll: &'a [u64; 1 << 9],
    pub ml: &'a [u64; 1 << 9],
    pub of: &'a [u64; 1 << 8],
    pub ll_log: usize,
    pub ml_log: usize,
    pub of_log: usize,
}

impl SeqTables {
    pub fn new() -> Box<Self> {
        let mut t = Box::new(Self {
            ll: [0; 1 << 9],
            ml: [0; 1 << 9],
            of: [0; 1 << 8],
            ll_def: [0; 1 << 9],
            ml_def: [0; 1 << 9],
            of_def: [0; 1 << 8],
            ll_t: UNSET,
            ml_t: UNSET,
            of_t: UNSET,
        });
        build_seq_table(&mut t.ll_def, &LL_NORM, 35, 6, &LL_BASE, &LL_BITS);
        build_seq_table(&mut t.ml_def, &ML_NORM, 52, 6, &ML_BASE, &ML_BITS);
        build_seq_table(&mut t.of_def, &OF_NORM, 28, 5, &OF_BASE, &OF_BITS);
        t
    }

    pub fn tables(&self) -> Tables<'_> {
        let ll = match self.ll_t.kind {
            Kind::Default => &self.ll_def,
            _ => &self.ll,
        };
        let ml = match self.ml_t.kind {
            Kind::Default => &self.ml_def,
            _ => &self.ml,
        };
        let of = match self.of_t.kind {
            Kind::Default => &self.of_def,
            _ => &self.of,
        };
        Tables {
            ll,
            ml,
            of,
            ll_log: self.ll_t.log,
            ml_log: self.ml_t.log,
            of_log: self.of_t.log,
        }
    }
}

/// Ports `ZSTD_buildFSETable_body`.
fn build_seq_table(
    dt: &mut [u64],
    norm: &[i16],
    max_sv: usize,
    table_log: usize,
    base: &[u32],
    add_bits: &[u8],
) {
    let mut sym = [0u8; 1 << 9];
    // Indexed by s & 63 so the hot loop has no bounds checks.
    let mut next = [0u16; 64];
    let mut tmpl = [0u64; 64];
    for s in 0..=max_sv {
        next[s] = if norm[s] == -1 { 1 } else { norm[s] as u16 };
        tmpl[s] = (u64::from(add_bits[s]) << 16) | (u64::from(base[s]) << 32);
    }
    spread_symbols(&mut sym, norm, max_sv, table_log);
    let size = 1usize << table_log;
    for (cell, &s) in dt[..size].iter_mut().zip(&sym[..size]) {
        let s = usize::from(s & 63);
        let ns = usize::from(next[s]);
        next[s] += 1;
        let nb = table_log.wrapping_sub(highbit(ns as u32));
        let new_state = ns.wrapping_shl(nb as u32).wrapping_sub(size);
        *cell = tmpl[s] | (new_state as u64) | ((nb as u64) << 24);
    }
}

/// Ports `ZSTD_buildSeqTable` for one of the three tables. It returns the bytes read.
#[allow(clippy::too_many_arguments)]
fn build_seq(
    mode: u8,
    src: &[u8],
    own: &mut [u64],
    def_log: usize,
    max_sv: usize,
    max_log: usize,
    base: &[u32],
    add_bits: &[u8],
    fse_ok: bool,
    tab: &mut Tab,
) -> Option<usize> {
    match mode {
        0 => {
            // predefined
            *tab = Tab {
                kind: Kind::Default,
                log: def_log,
            };
            Some(0)
        }
        1 => {
            // RLE
            let s = usize::from(*src.first()?);
            if s > max_sv {
                return None;
            }
            own[0] = (u64::from(add_bits[s]) << 16) | (u64::from(base[s]) << 32);
            *tab = Tab {
                kind: Kind::Own,
                log: 0,
            };
            Some(1)
        }
        2 => {
            // FSE compressed
            let mut norm = [0i16; 53];
            let (n, max_sv, table_log) = read_ncount(&mut norm, max_sv, src)?;
            if table_log > max_log {
                return None;
            }
            build_seq_table(own, &norm, max_sv, table_log, base, add_bits);
            *tab = Tab {
                kind: Kind::Own,
                log: table_log,
            };
            Some(n)
        }
        // repeat
        _ => fse_ok.then_some(0),
    }
}

impl Decoder {
    /// Ports `ZSTD_decodeSeqHeaders`. It returns the sequence count and the bytes read.
    pub(crate) fn decode_seq_headers(&mut self, src: &[u8]) -> Result<(usize, usize), Error> {
        const ERR: Error = Error::Corrupt("bad sequences section");
        const FSE: Error = Error::Corrupt("bad FSE table");
        let Some(&first) = src.first() else {
            return Err(ERR);
        };
        let mut nb_seq = usize::from(first);
        let mut ip = 1usize;
        if nb_seq > 0x7F {
            if nb_seq == 0xFF {
                if ip + 2 > src.len() {
                    return Err(ERR);
                }
                nb_seq = (usize::from(src[1]) | (usize::from(src[2]) << 8)) + 0x7F00;
                ip += 2;
            } else {
                if ip >= src.len() {
                    return Err(ERR);
                }
                nb_seq = ((nb_seq - 0x80) << 8) + usize::from(src[1]);
                ip += 1;
            }
        }
        if nb_seq == 0 {
            if ip != src.len() {
                return Err(ERR);
            }
            return Ok((0, ip));
        }
        if ip + 1 > src.len() || src[ip] & 3 != 0 {
            return Err(ERR);
        }
        let modes = src[ip];
        ip += 1;
        let t = &mut *self.seq;
        let fse_ok = self.fse_ok;
        let k = build_seq(
            modes >> 6,
            &src[ip..],
            &mut t.ll,
            6,
            35,
            9,
            &LL_BASE,
            &LL_BITS,
            fse_ok,
            &mut t.ll_t,
        )
        .ok_or(FSE)?;
        ip += k;
        let k = build_seq(
            (modes >> 4) & 3,
            &src[ip..],
            &mut t.of,
            5,
            31,
            8,
            &OF_BASE,
            &OF_BITS,
            fse_ok,
            &mut t.of_t,
        )
        .ok_or(FSE)?;
        ip += k;
        let k = build_seq(
            (modes >> 2) & 3,
            &src[ip..],
            &mut t.ml,
            6,
            52,
            9,
            &ML_BASE,
            &ML_BITS,
            fse_ok,
            &mut t.ml_t,
        )
        .ok_or(FSE)?;
        Ok((nb_seq, ip + k))
    }
}

/// The state of the fused decode+execute loop.
pub(crate) struct SeqState<'a> {
    pub br: BitReader<'a>,
    pub ll_t: &'a [u64; 1 << 9],
    pub ml_t: &'a [u64; 1 << 9],
    pub of_t: &'a [u64; 1 << 8],
    pub ll_s: usize,
    pub ml_s: usize,
    pub of_s: usize,
    pub rep: [u64; 3],
    pub nb_seq: usize,

    /// `out.len()` is the end of the frame's output.
    pub out: &'a mut [u8],
    pub op: usize,
    /// Frame start; matches may not reach before it.
    pub base: usize,
    pub lits: &'a [u8],
    pub lp: usize,
    pub lit_end: usize,

    /// The decoded sequence left for `exec_end`.
    pub ll: u64,
    pub ml: u64,
    pub off: u64,

    /// Checked every `CANCEL_STRIDE` bytes of output.
    pub cancel: &'a AtomicBool,
    pub yield_at: usize,
}

/// Output bytes between cancel checks inside a block (a block with a content size is not bounded
/// by 128 KiB of output).
pub(crate) const CANCEL_STRIDE: usize = 128 << 10;

/// Results of `seq_loop`.
pub(crate) const SEQ_DONE: u8 = 0; // no sequences left
pub(crate) const SEQ_SLOW_BITS: u8 = 1; // bitstream near its start: decode one carefully
pub(crate) const SEQ_PENDING: u8 = 2; // ll, ml, off need exec_end
pub(crate) const SEQ_BAD_OFFSET: u8 = 3; // match reaches before the frame

impl SeqState<'_> {
    /// Decodes and executes all sequences of a block.
    pub fn run(&mut self) -> Result<(), Error> {
        while self.nb_seq > 0 {
            match seq_loop(self) {
                SEQ_SLOW_BITS => {
                    let (ll, ml, off) = self.decode_careful();
                    self.exec_end(ll, ml, off)?;
                }
                SEQ_PENDING => self.exec_end(self.ll, self.ml, self.off)?,
                SEQ_BAD_OFFSET => return Err(Error::Corrupt("match offset out of range")),
                _ => {}
            }
            // The fast loop leaves a sequence that reaches `yield_at` to `exec_end`.
            if self.op >= self.yield_at {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                self.yield_at = self.op + CANCEL_STRIDE;
            }
        }
        Ok(())
    }

    /// Ports `ZSTD_decodeSequence` with the checked reloads.
    fn decode_careful(&mut self) -> (u64, u64, u64) {
        let br = &mut self.br;
        let lld = self.ll_t[self.ll_s];
        let mld = self.ml_t[self.ml_s];
        let ofd = self.of_t[self.of_s];
        let mut ll = base(lld);
        let mut ml = base(mld);
        let (ll_bits, ml_bits, of_bits) = (add_bits(lld), add_bits(mld), add_bits(ofd));
        let [mut r0, mut r1, mut r2] = self.rep;
        let off;
        if of_bits > 1 {
            off = base(ofd) + br.read_fast(of_bits);
            (r2, r1, r0) = (r1, r0, off);
        } else {
            let ll0 = u64::from(ll == 0);
            if of_bits == 0 {
                if ll0 == 1 {
                    off = r1;
                    (r1, r0) = (r0, off);
                } else {
                    off = r0;
                }
            } else {
                let o = base(ofd) + ll0 + br.read_fast(1);
                let mut t = match o {
                    1 => r1,
                    3 => r0.wrapping_sub(1),
                    _ => r2,
                };
                if t == 0 {
                    t = t.wrapping_sub(1); // offset 0 is corrupt; -1 fails the range check
                }
                if o != 1 {
                    r2 = r1;
                }
                (r1, r0) = (r0, t);
                off = t;
            }
        }
        self.rep = [r0, r1, r2];
        if ml_bits > 0 {
            ml += br.read_fast(ml_bits);
        }
        if ll_bits + ml_bits + of_bits >= 57 - (9 + 9 + 8) {
            br.reload();
        }
        if ll_bits > 0 {
            ll += br.read_fast(ll_bits);
        }
        if self.nb_seq > 1 {
            self.ll_s = next_state(lld) + br.read(nb_bits(lld)) as usize;
            self.ml_s = next_state(mld) + br.read(nb_bits(mld)) as usize;
            self.of_s = next_state(ofd) + br.read(nb_bits(ofd)) as usize;
            br.reload();
        }
        self.nb_seq -= 1;
        (ll, ml, off)
    }

    /// Ports `ZSTD_execSequenceEnd` without dictionaries: exact copies, nothing written past the
    /// match end.
    fn exec_end(&mut self, ll: u64, ml: u64, off: u64) -> Result<(), Error> {
        let (out, op) = (&mut *self.out, self.op);
        if ll + ml > (out.len() - op) as u64 || ll > (self.lit_end - self.lp) as u64 {
            return Err(Error::Corrupt("bad sequences section"));
        }
        let o_lit_end = op + ll as usize;
        out[op..o_lit_end].copy_from_slice(&self.lits[self.lp..self.lp + ll as usize]);
        self.lp += ll as usize;
        if off == 0 || off > (o_lit_end - self.base) as u64 {
            return Err(Error::Corrupt("match offset out of range"));
        }
        let m = o_lit_end - off as usize;
        let end = o_lit_end + ml as usize;
        if off >= ml {
            out.copy_within(m..m + ml as usize, o_lit_end);
        } else {
            // out[m..i] is whole periods, so it doubles safely.
            let mut i = o_lit_end;
            while i < end {
                let n = (end - i).min(i - m);
                out.copy_within(m..m + n, i);
                i += n;
            }
        }
        self.op = end;
        Ok(())
    }
}
