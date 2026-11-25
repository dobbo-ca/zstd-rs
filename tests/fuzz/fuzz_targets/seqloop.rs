#![no_main]
use libfuzzer_sys::fuzz_target;

// The plain and BMI2 sequence loops decode any input as the careful path does: the same result and
// the same output of the blocks that decoded, on success and on failure.
fuzz_target!(|src: &[u8]| {
    let [careful, plain, bmi2] = zstd::seqloop_modes(src);
    assert_eq!(plain, careful, "plain loop differs from the careful path");
    assert_eq!(bmi2, careful, "BMI2 loop differs from the careful path");
});
