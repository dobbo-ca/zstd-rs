//! CZI zstd0/zstd1 subblocks (ports czi/czi_test.go: `refDecode` is libCZI's decoder_zstd.cpp,
//! and the cases run on raw-block frames and, when the zstd CLI is there, on its frames).

#![allow(clippy::type_complexity)]

mod common;
use common::*;
use std::sync::atomic::AtomicBool;
use zstd::Error;
use zstd::Limits;
use zstd::czi::{BGR48, Decoder, GRAY16, ZSTD0, ZSTD1};

/// libCZI's decoder: decode everything, then pack, then cut or zero-fill to `dst.len()`.
fn ref_decode(dst: &mut [u8], src: &[u8], mode: u32, pixel_type: u32) -> Result<(), Error> {
    let mut src = src;
    let mut hilo = false;
    match mode {
        ZSTD0 => {}
        ZSTD1 => {
            let (n, h) = if src.len() > 1 && src[0] == 1 {
                (1, false)
            } else if src.len() > 3 && src[0] == 3 && src[1] == 1 {
                (3, src[2] & 1 != 0)
            } else {
                return Err(Error::Corrupt("header"));
            };
            if h && ((pixel_type != GRAY16 && pixel_type != BGR48) || !dst.len().is_multiple_of(2))
            {
                return Err(Error::Corrupt("hilo"));
            }
            src = &src[n..];
            hilo = h;
        }
        _ => return Err(Error::Mode),
    }
    let Some(fcs) = zstd::content_size(src, &Limits::default())? else {
        return Err(Error::Corrupt("no size"));
    };
    let mut blob = decode(src)?;
    if blob.len() as u64 != fcs {
        return Err(Error::Corrupt("size left"));
    }
    if hilo {
        let half = blob.len() / 2;
        let mut p = vec![0; 2 * half];
        for i in 0..half {
            p[2 * i] = blob[i];
            p[2 * i + 1] = blob[half + i];
        }
        blob = p;
    }
    let k = blob.len().min(dst.len());
    dst[..k].copy_from_slice(&blob[..k]);
    dst[k..].fill(0);
    Ok(())
}

/// Splits 16-bit words into a low plane then a high plane, as libCZI's encoder does.
fn unpack(img: &[u8]) -> Vec<u8> {
    let half = img.len() / 2;
    let mut out = vec![0; 2 * half];
    for i in 0..half {
        out[i] = img[2 * i];
        out[half + i] = img[2 * i + 1];
    }
    out
}

/// A smooth 16-bit image with noise in the low byte.
fn image16(words: usize) -> Vec<u8> {
    let mut r = Rng((3 << 32) | 4);
    let mut img = Vec::with_capacity(2 * words);
    for i in 0..words {
        let v = (i / 7 % 4096 + r.below(64)) as u16;
        img.extend_from_slice(&v.to_le_bytes());
    }
    img
}

fn run(d: &mut Decoder, dst: &mut [u8], src: &[u8], mode: u32, pt: u32) -> Result<(), Error> {
    d.decode(dst, src, mode, pt, &Limits::default(), &NO_CANCEL)
}

/// Frames of the image (`plain`) and of its hi/lo planes (`hl`): of raw blocks, and of the zstd
/// CLI at level 3.
fn frames(img: &[u8]) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut v = vec![(
        "raw",
        raw_frame(img, true, false, 0),
        raw_frame(&unpack(img), true, false, 0),
    )];
    if let (Some(plain), Some(hl)) = (
        zstd_cli(img, &["-3", "--no-check"]),
        zstd_cli(&unpack(img), &["-3"]),
    ) {
        v.push(("cli", plain, hl));
    }
    v
}

