#![no_main]
use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use zstd::{Decoder, Limits};

const LIMITS: Limits = Limits {
    max_output: 1 << 26,
    max_alloc: 1 << 27,
    max_threads: 1,
};

/// `max_alloc` out of the way, for a decode that the slice path did without it.
const HUGE: Limits = Limits {
    max_output: 1 << 26,
    max_alloc: 1 << 28,
    max_threads: 1,
};

static NO: AtomicBool = AtomicBool::new(false);

thread_local! {
    static SHARED: RefCell<Decoder> = RefCell::new(Decoder::new(&Limits::default()).unwrap());
}

// The decode of any input does not panic, does not depend on the dst buffer, on a Decoder's
// earlier use or on spare capacity, and a failed decode leaves dst as it was. `decode_to_slice`
// gives the same bytes into a roomy buffer, fails in one a byte short, and on hostile input into
// buffers sized from the input does not panic.
fuzz_target!(|src: &[u8]| {
    let mut got = Vec::new();
    let r = SHARED.with(|d| d.borrow_mut().decode_all_into(&mut got, src, &LIMITS, &NO));
    // Same dst shape, so the same room: a decoder's earlier use must not show.
    let mut fresh = Vec::new();
    let r2 = Decoder::new(&Limits::default())
        .unwrap()
        .decode_all_into(&mut fresh, src, &LIMITS, &NO);
    assert_eq!(r, r2, "shared and fresh decoder");
    // As in Go, the growth steps follow cap(dst), so a prefix may change the room a corrupt frame
    // gets: only a success is compared.
    let mut pre = b"pre".to_vec();
    let r3 = Decoder::new(&Limits::default())
        .unwrap()
        .decode_all_into(&mut pre, src, &LIMITS, &NO);
    assert_eq!(&pre[..3], b"pre");
    if r3.is_err() {
        assert!(pre.len() == 3, "failed decode appended");
    }
    match &r {
        Ok(()) => {
            assert!(got == fresh, "output depends on the decoder's earlier use");
            if r3.is_ok() {
                assert!(got == pre[3..], "output depends on dst");
            }
            let mut roomy = Vec::with_capacity(got.len() + 100);
            let r3 = SHARED.with(|d| {
                d.borrow_mut()
                    .decode_all_into(&mut roomy, src, &LIMITS, &NO)
            });
            assert!(r3.is_ok() && roomy == got, "spare capacity");
            assert!(zstd::decode_all(src, &LIMITS, &NO).is_ok_and(|v| v == got));
            // A buffer with a block of room gives the same bytes, and one less than the output
            // does not fit (or the output is empty).
            let mut buf = vec![0xAA; got.len() + (128 << 10) + 64];
            let n = SHARED.with(|d| d.borrow_mut().decode_to_slice(&mut buf, src, &LIMITS, &NO));
            assert!(
                n == Ok(got.len()) && buf[..got.len()] == got[..],
                "decode_to_slice"
            );
            if !got.is_empty() {
                let mut short = vec![0xAA; got.len() - 1];
                let n = SHARED.with(|d| {
                    d.borrow_mut()
                        .decode_to_slice(&mut short, src, &LIMITS, &NO)
                });
                assert!(n.is_err(), "decode_to_slice fits one byte less: {n:?}");
            }
        }
        Err(_) => {
            assert!(got.is_empty(), "failed decode appended");
            assert_eq!(zstd::decode_all(src, &LIMITS, &NO).err(), r.clone().err());
            // Hostile input into buffers of the input's size, stream frames included: no panic,
            // and a success (max_alloc does not apply here) gives the bytes of a decode.
            for k in [1, 16, 200] {
                let mut buf = vec![0xAA; src.len() * k];
                let n =
                    SHARED.with(|d| d.borrow_mut().decode_to_slice(&mut buf, src, &LIMITS, &NO));
                if let Ok(n) = n {
                    assert!(
                        zstd::decode_all(src, &HUGE, &NO).is_ok_and(|v| v == buf[..n]),
                        "decode_to_slice succeeded where decode_all fails or differs"
                    );
                }
            }
        }
    }
});
