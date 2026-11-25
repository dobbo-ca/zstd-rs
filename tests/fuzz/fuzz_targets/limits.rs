#![no_main]
use libfuzzer_sys::fuzz_target;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use zstd::{Decoder, Error, Limits, czi};

const BIG: Limits = Limits {
    max_output: 1 << 26,
    max_alloc: 1 << 27,
    max_threads: 1,
};

/// Allocations of the fuzzer itself, other threads included.
const SLACK: usize = 64 << 10;

/// Counts the live bytes and their peak since `measure` began, so an allocation past `max_alloc`
/// is a failed assertion and not only a crash at `-malloc_limit_mb`. `realloc` and
/// `alloc_zeroed` go through `alloc` and `dealloc` by default, so they are counted.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded unchanged to `System`, which upholds the `GlobalAlloc`
// contract; the counters are only bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: the caller's `Layout` contract is passed on to `System`.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: `p` came from `alloc` with `l`, as the caller promises.
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Runs `f` and reports the most bytes it held live above what was live before.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (r, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

thread_local! {
    static SHARED: RefCell<Decoder> = RefCell::new(Decoder::new(&Limits::default()).unwrap());
    static CZI: RefCell<czi::Decoder> = RefCell::new(czi::Decoder::new(&Limits::default()).unwrap());
}

// data = [max_output / 16: u16 LE][max_alloc / 16: u16 LE][flags: u8][frames]. Bit 0 of flags
// sets the cancel flag, bits 1-2 pick the call (`decode_all`; `decode_all_into` on a shared
// decoder into a Vec with room; `decode_to_slice` into a buffer; `czi::Decoder::decode` on a
// shared decoder, with `src` = [mode][pixel type][payload]) and bits 3-7 give the room or the
// buffer length in 4 KiB. A decode under small limits does not panic, never holds more than
// `max_alloc` (the counting allocator), and never writes past `max_output`; if it succeeds it
// gives the bytes of the decode under big limits; with the cancel flag set (bit 0 of flags, so it
// is set before the call) it succeeds only for input with nothing to decode, and is
// `Error::Cancelled` when the same decode without the flag would give output.
fuzz_target!(|data: &[u8]| {
    let Some((&[a, b, c, d, flags], src)) = data.split_first_chunk::<5>() else {
        return;
    };
    let limits = Limits {
        max_output: usize::from(u16::from_le_bytes([a, b])) << 4,
        max_alloc: usize::from(u16::from_le_bytes([c, d])) << 4,
        max_threads: 1,
    };
    // Built before the first measure: the tables are not the call's.
    SHARED.with(|_| ());
    CZI.with(|_| ());
    let cancel = AtomicBool::new(flags & 1 != 0);
    let room = usize::from(flags >> 3) << 12;
    match (flags >> 1) & 3 {
        0 => {
            let (got, peak) = measure(|| zstd::decode_all(src, &limits, &cancel));
            assert!(peak <= limits.max_alloc + SLACK, "peak {peak}");
            check(src, &limits, &cancel, got);
        }
        1 => {
            let mut dst = Vec::with_capacity(room);
            let (got, peak) = measure(|| {
                SHARED.with(|z| {
                    z.borrow_mut()
                        .decode_all_into(&mut dst, src, &limits, &cancel)
                })
            });
            assert!(peak <= limits.max_alloc + SLACK, "peak {peak}");
            if got.is_err() {
                assert!(dst.is_empty(), "failed decode appended");
            }
            check(src, &limits, &cancel, got.map(|()| dst));
        }
        2 => {
            let mut buf = vec![0xAA; room];
            let (got, peak) = measure(|| {
                SHARED.with(|z| {
                    z.borrow_mut()
                        .decode_to_slice(&mut buf, src, &limits, &cancel)
                })
            });
            assert!(peak <= SLACK, "decode_to_slice allocated {peak}");
            assert_ne!(got, Err(Error::AllocLimit), "max_alloc does not apply");
            if let Ok(n) = got {
                assert!(n <= limits.max_output && n <= room, "slice output limit");
                if cancel.load(Ordering::Relaxed) {
                    assert_eq!(n, 0, "cancelled decode gave output");
                } else {
                    let no = AtomicBool::new(false);
                    assert!(
                        zstd::decode_all(src, &BIG, &no).is_ok_and(|v| v == buf[..n]),
                        "decode_to_slice differs from decode_all"
                    );
                }
            }
        }
        _ => {
            let Some((&[mode, pt], payload)) = src.split_first_chunk::<2>() else {
                return;
            };
            let mut dst = vec![0xAA; room];
            let (got, peak) = measure(|| {
                CZI.with(|z| {
                    z.borrow_mut().decode(
                        &mut dst,
                        payload,
                        mode.into(),
                        pt.into(),
                        &limits,
                        &cancel,
                    )
                })
            });
            assert!(peak <= limits.max_alloc + SLACK, "czi peak {peak}");
            if got.is_ok() {
                if cancel.load(Ordering::Relaxed) {
                    assert!(
                        dst.iter().all(|&x| x == 0),
                        "cancelled czi decode gave output"
                    );
                } else {
                    let mut want = vec![0xAA; room];
                    let no = AtomicBool::new(false);
                    let r = czi::Decoder::new(&Limits::default()).unwrap().decode(
                        &mut want,
                        payload,
                        mode.into(),
                        pt.into(),
                        &BIG,
                        &no,
                    );
                    assert!(r.is_ok() && want == dst, "czi differs under big limits");
                }
            }
        }
    }
});

/// The result of a call that returns the output as a `Vec`.
fn check(src: &[u8], limits: &Limits, cancel: &AtomicBool, got: Result<Vec<u8>, Error>) {
    if cancel.load(Ordering::Relaxed) {
        assert!(
            got.as_ref().is_err() || got.as_ref().is_ok_and(|v| v.is_empty()),
            "cancelled decode gave output"
        );
        // A frame with output has a block, and the block loop checks the flag before anything.
        let no = AtomicBool::new(false);
        if zstd::decode_all(src, limits, &no).is_ok_and(|v| !v.is_empty()) {
            assert_eq!(got.err(), Some(Error::Cancelled), "cancel flag ignored");
        }
        return;
    }
    match got {
        Ok(out) => {
            assert!(out.len() <= limits.max_output, "output limit");
            assert_eq!(zstd::decode_all(src, &BIG, cancel), Ok(out));
        }
        Err(Error::OutputLimit) => {
            // The same input under big limits must not fit in the small output limit.
            if let Ok(out) = zstd::decode_all(src, &BIG, cancel) {
                assert!(out.len() > limits.max_output, "spurious output limit");
            }
        }
        Err(_) => {}
    }
}
