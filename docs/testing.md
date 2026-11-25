# Tests and CI

## Tests

`tests/` ports the Go tests: `golden.rs` (libzstd's golden frames and the errata/permissive doc
frames), `frames.rs` (frames, skippable frames, every error, content size, limits, cancellation),
`conformance.rs` (the 304 decodecorpus pairs, the klauspost good/bad files, the synthetic tiles),
`differential.rs` (frames written by the zstd CLI at levels 1 to 19, `--long`, with and without
content size and checksum), `czi.rs` (against a port of libCZI's decoder, on raw-block frames
and CLI frames), `frames_parallel.rs` (inputs of several tiles, skippable frames between some,
decoded at `max_threads` 1, 2, 3 and 8 through `decode_to_slice`, `decode_all_into` with room
and the czi decoder, intact and with a corrupt byte, a bad checksum or a cut in one frame: every
thread count gives the result of 1) and `resources.rs` (the peak memory against `max_alloc` and
the threads that allocate against `max_threads`, measured with a counting global allocator, on
the tiles, large RLE frames and an input of several tiles, through `decode_all`,
`decode_all_into` and the czi decoder's hi/lo, scratch-buffer and spill paths; it has no libtest
harness, so that no other thread allocates during a measurement). Go's fuzz tests and benchmarks are the `tests/fuzz/` targets ([fuzzing.md](fuzzing.md)) and the benchmark ([benchmarks.md](benchmarks.md)). Its
kernel-vs-asm check is, for the sequence loop, the `seqloop` unit tests and fuzz target, and for
the Huffman loops the `hufloop` unit tests and the `huf` fuzz target; the local `corpus/` tests
have no counterpart. `tests/data/README.md` says where the
fixtures come from. The whole of the Go `testdata` is here except the decodecorpus tarball (extracted)
and `gen-decodecorpus.sh`.

## CI

CI (`.github/workflows/ci.yml`, `.github/ci/Dockerfile`) runs, in the `test` job,
`cargo fmt --check`, clippy `-D warnings` (also for aarch64), the tests with and
without `ZSTD_SIMD=scalar` (with the zstd CLI installed, and `ZSTD_REQUIRE_CLI=1` so they cannot
skip) and an aarch64 release build. The `arm64` job runs the unit tests and the `conformance` and
`czi` tests on a native aarch64 runner (`ubuntu-24.04-arm`; that stage has no zstd CLI, so the CLI-based tests skip). The
`parity` job runs the Go oracle (`tests/parity/go/oracle`, over the zstd-go snapshot in
`tests/parity/go/upstream`) and `examples/oracle.rs` on the same inputs and diffs the output trees,
partial outputs included, at a 256 MiB memory limit and again at 1 MiB on the small inputs; [parity.md](parity.md) has the inputs, the operations and the error-class
mapping. The `parity-arm64` job repeats the parity run on a native aarch64 runner.
