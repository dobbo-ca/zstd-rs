//! Golden frames and the libzstd errata/permissive doc frames (ports TestGolden, TestGoldenErrors
//! and TestDocFrames).

mod common;
use common::*;

/// Expected outputs come from libzstd 1.6.0 (facebook/zstd 01b7154f).
#[test]
fn golden() {
    let want = [
        (
            "block-128k.zst",
            131068,
            "672003418993584239ec5c79232de911699178ad820cd3ca9779abbfe7f7ba7d",
        ),
        (
            "empty-block.zst",
            0,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "rle-first-block.zst",
            1048576,
            "30e14955ebf1352266dc2ff8067e68104607e750abb9d3b36582b8af909fcb58",
        ),
        (
            "zeroSeq_2B.zst",
            13,
            "03ba204e50d126e4674c005e04d82e84c21366780af1f43bd54a37816b6ab340",
        ),
    ];
    let files = list("golden");
    assert_eq!(files.len(), want.len(), "golden files: {files:?}");
    for (name, n, sum) in want {
        let got =
            decode(&read(&format!("golden/{name}"))).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((got.len(), sha256_hex(&got).as_str()), (n, sum), "{name}");
    }
}

#[test]
fn golden_errors() {
    let files = list("golden-errors");
    assert_eq!(files.len(), 3, "golden error files: {files:?}");
    for name in files {
        let err = decode(&read(&format!("golden-errors/{name}"))).expect_err(&name);
        assert!(is_corrupt(&err), "{name}: {err:?}");
    }
}

enum Want {
    Corrupt,
    Hex(&'static str),
    Sum(usize, &'static str),
}

/// Frames from zstd doc/decompressor_errata.md and doc/decompressor_permissive.md, with libzstd
/// 1.6.0's verdict.
#[test]
fn doc_frames() {
    let cases = [
        // libzstd 1.6.0 rejects this errata example: FCS 0 makes the block size limit 0, so its
        // 2-byte compressed block is too large.
        (
            "errata/0lit-0seq",
            "28b5 2ffd 2000 1500 0000 00",
            Want::Corrupt,
        ),
        (
            "errata/rle-first-block",
            "28b5 2ffd a001 0002 0002 0010 000b 0000 00",
            Want::Sum(
                131073,
                "d281209cc72d47b090175b22621840d9eb8267d09cc05dc122bfaa759a82830f",
            ),
        ),
        (
            "errata/tiny-fse",
            "28b5 2ffd 2027 c500 0080 f3f1 f0ec ebc6 c5c7 f09d 4300 0000 e0e0 0658 0100 603e 52",
            Want::Hex(
                "f3f3f3f3f3f1f1f1f1f1f0f0f0f0f0ecececececebebebebebc6c6c6c6c5c7f09d43000000e0e0",
            ),
        ),
        // Magicless frames are a libzstd option this package does not offer.
        (
            "errata/magicless",
            "27 b5 2f fd 00 03 19 00 00 66 6f 6f 3f ba c4 59",
            Want::Corrupt,
        ),
        (
            "permissive/truncated-huffman-states",
            "28b5 2ffd 0000 5500 0072 8001 0420 7e1f 02aa 00",
            Want::Corrupt,
        ),
        (
            "permissive/offset-0",
            "28b5 2ffd 0000 4500 0008 0002 002f 430b ae",
            Want::Corrupt,
        ),
    ];
    for (name, h, want) in cases {
        let got = decode(&hex(h));
        match want {
            Want::Corrupt => assert!(is_corrupt(&got.expect_err(name)), "{name}"),
            Want::Hex(w) => {
                let got = got.unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(
                    got.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    w,
                    "{name}"
                );
            }
            Want::Sum(n, sum) => {
                let got = got.unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!((got.len(), sha256_hex(&got).as_str()), (n, sum), "{name}");
            }
        }
    }
}
