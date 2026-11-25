//! Frames, skippable frames, errors, content size, limits and cancellation (ports
//! zstd_test.go). Go's `TestOverlap` has no counterpart (the borrow checker rules it out), nor
//! has `TestZeroAllocs` (the decoder grows a `Vec`, which `Vec::with_capacity` already
//! avoids).

#![allow(clippy::type_complexity)]

mod common;
use common::*;
use std::sync::atomic::AtomicBool;
use zstd::{Decoder, Error, Limits, content_size};

const LIM: Limits = Limits {
    max_output: usize::MAX,
    max_alloc: 1 << 30,
    max_threads: 1,
};

/// A frame with a content size of `blocks` RLE blocks of 128 KiB (zeros), 4 bytes each.
fn rle_frame(blocks: usize) -> Vec<u8> {
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0xC0, 0x58];
    f.extend_from_slice(&((blocks as u64) << 17).to_le_bytes());
    for i in 0..blocks {
        f.extend_from_slice(&[2 | u8::from(i + 1 == blocks), 0, 0x10, 0]);
    }
    f
}

#[test]
fn frames_and_errors() {
    let data = b"zstd-go ".repeat(40000); // 320000 bytes, 3 blocks
    let cs = raw_frame(&data, true, true, 0);
    let skip = [0x5A, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3];

    let mut src = raw_frame(&data, false, false, 0);
    src.extend_from_slice(&skip);
    src.extend_from_slice(&cs);
    src.extend_from_slice(&raw_frame(&[], true, false, 0));
    let want = [data.clone(), data.clone()].concat();

    let mut d = Decoder::new(&Limits::default()).unwrap();
    for dst in [
        Vec::new(),
        Vec::with_capacity(want.len()),
        b"prefix".to_vec(),
    ] {
        let mut got = Vec::with_capacity(dst.capacity());
        got.extend_from_slice(&dst);
        d.decode_all_into(&mut got, &src, &Limits::default(), &NO_CANCEL)
            .unwrap();
        assert_eq!(
            got,
            [dst.clone(), want.clone()].concat(),
            "dst cap {}",
            dst.capacity()
        );
    }

    let mut bad = cs.clone();
    *bad.last_mut().unwrap() ^= 1;
    let dict = raw_frame(&data, true, false, 7);
    let mut window = raw_frame(&data, false, false, 0);
    window[5] = (28 - 10) << 3; // 256 MiB without a content size
    let trailing = [cs.clone(), vec![1, 2]].concat();
    // None: any corrupt-input error.
    let cases: Vec<(&str, Vec<u8>, Option<Error>)> = vec![
        ("checksum", bad, Some(Error::Checksum)),
        ("dictionary", dict, Some(Error::Dictionary)),
        ("window", window, Some(Error::WindowTooLarge)),
        ("truncated", cs[..cs.len() - 1].to_vec(), None),
        ("trailing", trailing, None),
        ("garbage", b"not a zstd frame".to_vec(), None),
        ("skippable-truncated", skip[..10].to_vec(), None),
        // No content size: a 1028-byte block over a 1 KiB window fails in ZSTD_decompressStream,
        // so here too.
        (
            "stream-block-over-window",
            hex("28b52ffd00004d00000861015401022e0010"),
            None,
        ),
    ];
    for (name, src, want) in cases {
        let mut dst = b"keep".to_vec();
        let err = d
            .decode_all_into(&mut dst, &src, &Limits::default(), &NO_CANCEL)
            .expect_err(name);
        match want {
            Some(e) => assert_eq!(err, e, "{name}"),
            None => assert!(is_corrupt(&err), "{name}: {err:?}"),
        }
        assert_eq!(dst, b"keep", "{name}: a failed decode appends nothing");
    }
    assert_eq!(decode(&[]), Ok(Vec::new()));
}

/// Frames with a content size that libzstd 1.5.7's one-shot `ZSTD_decompress` (the libCZI path)
/// accepts.
#[test]
fn one_shot_parity() {
    let cases = [
        (
            "window-2^28-small-fcs",
            "28b52ffd4090000003080061",
            vec![b'a'; 256],
        ),
        (
            "window-2^31-small-fcs",
            "28b52ffd40a8e802431f0007",
            vec![7; 1000],
        ),
        (
            "rle-above-window",
            "28b52ffd400001030b200061",
            vec![b'a'; 1025],
        ),
        ("rle-0-in-fcs-0", "28b52ffd200003000005", vec![]),
    ];
    for (name, h, want) in cases {
        assert_eq!(decode(&hex(h)), Ok(want), "{name}");
    }
}

