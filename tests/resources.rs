//! `Limits::max_alloc` and `Limits::max_threads`, measured: a counting global allocator tracks the
//! live bytes and their peak, and how many threads allocated, during a decode, and a sampler
//! thread the most OS threads of the process (on Linux; the helper threads allocate nothing).
//! The peak must stay within `max_alloc` (or the call must fail with `AllocLimit`), and a call
//! may use at most `max_threads` threads: an input of several frames whose output fits the
//! buffer decodes on up to `max_threads` threads, with a decoder for each extra one (its tables
//! charged to `max_alloc`); anything else decodes on the caller's thread.
//!
//! It has no libtest harness (see `Cargo.toml`): the harness's own threads would allocate during
//! a measurement, and the counters are global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use zstd::{Decoder, Error, Limits, czi};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The threads that allocated while armed.
static THREADS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);
/// The most OS threads of the process seen while armed.
static TASKS: AtomicUsize = AtomicUsize::new(0);
/// The most threads one measured call used, by the OS count.
static MOST: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static SEEN: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` with the same arguments; the counters are only
// bookkeeping. `realloc` and `alloc_zeroed` use the defaults, which go through `alloc` and
// `dealloc`, so a growing buffer counts its old and new size together, as the budget does.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ARMED.load(Relaxed) && SEEN.try_with(|s| !s.replace(true)).unwrap_or(false) {
            THREADS.fetch_add(1, Relaxed);
        }
        // SAFETY: the caller's `Layout` contract is passed on to `System`.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: `p` came from `alloc` with `l`, as the caller promises.
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Relaxed);
    }
}

#[global_allocator]
static A: Counting = Counting;

/// The OS threads of the process (`Threads:` in `/proc/self/status`), 0 where that is missing.
/// It allocates nothing (a path this short goes through a stack buffer), so the sampler is not
/// counted by the allocator.
fn os_threads() -> usize {
    use std::io::Read;
    let mut b = [0u8; 4096];
    let n = std::fs::File::open("/proc/self/status")
        .and_then(|mut f| f.read(&mut b))
        .unwrap_or(0);
    std::str::from_utf8(&b[..n])
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Threads:")))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Runs `f` and reports the most bytes it held live above what was live before, and how many
/// threads it used: those that allocated, or the most OS threads above the count before it, with
/// the caller's.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    let (ready, stop) = (AtomicBool::new(false), AtomicBool::new(false));
    std::thread::scope(|s| {
        // Started before the counts are taken, so its own start is not counted; it samples only
        // while armed, so every thread it sees above `os_base` is the call's.
        s.spawn(|| {
            while !stop.load(Relaxed) {
                if ARMED.load(Relaxed) {
                    TASKS.fetch_max(os_threads(), Relaxed);
                }
                ready.store(true, Relaxed);
            }
        });
        while !ready.load(Relaxed) {
            std::thread::yield_now();
        }
        TASKS.store(0, Relaxed);
        let os_base = os_threads();
        let base = LIVE.load(Relaxed);
        PEAK.store(base, Relaxed);
        THREADS.store(0, Relaxed);
        SEEN.with(|s| s.set(false));
        ARMED.store(true, Relaxed);
        let r = f();
        ARMED.store(false, Relaxed);
        stop.store(true, Relaxed);
        let os = TASKS.load(Relaxed).saturating_sub(os_base) + 1;
        MOST.fetch_max(os, Relaxed);
        (
            r,
            PEAK.load(Relaxed).saturating_sub(base),
            THREADS.load(Relaxed).max(os),
        )
    })
}

/// A frame of RLE blocks of `n` copies of `c`, with a 128 KiB window, with or without a content
/// size.
fn rle_frame(c: u8, n: usize, fcs: bool) -> Vec<u8> {
    let mut f = vec![
        0x28,
        0xB5,
        0x2F,
        0xFD,
        if fcs { 3 << 6 } else { 0 },
        (17 << 3) - (10 << 3),
    ];
    if fcs {
        f.extend_from_slice(&(n as u64).to_le_bytes());
    }
    let mut left = n;
    while left > 0 {
        let k = left.min(128 << 10);
        left -= k;
        let bh = ((k as u32) << 3) | (1 << 1) | u32::from(left == 0);
        f.extend_from_slice(&bh.to_le_bytes()[..3]);
        f.push(c);
    }
    f
}

fn main() {
    let dir = format!("{}/tests/data/tiles", env!("CARGO_MANIFEST_DIR"));
    let mut inputs: Vec<(String, Vec<u8>, Vec<u8>)> = Vec::new(); // name, frames, expected output
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {dir}: {e}"))
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    let no = AtomicBool::new(false);
    for name in names {
        let src = std::fs::read(format!("{dir}/{name}")).unwrap();
        let want = zstd::decode_all(&src, &Limits::default(), &no).unwrap();
        inputs.push((name, src, want));
    }
    // An input of several frames (three tiles, a skippable frame between two), for the parallel
    // path.
    let mut multi: (String, Vec<u8>, Vec<u8>) = ("multi-3-tiles".into(), Vec::new(), Vec::new());
    for (i, (_, src, want)) in inputs
        .iter()
        .filter(|t| t.0.contains("_512_"))
        .take(3)
        .enumerate()
    {
        if i == 2 {
            multi
                .1
                .extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3]);
        }
        multi.1.extend_from_slice(src);
        multi.2.extend_from_slice(want);
    }
    inputs.push(multi);
    for (c, n) in [(7u8, 8usize << 20), (9, 40 << 20)] {
        for fcs in [false, true] {
            inputs.push((
                format!("rle-{}mib-fcs-{fcs}", n >> 20),
                rle_frame(c, n, fcs),
                vec![c; n],
            ));
        }
    }

    let default = Limits::default();
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    assert_eq!(default.max_threads, cpus, "default max_threads");
    assert!(default.max_threads >= 1);

    let (mut ok, mut over) = (0, 0);
    // Each czi case must decode (hi/lo, scratch) or fail as Go does (spill) at least once.
    let (mut czi_ok, mut czi_corrupt) = (0, 0);
    for max_alloc in [1 << 20, 4 << 20, 64 << 20, default.max_alloc] {
        for max_threads in [1, 2, default.max_threads] {
            let limits = Limits {
                max_alloc,
                max_threads,
                ..default
            };
            for (name, src, want) in &inputs {
                let ctx = format!("{name} at max_alloc {max_alloc}, max_threads {max_threads}");
                // `want` is the output, or `None` where the call must fail with `Corrupt`.
                let check = |what: &str,
                             r: Result<Vec<u8>, Error>,
                             want: Option<&[u8]>,
                             peak: usize,
                             threads: usize| {
                    assert!(
                        threads <= max_threads,
                        "{ctx}: {what}: {threads} threads used"
                    );
                    assert!(
                        peak <= max_alloc,
                        "{ctx}: {what}: peak {peak} over max_alloc"
                    );
                    match (r, want) {
                        (Ok(got), Some(want)) => {
                            assert!(got == want, "{ctx}: {what}: wrong output")
                        }
                        (Err(Error::AllocLimit), _) | (Err(Error::Corrupt(_)), None) => {}
                        (r, _) => panic!("{ctx}: {what}: {:?}", r.map(|v| v.len())),
                    }
                };
                let (r, peak, threads) = measure(|| zstd::decode_all(src, &limits, &no));
                if r.is_ok() {
                    ok += 1;
                } else {
                    over += 1;
                }
                check("decode_all", r, Some(&want[..]), peak, threads);
                // The decoder is built before the measure, as a caller that reuses one does.
                let mut d = Decoder::new(&limits).unwrap();
                let mut dst = Vec::new();
                let (r, peak, threads) =
                    measure(|| d.decode_all_into(&mut dst, src, &limits, &no).map(|()| dst));
                check("decode_all_into", r, Some(&want[..]), peak, threads);
                let multi = name.starts_with("multi");
                if multi {
                    // A Vec with room for the output, on a fresh decoder: nothing grows, and the
                    // frames decode on up to max_threads threads, so the helpers' tables (about
                    // 200 KiB each) are made in the call; at max_threads = 1 nothing is.
                    let mut d = Decoder::new(&limits).unwrap();
                    let mut dst = Vec::with_capacity(want.len());
                    let (r, peak, threads) =
                        measure(|| d.decode_all_into(&mut dst, src, &limits, &no).map(|()| dst));
                    assert_eq!(
                        peak > 100 << 10,
                        max_threads > 1,
                        "{ctx}: helpers made (peak {peak})"
                    );
                    check(
                        "decode_all_into with room",
                        r,
                        Some(&want[..]),
                        peak,
                        threads,
                    );
                }
                // No allocation at all, but for the helpers of a parallel decode.
                let mut buf = vec![0u8; want.len() + (128 << 10)];
                let (r, peak, threads) = measure(|| d.decode_to_slice(&mut buf, src, &limits, &no));
                assert_eq!(r, Ok(want.len()), "{ctx}: decode_to_slice");
                assert!(
                    buf[..want.len()] == want[..],
                    "{ctx}: decode_to_slice output"
                );
                if multi {
                    assert!(
                        peak <= max_alloc && threads <= max_threads,
                        "{ctx}: decode_to_slice: peak {peak}, {threads} threads"
                    );
                } else {
                    assert!(
                        peak == 0 && threads == 1,
                        "{ctx}: decode_to_slice: peak {peak}, {threads} threads"
                    );
                }

                // The czi decoder, on a frame whose content size is its output: hi/lo packing, the
                // scratch buffer of a frame larger than dst, and a second frame that spills out of
                // dst (a size mismatch, as in Go). Each on a fresh decoder, built before the
                // measure, so the scratch buffer starts empty.
                if zstd::content_size(src, &default) != Ok(Some(want.len() as u64)) {
                    continue;
                }
                let half = want.len() / 2;
                let hilo: Vec<u8> = (0..half).flat_map(|i| [want[i], want[half + i]]).collect();
                let twice = [&src[..], &src[..]].concat();
                let cases = [
                    (
                        "czi hi/lo",
                        czi::ZSTD1,
                        czi::GRAY16,
                        [&[3u8, 1, 1][..], &src[..]].concat(),
                        2 * half,
                        Some(&hilo[..]),
                    ),
                    (
                        "czi scratch",
                        czi::ZSTD0,
                        czi::GRAY16,
                        src.clone(),
                        half,
                        Some(&want[..half]),
                    ),
                    (
                        "czi spill",
                        czi::ZSTD0,
                        czi::GRAY16,
                        twice,
                        want.len(),
                        None,
                    ),
                ];
                for (what, mode, pt, payload, n, want) in cases {
                    let mut d = czi::Decoder::new(&limits).unwrap();
                    let mut dst = vec![0xAA; n];
                    let (r, peak, threads) = measure(|| {
                        d.decode(&mut dst, &payload, mode, pt, &limits, &no)
                            .map(|()| dst)
                    });
                    match &r {
                        Ok(_) => czi_ok += 1,
                        Err(Error::Corrupt(_)) => czi_corrupt += 1,
                        Err(_) => {}
                    }
                    check(what, r, want, peak, threads);
                }
            }
        }
    }
    // A budget that binds on the helpers: room for two helpers' tables and not three. A fresh
    // decoder decodes eight frames of 1 MiB at max_threads 8 on three threads, not eight, and
    // holds two helpers' tables at its peak.
    let (_, t, _) = measure(|| drop(Decoder::new(&default)));
    let eight: Vec<u8> = (0..8).flat_map(|c| rle_frame(c, 1 << 20, true)).collect();
    let limits = Limits {
        max_alloc: 2 * t + t / 2,
        max_threads: 8,
        ..default
    };
    let mut d = Decoder::new(&limits).unwrap();
    let mut buf = vec![0xAAu8; 8 << 20];
    let (r, peak, threads) = measure(|| d.decode_to_slice(&mut buf, &eight, &limits, &no));
    assert_eq!(r, Ok(8 << 20), "eight frames");
    assert!(
        buf.chunks(1 << 20)
            .zip(0u8..)
            .all(|(b, c)| b.iter().all(|&x| x == c)),
        "eight frames: wrong output"
    );
    assert!(
        (2 * t..=limits.max_alloc).contains(&peak),
        "eight frames: peak {peak}, tables {t}"
    );
    assert!(threads <= 3, "eight frames: {threads} threads");

    // The budget must have been tight somewhere and loose somewhere, or the test proves nothing.
    assert!(ok > 0 && over > 0, "ok {ok}, over {over}");
    // And the sampler must have seen the threads of a parallel decode, where it can count them.
    let most = MOST.load(Relaxed);
    assert!(os_threads() == 0 || most > 1, "threads seen: {most}");
    assert!(
        czi_ok > 0 && czi_corrupt > 0,
        "czi: ok {czi_ok}, corrupt {czi_corrupt}"
    );
    println!("resources: {ok} decodes within budget, {over} refused with AllocLimit");
}
