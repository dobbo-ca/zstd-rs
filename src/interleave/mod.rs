//! The CZI hi/lo interleave, the one zstd-go asm kernel that is a vector kernel: `scalar` is the
//! reference, with AVX2 (x86_64) and NEON (aarch64) beside it. The tier is picked once;
//! `ZSTD_SIMD=scalar` forces the scalar one. (The Go sequence and Huffman asm loops are scalar
//! register code, not vector code: the sequence loop and the Huffman loops are the raw-pointer
//! kernels in `seqloop` and `hufloop`.)

#[cfg(all(target_arch = "x86_64", not(miri)))]
#[allow(unsafe_code)]
mod avx2;
#[cfg(all(target_arch = "aarch64", not(miri)))]
#[allow(unsafe_code)]
mod neon;
mod scalar;

use std::sync::OnceLock;

type Kernel = fn(&mut [u8], &[u8], &[u8]);

/// The SIMD kernel this CPU supports, if any. Miri does not run the intrinsics: scalar only.
#[cfg(all(target_arch = "x86_64", not(miri)))]
use avx2::detect;
#[cfg(all(target_arch = "aarch64", not(miri)))]
use neon::detect;
#[cfg(any(miri, not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
fn detect() -> Option<Kernel> {
    None
}

/// Writes `lo[i], hi[i]` as the little-endian word `dst[2i..2i + 2]`, for `i` below the shorter
/// of `lo` and `hi`. `dst` must hold twice that.
pub(crate) fn interleave(dst: &mut [u8], lo: &[u8], hi: &[u8]) {
    static KERNEL: OnceLock<Option<Kernel>> = OnceLock::new();
    let k = KERNEL.get_or_init(|| {
        if std::env::var_os("ZSTD_SIMD").is_some_and(|v| v == "scalar") {
            None
        } else {
            detect()
        }
    });
    let n = lo.len().min(hi.len());
    let (dst, lo, hi) = (&mut dst[..2 * n], &lo[..n], &hi[..n]);
    match k {
        Some(k) => k(dst, lo, hi),
        None => scalar::interleave(dst, lo, hi),
    }
}

/// The SIMD tier on its own, for the differential fuzz target: `false` if this CPU has none.
/// It handles `min(lo.len(), hi.len(), dst.len() / 2)` words.
#[cfg(fuzzing)]
pub fn interleave_simd(dst: &mut [u8], lo: &[u8], hi: &[u8]) -> bool {
    let Some(k) = detect() else { return false };
    let n = lo.len().min(hi.len()).min(dst.len() / 2);
    k(&mut dst[..2 * n], &lo[..n], &hi[..n]);
    true
}

/// The scalar reference on its own, for the differential fuzz target; same contract.
#[cfg(fuzzing)]
pub fn interleave_scalar(dst: &mut [u8], lo: &[u8], hi: &[u8]) {
    let n = lo.len().min(hi.len()).min(dst.len() / 2);
    scalar::interleave(&mut dst[..2 * n], &lo[..n], &hi[..n]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xorshift(state: &mut u64) -> u8 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 24) as u8
    }

    /// The SIMD kernel against scalar, bit for bit: random data, lengths around the lane sizes,
    /// unaligned starts, and a guard byte after `dst`.
    #[test]
    fn simd_matches_scalar() {
        #[cfg(all(target_arch = "aarch64", not(miri)))]
        assert!(detect().is_some(), "NEON expected on aarch64");
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        assert!(detect().is_some(), "AVX2 expected on x86_64");
        let Some(k) = detect() else { return };
        let mut st = 0x9e37_79b9_7f4a_7c15u64;
        let rand: Vec<u8> = (0..4200).map(|_| xorshift(&mut st)).collect();
        let lens = (0..300).chain([511, 512, 513, 1000, 1023, 1024, 1025, 2047, 2048, 2049]);
        for n in lens {
            for off in 0..4 {
                let lo = &rand[off..off + n];
                let hi = &rand[2100 + off..2100 + off + n];
                let mut got = vec![0xAA; 2 * n + 1];
                let mut want = vec![0xAA; 2 * n + 1];
                k(&mut got[..2 * n], lo, hi);
                scalar::interleave(&mut want[..2 * n], lo, hi);
                assert_eq!(got, want, "n {n} off {off}");
            }
        }
    }

    /// Many random lengths and start offsets, so every combination of full lanes and tail occurs.
    #[test]
    fn simd_matches_scalar_random() {
        let Some(k) = detect() else { return };
        let mut st = 0x2545_f491_4f6c_dd1du64;
        let rand: Vec<u8> = (0..2 * 4200).map(|_| xorshift(&mut st)).collect();
        for _ in 0..2000 {
            let n = usize::from(xorshift(&mut st)) * 8 + usize::from(xorshift(&mut st) & 7);
            let (a, b) = (
                usize::from(xorshift(&mut st)),
                usize::from(xorshift(&mut st)),
            );
            let (lo, hi) = (&rand[a..a + n], &rand[4200 + b..4200 + b + n]);
            let mut got = vec![0x55; 2 * n + 1];
            let mut want = got.clone();
            k(&mut got[..2 * n], lo, hi);
            scalar::interleave(&mut want[..2 * n], lo, hi);
            assert_eq!(got, want, "n {n} a {a} b {b}");
        }
    }

    /// The dispatching entry point equals scalar whichever tier it picked.
    #[test]
    fn dispatch_matches_scalar() {
        let lo: Vec<u8> = (0..=255).cycle().take(1001).collect();
        let hi: Vec<u8> = (0..=255).rev().cycle().take(1001).collect();
        let mut got = vec![0; 2002];
        let mut want = vec![0; 2002];
        interleave(&mut got, &lo, &hi);
        scalar::interleave(&mut want, &lo, &hi);
        assert_eq!(got, want);
    }
}
