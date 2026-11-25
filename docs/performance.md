# Performance

## Recommended consumer build

The library keeps runtime SIMD dispatch (AVX2, NEON, scalar), so one build runs on any CPU. A
binary that uses it, such as a tile worker, gets the most from it with this profile in its own
`Cargo.toml`:

```toml
[profile.release]
lto = "fat"
codegen-units = 1
panic = "abort"
```

and, for a machine with AVX-512 (for example AMD Zen 4 or Zen 5):

```sh
RUSTFLAGS="-C target-cpu=x86-64-v4" cargo build --release
```

Caution: a v4 binary stops with SIGILL on an x86-64 CPU without AVX-512. Build a separate
`x86-64-v3` binary for such a machine. GitHub-hosted x86_64 runners may lack AVX-512, so
`bench.yml` then times the default builds in the v4 columns.

## Reuse

Make one `Decoder` (or `czi::Decoder`) per worker and reuse it. After the first decode, a decode
of the same size allocates nothing through `Decoder::decode_to_slice`, `Decoder::decode_all_into`
into a `Vec` with room, and `czi::Decoder::decode` (`tests/zero_allocs.rs`), when it runs on the
caller's thread: at `max_threads = 1`, or for an input that is not decoded in parallel (one
frame, or under 256 KiB of output; [threading.md](threading.md)). A parallel decode allocates
on every call (the `jobs` list and std's bookkeeping for each spawned thread, under 1 KiB a
thread), and on its first call the helper decoders' tables, about 200 KiB per extra thread.

## Bench columns

`bench.yml` prints the default build (Go and Rust as shipped) and a v4 build (Go `GOAMD64=v4`,
Rust `-C target-cpu=x86-64-v4`) side by side; see [benchmarks.md](benchmarks.md).

## PGO and BOLT

`bench.yml` with the `pgo` input builds the bench example with `cargo-pgo` (profile: the tiles),
then with PGO and BOLT, and times each against the same Go oracle.

Result (AMD Ryzen 7 9800X3D; the machine was busy, so compare
the ratios, not the ms/MB), Rust time over Go time, all 32 tiles, `decode_to_slice`:

| Rust build | Rust / Go | through `decode_all_into` |
|---|---|---|
| release | 1.03 | 1.06 |
| PGO | 0.96 | 0.98 |
| PGO + BOLT | 0.97 | 0.98 |

PGO gains about 7 percent; BOLT adds nothing measurable on top of it (the decoder's hot code is
two small loops, so code layout matters little). Apply PGO in the binary that ships, with a
profile from real tiles; BOLT is not worth its build step here.

## v4 build

A bench run (default threads, `decode_to_slice`, all 32 tiles): the default builds give
Rust / Go 1.01, the v4 builds (Go `GOAMD64=v4`, Rust `-C target-cpu=x86-64-v4`) 0.98. The v4
build changes little: the hot loops already use BMI2 and AVX2 through runtime dispatch.
