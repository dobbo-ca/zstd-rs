//! The Huffman 4-stream fast loops as raw-pointer kernels. They port hufloop_amd64.s, the asm form
//! of hufloop.go, which ports `HUF_decompress4X{1,2}_usingDTable_internal_fast_c_loop`: the four
//! streams' input and output positions are pointers, the inner loop has no bounds checks, an X2
//! symbol is one unaligned 16-bit store with the output advanced by its length, and each stream
//! refills from the trailing zeros of its bit container. The outer loop is the one of
//! `huf::huf4x1_loop` and `huf4x2_loop`, which stay the reference: it sizes each run of the inner
//! loop as they do and proves that run's bounds in safe code, so a kernel leaves the same output
//! bytes, positions and bit containers, and the careful decoder that finishes each stream does not
//! see which loop ran.
//!
//! The body is written once and built twice, as in `seqloop`: plain, and on x86_64 with BMI1 and
//! BMI2 (SHLX, SHRX, TZCNT) for CPUs that have them. `ZSTD_SIMD=scalar` runs the safe loops.

use std::sync::OnceLock;

#[cfg(any(test, fuzzing))]
use crate::huf::HufTable;
use crate::huf::{FastArgs, huf4x1_loop, huf4x2_loop};
use crate::seqloop::load64;

/// The bounds of one run of the inner loop, which the state `fast_args` makes always has: every
/// stream's 8-byte reads, at most at `ip[3]` (the callers checked `ip[0] <= ip[1] <= ip[2] <=
/// ip[3]` first), lie in `src` (`sn` bytes), and its writes, `w` bytes or fewer from its `op`, in
/// `dst` (`dn` bytes). `w` is not 0.
#[inline(always)]
fn fits(ip: [usize; 4], op: [usize; 4], sn: usize, dn: usize, w: usize) -> bool {
    sn.saturating_sub(ip[3]) >= 8 && op.iter().all(|&o| dn.saturating_sub(o) >= w)
}

/// `huf4x1_loop` with the inner loop on pointers, for both builds. With a table from `read_x1`
/// (log 11, so a symbol is at most 11 bits) it leaves exactly what `huf4x1_loop` leaves.
#[inline(always)]
fn x1_body(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u16; 1 << 12]) {
    let (mut ip, mut op, mut b) = (a.ip, a.op, a.bits);
    let (sn, dn) = (src.len(), dst.len());
    let (sb, db) = (src.as_ptr(), dst.as_mut_ptr());
    loop {
        // Five symbols of at most 11 bits use under 7 input bytes.
        let iters = ((a.oend - op[3]) / 5).min(ip[0] / 7);
        let olimit = op[3] + iters * 5;
        if op[3] == olimit {
            break;
        }
        if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
            break;
        }
        if !fits(ip, op, sn, dn, iters * 5) {
            break;
        }
        // This run is `iters` iterations: every stream moves `o` 5 bytes per iteration, and the
        // loop stops when `o[3]` reaches `olimit`. Stream `s` writes `op[s]..op[s] + 5 * iters`,
        // inside `dst` by `fits`. It reads 8 bytes at `i[s]`, which starts at `ip[s]`, at most
        // `ip[3]` (the order check), with `ip[3] + 8 <= sn` by `fits`, and moves back at most 7
        // bytes per iteration (see `refill!`) from `ip[s] >= ip[0] >= 7 * iters`: the reads stay
        // in `src`.
        let mut i = ip.map(|p| sb.wrapping_add(p));
        let mut o = op.map(|p| db.wrapping_add(p));
        let olim = db.wrapping_add(olimit);
        // Symbol `k` of the five of stream `s`; the cell is `byte << 8 | nb_bits`.
        macro_rules! sym {
            ($s:literal, $k:literal) => {{
                let c = dt[(b[$s] >> 53) as usize];
                b[$s] <<= c & 0x3F;
                // SAFETY: `o[$s] + $k` is below `o[$s] + 5`, inside this run's writes (above).
                unsafe { o[$s].add($k).write((c >> 8) as u8) };
            }};
        }
        macro_rules! round {
            ($k:literal) => {
                sym!(0, $k);
                sym!(1, $k);
                sym!(2, $k);
                sym!(3, $k);
            };
        }
        // The trailing zeros of a container are the bits consumed since its last refill plus the
        // position of its end marker, so `i` moves back by their whole bytes. Setting bit 63 caps
        // them at 63, a step of 7 bytes; it changes nothing while the marker is in the container,
        // which holds for symbols of at most 11 bits (at most 8 + 5 * 11 bits are shifted out).
        macro_rules! refill {
            ($s:literal) => {{
                let ctz = (b[$s] | (1 << 63)).trailing_zeros();
                i[$s] = i[$s].wrapping_sub((ctz >> 3) as usize);
                // SAFETY: `i[$s]..i[$s] + 8` lies in `src` (above).
                let v = unsafe { load64(i[$s]) };
                b[$s] = (v | 1) << (ctz & 7);
            }};
        }
        while o[3] < olim {
            round!(0);
            round!(1);
            round!(2);
            round!(3);
            sym!(0, 4);
            refill!(0);
            sym!(1, 4);
            refill!(1);
            sym!(2, 4);
            refill!(2);
            sym!(3, 4);
            refill!(3);
            o = o.map(|p| p.wrapping_add(5));
        }
        ip = i.map(|p| p.addr() - sb.addr());
        op = o.map(|p| p.addr() - db.addr());
    }
    (a.ip, a.op, a.bits) = (ip, op, b);
}

