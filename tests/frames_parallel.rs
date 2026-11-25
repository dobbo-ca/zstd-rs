//! Frames decoded in parallel: an input of several frames gives, at every `max_threads`, the
//! output and the error of `max_threads = 1` (the serial loop, as zstd-go decodes), through the
//! entry points that can take the parallel path (`decode_to_slice`, `decode_all_into` into a
//! `Vec` with room, the czi decoder), intact and with an error in one frame.

mod common;
use common::*;
use zstd::czi::{self, GRAY16, ZSTD0, ZSTD1};
use zstd::{Decoder, Error, Limits};

const THREADS: [usize; 4] = [1, 2, 3, 8];
/// A skippable frame of 3 bytes.
const SKIP: [u8; 11] = [0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3];

fn limits(max_threads: usize) -> Limits {
    Limits {
        max_threads,
        ..Limits::default()
    }
}

/// The tiles of at most 512x512 whose name has `tag`, each with its output (the larger tiles
/// would only make the test slower).
fn tiles(tag: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    list("tiles")
        .into_iter()
        .filter(|n| n.contains(tag) && !n.contains("_1024_"))
        .map(|n| {
            let src = read(&format!("tiles/{n}"));
            let out = decode(&src).unwrap();
            (src, out)
        })
        .collect()
}

/// The results of `src` on `d` at `max_threads`: `decode_to_slice` into a buffer of `room` bytes
/// (its output; on an error the buffer is undefined), `decode_all_into` into a `Vec` with
/// capacity `room` and into one that holds "pre" (its contents either way).
fn results(
    d: &mut Decoder,
    src: &[u8],
    room: usize,
    max_threads: usize,
) -> Vec<(Result<(), Error>, Vec<u8>)> {
    let l = limits(max_threads);
    let mut buf = vec![0xAA; room];
    let slice = match d.decode_to_slice(&mut buf, src, &l, &NO_CANCEL) {
        Ok(n) => (Ok(()), buf[..n].to_vec()),
        Err(e) => (Err(e), Vec::new()),
    };
    let mut v = Vec::with_capacity(room);
    let all = d.decode_all_into(&mut v, src, &l, &NO_CANCEL);
    let mut pre = Vec::with_capacity(room + 3);
    pre.extend_from_slice(b"pre");
    let pre_r = d.decode_all_into(&mut pre, src, &l, &NO_CANCEL);
    vec![slice, (all, v), (pre_r, pre)]
}

/// 2 to 6 tiles back to back, a skippable frame before some, intact, with a byte changed in the
/// middle of one frame, with that frame's checksum wrong, cut in that frame, and with a byte
/// changed in the first frame and the last frame's checksum wrong (two runs fail: the error is
/// the first frame's, as in the serial loop). Some of the changed inputs take the serial path at
/// every thread count (a changed block header, the cut); the result must be the same anyway. One
/// decoder per thread count, reused, so its helpers are.
#[test]
fn frames_match_serial() {
    let t = tiles("_");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ds: Vec<Decoder> = THREADS
        .iter()
        .map(|&n| Decoder::new(&limits(n)).unwrap())
        .collect();
    for n in [2, 3, 4, 5, 6, 3] {
        let (mut src, mut want, mut frames) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..n {
            if k > 0 && rng.below(2) == 0 {
                src.extend_from_slice(&SKIP);
            }
            let (s, o) = &t[rng.below(t.len())];
            frames.push((src.len(), s.len()));
            src.extend_from_slice(s);
            want.extend_from_slice(o);
        }
        let room = want.len() + (128 << 10);
        let (at, len) = frames[rng.below(n)];
        let mut corrupt = src.clone();
        corrupt[at + len / 2] ^= 0x5A;
        let mut checksum = src.clone();
        checksum[at + len - 1] ^= 1; // every tile has a checksum
        let cut = src[..at + len / 2].to_vec();
        let ((first, first_len), (last, last_len)) = (frames[0], frames[n - 1]);
        let mut two = src.clone();
        two[first + first_len / 2] ^= 0x5A;
        two[last + last_len - 1] ^= 1;
        for (what, v) in [
            ("intact", &src),
            ("corrupt", &corrupt),
            ("checksum", &checksum),
            ("cut", &cut),
            ("two", &two),
        ] {
            let serial = results(&mut ds[0], v, room, 1);
            if what == "intact" {
                let pre = [&b"pre"[..], &want[..]].concat();
                let expect: Vec<(Result<(), Error>, Vec<u8>)> = vec![
                    (Ok(()), want.clone()),
                    (Ok(()), want.clone()),
                    (Ok(()), pre),
                ];
                assert!(serial == expect, "{n} tiles: serial output");
            } else if what != "corrupt" {
                assert!(serial.iter().all(|r| r.0.is_err()), "{what}: {n} tiles");
            }
            for (d, &th) in ds.iter_mut().zip(&THREADS).skip(1) {
                assert!(
                    results(d, v, room, th) == serial,
                    "{what}, {n} tiles: max_threads {th} differs from 1"
                );
            }
        }
    }
}

/// The czi decoder, zstd0 straight into `dst` and zstd1 hi/lo through the scratch buffer (whose
/// second call has the room to decode in parallel): a tile and an empty frame decode; two tiles
/// are a size mismatch; a bad checksum in the tile is an error. Every thread count gives the
/// result of 1.
#[test]
fn czi_frames_match_serial() {
    let t = tiles("_hilo");
    let empty = raw_frame(&[], true, false, 0);
    let mut cases = Vec::new();
    for (i, (a, out)) in t.iter().enumerate() {
        let (b, out_b) = &t[(i + 1) % t.len()];
        let one = [&a[..], &empty[..]].concat();
        let two = [&a[..], &SKIP[..], &b[..]].concat();
        let mut bad = one.clone();
        bad[a.len() - 1] ^= 1; // the tile's checksum
        let both = out.len() + out_b.len();
        let hilo = |f: &[u8]| [&[3u8, 1, 1][..], f].concat();
        cases.extend([
            (ZSTD0, one.clone(), out.len(), true),
            (ZSTD0, two.clone(), both, false),
            (ZSTD0, bad.clone(), out.len(), false),
            (ZSTD1, hilo(&one), out.len(), true),
            (ZSTD1, hilo(&two), out.len(), false),
            (ZSTD1, hilo(&bad), out.len(), false),
        ]);
    }
    for (mode, payload, n, ok) in &cases {
        let run = |th: usize| {
            let mut d = czi::Decoder::new(&limits(th)).unwrap();
            (0..2)
                .map(|_| {
                    let mut dst = vec![0xAA; *n];
                    d.decode(&mut dst, payload, *mode, GRAY16, &limits(th), &NO_CANCEL)
                        .map(|()| dst)
                })
                .collect::<Vec<_>>()
        };
        let serial = run(1);
        assert!(
            serial.iter().all(|r| r.is_ok() == *ok),
            "mode {mode}, {n} bytes: serial {:?}",
            serial
                .iter()
                .map(|r| r.as_ref().map(Vec::len))
                .collect::<Vec<_>>()
        );
        for th in &THREADS[1..] {
            assert!(
                run(*th) == serial,
                "mode {mode}, {n} bytes: max_threads {th} differs from 1"
            );
        }
    }
}
