# Fuzzing

`tests/fuzz/` is a cargo-fuzz crate (the only place `libfuzzer-sys` is used): `decode` (any input: no
panic, the same result from a shared and a fresh decoder, with a prefix in `dst` and with spare
capacity, a failed decode appends nothing; `decode_to_slice` on every input, a byte short of the
output included), `limits` (random `Limits` and cancel flag, seeded from `tests/fuzz/seeds-limits`: the
frames behind a prefix of big limits, through `decode_all`, `decode_all_into` on a shared decoder
and a `Vec` with room, `decode_to_slice` and the czi decoder; a counting allocator asserts that a
call never holds more than `max_alloc`, and that a call with the cancel flag set returns
`Cancelled` when the same decode without it gives output), `content_size`,
`czi` (against a port of libCZI's decoder, as zstd-go's `FuzzDecode`, with one reused decoder) and `interleave` (the SIMD
tier against scalar, from any alignment into a canary-padded buffer, asserting that AVX2/NEON was
detected), `huf` (the plain and BMI2 Huffman 4-stream kernels against the safe loop:
`[x2 flag][output length u16][table header][4-stream literals]`, the table read through `read_x1`
or `read_x2`; the same output bytes, input and output positions and bit containers; asserting
that BMI2 was detected; seeded with `tests/fuzz/seeds-huf`, X1 and X2 tables and streams in that
layout from the generator of `hufloop::tests`) and `seqloop` (the plain and BMI2 sequence loops against the careful path: the same
result and the same output of the blocks that decoded, so a wrong block shows even when the frame
then fails; a frame's checksum flag is cleared and the checksum dropped first; seeded with
`tests/fuzz/seeds`, the golden frames, the tiles and the decodecorpus frames) and `frames` (the
input, and the input repeated with a skippable frame, an empty frame and a cut or a changed byte
in the last copy, or in the first and last copies, decoded at `max_threads` 1 and 4 into a buffer
and a `Vec` with room, sized from the output or, where the input fails, from its frames' content
sizes: the same result, output and error, so the parallel decode of frames is the serial one). Seeds are
zstd-go's `FuzzDecodeAll` entries (`tests/fuzz/seeds`, `tests/fuzz/seeds-czi`) and the golden and conformance
frames; the multi-block hi/lo `FuzzDecode` entry is `tests/fuzz/seeds-czi/3238c7f359f276cc`. The targets
are built with debug assertions (`cargo fuzz build -a`, the aarch64 builds too), so an integer
overflow is a crash, and
the corpus is kept between runs with `actions/cache`. `.github/workflows/fuzz.yml` runs them weekly and on `workflow_dispatch`
(`max_total_time` per target, default 600 s; `-rss_limit_mb=2048 -malloc_limit_mb=512`), with
`fuzz-arm64` running `interleave` (NEON) and `seqloop` (the plain loop) on a native aarch64
runner.
`czi` runs with `-max_len=131086`, so its payloads can span several blocks.