#[test]
fn synthetic() {
    let img = image16(3 * 300 * 257); // Bgr48 300x257, or Gray16 900x257
    let mut d = Decoder::new(&Limits::default()).unwrap();
    for (kind, plain, hl) in frames(&img) {
        let with = |h: &[u8], f: &[u8]| [h, f].concat();
        // name, src, mode, pixel type, dst len, output must equal img[..dst len]
        let cases = [
            ("zstd0", plain.clone(), ZSTD0, GRAY16, img.len(), true),
            (
                "zstd1-short-header",
                with(&[1], &plain),
                ZSTD1,
                GRAY16,
                img.len(),
                true,
            ),
            (
                "zstd1-plain",
                with(&[3, 1, 0], &plain),
                ZSTD1,
                3,
                img.len(),
                true,
            ),
            (
                "zstd1-hilo-gray16",
                with(&[3, 1, 1], &hl),
                ZSTD1,
                GRAY16,
                img.len(),
                true,
            ),
            (
                "zstd1-hilo-bgr48",
                with(&[3, 1, 0xFF], &hl),
                ZSTD1,
                BGR48,
                img.len(),
                true,
            ),
            (
                "zstd0-fcs-short",
                plain.clone(),
                ZSTD0,
                GRAY16,
                img.len() + 1000,
                false,
            ),
            (
                "zstd0-fcs-long",
                plain.clone(),
                ZSTD0,
                GRAY16,
                img.len() - 1000,
                true,
            ),
            (
                "hilo-fcs-short",
                with(&[3, 1, 1], &hl),
                ZSTD1,
                GRAY16,
                img.len() + 1000,
                false,
            ),
            (
                "hilo-fcs-long",
                with(&[3, 1, 1], &hl),
                ZSTD1,
                GRAY16,
                img.len() - 1000,
                true,
            ),
        ];
        for (name, src, mode, pt, n, exact) in cases {
            let mut got = vec![0xAA; n];
            run(&mut d, &mut got, &src, mode, pt).unwrap_or_else(|e| panic!("{kind} {name}: {e}"));
            let mut want = vec![0; n];
            ref_decode(&mut want, &src, mode, pt).unwrap();
            assert!(got == want, "{kind} {name}: differs from the libCZI port");
            if exact {
                assert!(got == img[..n], "{kind} {name}: differs from the image");
            }
        }
    }
}

#[test]
fn errors() {
    let img = image16(100);
    let frame = raw_frame(&img, true, false, 0);
    let no_size = raw_frame(&img, false, false, 0);
    let two = [frame.clone(), frame.clone()].concat();
    let with = |h: &[u8]| [h, &frame[..]].concat();
    // name, src, mode, pixel type, dst len, None for any corrupt-input error
    let cases: Vec<(&str, Vec<u8>, u32, u32, usize, Option<Error>)> = vec![
        ("mode", frame.clone(), 4, GRAY16, 200, Some(Error::Mode)),
        ("header-empty", vec![], ZSTD1, GRAY16, 200, None),
        ("header-only", vec![1], ZSTD1, GRAY16, 200, None),
        ("header-3-only", vec![3, 1, 1], ZSTD1, GRAY16, 200, None),
        (
            "header-bad-chunk",
            with(&[3, 2, 1]),
            ZSTD1,
            GRAY16,
            200,
            None,
        ),
        ("header-size", with(&[5, 1, 1]), ZSTD1, GRAY16, 200, None),
        ("hilo-gray8", with(&[3, 1, 1]), ZSTD1, 0, 200, None),
        ("hilo-odd-size", with(&[3, 1, 1]), ZSTD1, GRAY16, 201, None),
        ("no-content-size", no_size, ZSTD0, GRAY16, 200, None),
        ("two-frames", two, ZSTD0, GRAY16, 200, None),
        ("garbage", vec![1, 2, 3, 4, 5, 6], ZSTD0, GRAY16, 200, None),
    ];
    let mut d = Decoder::new(&Limits::default()).unwrap();
    for (name, src, mode, pt, n, want) in cases {
        let err = run(&mut d, &mut vec![0u8; n], &src, mode, pt).expect_err(name);
        match want {
            Some(e) => assert_eq!(err, e, "{name}"),
            None => assert!(is_corrupt(&err), "{name}: {err:?}"),
        }
    }
}

