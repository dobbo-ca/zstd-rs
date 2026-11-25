# Benchmark

`.github/workflows/bench.yml` (`workflow_dispatch` and push to `main`, not required) runs
`oracle -bench` (Go) and `examples/bench.rs` (Rust) on `tests/data/tiles` and on the Huffman frames
below in one container and prints the ratio per format and content to the job summary
(`tests/parity/bench.sh`); both sides reuse
their decoder and a pre-sized buffer (`decode_to_slice`). It also times each tile through
`decode_all_into` with a cleared, reused `Vec`, Go's `DecodeAll(dst[:0], src)`, in a second table:
that path zero-fills the output it writes into, on each call. A decode that fails ends the run.
The target is Rust at or below 1.10x Go on every class. `tests/parity/bench.sh` runs 3 rounds, Go then
Rust, and keeps the fastest of each tile, twice: with Go at its default `GOMAXPROCS` and Rust at
the default `max_threads` (100 decodes per tile), then single-threaded, `GOMAXPROCS=1` and
`max_threads = 1` (`BENCH_MAX_THREADS=1`, 50 decodes per tile). On the tiles, each one frame,
neither side spawns a thread, so the two passes agree within noise. The `multi-frame` class
times the `mf-*` inputs of `tests/parity/mf.sh` (2 to 4 tiles back to back, written into the
tiles directory by the bench image): in the default pass Rust decodes their frames on several
threads and Go one after another, so its ratio there is threaded Rust against serial Go, and
only the single-threaded pass compares like with like. It is not in the `all` row.

On the tiles the Huffman 4-stream loops are 1 to 2 percent of the instructions, so the `huf-x1` and
`huf-x2` classes time frames that are mostly literals: the bench image (`bench-tools` in
`.github/ci/Dockerfile`) writes 128 KiB and 1 MiB of bytes from a fixed LCG with a geometric
distribution (few matches) and compresses them with the zstd CLI at levels 1 and 19 into
`/bench/huf` (`BENCH_HUF` in `bench.sh`). The p = 0.985 source codes at about 7.5 bits a byte,
where `HUF_selectDecoder` picks X1 (`huf_x1_*`); p = 0.9, at about 4.7, gets X2 (`huf_x2_*`). These
classes show the Huffman kernels (`src/hufloop.rs`, BMI2 build) against Go's asm loops; they are
not in the `all` row, which stays the tiles. The `tiers` input of `bench.yml` reruns the bench with
`ZSTD_SIMD=scalar`, which times the safe Huffman loops on the same frames. Gather-based AVX2 and
AVX-512 Huffman tiers were removed after a bench run timed them at about 3x Go on these
classes ([safety.md](safety.md)).

## Hosted runner baseline

Bench run 45715841702 (2026-11-25) on a GitHub-hosted `ubuntu-24.04` runner: Intel Xeon 6975P-C,
2 CPUs, AVX-512 present (the v4 tables are real v4 builds). It is the baseline for later hosted
runs; do not compare it with the Ryzen 7 9800X3D numbers below as a regression. Rust / Go:

| class | `decode_to_slice`, default threads | single-threaded | `decode_all_into`, default threads | single-threaded |
|---|---|---|---|---|
| all tiles | 1.00 | 1.00 | 1.04 | 1.03 |
| worst tile class | 1.01 | 1.01 | 1.19 (bgr24 noise) | 1.18 (bgr24 noise) |
| huf-x1 | 1.01 | 1.01 | 1.03 | 1.03 |
| huf-x2 | 1.08 | 1.08 | 1.10 | 1.10 |
| multi-frame | 0.77 | 0.99 | 0.84 | 1.05 |

The Rust v4 build is slower here (all tiles 0.587 ms/MB against 0.513 for the default build,
1.15 against Go v4), so no v4 flags are recommended.

## Ryzen 7 9800X3D

The last run, on an AMD Ryzen 7 9800X3D, Rust time over Go time through
`decode_to_slice` (a cell is one sample, about 0.03 wide):

| input | first port | default threads | single-threaded |
|---|---|---|---|
| bgr24 noise | 3.9 | 0.82 | 0.81 |
| bgr24 blobs | 1.9 | 0.93 | 0.93 |
| bgr24 gradient | 1.9 | 0.94 | 0.94 |
| gray16 noise | 3.9 | 0.80 | 0.78 |
| gray16 blobs | 2.2 | 0.98 | 0.98 |
| gray16 gradient | 2.3 | 0.98 | 0.99 |
| gray16 hi/lo | 2.0 | 0.93 | 0.95 |
| czi hi/lo | 1.9 | 0.93 | 0.96 |
| all | 1.95 | 0.93 | 0.95 |

In an earlier measurement bgr24 gradient, gray16 hi/lo and czi hi/lo were at
1.07, 1.09 and 1.09. Through `decode_all_into` (zero-filled `Vec`) the run above gives 0.86 to
1.00 per class, 0.96 over all tiles, in both passes.

The sequence loop is most of the decode time on the tiles. It ports `seqloop_amd64.s` closely:
pointers rather than base and index, the next sequence's table cells kept between sequences, bit
counts taken from 32-bit rotates of the cells (RORX) that BZHI reads without a zero-extension, and
the cancel yield folded into the output-end check. On `gray16_1024_gradient_z3_hilo` (callgrind)
it went from about 136 to 116 instructions per sequence, against 104 in Go's asm; the rest is
register moves at the loop head and table-pointer reloads that the compiler's register allocation
leaves in.