/// A header claiming 8 GiB costs no more than the output the frame really produces.
#[test]
fn lying_content_size() {
    let mut src = vec![0u8; 256 << 10];
    src[..6].copy_from_slice(&[0x28, 0xB5, 0x2F, 0xFD, 0xC0, 0]);
    let n = (src.len() as u64 / 4) * (128 << 10);
    src[6..14].copy_from_slice(&n.to_le_bytes());
    // max_output off, so that the frame is judged by its content, not by the 8 GiB it claims.
    let limits = Limits {
        max_output: usize::MAX,
        max_alloc: 64 << 20,
        max_threads: 1,
    };
    let err = zstd::decode_all(&src, &limits, &NO_CANCEL).unwrap_err();
    assert!(is_corrupt(&err), "{err:?}");
}

#[test]
fn content_sizes() {
    assert_eq!(
        content_size(&raw_frame(&[0; 1000], true, false, 0), &LIM),
        Ok(Some(1000))
    );
    assert_eq!(
        content_size(&raw_frame(&[0; 1000], false, false, 0), &LIM),
        Ok(None)
    );
    // Like ZSTD_getFrameContentSize, a skippable header alone is enough.
    assert_eq!(
        content_size(&[0x50, 0x2A, 0x4D, 0x18, 0xFF, 0, 0, 0], &LIM),
        Ok(Some(0))
    );
    assert!(is_corrupt(
        &content_size(&[1, 2, 3, 4, 5, 6], &LIM).unwrap_err()
    ));
}

/// zstd-go charges every reallocation to its budget and keeps the sum, so a frame whose output is
/// between half the limit and the limit fails (`TestMemoryLimit`'s edge; `tests/parity/` compares the
/// same frames with Go).
#[test]
fn alloc_limit_sums_growth() {
    let at = |n| Limits {
        max_alloc: n,
        ..LIM
    };
    let src = rle_frame(1120); // 140 MiB, 4494 bytes: the growths sum to 280 MiB
    assert_eq!(src.len(), 4494);
    assert_eq!(
        zstd::decode_all(&src, &at(256 << 20), &NO_CANCEL),
        Err(Error::AllocLimit)
    );
    assert_eq!(
        zstd::decode_all(&src, &at(300 << 20), &NO_CANCEL).map(|v| v.len()),
        Ok(1120 << 17)
    );
    // Just under: 960 blocks (120 MiB) fit in 256 MiB.
    assert_eq!(
        zstd::decode_all(&rle_frame(960), &at(256 << 20), &NO_CANCEL).map(|v| v.len()),
        Ok(960 << 17)
    );
}

/// A frame without a content size guesses its first buffer at 32 times the input, and as in
/// zstd-go the guess is charged: a 4 MiB output that would fit 16 MiB is a memory-limit error
/// (`tests/parity/` compares the frame `fr-hint-memory-limit` with Go).
#[test]
fn alloc_limit_charges_guess() {
    let data = vec![7u8; 4 << 20];
    let src = raw_frame(&data, false, false, 0); // 32 raw blocks: the guess is 128 MiB
    let at = |n| Limits {
        max_alloc: n,
        ..LIM
    };
    assert_eq!(
        zstd::decode_all(&src, &at(16 << 20), &NO_CANCEL),
        Err(Error::AllocLimit)
    );
    assert_eq!(zstd::decode_all(&src, &LIM, &NO_CANCEL), Ok(data));
}

/// The guess is cut to `max_output`: one output byte followed by a 20 MiB skippable frame (the
/// guess counts the input to come) allocates 33 bytes, not 640 MiB.
#[test]
fn guess_cut_to_max_output() {
    let mut src = raw_frame(b"A", false, false, 0);
    src.extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18]);
    src.extend_from_slice(&(20u32 << 20).to_le_bytes());
    src.resize(src.len() + (20 << 20), 0);
    let limits = Limits {
        max_output: 1,
        ..LIM
    };
    let mut dst = Vec::new();
    Decoder::new(&Limits::default())
        .unwrap()
        .decode_all_into(&mut dst, &src, &limits, &NO_CANCEL)
        .unwrap();
    assert_eq!(dst, b"A");
    assert!(dst.capacity() < 1 << 16, "capacity {}", dst.capacity());
}

