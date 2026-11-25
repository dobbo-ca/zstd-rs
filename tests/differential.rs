//! The decoder against frames the zstd CLI writes (ports TestDifferentialCLI). Skipped when the
//! CLI is missing, unless `ZSTD_REQUIRE_CLI` is set, as it is in CI.

mod common;
use common::*;
use zstd::{Decoder, Limits};

/// Inputs that hit raw, RLE, short-offset and long-distance paths.
pub fn synthetic() -> Vec<(&'static str, Vec<u8>)> {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let random: Vec<u8> = (0..200 << 10).map(|_| r.next_u64() as u8).collect();

    let words = [
        "tile", "slide", "pixel", "stain", "tissue", "zoom", "level", "jpeg", "zstd", "frame",
        "block", "the", "of", "and", "a",
    ];
    let mut text = Vec::new();
    while text.len() < 300 << 10 {
        text.extend_from_slice(words[r.below(words.len())].as_bytes());
        text.push(if r.below(8) == 7 { b'\n' } else { b' ' });
    }

    // A smooth 512x512 RGB gradient with a little noise.
    let mut img = Vec::with_capacity(512 * 512 * 3);
    for y in 0..512usize {
        for x in 0..512usize {
            img.push((x / 2 + r.below(3)) as u8);
            img.push((y / 2) as u8);
            img.push(((x + y) / 4 + r.below(2)) as u8);
        }
    }

    // Periods 1..15 exercise the overlapping match copy.
    let mut short = Vec::new();
    while short.len() < 100 << 10 {
        let p = 1 + r.below(15);
        for _ in 0..20 + r.below(200) {
            short.extend_from_slice(&random[..p]);
        }
        short.push(r.next_u64() as u8);
    }

    // Repeats far apart need a large window.
    let mut far = Vec::new();
    for _ in 0..3 {
        far.extend_from_slice(&random);
        far.extend_from_slice(&text);
    }
    vec![
        ("empty", vec![]),
        ("one", vec![b'a']),
        ("zeros", vec![0; 1 << 20]),
        ("random", random),
        ("text", text),
        ("image", img),
        ("short-period", short),
        ("far-repeats", far),
    ]
}

#[test]
fn differential_cli() {
    let levels: [&[&str]; 5] = [&["-1"], &["-3"], &["-9"], &["-19"], &["--long"]];
    let variants: [&[&str]; 4] = [
        &[],
        &["--no-content-size"],
        &["--no-check"],
        &["--no-content-size", "--no-check"],
    ];
    let mut d = Decoder::new(&Limits::default()).unwrap();
    let mut ran = 0;
    for (name, data) in synthetic() {
        for lv in levels {
            for v in variants {
                let args = [lv, v].concat();
                let Some(c) = zstd_cli(&data, &args) else {
                    return;
                };
                let got = decode(&c).unwrap_or_else(|e| panic!("{name} {args:?}: {e}"));
                assert!(
                    got == data,
                    "{name} {args:?}: nil dst: {} bytes, want {}",
                    got.len(),
                    data.len()
                );
                let mut dst = Vec::with_capacity(data.len() + (128 << 10));
                d.decode_all_into(&mut dst, &c, &Limits::default(), &NO_CANCEL)
                    .unwrap_or_else(|e| panic!("{name} {args:?}: sized dst: {e}"));
                assert!(dst == data, "{name} {args:?}: sized dst");
                ran += 1;
            }
        }
    }
    assert_eq!(ran, 8 * 5 * 4);
}