/// `x`, kept in a general register: an empty `asm!` that emits no instruction. Without it, a
/// `x86-64-v4` build vectorizes the four streams of `x2_body` and turns their table lookups into
/// one `vpgatherqd`, whose latency is far above four loads (bench: huf-x2 at 2.96x Go). Miri and
/// other targets use the value as is.
#[inline(always)]
fn opaque(x: u64) -> u64 {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    {
        let mut x = x;
        // SAFETY: the template is empty: no instruction runs, no memory, stack or flags are
        // touched, and `x` comes back unchanged.
        unsafe {
            std::arch::asm!("/* {0} */", inout(reg) x, options(pure, nomem, nostack, preserves_flags))
        };
        x
    }
    #[cfg(not(all(target_arch = "x86_64", not(miri))))]
    x
}

/// `huf4x2_loop` with the inner loop on pointers, for both builds. With a table from `read_x2`
/// (log 11; a cell's length is 1 or 2 and its bits at most 11) it leaves exactly what
/// `huf4x2_loop` leaves.
#[inline(always)]
fn x2_body(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u32; 1 << 12]) {
    let (mut ip, mut op, mut b) = (a.ip, a.op, a.bits);
    let oend = [op[1], op[2], op[3], a.oend];
    let (sn, dn) = (src.len(), dst.len());
    let (sb, db) = (src.as_ptr(), dst.as_mut_ptr());
    loop {
        // Five lookups write at most 10 bytes per stream.
        let mut iters = ip[0] / 7;
        for s in 0..4 {
            iters = iters.min((oend[s] - op[s]) / 10);
        }
        let olimit = op[3] + iters * 5;
        if op[3] == olimit {
            break;
        }
        if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
            break;
        }
        if !fits(ip, op, sn, dn, iters * 10) {
            break;
        }
        // A symbol moves `o` 1 or 2 bytes (`sym!` takes the length from bit 25 of the cell), so
        // an iteration moves it 5 to 10 and writes inside the 10 bytes from where it started (the
        // last 2-byte store starts at most 8 bytes in, and may write one byte past the new `o`). This run is at most `iters` iterations: `o[3]` gains 5 or
        // more in each, and the loop stops when it reaches `olimit`. Stream `s` writes inside
        // `op[s]..op[s] + 10 * iters`, inside `dst` by `fits`. Its reads stay in `src` as in
        // `x1_body`: 8 bytes at `i[s]`, at most `ip[3]`, with `ip[3] + 8 <= sn`, moving back at
        // most 7 bytes per iteration from `ip[s] >= ip[0] >= 7 * iters`.
        let mut i = ip.map(|p| sb.wrapping_add(p));
        let mut o = op.map(|p| db.wrapping_add(p));
        let olim = db.wrapping_add(olimit);
        // One symbol of stream `s`; the cell is `seq | nb_bits << 16 | len << 24`, `len` 1 or 2.
        macro_rules! sym {
            ($s:literal) => {{
                let c = opaque(u64::from(dt[(b[$s] >> 53) as usize])) as u32;
                // SAFETY: `o[$s]..o[$s] + 2` is inside this run's writes (above).
                unsafe { o[$s].cast::<u16>().write_unaligned((c as u16).to_le()) };
                b[$s] <<= (c >> 16) & 0x3F;
                o[$s] = o[$s].wrapping_add(1 + ((c >> 25) & 1) as usize);
            }};
        }
        macro_rules! round {
            () => {
                sym!(0);
                sym!(1);
                sym!(2);
                sym!(3);
            };
        }
        // As in `x1_body`.
        macro_rules! refill {
            ($s:literal) => {{
                let ctz = (b[$s] | (1 << 63)).trailing_zeros();
                i[$s] = i[$s].wrapping_sub((ctz >> 3) as usize);
                // SAFETY: `i[$s]..i[$s] + 8` lies in `src` (above).
                let v = unsafe { load64(i[$s]) };
                b[$s] = opaque((v | 1) << (ctz & 7));
            }};
        }
        while o[3] < olim {
            round!();
            round!();
            round!();
            round!();
            round!();
            refill!(0);
            refill!(1);
            refill!(2);
            refill!(3);
        }
        ip = i.map(|p| p.addr() - sb.addr());
        op = o.map(|p| p.addr() - db.addr());
    }
    (a.ip, a.op, a.bits) = (ip, op, b);
}

