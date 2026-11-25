//! Go's `TestZeroAllocs`: a reused `Decoder` decoding into a `Vec` that has room allocates
//! nothing. Every decoder a worker calls per tile (`Decoder::decode_to_slice`,
//! `czi::Decoder::decode`) allocates nothing on a second decode of the same size. A counting
//! global allocator, armed only on the test's own thread.

mod common;

use common::{NO_CANCEL, read};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use zstd::{Decoder, Limits, czi};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

fn count() {
    if ARMED.try_with(Cell::get).unwrap_or(false) {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to `System` with the same arguments.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn zero_allocs() {
    for f in ["golden/block-128k.zst", "golden/rle-first-block.zst"] {
        let src = read(f);
        // Unknown-size frames need room for a whole block past the content.
        let mut d = Decoder::new(&Limits::default()).unwrap();
        let mut dst: Vec<u8> = Vec::with_capacity((1 << 20) + (128 << 10));
        let cap = dst.capacity();
        let limits = Limits::default();
        d.decode_all_into(&mut dst, &src, &limits, &NO_CANCEL)
            .unwrap_or_else(|e| panic!("{f}: {e}")); // warm up
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.with(|a| a.set(true));
        for _ in 0..20 {
            dst.clear();
            let r = d.decode_all_into(&mut dst, &src, &limits, &NO_CANCEL);
            if r.is_err() {
                ARMED.with(|a| a.set(false));
                panic!("{f}: {r:?}");
            }
        }
        ARMED.with(|a| a.set(false));
        assert_eq!(ALLOCS.load(Ordering::Relaxed), 0, "{f}: allocations");
        assert_eq!(dst.capacity(), cap, "{f}: dst grew");
    }
}

/// Runs `f` 20 times with the counter armed and returns the allocations.
fn allocs(mut f: impl FnMut()) -> usize {
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.with(|a| a.set(true));
    for _ in 0..20 {
        f();
    }
    ARMED.with(|a| a.set(false));
    ALLOCS.load(Ordering::Relaxed)
}

/// A worker that decodes one tile at a time (`max_threads = 1`) reuses its decoders: after the
/// first decode, a decode of the same size allocates nothing, through `decode_to_slice` and
/// through the czi decoder (zstd0, zstd1 and zstd1 hi/lo).
#[test]
fn reuse_allocates_nothing() {
    let limits = Limits {
        max_threads: 1,
        ..Limits::default()
    };
    for f in [
        "tiles/bgr24_512_blobs_z3.zst",
        "tiles/gray16_512_gradient_z1_hilo.zst",
        "tiles/gray16_128_noise_z1.zst",
    ] {
        let src = read(f);
        let size = zstd::content_size(&src, &limits).unwrap().unwrap() as usize;
        let mut d = Decoder::new(&limits).unwrap();
        let mut buf = vec![0u8; size];
        let mut ok = true;
        let mut run = || {
            ok &= d.decode_to_slice(&mut buf, &src, &limits, &NO_CANCEL) == Ok(size);
        };
        run(); // warm up
        assert_eq!(allocs(run), 0, "{f}: decode_to_slice allocations");
        assert!(ok, "{f}: decode_to_slice");

        let mut cd = czi::Decoder::new(&limits).unwrap();
        let mut dst = vec![0u8; size];
        for (hdr, mode) in [
            (&[][..], czi::ZSTD0),
            (&[1][..], czi::ZSTD1),
            (&[3, 1, 1][..], czi::ZSTD1),
        ] {
            let payload = [hdr, &src].concat();
            let mut ok = true;
            let mut run = || {
                ok &= cd
                    .decode(&mut dst, &payload, mode, czi::GRAY16, &limits, &NO_CANCEL)
                    .is_ok();
            };
            run(); // warm up
            assert_eq!(allocs(run), 0, "{f}: czi {hdr:?} allocations");
            assert!(ok, "{f}: czi {hdr:?}");
        }
    }
}
