//! The fused sequence decode+execute fast loop. It ports seqloop_amd64.s, the asm form of
//! seqloop.go, which ports `ZSTD_decodeSequence` (aarch64 variant) and the wildcopy path of
//! `ZSTD_execSequence`. It returns to `SeqState::run` for everything else.
//!
//! The body is written once and built twice: plain, and on x86_64 with BMI1 and BMI2 (SHRX, BZHI)
//! for CPUs that have them, as Go's asm requires them. `ZSTD_SIMD=scalar` forces the plain build.
//! The body copies through raw pointers without bounds checks: it proves the bounds in safe code,
//! before the loop and once per sequence, as the asm does.
use std::ptr;

use crate::WILDCOPY_SLACK;
use crate::seq::{SEQ_BAD_OFFSET, SEQ_DONE, SEQ_PENDING, SEQ_SLOW_BITS, SeqState};

/// `ZSTD_overlapCopy8` for offsets 1 to 7, by offset (the asm's `ovTab`), in one table so one
/// address serves all four rows: the source bytes that repeat in the first 4 output bytes, the
/// multiplier that repeats them, where the next 4 output bytes come from past the match source
/// (`dec32`), and where the 8-byte loop's source starts (`dec32 + 8 - dec64`), 8 to 15 bytes
/// behind its output.
static OV: [[u32; 8]; 4] = [
    [0, 0xFF, 0xFFFF, 0xFF_FFFF, !0, !0, !0, !0],
    [0, 0x0101_0101, 0x0001_0001, 0x0100_0001, 1, 1, 1, 1],
    [0, 1, 2, 1, 4, 4, 4, 4],
    [0, 1, 2, 2, 4, 3, 2, 1],
];

/// The `n` bits of `bits` from bit `at` up, where `n` is the low byte of `nb` (`n` = 0 gives 0).
/// With BMI2 this is SHRX and BZHI, which reads only that byte, so `nb` can be the low half of a
/// table cell rotated to put the count in its low byte (`rot`), as the asm's RORX does; the plain
/// form takes the byte (the tables keep it at 31 or less).
///
/// # Safety
/// With `BMI2`, the CPU has BMI1 and BMI2.
#[inline(always)]
unsafe fn get<const BMI2: bool>(bits: u64, at: u8, nb: u32) -> u64 {
    let x = bits >> (at & 63);
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    if BMI2 {
        // SAFETY: the CPU has BMI2, by the caller's contract.
        return unsafe { std::arch::x86_64::_bzhi_u64(x, nb) };
    }
    x & ((1u64 << (nb as u8 & 63)) - 1)
}

/// The low half of cell `c` rotated right by `n` bits: one RORX with BMI2, whose 32-bit result
/// needs no zero-extension for BZHI (a 64-bit rotate or shift that is cut to a byte compiles to a
/// shift and a zero-extension).
#[inline(always)]
fn rot(c: u64, n: u32) -> u32 {
    (c as u32).rotate_right(n)
}

/// The base of a table cell (bits 32..63) plus `v`, which is below 2^31: the sum fits 32 bits.
#[inline(always)]
fn add_base(c: u64, v: u64) -> u32 {
    (c.rotate_right(32) as u32).wrapping_add(v as u32)
}

/// The 8 bytes at `p`, little-endian.
///
/// # Safety
/// `p` is readable for 8 bytes.
#[inline(always)]
pub(crate) unsafe fn load64(p: *const u8) -> u64 {
    // SAFETY: readable by the caller's contract; the read is unaligned.
    u64::from_le(unsafe { p.cast::<u64>().read_unaligned() })
}

/// # Safety
/// `s` is readable and `d` writable for 16 bytes, and the two do not overlap.
#[inline(always)]
unsafe fn copy16(d: *mut u8, s: *const u8) {
    // SAFETY: the caller's contract is `copy_nonoverlapping`'s.
    unsafe { ptr::copy_nonoverlapping(s, d, 16) }
}

/// # Safety
/// `s` is readable and `d` writable for 8 bytes, and the two do not overlap.
#[inline(always)]
unsafe fn copy8(d: *mut u8, s: *const u8) {
    // SAFETY: the caller's contract is `copy_nonoverlapping`'s.
    unsafe { ptr::copy_nonoverlapping(s, d, 8) }
}