#[inline(never)] // its own function, so its registers are its own
fn x1_plain(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u16; 1 << 12]) {
    x1_body(a, src, dst, dt)
}

#[inline(never)]
fn x2_plain(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u32; 1 << 12]) {
    x2_body(a, src, dst, dt)
}

/// `x1_body` with SHLX, SHRX and TZCNT.
#[cfg(all(target_arch = "x86_64", not(miri)))]
#[target_feature(enable = "bmi1,bmi2")]
#[inline(never)]
fn x1_bmi2(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u16; 1 << 12]) {
    x1_body(a, src, dst, dt)
}

/// `x2_body` with SHLX, SHRX and TZCNT.
#[cfg(all(target_arch = "x86_64", not(miri)))]
#[target_feature(enable = "bmi1,bmi2")]
#[inline(never)]
fn x2_bmi2(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u32; 1 << 12]) {
    x2_body(a, src, dst, dt)
}

/// BMI1 and BMI2 are there.
#[cfg(all(target_arch = "x86_64", not(miri)))]
fn bmi2() -> bool {
    is_x86_feature_detected!("bmi1") && is_x86_feature_detected!("bmi2")
}

/// The loop to run, read once: 0 (the safe loops) under `ZSTD_SIMD=scalar`, else 2 (a kernel).
fn mode() -> u8 {
    static MODE: OnceLock<u8> = OnceLock::new();
    *MODE.get_or_init(|| {
        if std::env::var_os("ZSTD_SIMD").is_some_and(|v| v == "scalar") {
            0
        } else {
            2
        }
    })
}

/// `huf4x1_loop` on the loop `how` picks: 0 the safe loop, 1 the plain kernel, 2 the BMI2 kernel
/// (the plain one where the CPU lacks BMI1 or BMI2).
fn run_x1(how: u8, a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u16; 1 << 12]) {
    match how {
        0 => huf4x1_loop(a, src, dst, dt),
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        2 if bmi2() => {
            // SAFETY: `bmi2()` found BMI1 and BMI2 just above, the features `x1_bmi2` enables.
            unsafe { x1_bmi2(a, src, dst, dt) }
        }
        _ => x1_plain(a, src, dst, dt),
    }
}

