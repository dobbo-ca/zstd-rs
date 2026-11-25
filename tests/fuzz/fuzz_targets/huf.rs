#![no_main]
use libfuzzer_sys::fuzz_target;

/// A runner without BMI2 would fuzz the plain kernel twice: fail instead.
#[cfg(target_arch = "x86_64")]
fn check_bmi2() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        assert!(
            is_x86_feature_detected!("bmi1") && is_x86_feature_detected!("bmi2"),
            "no BMI2: plain kernel only"
        );
    });
}

#[cfg(not(target_arch = "x86_64"))]
fn check_bmi2() {}

// data = [x2 flag][output length u16 LE][Huffman table header][4-stream literals]. The plain and
// BMI2 kernels leave the safe loop's state, bit for bit: the output bytes and the final input
// positions, output positions and bit containers.
fuzz_target!(|data: &[u8]| {
    check_bmi2();
    let r = zstd::huf_kernels(data);
    for t in r.iter().skip(1) {
        assert!(
            (&t.1, t.2, t.3, t.4) == (&r[0].1, r[0].2, r[0].3, r[0].4),
            "{} differs from the safe loop",
            t.0
        );
    }
});