/// Copies `ll` literals from `s` to `d`: 16 bytes, then 32 at a time while fewer than `ll` are
/// done, so it touches fewer than `ll + 32` bytes of each.
///
/// # Safety
/// `s` is readable and `d` writable for `ll + 32` bytes, and the two do not overlap.
#[inline(always)]
unsafe fn copy_lits(d: *mut u8, s: *const u8, ll: usize) {
    // SAFETY: each move starts below `ll` (the first at 0, the others at `i < ll`) and ends at
    // most 32 bytes later, within the caller's `ll + 32` bytes.
    unsafe {
        copy16(d, s);
        if ll > 16 {
            let mut i = 16;
            loop {
                copy16(d.add(i), s.add(i));
                copy16(d.add(i + 16), s.add(i + 16));
                i += 32;
                if i >= ll {
                    break;
                }
            }
        }
    }
}

/// Copies the match at `m = o - off` to `o..end`, overlap included, as the wildcopy path of
/// `ZSTD_execSequence` does: it may write up to 31 bytes past `end`.
///
/// # Safety
/// `1 <= off`, `m = o - off`, `o <= end`, all three in one allocation, `m..end + 32` valid for
/// reads and `o..end + 32` for writes.
#[inline(always)]
unsafe fn copy_match(o: *mut u8, m: *const u8, off: usize, end: *mut u8) {
    // SAFETY: every write starts at or below `end` and is at most 16 bytes, or 32 for a pair of
    // moves that starts below `end`, so it ends before `end + 32`. Every read starts at or after
    // `m` and lies before the end of its write. A 16-byte move is `off >= 16` bytes behind its
    // write, an 8-byte move 8 to 15 bytes (`OV[3]`), so neither overlaps; the second move of a
    // pair reads only bytes the first one already wrote. The `off < 8` setup reads `m[..4]` and
    // `m[dec32..dec32 + 4]`, inside `m..o + 8`; the bits it does not keep are masked off.
    unsafe {
        if off >= 16 {
            copy16(o, m);
            let (mut o, mut m) = (o.add(16), m.add(16));
            while o < end {
                copy16(o, m);
                copy16(o.add(16), m.add(16));
                o = o.add(32);
                m = m.add(32);
            }
        } else {
            let mut m = if off < 8 {
                // ZSTD_overlapCopy8: the multiply repeats a 1 to 3 byte pattern over 4 bytes.
                let v = u32::from_le(m.cast::<u32>().read_unaligned());
                let v = (v & OV[0][off]).wrapping_mul(OV[1][off]);
                o.cast::<u32>().write_unaligned(v.to_le());
                let v = m.add(OV[2][off] as usize).cast::<u32>().read_unaligned();
                o.add(4).cast::<u32>().write_unaligned(v);
                m.add(OV[3][off] as usize)
            } else {
                copy8(o, m);
                m.add(8)
            };
            let mut o = o.add(8);
            while o < end {
                copy8(o, m);
                o = o.add(8);
                m = m.add(8);
            }
        }
    }
}

