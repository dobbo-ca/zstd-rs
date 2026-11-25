//! `bench <tiles> <n>`: prints name, output bytes and ns per decode for each tile, and for each
//! hi/lo tile again as a czi zstd1 payload (`czi-<name>`), as `oracle -bench` does for Go. Like
//! Go's `BenchmarkDecodeAll` it reuses its decoder and a pre-sized buffer (`decode_to_slice`). Each
//! tile is also timed through `decode_all_into` with a cleared, reused `Vec` (`vec-<name>`), the
//! API level of Go's `DecodeAll(dst[:0], src)`; that path zero-fills the output on each call. A
//! decode that fails ends the run, as in Go. A file of several frames (`mf-*`) is timed on its
//! whole output; its frames decode on several threads. `BENCH_MAX_THREADS=n` sets `Limits::max_threads`
//! (default: `Limits::default()`'s). See `tests/parity/bench.sh`.

use std::sync::atomic::AtomicBool;
use std::time::Instant;
use std::{env, fs, io};
use zstd::{Limits, czi};

fn die(name: &str, what: impl std::fmt::Debug) -> ! {
    eprintln!("bench: {name}: {what:?}");
    std::process::exit(1)
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let [_, dir, n] = args.as_slice() else {
        eprintln!("usage: bench <tiles> <n>");
        std::process::exit(2);
    };
    let n: u32 = n.parse().unwrap_or(1).max(1);
    let def = Limits::default();
    let limits = Limits {
        max_threads: env::var("BENCH_MAX_THREADS")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(def.max_threads),
        ..def
    };
    let cancel = AtomicBool::new(false);
    let mut d = zstd::Decoder::new(&limits).unwrap();
    let mut cd = czi::Decoder::new(&limits).unwrap();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let Some(name) = name.strip_suffix(".zst") else {
            continue;
        };
        let src = fs::read(format!("{dir}/{name}.zst"))?;
        let size = match zstd::content_size(&src, &limits) {
            Ok(Some(size)) => usize::try_from(size).unwrap_or(0),
            r => die(name, r),
        };
        // An input of several frames (mf-*) decodes to more than the first frame's size.
        let all = zstd::decode_all(&src, &limits, &cancel)
            .unwrap_or_else(|e| die(name, e))
            .len();
        let mut buf = vec![0u8; all];
        let mut run = || {
            d.decode_to_slice(&mut buf, &src, &limits, &cancel)
                .unwrap_or_else(|e| die(name, e))
        };
        let len = run();
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(run());
        }
        println!("{name}\t{len}\t{}", t.elapsed().as_nanos() / u128::from(n));
        let mut v = Vec::new();
        let mut run = || {
            v.clear();
            d.decode_all_into(&mut v, &src, &limits, &cancel)
                .unwrap_or_else(|e| die(name, e));
            v.len()
        };
        let vlen = run();
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(run());
        }
        println!(
            "vec-{name}\t{vlen}\t{}",
            t.elapsed().as_nanos() / u128::from(n)
        );
        if !name.ends_with("_hilo") {
            continue;
        }
        let payload = [&[3, 1, 1][..], &src].concat();
        let mut out = vec![0u8; size];
        let mut run = || {
            cd.decode(
                &mut out,
                &payload,
                czi::ZSTD1,
                czi::GRAY16,
                &limits,
                &cancel,
            )
            .unwrap_or_else(|e| die(name, e));
            out.len()
        };
        let clen = run();
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(run());
        }
        println!(
            "czi-{name}\t{clen}\t{}",
            t.elapsed().as_nanos() / u128::from(n)
        );
    }
    Ok(())
}