/// Growth starts from the capacity of `dst` as zstd-go's does from `cap(dst)`: 8 RLE blocks
/// (1 MiB) into a dst with 100 KiB grow 200K, 400K, 800K and 1024K, which is over 2 MiB in sum;
/// from nothing the growths are 128K, 256K, 512K and 1024K.
#[test]
fn alloc_limit_starts_from_capacity() {
    let src = rle_frame(8);
    let limits = Limits {
        max_alloc: 2 << 20,
        ..LIM
    };
    let mut d = Decoder::new(&Limits::default()).unwrap();
    let mut dst = Vec::new();
    assert_eq!(
        d.decode_all_into(&mut dst, &src, &limits, &NO_CANCEL),
        Ok(())
    );
    assert_eq!(dst.len(), 1 << 20);
    let mut dst = Vec::with_capacity(100 << 10);
    assert_eq!(
        d.decode_all_into(&mut dst, &src, &limits, &NO_CANCEL),
        Err(Error::AllocLimit)
    );
    assert!(dst.is_empty());
}

#[test]
fn content_size_limit() {
    let src = raw_frame(&[0; 1000], true, false, 0);
    let at = |n| Limits {
        max_output: n,
        ..LIM
    };
    assert_eq!(content_size(&src, &at(1000)), Ok(Some(1000)));
    assert_eq!(content_size(&src, &at(999)), Err(Error::OutputLimit));
}

#[test]
fn cancel() {
    let src = raw_frame(&[0; 1000], true, false, 0);
    let flag = AtomicBool::new(true);
    assert_eq!(
        zstd::decode_all(&src, &Limits::default(), &flag),
        Err(Error::Cancelled)
    );
}

/// `decode_to_slice` gives the bytes of `decode_all` into a buffer that is exact or larger, and
/// `OutputLimit` into one that is smaller.
#[test]
fn to_slice() {
    let (limits, mut d) = (Limits::default(), Decoder::new(&Limits::default()).unwrap());
    for name in list("tiles") {
        let src = read(&format!("tiles/{name}"));
        let want = decode(&src).unwrap();
        for extra in [0, 1000] {
            let mut buf = vec![0xAA; want.len() + extra];
            assert_eq!(
                d.decode_to_slice(&mut buf, &src, &limits, &NO_CANCEL),
                Ok(want.len()),
                "{name}"
            );
            assert!(buf[..want.len()] == want[..], "{name}");
        }
        let mut small = vec![0; want.len() - 1];
        assert_eq!(
            d.decode_to_slice(&mut small, &src, &limits, &NO_CANCEL),
            Err(Error::OutputLimit),
            "{name}"
        );
    }
    // Without a content size a raw block needs its own size, and a compressed one (not built
    // here) a whole block.
    let data = b"to slice ".repeat(1000);
    let src = raw_frame(&data, false, false, 0);
    let mut buf = vec![0; data.len() - 1];
    assert_eq!(
        d.decode_to_slice(&mut buf, &src, &limits, &NO_CANCEL),
        Err(Error::OutputLimit)
    );
    let mut buf = vec![0; data.len()];
    assert_eq!(
        d.decode_to_slice(&mut buf, &src, &limits, &NO_CANCEL),
        Ok(data.len())
    );
    assert!(buf == data);
}

#[test]
fn alloc_limit() {
    let src = read("golden/rle-first-block.zst");
    let at = |n| Limits {
        max_alloc: n,
        ..Limits::default()
    };
    assert_eq!(
        zstd::decode_all(&src, &at(1 << 19), &NO_CANCEL),
        Err(Error::AllocLimit)
    );
    // A dst with room needs no reservation.
    let mut dst = Vec::with_capacity(1 << 20);
    let mut d = Decoder::new(&Limits::default()).unwrap();
    d.decode_all_into(&mut dst, &src, &at(1 << 19), &NO_CANCEL)
        .unwrap();
    assert_eq!(dst.len(), 1 << 20);
    assert_eq!(
        zstd::decode_all(&src, &at(64 << 20), &NO_CANCEL).map(|v| v.len()),
        Ok(1 << 20)
    );
}