/// The loop body, for both builds. It runs sequences while the bitstream has 16+ bytes left, so
/// every reload is a full one, and while each copy fits the wildcopy fast path. Like the asm it
/// works on pointers, keeps the next sequence's table cells (not only the states) between
/// sequences, and reads each field's bit count from a rotated cell.
///
/// # Safety
/// With `BMI2`, the CPU has BMI1 and BMI2: `body::<true>` is called only from `seq_loop_bmi2`.
#[inline(always)]
unsafe fn body<const BMI2: bool>(s: &mut SeqState) -> u8 {
    // `get::<BMI2>`, whose contract is this function's.
    macro_rules! get {
        ($bits:expr, $at:expr, $nb:expr) => {
            // SAFETY: with `BMI2`, the CPU has BMI1 and BMI2, by the caller's contract.
            unsafe { get::<BMI2>($bits, $at, $nb) }
        };
    }
    let inp = s.br.inp;
    let lits = s.lits;
    let (ptr, mut bits, consumed) = (s.br.ptr, s.br.bits, s.br.consumed);
    let mut nb = s.nb_seq;
    if nb == 0 {
        return SEQ_DONE;
    }
    // The preconditions of the raw-pointer code. When one fails, the call takes the careful path
    // one sequence at a time, which decodes the same. `consumed <= 7` (the state after any full
    // reload) keeps every read below 64 bits between reloads (see the reloads).
    let out = &mut *s.out;
    if ptr < 16
        || ptr.saturating_add(8) > inp.len()
        || consumed > 7
        || out.len() < WILDCOPY_SLACK
        || lits.len() < WILDCOPY_SLACK
        || s.base > s.op
    {
        return SEQ_SLOW_BITS;
    }
    let (ib, lb, ob) = (inp.as_ptr(), lits.as_ptr(), out.as_mut_ptr());
    // Copies end at most 32 bytes past these offsets into `out` and `lits`. A sequence past either
    // is left to `exec_end`, which checks it against `lit_end` and the end of `out` and copies
    // exactly. A sequence that ends at or past `yield_at` is left to it too, so `run` checks the
    // cancel flag and the loop need not test `yield_at`.
    let oend = (out.len() - WILDCOPY_SLACK).min(s.yield_at.saturating_sub(1));
    let lit_lim = s.lit_end.min(lits.len() - WILDCOPY_SLACK);
    let base = ob.addr() + s.base;
    let (ll_t, ml_t, of_t) = (s.ll_t, s.ml_t, s.of_t);
    let (mut ll_s, mut ml_s, mut of_s) = (s.ll_s, s.ml_s, s.of_s);
    // A state is below its table size, so the masks change nothing and drop the bounds checks.
    let (mut lld, mut mld, mut ofd) = (ll_t[ll_s & 511], ml_t[ml_s & 511], of_t[of_s & 255]);
    let [mut r0, mut r1, mut r2] = s.rep;
    // Pointers only move forward from in-bounds starts; every copy is checked before it runs.
    let mut ip = ib.wrapping_add(ptr);
    let mut op = ob.wrapping_add(s.op);
    let mut lp = lb.wrapping_add(s.lp);
    // `bx` is 64 minus the bits consumed: a field of `n` bits starts at bit `bx - n`. It stays in
    // 0..=64, so byte subtractions do not wrap.
    let mut bx = 64 - consumed as u8;
    // The ip check at the end of a sequence; after the last one it always fails.
    let mut lim = ib.addr() + 16;
    let status;
    loop {
        // A cell is next_state 0..15 | extra bits 16..23 | state bits 24..31 | base 32..63.
        // The offset is always the new rep[0].
        let oft = rot(ofd, 16);
        if oft as u8 > 1 {
            bx = bx.wrapping_sub(oft as u8);
            (r2, r1) = (r1, r0);
            r0 = u64::from(add_base(ofd, get!(bits, bx, oft)));
        } else {
            let ll0 = lld >> 32 == 0;
            if oft as u8 == 0 {
                if ll0 {
                    (r0, r1) = (r1, r0);
                }
            } else {
                bx = bx.wrapping_sub(1);
                let o = (ofd >> 32) + u64::from(ll0) + get!(bits, bx, 1);
                let t = match o {
                    1 => r1,
                    3 => r0.wrapping_sub(1),
                    _ => r2,
                };
                // Offset 0 is corrupt; -1 fails the range check.
                let t = t.wrapping_sub(u64::from(t == 0));
                if o != 1 {
                    r2 = r1;
                }
                (r1, r0) = (r0, t);
            }
        }
        let mlt = rot(mld, 16);
        bx = bx.wrapping_sub(mlt as u8);
        let ml = add_base(mld, get!(bits, bx, mlt)) as usize;
        // One add sums the three cells' fields: next states below 512 do not carry into the extra
        // bits, and those (at most 16 + 16 + 31, plus 1) do not carry into the state bits. With
        // the 1, bits 5 and 6 of the extra-bits byte test `ll + ml + of extra bits >= 31`.
        let sum = lld
            .wrapping_add(mld)
            .wrapping_add(ofd)
            .wrapping_add(0x1_0000);
        if sum & 0x60_0000 != 0 {
            // At most 7 + 31 + 16 bits are consumed here, so `k` is the whole bytes consumed;
            // the mask also caps the step at 7 bytes for the SAFETY argument below.
            let k = 64u8.wrapping_sub(bx) & 0x38;
            bx = bx.wrapping_add(k);
            ip = ip.wrapping_sub(usize::from(k >> 3));
            // SAFETY: `ip + 8 <= ib + inp.len()` held at entry and `ip` only decreases.
            // `ip >= ib + 16` at the top of each sequence (checked at entry and at the end of
            // each sequence), and each of the two reloads in a sequence moves it back at most 7
            // bytes, so `ip >= ib + 2` and `ip..ip + 8` lies in `inp`.
            bits = unsafe { load64(ip) };
        }
        let llt = rot(lld, 16);
        bx = bx.wrapping_sub(llt as u8);
        let ll = add_base(lld, get!(bits, bx, llt)) as usize;
        nb -= 1;
        if nb > 0 {
            // Each state's bits start at a position known without the reads before it, so the
            // three reads do not wait on each other. A next state plus its bits is below the
            // table size, so the mask (in place of taking the 16-bit field) changes nothing.
            let n_all = rot(sum, 24) as u8;
            let n_lm = rot(lld.wrapping_add(mld), 24) as u8;
            let n_ll = rot(lld, 24);
            ll_s = (lld as usize + get!(bits, bx.wrapping_sub(n_ll as u8), n_ll) as usize) & 511;
            lld = ll_t[ll_s];
            let n_ml = rot(mld, 24);
            ml_s = (mld as usize + get!(bits, bx.wrapping_sub(n_lm), n_ml) as usize) & 511;
            mld = ml_t[ml_s];
            let n_of = rot(ofd, 24);
            of_s = (ofd as usize + get!(bits, bx.wrapping_sub(n_all), n_of) as usize) & 255;
            ofd = of_t[of_s];
            bx = bx.wrapping_sub(n_all);
            // At most 7 + 30 + 26 bits, or 7 + 16 + 26 after the reload above, are consumed here.
            let k = 64u8.wrapping_sub(bx) & 0x38;
            bx = bx.wrapping_add(k);
            ip = ip.wrapping_sub(usize::from(k >> 3));
            // SAFETY: as for the reload above.
            bits = unsafe { load64(ip) };
        } else {
            lim = usize::MAX;
        }

        // Tested as offsets, which cannot overflow where an address could: `lp - lb` and
        // `op - ob` are at most `isize::MAX` (the lengths of `lits` and `out`), and `ll` and `ml`
        // are below 2^18 (the largest base, 2^16 + 3, plus 16 extra bits), so the wrapping
        // differences are the exact offsets of the ends.
        let lp_end = lp.wrapping_add(ll);
        let o_lit_end = op.wrapping_add(ll);
        let o_match_end = o_lit_end.wrapping_add(ml);
        if lp_end.addr().wrapping_sub(lb.addr()) > lit_lim
            || o_match_end.addr().wrapping_sub(ob.addr()) > oend
        {
            (s.ll, s.ml, s.off) = (ll as u64, ml as u64, r0);
            status = SEQ_PENDING;
            break;
        }
        // SAFETY: `lb <= lp` (it starts at `lb + s.lp` and only grows) and
        // `lp + ll + 32 <= lb + lit_lim + 32 <= lb + lits.len()`; `ob <= op` likewise and
        // `op + ll + 32 <= o_match_end + 32 <= ob + oend + 32 <= ob + out.len()`, by the test above
        // (both lengths are 32 or more, checked at entry). `lits` and `out` are distinct slices.
        unsafe { copy_lits(op, lp, ll) };
        lp = lp_end;

        // `base <= m < o_lit_end`, so `1 <= off <= o_lit_end - base`; a wrapped `m` (off past
        // the start of the address space) and off 0 both fail it.
        let m = o_lit_end.wrapping_sub(r0 as usize);
        if m.addr() >= o_lit_end.addr() || m.addr() < base {
            status = SEQ_BAD_OFFSET;
            break;
        }
        // SAFETY: `ob <= base <= m < o_lit_end <= o_match_end` by the test above (`s.base <= s.op`
        // at entry), so `off >= 1` and all three lie in `out`, and `o_match_end + 32 <= ob +
        // out.len()` by the test before the literal copy.
        unsafe { copy_match(o_lit_end, m, r0 as usize, o_match_end) };
        op = o_match_end;
        if ip.addr() < lim {
            status = if nb == 0 { SEQ_DONE } else { SEQ_SLOW_BITS };
            break;
        }
    }
    s.br.ptr = ip.addr() - ib.addr();
    s.br.bits = bits;
    s.br.consumed = usize::from(64u8.wrapping_sub(bx));
    (s.ll_s, s.ml_s, s.of_s) = (ll_s, ml_s, of_s);
    s.rep = [r0, r1, r2];
    s.op = op.addr() - ob.addr();
    s.lp = lp.addr() - lb.addr();
    s.nb_seq = nb;
    status
}

