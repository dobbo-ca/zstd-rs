//! NEON interleave: `vzip1q_u8`/`vzip2q_u8` interleave 16 lo and 16 hi bytes into 32.
//!
//! `unsafe` is confined to `interleave` (the dispatch, next to the detection) and to `mem` (the
//! load and store helpers, which take arrays); `kernel` has none.

use std::arch::aarch64::*;

pub(super) fn detect() -> Option<super::Kernel> {
    std::arch::is_aarch64_feature_detected!("neon").then_some(interleave)
}

/// Only reachable through `detect`, which checked NEON. `lo` and `hi` have the same length and
/// `dst` is twice that.
fn interleave(dst: &mut [u8], lo: &[u8], hi: &[u8]) {
    let n = lo.len() & !15;
    // NEON is in the aarch64 baseline, so rustc may already treat the call as safe.
    #[allow(unused_unsafe)]
    // SAFETY: `detect` returned this fn only after `is_aarch64_feature_detected!("neon")`, which
    // is the only requirement of calling `kernel`; its slice arguments are bounds-checked here.
    unsafe {
        kernel(&mut dst[..2 * n], &lo[..n], &hi[..n])
    };
    super::scalar::interleave(&mut dst[2 * n..], &lo[n..], &hi[n..]);
}

/// Interleaves the whole 16-byte chunks of `lo` and `hi` into the 32-byte chunks of `dst`.
#[target_feature(enable = "neon")]
fn kernel(dst: &mut [u8], lo: &[u8], hi: &[u8]) {
    for ((d, l), h) in dst
        .as_chunks_mut::<32>()
        .0
        .iter_mut()
        .zip(lo.as_chunks::<16>().0)
        .zip(hi.as_chunks::<16>().0)
    {
        let [d0, d1] = d.as_chunks_mut::<16>().0 else {
            return;
        };
        let l = mem::load128(l);
        let h = mem::load128(h);
        mem::store128(d0, vzip1q_u8(l, h));
        mem::store128(d1, vzip2q_u8(l, h));
    }
}

/// The only pointer operations of this tier. The array types prove the 16 bytes are there;
/// vld1q/vst1q have no alignment requirement.
mod mem {
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon")]
    #[inline]
    pub(super) fn load128(src: &[u8; 16]) -> uint8x16_t {
        // SAFETY: `src` is a reference to exactly 16 readable bytes.
        unsafe { vld1q_u8(src.as_ptr()) }
    }

    #[target_feature(enable = "neon")]
    #[inline]
    pub(super) fn store128(dst: &mut [u8; 16], v: uint8x16_t) {
        // SAFETY: `dst` is a reference to exactly 16 writable bytes.
        unsafe { vst1q_u8(dst.as_mut_ptr(), v) }
    }
}