#[test]
fn output_limit() {
    let src = read("golden/rle-first-block.zst");
    let at = |n| Limits {
        max_output: n,
        ..Limits::default()
    };
    assert_eq!(
        zstd::decode_all(&src, &at((1 << 20) - 1), &NO_CANCEL),
        Err(Error::OutputLimit)
    );
    assert_eq!(
        zstd::decode_all(&src, &at(1 << 20), &NO_CANCEL).map(|v| v.len()),
        Ok(1 << 20)
    );
    // The limit counts what a call appends, not what dst already holds.
    let mut dst = vec![1u8; 100];
    Decoder::new(&Limits::default())
        .unwrap()
        .decode_all_into(&mut dst, &src, &at(1 << 20), &NO_CANCEL)
        .unwrap();
    assert_eq!(dst.len(), 100 + (1 << 20));
}

/// A reused decoder gives the same result as a new one, whatever it decoded before.
#[test]
fn decoder_reuse() {
    let mut d = Decoder::new(&Limits::default()).unwrap();
    let mut files: Vec<Vec<u8>> = list("golden")
        .iter()
        .map(|n| read(&format!("golden/{n}")))
        .collect();
    files.extend(
        list("golden-errors")
            .iter()
            .map(|n| read(&format!("golden-errors/{n}"))),
    );
    for _ in 0..2 {
        for f in &files {
            let mut got = Vec::new();
            let r = d.decode_all_into(&mut got, f, &Limits::default(), &NO_CANCEL);
            match decode(f) {
                Ok(want) => assert!(r.is_ok() && got == want),
                Err(e) => assert_eq!(r, Err(e)),
            }
        }
    }
}

/// A fuzz crash (tests/fuzz/seeds/c0d635eb5b707cc8): a reused decoder and a new one agree on it.
#[test]
fn fuzz_crash_reuse() {
    let src = include_bytes!("fuzz/seeds/c0d635eb5b707cc8");
    let mut d = Decoder::new(&Limits::default()).unwrap();
    for _ in 0..2 {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let r = d.decode_all_into(&mut a, src, &LIM, &NO_CANCEL);
        assert_eq!(
            r,
            Decoder::new(&Limits::default())
                .unwrap()
                .decode_all_into(&mut b, src, &LIM, &NO_CANCEL)
        );
        assert!(a == b);
    }
}

/// A failed decode leaves the `Vec` the capacity zstd-go's returned slice has: the original, or
/// what the frames before the failing one grew, not what the failed frame grew.
#[test]
fn failed_decode_restores_capacity() {
    let bad = [0x28, 0xB5, 0x2F, 0xFD, 0x20, 16, 7, 0, 0]; // a reserved block type
    let mut d = Decoder::new(&Limits::default()).unwrap();
    // The first frame itself fails after growing the Vec: an RLE block of 500000 bytes, then a
    // reserved block type.
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0xA0];
    f.extend_from_slice(&600_000u32.to_le_bytes());
    f.extend_from_slice(&((500_000u32 << 3) | 2).to_le_bytes()[..3]);
    f.push(b'x');
    f.extend_from_slice(&[6, 0, 0]);
    let mut v = Vec::with_capacity(1000);
    assert!(d.decode_all_into(&mut v, &f, &LIM, &NO_CANCEL).is_err());
    assert!(v.is_empty());
    assert_eq!(v.capacity(), 1000);
    // The first frame decodes (and grows the Vec), the second fails.
    let mut grown = Vec::with_capacity(1000);
    d.decode_all_into(&mut grown, &rle_frame(4), &LIM, &NO_CANCEL)
        .unwrap();
    let mut v = Vec::with_capacity(1000);
    let src = [rle_frame(4), bad.to_vec()].concat();
    assert!(d.decode_all_into(&mut v, &src, &LIM, &NO_CANCEL).is_err());
    assert!(v.is_empty());
    assert_eq!(v.capacity(), grown.capacity());
}