#[inline(never)] // its own function, so its registers are its own
fn seq_loop_plain(s: &mut SeqState) -> u8 {
    // SAFETY: the plain build needs no CPU feature.
    unsafe { body::<false>(s) }
}

/// `body` with SHRX and BZHI.
#[cfg(all(target_arch = "x86_64", not(miri)))]
#[target_feature(enable = "bmi1,bmi2")]
#[inline(never)]
fn seq_loop_bmi2(s: &mut SeqState) -> u8 {
    // SAFETY: this `#[target_feature]` fn is called only where BMI1 and BMI2 were detected
    // (calling it is unsafe elsewhere), so the CPU has them.
    unsafe { body::<true>(s) }
}

/// BMI1 and BMI2 are there, and `ZSTD_SIMD=scalar` is not set.
#[cfg(all(target_arch = "x86_64", not(miri)))]
fn bmi2() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !std::env::var_os("ZSTD_SIMD").is_some_and(|v| v == "scalar")
            && is_x86_feature_detected!("bmi1")
            && is_x86_feature_detected!("bmi2")
    })
}

// Tests and the fuzz target pick the path: 0 the detected loop, 1 the plain loop, 2 the BMI2 loop
// (the plain one where the CPU lacks BMI2), 3 none (every sequence takes the careful path).
#[cfg(any(test, fuzzing))]
thread_local! {
    static FORCE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Runs sequences until one needs the careful path or `exec_end` (a sequence that reaches
/// `yield_at` included), or the block ends; the result says which.
pub(crate) fn seq_loop(s: &mut SeqState) -> u8 {
    #[cfg(any(test, fuzzing))]
    match FORCE.get() {
        1 => return seq_loop_plain(s),
        2 => {
            #[cfg(all(target_arch = "x86_64", not(miri)))]
            if is_x86_feature_detected!("bmi1") && is_x86_feature_detected!("bmi2") {
                // SAFETY: the CPU has BMI1 and BMI2, detected just above.
                return unsafe { seq_loop_bmi2(s) };
            }
            return seq_loop_plain(s);
        }
        3 => return SEQ_SLOW_BITS,
        _ => {}
    }
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    if bmi2() {
        // SAFETY: `bmi2()` is true only when `is_x86_feature_detected!` found BMI1 and BMI2.
        return unsafe { seq_loop_bmi2(s) };
    }
    seq_loop_plain(s)
}

/// For the differential fuzz target: `src` decoded with every sequence on the careful path, with
/// the plain loop and with the BMI2 loop (the plain one where the CPU lacks BMI2). Each gives its
/// result and the output of the blocks that decoded, so a block that decodes to different bytes
/// shows even when a later block or the checksum fails.
#[cfg(fuzzing)]
pub fn seqloop_modes(src: &[u8]) -> [(Result<(), crate::Error>, Vec<u8>); 3] {
    let limits = crate::Limits {
        max_output: 1 << 26,
        max_alloc: 1 << 27,
        ..crate::Limits::default()
    };
    let no = std::sync::atomic::AtomicBool::new(false);
    [3, 1, 2].map(|m| {
        FORCE.set(m);
        let (mut v, mut done) = (Vec::new(), 0);
        let r = crate::Decoder::new(&limits).and_then(|mut d| {
            d.decode_frames(
                &mut crate::frame::Out::Grow(&mut v, 0, 0),
                0,
                src,
                &limits,
                &no,
                &mut |o: &[u8]| done = o.len(),
            )
        });
        FORCE.set(0);
        v.truncate(done);
        (r.map(|_| ()), v)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::BitReader;
    use crate::{Decoder, Error, Limits};
    use std::sync::atomic::AtomicBool;

    static NO: AtomicBool = AtomicBool::new(false);

    fn with<T>(mode: u8, f: impl FnOnce() -> T) -> T {
        FORCE.set(mode);
        let r = f();
        FORCE.set(0);
        r
    }

    /// The tiles and the decodecorpus frames decode the same through the plain and BMI2 loops as
    /// with every sequence on the careful path.
    #[test]
    #[cfg_attr(miri, ignore)] // reads files
    fn frames_match_careful() {
        for dir in ["tiles", "decodecorpus"] {
            let dir = format!("{}/tests/data/{dir}", env!("CARGO_MANIFEST_DIR"));
            for e in std::fs::read_dir(dir).unwrap() {
                let path = e.unwrap().path();
                if path.extension().is_none_or(|x| x != "zst") {
                    continue;
                }
                let src = std::fs::read(&path).unwrap();
                let want = with(3, || crate::decode_all(&src, &Limits::default(), &NO));
                for mode in [1, 2] {
                    let got = with(mode, || crate::decode_all(&src, &Limits::default(), &NO));
                    assert!(got == want, "{} mode {mode}", path.display());
                }
            }
        }
    }

    fn rand(st: &mut u64, n: usize) -> usize {
        *st ^= *st << 13;
        *st ^= *st >> 7;
        *st ^= *st << 17;
        (*st >> 11) as usize % n.max(1)
    }

    /// What a run leaves: its result and, on success, the state and the output so far.
    type Run = (Result<(), Error>, Option<(Vec<u8>, [usize; 7], [u64; 4])>);

    /// Random blocks: tables from random sequence headers, random bitstreams, literals and
    /// history, short buffers and early yields included. Every mode leaves the same.
    #[test]
    fn random_streams_match_careful() {
        let iters = if cfg!(miri) { 40 } else { 20_000 };
        let mut st = 0x2545_f491_4f6c_dd1du64;
        let mut d = Decoder::new(&Limits::default()).unwrap();
        for _ in 0..iters {
            let mut hdr: Vec<u8> = (0..80).map(|_| rand(&mut st, 256) as u8).collect();
            hdr[0] = 1;
            hdr[1] &= 0xFC;
            if d.decode_seq_headers(&hdr).is_err() {
                continue;
            }
            d.fse_ok = true;
            let t = d.seq.tables();
            let mut bs: Vec<u8> = (0..1 + rand(&mut st, 300))
                .map(|_| rand(&mut st, 256) as u8)
                .collect();
            *bs.last_mut().unwrap() |= 1 << rand(&mut st, 8);
            let lits: Vec<u8> = (0..rand(&mut st, 600))
                .map(|_| rand(&mut st, 256) as u8)
                .collect();
            let lit_end = lits.len() - rand(&mut st, lits.len().min(40) + 1);
            let out: Vec<u8> = (0..rand(&mut st, 3000))
                .map(|_| rand(&mut st, 256) as u8)
                .collect();
            let op = rand(&mut st, out.len().min(800) + 1);
            let base = rand(&mut st, op + 1);
            let rep = [0; 3].map(|_| 1 + rand(&mut st, 20) as u64);
            let nb_seq = 1 + rand(&mut st, 80);
            let yield_at = op + rand(&mut st, 1000);
            let run = |mode: u8| -> Run {
                let mut out = out.clone();
                let mut br = BitReader::new(&bs).unwrap();
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
                    rep,
                    nb_seq,
                    out: &mut out,
                    op,
                    base,
                    lits: &lits,
                    lp: 0,
                    lit_end,
                    ll: 0,
                    ml: 0,
                    off: 0,
                    cancel: &NO,
                    yield_at,
                };
                let r = with(mode, || s.run());
                if r.is_err() {
                    return (r, None);
                }
                let state = [s.op, s.lp, s.br.ptr, s.br.consumed, s.ll_s, s.ml_s, s.of_s];
                let [r0, r1, r2] = s.rep;
                let (op, regs) = (s.op, [r0, r1, r2, s.br.bits]);
                (r, Some((out[..op].to_vec(), state, regs)))
            };
            let want = run(3);
            for mode in [1, 2] {
                assert!(run(mode) == want, "mode {mode}");
            }
        }
    }
}