#[test]
fn limits_and_cancel() {
    let img = image16(1000);
    let src = raw_frame(&unpack(&img), true, false, 0);
    let src = [&[3, 1, 1][..], &src].concat();
    let mut d = Decoder::new(&Limits::default()).unwrap();
    let small = Limits {
        max_alloc: 100,
        ..Limits::default()
    };
    let mut dst = vec![0; img.len()];
    assert_eq!(
        d.decode(&mut dst, &src, ZSTD1, GRAY16, &small, &NO_CANCEL),
        Err(Error::AllocLimit)
    );
    let flag = AtomicBool::new(true);
    assert_eq!(
        d.decode(&mut dst, &src, ZSTD1, GRAY16, &Limits::default(), &flag),
        Err(Error::Cancelled)
    );
    run(&mut d, &mut dst, &src, ZSTD1, GRAY16).unwrap();
    assert!(dst == img);
}

/// zstd-go's `czi/testdata/fuzz/FuzzDecode` entry 3238c7f359f276cc (`tests/fuzz/seeds-czi` has it as
/// `[mode][pt][n u16 LE][src]`): a multi-block hi/lo payload decoded into 5913 bytes. The czi
/// decoder agrees with the libCZI port on it, success or failure.
#[test]
fn go_fuzz_seed() {
    let seed = include_bytes!("fuzz/seeds-czi/3238c7f359f276cc");
    let (mode, pt) = (u32::from(seed[0]), u32::from(seed[1]));
    let n = usize::from(u16::from_le_bytes([seed[2], seed[3]]));
    let src = &seed[4..];
    let (mut got, mut want) = (vec![0xAA; n], vec![0xAA; n]);
    let r = Decoder::new(&Limits::default()).unwrap().decode(
        &mut got,
        src,
        mode,
        pt,
        &Limits::default(),
        &NO_CANCEL,
    );
    let w = ref_decode(&mut want, src, mode, pt);
    assert_eq!(r.is_ok(), w.is_ok(), "{r:?} against the libCZI port {w:?}");
    if r.is_ok() {
        assert!(got == want);
    }
}

/// A frame that grows the decoder's buffer (an RLE block of 500000 bytes, 600000 declared) and
/// then fails on a reserved block type.
fn growing_failure() -> Vec<u8> {
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0xA0];
    f.extend_from_slice(&600_000u32.to_le_bytes());
    f.extend_from_slice(&((500_000u32 << 3) | 2).to_le_bytes()[..3]);
    f.push(b'x');
    f.extend_from_slice(&[6, 0, 0]);
    f
}

/// Fuzz crashes (tests/fuzz/seeds-czi): the czi fuzz target found that a decoder whose last decode
/// failed after growing its buffer gave another error than a new one, since the failed decode
/// kept the room (Go's failed `DecodeAll` returns the buffer it was given, so the growth is gone).
#[test]
fn reuse_after_failed_growth() {
    for seed in [
        &include_bytes!("fuzz/seeds-czi/fe0725bd70a09a19")[..],
        &include_bytes!("fuzz/seeds-czi/201ac5f139687fba")[..],
    ] {
        let (mode, pt) = (u32::from(seed[0]), u32::from(seed[1]));
        let n = usize::from(u16::from_le_bytes([seed[2], seed[3]]));
        let src = &seed[4..];
        let mut d = Decoder::new(&Limits::default()).unwrap();
        let mut small = vec![0; 1000];
        assert_eq!(
            run(&mut d, &mut small, &growing_failure(), ZSTD0, GRAY16),
            Err(Error::Corrupt("bad block"))
        );
        let (mut a, mut b) = (vec![0xAA; n], vec![0xAA; n]);
        let r = run(&mut d, &mut a, src, mode, pt);
        let r2 = run(
            &mut Decoder::new(&Limits::default()).unwrap(),
            &mut b,
            src,
            mode,
            pt,
        );
        assert_eq!(r, r2);
        if r.is_ok() {
            assert!(a == b);
        }
    }
}

