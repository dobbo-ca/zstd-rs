# Safety, SIMD and kernels

`unsafe` is allowed only in the kernel modules (the SIMD interleave in `src/interleave/`, the
sequence loop in `src/seqloop.rs` and the Huffman 4-stream loops in `src/hufloop.rs`), each block
with a `// SAFETY:` comment; the crate root is `deny(unsafe_code)`, which `src/interleave/mod.rs`
keeps (its `avx2` and `neon` submodules `allow` it), and the other modules `forbid` it.

Of zstd-go's four asm kernels, only czi's hi/lo interleave is vector code. Its SIMD tiers are in
`src/interleave/`: `scalar.rs` is the reference and the fallback, `avx2.rs` is the
x86_64 tier and `neon.rs` the aarch64 one. The interleave kernels are safe `#[target_feature]`
fns; `unsafe` is only in their dispatch and load/store helpers.

The sequence loop (`seqloop_amd64.s`) is scalar register code with BMI2 shifts. `src/seqloop.rs`
ports it as an unsafe kernel: the fused decode and execute loop works on raw in/out/literal
pointers, with its bounds proved in safe code before the loop and once per sequence, as the asm
does, and the literal and match copies are unaligned 16-byte (8-byte for overlapping matches)
raw-pointer moves that may write up to 31 bytes past a copy's end. The body is written once and
built twice: plain, and on x86_64 with BMI1/BMI2 (`SHRX`, `BZHI`, `RORX`) when the CPU has them.
Every sequence the loop cannot prove safe goes to the careful safe path (`decode_careful`,
`exec_end`), which is also its test oracle: the unit tests and the `seqloop` fuzz target decode
every input with the plain loop, the BMI2 loop and the careful path only and compare; the unit
tests run under Miri in CI (the `miri` job).

The Huffman 4-stream fast loops (`hufloop_amd64.s`) are scalar register code too. `src/hufloop.rs`
ports them as unsafe kernels: the X1 and X2 inner loops keep the four streams' input and output
positions as pointers, with no bounds checks inside; an X2 symbol is one unaligned 16-bit store and
the output pointer moves by its length; each stream refills after its five symbols from the
trailing zeros of its bit container, as the asm does. The outer loop is the one of the safe loops
`huf::huf4x1_loop` and `huf4x2_loop`, which stay the reference and run under `ZSTD_SIMD=scalar`:
it sizes each run of the inner loop with their `iters` and `olimit`, and proves that run's bounds
once, in safe code (`fits`), so a kernel leaves the same output bytes, positions and bit
containers, and the careful decoder that finishes each stream is unchanged. The body is built
twice, as the sequence loop's: plain, and on x86_64 with BMI1/BMI2 (`SHLX`, `SHRX`, `TZCNT`).

The `unsafe` of `src/hufloop.rs` (the bounds hold for every state `fast_args` makes; a run that
would pass them stops the loop instead, which never happens):

| where | what | why it holds |
|---|---|---|
| `run_x1`, `run_x2` | call `x1_bmi2` / `x2_bmi2` (`#[target_feature(enable = "bmi1,bmi2")]`) | the arm runs only after `is_x86_feature_detected!` found BMI1 and BMI2 |
| X1 `sym!` | `write` of one byte at `o[s] + k`, `k < 5` | the run is exactly `iters` iterations (each moves every `o` by 5; it stops when `o[3]` reaches `olimit = op[3] + 5 * iters`), so stream `s` writes `op[s]..op[s] + 5 * iters`, and `fits` checked `op[s] + 5 * iters <= dst.len()` |
| `opaque` (X2 cells and bit containers) | an empty `asm!` with one register operand (`pure, nomem, nostack, preserves_flags`) | it runs no instruction and returns its input; it only stops a `x86-64-v4` build from turning the four streams' lookups into a `vpgatherqd` (huf-x2 went from 1.04 to 2.96x Go in the v4 build without it) |
| X2 `sym!` | `write_unaligned` of 2 bytes at `o[s]` | the length is `1 + (cell bit 25)`, so 1 or 2 (exact for `read_x2`'s cells, whose length is 1 or 2); an iteration moves `o` 5 to 10 bytes, and each 2-byte store starts at most 8 bytes past where the iteration started, `o[3]` gains at least 5 per iteration, so a run is at most `iters` iterations and stream `s` writes inside `op[s]..op[s] + 10 * iters`, which `fits` checked against `dst.len()` |
| `refill!` (both) | `seqloop::load64`: 8 bytes at `i[s]` | `i[s]` starts at `ip[s] <= ip[3]` (the stream order check of the safe loops), `fits` checked `ip[3] + 8 <= src.len()`, and `i[s]` only moves back, by the trailing zeros of `bits \| 1 << 63` over 8, at most 7 bytes per iteration, from `ip[s] >= ip[0] >= 7 * iters` |

Setting bit 63 before the trailing-zeros count changes nothing for a log-11 table: the end marker
starts at bit 8 or below and five symbols shift out at most 55 bits, so it is still in the
container. The table lookups are safe indexing (`bits >> 53` is below 2048, so the compiler drops
the check). The unit tests (`hufloop::tests`) run every kernel against the safe loop on random X1
and X2 tables read through `read_x1` and `read_x2`, random streams and edge sizes, comparing the
output and the final positions and bit containers; they run under Miri in CI with the plain
kernel, and the `huf` fuzz target does the same on fuzzed tables and streams.

There is no SIMD Huffman tier. AVX2 and AVX-512 tiers that kept the four bit containers in one
vector and gathered the four table cells per symbol were measured (Ryzen 7
9800X3D) at 3.30x (AVX2) and 3.29x (AVX-512) Go's time on `huf-x1`, against 1.01x for the safe
scalar loop, 3.00x and 3.10x against 1.64x on `huf-x2`, and 2.9 to 3.0x against 1.0x on the
gray16 blobs and gradient tiles: each symbol's lookup depends on the one before it, a gather's
latency is several times a load's, and the scalar loop already interleaves the four streams in
registers, as Go's asm does.

The tier is detected once (`is_x86_feature_detected!`, `is_aarch64_feature_detected!`);
`ZSTD_SIMD=scalar` forces the scalar interleave, the safe Huffman loops and the plain sequence
loop. The tests assert that AVX2 (x86_64) or NEON (aarch64) was detected and compare each kernel
with its reference, bit for bit. CI runs the whole suite with and without `ZSTD_SIMD=scalar` on x86_64, the unit tests and the
`conformance` and `czi` tests the same two ways on a native aarch64 runner (the `arm64` job), and
lints the aarch64 target.
