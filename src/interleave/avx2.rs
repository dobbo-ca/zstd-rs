//! AVX2 interleave: 32 lo and 32 hi bytes become 64 output bytes per step. The unpacks work
//! within 128-bit lanes, so two cross-lane permutes put the halves in order.
//!
//! `unsafe` is confined to `interleave` (the dispatch, next to the detection) and to `mem` (the
//! load and store helpers, which take arrays); `kernel` has none.

use std::arch::x86_64::*;

pub(super) fn detect() -> Option<super::Kernel> {
    is_x86_feature_detected!("avx2").then_some(interleave)
}

/// Only reachable through `detect`, which checked AVX2. `lo` and `hi` have the same length and
/// `dst` is twice that.
fn interleave(dst: &mut [u8], lo: &[u8], hi: &[u8]) {
    let n = lo.len() & !31;
    // SAFETY: `detect` returned this fn only after `is_x86_feature_detected!("avx2")`, which is
    // the only requirement of calling `kernel`; its slice arguments are bounds-checked here.
    unsafe { kernel(&mut dst[..2 * n], &lo[..n], &hi[..n]) };
    super::scalar::interleave(&mut dst[2 * n..], &lo[n..], &hi[n..]);
}

/// Interleaves the whole 32-byte chunks of `lo` and `hi` into the 64-byte chunks of `dst`.
#[target_feature(enable = "avx2")]
fn kernel(dst: &mut [u8], lo: &[u8], hi: &[u8]) {
    for ((d, l), h) in dst
        .as_chunks_mut::<64>()
        .0
        .iter_mut()
        .zip(lo.as_chunks::<32>().0)
        .zip(hi.as_chunks::<32>().0)
    {
        let [d0, d1] = d.as_chunks_mut::<32>().0 else {
            return;
        };
        let l = mem::load256(l);
        let h = mem::load256(h);
        let a = _mm256_unpacklo_epi8(l, h);
        let b = _mm256_unpackhi_epi8(l, h);
        mem::store256(d0, _mm256_permute2x128_si256::<0x20>(a, b));
        mem::store256(d1, _mm256_permute2x128_si256::<0x31>(a, b));
    }
}

/// The only pointer operations of this tier. The array types prove the 32 bytes are there;
/// loadu/storeu have no alignment requirement.
mod mem {
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn load256(src: &[u8; 32]) -> __m256i {
        // SAFETY: `src` is a reference to exactly 32 readable bytes.
        unsafe { _mm256_loadu_si256(src.as_ptr().cast()) }
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn store256(dst: &mut [u8; 32], v: __m256i) {
        // SAFETY: `dst` is a reference to exactly 32 writable bytes.
        unsafe { _mm256_storeu_si256(dst.as_mut_ptr().cast(), v) }
    }
}