/// A frame with a content size of `blocks` RLE blocks of 128 KiB (zeros).
fn rle_frame(blocks: usize) -> Vec<u8> {
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0xC0, 0x58];
    f.extend_from_slice(&((blocks as u64) << 17).to_le_bytes());
    for i in 0..blocks {
        f.extend_from_slice(&[2 | u8::from(i + 1 == blocks), 0, 0x10, 0]);
    }
    f
}

/// A frame whose only block has the reserved type.
const BAD_BLOCK_FRAME: [u8; 9] = [0x28, 0xB5, 0x2F, 0xFD, 0x20, 16, 7, 0, 0];

/// When the output passes `dst`, zstd-go grows a slice of dst's capacity and charges only the
/// growth, whatever the first frame (which fit in dst) cost. Here: a first frame of 1 MiB that
/// fits dst, then a frame of 4 MiB. Go charges 2 + 4 + 5 MiB = 11 MiB; charging the first frame
/// again from 0 (as the czi decoder once did) adds about 1.9 MiB. Either way the result is a
/// size mismatch, `Corrupt`, unless the budget is passed.
#[test]
fn overflow_charges_like_go() {
    let src = [rle_frame(8), rle_frame(32)].concat();
    let at = |n| Limits {
        max_alloc: n,
        ..Limits::default()
    };
    let mut d = Decoder::new(&Limits::default()).unwrap();
    let mut dst = vec![0; 1 << 20];
    let r = d.decode(&mut dst, &src, ZSTD0, GRAY16, &at(12 << 20), &NO_CANCEL);
    assert!(is_corrupt(&r.unwrap_err()));
    let r = d.decode(&mut dst, &src, ZSTD0, GRAY16, &at(10 << 20), &NO_CANCEL);
    assert_eq!(r, Err(Error::AllocLimit));
}

/// As in Go, a decode that fails in a later frame leaves the decoder the buffer that the frames
/// before it grew: the next decode needs less. Frame 1 of 1 MiB, then a bad frame; then a frame
/// of 2 MiB, which grows from 1 MiB in one step (2 MiB charged) but from 0 in five (4 MiB).
#[test]
fn reuse_keeps_growth_of_decoded_frames() {
    let a = [rle_frame(8), BAD_BLOCK_FRAME.to_vec()].concat();
    let b = rle_frame(16);
    let at3 = Limits {
        max_alloc: 3 << 20,
        ..Limits::default()
    };
    let mut d = Decoder::new(&Limits::default()).unwrap();
    let mut dst = [0u8; 16];
    let r = d.decode(&mut dst, &a, ZSTD0, GRAY16, &at3, &NO_CANCEL);
    assert!(is_corrupt(&r.unwrap_err()));
    d.decode(&mut dst, &b, ZSTD0, GRAY16, &at3, &NO_CANCEL)
        .unwrap();
    let mut fresh = Decoder::new(&Limits::default()).unwrap();
    let r = fresh.decode(&mut dst, &b, ZSTD0, GRAY16, &at3, &NO_CANCEL);
    assert_eq!(r, Err(Error::AllocLimit));
}

/// The constructors take limits: the tables (about 200 KiB) must fit `max_alloc`.
#[test]
fn constructor_charges_tables() {
    let none = Limits {
        max_alloc: 0,
        ..Limits::default()
    };
    assert!(Decoder::new(&none).is_err_and(|e| e == Error::AllocLimit));
    assert!(zstd::Decoder::new(&none).is_err_and(|e| e == Error::AllocLimit));
    assert_eq!(
        zstd::decode_all(&raw_frame(b"abc", true, false, 0), &none, &NO_CANCEL),
        Err(Error::AllocLimit)
    );
}
