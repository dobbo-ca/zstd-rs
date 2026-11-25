#![no_main]
use libfuzzer_sys::fuzz_target;

/// A runner without AVX2 would fuzz only the scalar tier: fail instead.
#[cfg(target_arch = "x86_64")]
fn check_tier() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        assert!(
            is_x86_feature_detected!("avx2"),
            "no AVX2: scalar tier only"
        );
        eprintln!("interleave tier: avx2");
    });
}

/// As above for NEON: a runner that lacks it would fuzz only scalar.
#[cfg(target_arch = "aarch64")]
fn check_tier() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        assert!(
            std::arch::is_aarch64_feature_detected!("neon"),
            "no NEON: scalar tier only"
        );
        eprintln!("interleave tier: neon");
    });
}

/// Bytes of canary on each side of `dst`: a write past it, or before it, is caught.
const CANARY: usize = 64;

// data = [start: u8][bytes]. The low 5 bits of start offset lo and hi into the bytes, the high 3
// bits offset dst inside a larger buffer; the bytes split in two halves, lo and hi. The SIMD tier
// this CPU runs gives the scalar output, bit for bit, and writes no byte outside it.
fuzz_target!(|data: &[u8]| {
    check_tier();
    let Some((&start, rest)) = data.split_first() else {
        return;
    };
    let rest = &rest[usize::from(start % 32).min(rest.len())..];
    let n = rest.len() / 2;
    let (lo, hi) = (&rest[..n], &rest[n..2 * n]);
    let at = CANARY + usize::from(start >> 5);
    let len = at + 2 * n + CANARY;
    let (mut got, mut want) = (vec![0xAA; len], vec![0xAA; len]);
    zstd::interleave_scalar(&mut want[at..at + 2 * n], lo, hi);
    let ran = zstd::interleave_simd(&mut got[at..at + 2 * n], lo, hi);
    if ran {
        assert!(
            got == want,
            "simd differs from scalar or wrote outside dst, n = {n}"
        );
    }
});
