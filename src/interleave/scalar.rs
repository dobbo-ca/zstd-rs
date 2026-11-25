//! The reference interleave.
#![forbid(unsafe_code)]

/// Writes `lo[i], hi[i]` as the little-endian word `dst[2i..2i + 2]`.
pub(super) fn interleave(dst: &mut [u8], lo: &[u8], hi: &[u8]) {
    for (i, (&l, &h)) in lo.iter().zip(hi).enumerate() {
        dst[2 * i] = l;
        dst[2 * i + 1] = h;
    }
}
