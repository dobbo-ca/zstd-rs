//! XXH64 with seed 0, the zstd frame checksum (ports xxhash.go, which ports libzstd's XXH64).
#![forbid(unsafe_code)]

const PRIME1: u64 = 11400714785074694791;
const PRIME2: u64 = 14029467366897019727;
const PRIME3: u64 = 1609587929392839161;
const PRIME4: u64 = 9650029242287828579;
const PRIME5: u64 = 2870177450012600261;
const NEG_PRIME1: u64 = 7046029288634856825; // -PRIME1 mod 2^64

// One bounds check, then a single load: eight checked byte indexes do not fold into one.
#[inline(always)]
pub(crate) fn le32(b: &[u8]) -> u32 {
    let b = &b[..4];
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

#[inline(always)]
pub(crate) fn le64(b: &[u8]) -> u64 {
    let b = &b[..8];
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(PRIME2))
        .rotate_left(31)
        .wrapping_mul(PRIME1)
}

fn merge(acc: u64, v: u64) -> u64 {
    (acc ^ round(0, v))
        .wrapping_mul(PRIME1)
        .wrapping_add(PRIME4)
}

/// Input bytes between two polls of `stop`.
const POLL: usize = 1 << 20;

/// XXH64 with seed 0. Public only so tests build frames with checksums; it is not API.
#[cfg(test)]
pub fn xxh64(b: &[u8]) -> u64 {
    xxh64_until(b, || false).expect("never stops")
}

/// XXH64 with seed 0, polling `stop` before each `POLL` bytes: `None` if it said stop.
pub(crate) fn xxh64_until(mut b: &[u8], stop: impl Fn() -> bool) -> Option<u64> {
    let n = b.len();
    let mut h;
    if n >= 32 {
        let mut v1 = PRIME1.wrapping_add(PRIME2);
        let mut v2 = PRIME2;
        let mut v3 = 0u64;
        let mut v4 = NEG_PRIME1;
        let (chunks, rest) = b.as_chunks::<32>();
        for group in chunks.chunks(POLL / 32) {
            if stop() {
                return None;
            }
            for c in group {
                v1 = round(v1, le64(c));
                v2 = round(v2, le64(&c[8..]));
                v3 = round(v3, le64(&c[16..]));
                v4 = round(v4, le64(&c[24..]));
            }
        }
        b = rest;
        h = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        h = merge(h, v1);
        h = merge(h, v2);
        h = merge(h, v3);
        h = merge(h, v4);
    } else {
        h = PRIME5;
    }
    h = h.wrapping_add(n as u64);
    while b.len() >= 8 {
        h ^= round(0, le64(b));
        h = h.rotate_left(27).wrapping_mul(PRIME1).wrapping_add(PRIME4);
        b = &b[8..];
    }
    if b.len() >= 4 {
        h ^= u64::from(le32(b)).wrapping_mul(PRIME1);
        h = h.rotate_left(23).wrapping_mul(PRIME2).wrapping_add(PRIME3);
        b = &b[4..];
    }
    for &c in b {
        h ^= u64::from(c).wrapping_mul(PRIME5);
        h = h.rotate_left(11).wrapping_mul(PRIME1);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(PRIME2);
    h ^= h >> 29;
    h = h.wrapping_mul(PRIME3);
    h ^= h >> 32;
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::xxh64;

    /// Values from libzstd's XXH64.
    #[test]
    fn known_values() {
        for (s, want) in [
            ("", 0xef46db3751d8e999u64),
            ("a", 0xd24ec4f1a98c6e5b),
            ("abc", 0x44bc2cf5ad770999),
            ("0123456789abcdef0123456789abcdef", 0x642a94958e71e6c5),
            (
                "The quick brown fox jumps over the lazy dog, then keeps running for a while.",
                0x92789dfadb548fdb,
            ),
        ] {
            assert_eq!(xxh64(s.as_bytes()), want, "xxh64({s:?})");
        }
    }
}