/// `huf4x2_loop` on the loop `how` picks, as `run_x1`.
fn run_x2(how: u8, a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u32; 1 << 12]) {
    match how {
        0 => huf4x2_loop(a, src, dst, dt),
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        2 if bmi2() => {
            // SAFETY: `bmi2()` found BMI1 and BMI2 just above, the features `x2_bmi2` enables.
            unsafe { x2_bmi2(a, src, dst, dt) }
        }
        _ => x2_plain(a, src, dst, dt),
    }
}

/// `huf4x1_loop`: the BMI2 or plain kernel, or the safe loop under `ZSTD_SIMD=scalar`.
pub(crate) fn loop_x1(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u16; 1 << 12]) {
    run_x1(mode(), a, src, dst, dt)
}

/// `huf4x2_loop`: the BMI2 or plain kernel, or the safe loop under `ZSTD_SIMD=scalar`.
pub(crate) fn loop_x2(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u32; 1 << 12]) {
    run_x2(mode(), a, src, dst, dt)
}

/// What one loop left: its name, the output, and the final `ip`, `op` and bit containers.
#[cfg(any(test, fuzzing))]
type State = (&'static str, Vec<u8>, [usize; 4], [usize; 4], [u64; 4]);

/// Runs the safe loop, the plain kernel and the BMI2 kernel (the plain one where the CPU lacks
/// BMI2) on the 4-stream literals `src` into `n` bytes of output (filled with 0xA5 first). Empty
/// when `src` does not take the fast loop.
#[cfg(any(test, fuzzing))]
fn run_all(t: &HufTable, x2: bool, src: &[u8], n: usize) -> Vec<State> {
    [("safe", 0), ("plain", 1), ("bmi2", 2)]
        .into_iter()
        .filter_map(|(name, how)| {
            let mut a = t.fast_args(n, src).ok()?;
            let mut dst = vec![0xA5; n];
            if x2 {
                run_x2(how, &mut a, src, &mut dst, &t.x2);
            } else {
                run_x1(how, &mut a, src, &mut dst, &t.x1);
            }
            Some((name, dst, a.ip, a.op, a.bits))
        })
        .collect()
}

/// For the differential fuzz target: what each loop leaves, the safe loop first, for
/// `data = [x2 flag][output length u16 LE][Huffman table header][4-stream literals]`. The low bit
/// of the flag picks the X2 table. Empty when the table does not read or the literals do not take
/// the fast loop.
#[cfg(fuzzing)]
pub fn huf_kernels(data: &[u8]) -> Vec<State> {
    let [flag, n0, n1, rest @ ..] = data else {
        return Vec::new();
    };
    let x2 = flag & 1 != 0;
    let mut t = HufTable::new();
    let used = if x2 { t.read_x2(rest) } else { t.read_x1(rest) };
    let Some(used) = used else {
        return Vec::new();
    };
    run_all(
        &t,
        x2,
        &rest[used..],
        usize::from(u16::from_le_bytes([*n0, *n1])),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn below(st: &mut u64, n: usize) -> usize {
        (xorshift(st) >> 11) as usize % n
    }

    /// The direct `HUF_readStats` header (`127 + n`, then 4-bit weights) of a random complete
    /// prefix code of 2 to 128 symbols and at most 11 bits, with absent symbols among them.
    fn header(st: &mut u64) -> Vec<u8> {
        let n = 2 + below(st, 127);
        // Leaf depths; splitting the newest leaf half the time makes long codes too.
        let mut depth = vec![0u8];
        while depth.len() < n {
            let i = if xorshift(st) & 1 == 0 {
                depth.len() - 1
            } else {
                below(st, depth.len())
            };
            if depth[i] < 11 {
                depth[i] += 1;
                depth.push(depth[i]);
            }
        }
        let max = depth.iter().copied().max().unwrap_or(0);
        let mut w = Vec::new();
        for (j, &d) in depth.iter().enumerate() {
            if w.len() + depth.len() - j < 129 && below(st, 8) == 0 {
                w.push(0);
            }
            w.push(max + 1 - d);
        }
        // The last weight is implied.
        let o = w.len() - 1;
        let mut h = vec![127 + o as u8];
        h.extend(
            w[..o]
                .chunks(2)
                .map(|c| (c[0] << 4) | c.get(1).copied().unwrap_or(0)),
        );
        h
    }

    /// Random 4-stream literals for `n` bytes of output: the jump table and four streams of
    /// random bytes, each at least 8 long, so most take the fast loop.
    fn literals(st: &mut u64, n: usize) -> Vec<u8> {
        let mut l = [0usize; 4];
        for x in &mut l {
            *x = 8 + below(st, (n / 3).clamp(8, 20000));
        }
        let mut src = Vec::new();
        for x in &l[..3] {
            src.extend((*x as u16).to_le_bytes());
        }
        let total: usize = l.iter().sum();
        src.extend((0..total).map(|_| (xorshift(st) >> 24) as u8));
        src
    }

    /// The plain and BMI2 kernels leave the safe loop's state, bit for bit: random X1 and X2
    /// tables read through `read_x1` and `read_x2`, random streams, output sizes around the
    /// 4-stream and 5-symbol steps and random ones.
    #[test]
    fn kernels_match_safe() {
        // The CI runners have BMI1 and BMI2: without them the bmi2 runs below would fall back to
        // the plain kernel and the BMI2 build would go untested.
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        assert!(bmi2(), "BMI1/BMI2 not detected");
        let mut st = 0x9e37_79b9_7f4a_7c15u64;
        let (edges, random, max): (Vec<usize>, usize, usize) = if cfg!(miri) {
            (vec![1, 5, 20, 21, 40, 41, 63, 64, 65, 257, 1000], 12, 3000)
        } else {
            let e = (1..=48).chain([63, 64, 65, 255, 256, 257, 1000, 4095, 4096, 4097, 65535]);
            (e.collect(), 400, 40000)
        };
        let sizes: Vec<usize> = edges
            .into_iter()
            .chain((0..random).map(|_| 1 + below(&mut st, max)))
            .collect();
        let (mut cases, mut ran) = (0, 0);
        for n in sizes {
            for x2 in [false, true] {
                let h = header(&mut st);
                let mut t = HufTable::new();
                let used = if x2 { t.read_x2(&h) } else { t.read_x1(&h) };
                assert_eq!(used, Some(h.len()), "header {h:?}");
                let src = literals(&mut st, n);
                let r = run_all(&t, x2, &src, n);
                let Some(want) = r.first() else { continue };
                cases += 1;
                if want.3[0] > 0 {
                    ran += 1;
                }
                for got in &r[1..] {
                    assert!(
                        (&got.1, got.2, got.3, got.4) == (&want.1, want.2, want.3, want.4),
                        "{} differs from the safe loop: x2 {x2} n {n} header {h:?}",
                        got.0
                    );
                }
            }
        }
        // Most cases run the loop, so the comparison is not of untouched state.
        assert!(ran * 2 > cases, "{ran} of {cases} cases ran the loop");
    }

    /// The dispatching entry points equal the safe loop whichever loop they picked.
    #[test]
    fn dispatch_matches_safe() {
        let mut st = 0x2545_f491_4f6c_dd1du64;
        let n = if cfg!(miri) { 3000 } else { 30000 };
        for x2 in [false, true] {
            let h = header(&mut st);
            let mut t = HufTable::new();
            let used = if x2 { t.read_x2(&h) } else { t.read_x1(&h) };
            assert_eq!(used, Some(h.len()));
            let src = literals(&mut st, n);
            let run = |how: Option<u8>| {
                let mut a = t.fast_args(n, &src).ok()?;
                let mut dst = vec![0; n];
                match (how, x2) {
                    (Some(how), false) => run_x1(how, &mut a, &src, &mut dst, &t.x1),
                    (Some(how), true) => run_x2(how, &mut a, &src, &mut dst, &t.x2),
                    (None, false) => loop_x1(&mut a, &src, &mut dst, &t.x1),
                    (None, true) => loop_x2(&mut a, &src, &mut dst, &t.x2),
                }
                Some((dst, a.ip, a.op, a.bits))
            };
            let want = run(Some(0));
            assert!(want.is_some());
            assert!(run(None) == want, "x2 {x2}");
        }
    }
}
