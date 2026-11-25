# Parity with Go

`tests/parity/go/upstream/` is the non-test source of github.com/dobbo-ca/zstd-go (with `go.mod` and
`LICENSE`; zstd-go has no NOTICE), including its asm. `tests/parity/go/oracle/` is a Go program with its own
`go.mod` that `replace`s the module with `../upstream`. The CI job `parity` (`.github/ci/Dockerfile`,
stages `go-oracle`, `go-run`, `parity`) runs:

1. `oracle -gen tests/data /inputs` writes the inputs.
2. `oracle /inputs /go-out` decodes them with zstd-go (`SetMemoryLimit(256 MiB)`, so a hostile
   input cannot exhaust the memory of the build or CI machine).
3. `cargo run --release --example oracle -- /inputs /rust-out` decodes them with this crate
   (`Limits { max_output: usize::MAX, max_alloc: 256 MiB, max_threads: 4 }`: inputs of several
   frames whose output fits the buffer decode on several threads, and Go's serial result must
   come out).
4. `tests/parity/diff.sh /go-out /rust-out` (`diff -rq`, with a summary of the differences) must be empty.
   It compares about 32,000 inputs and as many `.res` files (one per input, every operation in it); the stage fails first if `/inputs`
   has fewer than 30,000 files or `/go-out` fewer than 30,000, so an empty tree cannot pass. A
   difference is a bug in the Rust port, never in the oracle.
5. The same Rust oracle runs again with `ZSTD_SIMD=scalar`, diffed against `/go-out`. The
   `parity-arm64` job (stage `parity-arm64`) runs steps 1 to 4 on a native aarch64 runner (NEON,
   with the Go oracle built there too): the SIMD tiers are compared with Go, the czi hi/lo corpus
   included.
6. The memory limit is also compared at a small value. Both oracles read `ORACLE_MEM_LIMIT`
   (bytes; default 256 MiB): `go-run` runs the Go oracle with `ORACLE_MEM_LIMIT=1048576` over
   `/inputs` with `ORACLE_MAX_INPUT=15360` (bytes; both oracles skip larger inputs, so only
   the inputs up to 15 KiB are read: most of the inputs, but not the tiles and their mutations), and `parity` runs the Rust oracle the same way
   on the default tier and diffs the trees (`/go-out-1m`). So the charging of every growth of the
   output buffer is compared at 1 MiB, not only at the edge of 256 MiB. The stage fails first if
   `/go-out-1m` has fewer than 1,000 files.

## Inputs

Every `.zst` under `tests/data/` (golden, golden-errors, the 304 decodecorpus frames, the klauspost
good and bad files, the tiles), each with its mutations (`<id>~<tag>`; a frame over 64 KiB, 18 of the tiles among them,
gets a capped set, `mutateBig`: 5 truncations, 8 bit flips and 6 byte sets anywhere past the
header, appended bytes, a bad checksum and the first block header's last flag and size):
truncations, 16 bit flips (6 of them in the first 16 bytes) and 9 byte sets (0x00, 0xff, a random
value) at positions from a fixed xorshift seed, appended bytes, 15 frame header descriptors, 6
window bytes, a bad and a skippable magic, every block type and the last flag and size of the
first block header, and a bad checksum. Also:

- `mf-<n>.zst`: 6 inputs of 2 to 4 tiles of at most 512x512 (a fixed xorshift choice), back to
  back, a skippable frame between some, each with `mutateBig`'s variants (`mf-<n>~<tag>`), so an
  error in a later frame is compared. Where the output fits (`room`, `slice`, and `again` after a
  `cap` that decoded), the Rust oracle decodes their frames on 4 threads; zstd-go decodes them
  one after another. A variant whose `dec` fails gets a `room` of 128 KiB, so it decodes on the
  serial path on both sides.
- `fr-<name>.zst`: the hand-written frames of the Go tests (`TestFramesAndErrors`' multi-frame
  input, bad checksum, dictionary, window, truncation, trailing bytes, garbage, skippable frames;
  `TestDocFrames`, `TestOneShotParity`, `TestContentSize`, `TestLyingContentSize`) and the hex
  seeds of `FuzzDecodeAll`; `seed-<hash>.zst`: the files of `tests/fuzz/seeds`, which include its
  `testdata/fuzz/FuzzDecodeAll` entry. `fr-rle-memory-limit` and `fr-rle-under-limit` are RLE
  frames of 140 MiB and 120 MiB (4,494 and 3,854 bytes) that put the `memory-limit` class at the
  edge of the 256 MiB limit: Go charges the sum of its reallocations, so the first fails and the
  second decodes (see `TestMemoryLimit` below).
- `seed-<hash>.czi`: the files of `tests/fuzz/seeds-czi`, zstd-go's `czi/testdata/fuzz/FuzzDecode`
  entries (3238c7f359f276cc, the multi-block hi/lo one, among them), with the length widened from
  u16 to u32.
- `syn-<name>-f<fcs>c<cks>.zst`: the Go `synthetic()` inputs (empty, one byte, random, text, image,
  short periods, far repeats) as raw-block frames with and without a content size and checksum,
  and zeros and a mix as RLE blocks. The Go test compresses them with the zstd CLI; the oracle
  has no CLI, so the compressed-block coverage comes from the fixtures and their mutations, and
  from `tests/differential.rs` on the Rust side. `syn-period-<p>.zst` (p 1 to 15) are hand-built
  compressed blocks: 40 sequences of p literals and a match of over 1 KiB at offset p, the
  overlapping match copy.
- `rf-<nnnn>.zst`: 3000 random frames: random header (descriptor, window, dictionary id, content
  size), one to three blocks of any type with random bodies, an optional random checksum.
- `<fixture>-<tag>.czi` and `czi-*.czi`: czi inputs, `[mode][pixel type][dst length u32 LE]
  [payload]`. Each fixture is wrapped as zstd0, zstd1 (short header, plain header, hi/lo with
  Gray16, Bgr48 and Gray8) with a destination that fits, is cut and is zero filled;
  `czi-<name>.czi` are `czi/czi_test.go`'s `TestSynthetic` cases (on raw-block frames) and
  `TestErrors`; `czi-mut-*` are the mutations of `TestErrors`' frame under three headers.
  `czi-zoverflow-z0` and `-z1h` (a first frame that fits dst, then one of 63 times the size: the
  budget is charged from dst's capacity, not again for the first frame) and `czi-reuse-a-fail-late`
  with `czi-reuse-b-big` (a decode that fails in its second frame leaves the buffer of the first
  to the next decode) put the czi scratch buffer at the edge of the 256 MiB limit.

### Go tests

| Go test | Go cases | Here |
|---|---|---|
| `TestGolden`, `TestGoldenErrors`, `TestDecodecorpus`, `TestKlauspostFixtures` | 4 + 3 + 304 + the klauspost files | the same files as `fx-*` inputs, about 50 mutations of each |
| `TestDocFrames`, `TestOneShotParity`, `TestContentSize`, `TestLyingContentSize`, `TestFramesAndErrors` | 6 + 4 + 4 + 1 + 9 | `fr-*`, 28 inputs |
| `TestDifferentialCLI` (`synthetic()` at levels -1, -3, -9, -19 and `--long`, each with and without a content size and a checksum) | 8 inputs (zeros included), 5 levels x 4 variants = 20 settings | `syn-*`: the 7 inputs as raw-block frames, and zeros and a mix as RLE blocks: 32 inputs, plus 15 hand-built compressed `syn-period-*` frames (47 inputs); the CLI-compressed frames still need the CLI |
| czi `TestSynthetic`, `TestErrors` | 9 + 11 | `czi-*`, 9 + 11 inputs, and 3 x about 50 mutations |
| `FuzzDecodeAll`, czi `FuzzDecode` | seeds | `tests/fuzz/` targets ([fuzzing.md](fuzzing.md)), seeds in `tests/fuzz/seeds` |

Every other Go test, with where it is covered here:

| Go test | Here |
|---|---|
| `TestContext` | the `cancel` operation below |
| `TestMemoryLimit` | `fr-rle-memory-limit` and `fr-rle-under-limit` through the oracle, and every small input again at 1 MiB (step 6) (Rust charges the sum of its reallocations to `max_alloc`, as Go's budget does, so the two agree at the edge); `tests/frames.rs` has `alloc_limit` and `alloc_limit_sums_growth`. A frame without a content size is charged its size guess (32 times the input) on both sides: `fr-hint-memory-limit` (one output byte, 8.5 MiB of skippable frame behind it) is `memory-limit` in Go and Rust, `alloc_limit_charges_guess`; only `max_output`, which the oracle leaves unset, cuts the guess (`guess_cut_to_max_output`). The `cap` operation (below) covers a dst with spare capacity, `alloc_limit_starts_from_capacity` |
| `TestOverlap` | none: the borrow checker rules it out |
| `TestZeroAllocs` | `tests/zero_allocs.rs`: a counting global allocator, one reused `Decoder` and a `Vec` of capacity 1 MiB + 128 KiB on the same two golden files; `decode_all_into` allocates nothing; `reuse_allocates_nothing` does the same for `decode_to_slice` and `czi::Decoder::decode` (zstd0, zstd1, hi/lo) on three tiles at `max_threads = 1` (an input of several large frames at `max_threads > 1` takes the threaded path, whose thread spawns allocate) |
| `TestXXH64` | `src/xxhash.rs` `known_values` (the same 5 vectors) |
| `TestKernelsMatchGo`, `TestInterleaveMatchesGo` | `src/interleave/mod.rs` `simd_matches_scalar` and the `interleave` fuzz target (SIMD against scalar); the czi hi/lo corpus is compared with Go through the oracle on every tier (step 5). For the sequence loop, `src/seqloop.rs` `frames_match_careful` and `random_streams_match_careful` and the `seqloop` fuzz target (the plain and BMI2 loops against the careful path), as Go's check runs its sequence asm against the Go kernel. For Go's Huffman asm loops, `src/hufloop.rs` `kernels_match_safe` and the `huf` fuzz target (the plain and BMI2 raw-pointer Huffman kernels against the safe loops `huf4x1_loop` and `huf4x2_loop`: output, positions and bit containers) |
| `TestCorpus`, `BenchmarkCorpus`, `TestDecodecorpusLocal` | local only (they skip without the corpus); `TestDecodecorpus` covers the 304 frames |
| `BenchmarkDecodeAll`, `BenchmarkDecodeAllSynthetic` | `examples/bench.rs` against `oracle -bench` |

## Operations and output files

Each input gets one file, `<input>.res`: an earlier version wrote a file per operation, about 377,000 small files, and ran out of disk. It holds one record per operation, in the order below, each a header line `<op> <kind> <len>\n` followed by exactly `<len>` bytes. The kind is `out` (the output bytes) or, for an operation that fails, `err` (the error class, no newline) and then `partial` (the bytes returned with the error). Both sides write the same records in the same order, so `diff -r` compares every one.

Per `.zst` input: `dec` (`DecodeAll` into a nil dst), `cap` (into an empty dst whose capacity is a
third of the output, 64 KiB if `dec` failed: growth starts from `cap(dst)`), `again` (into the dst that `cap` returned, emptied: after a failed decode its capacity is the original, or what the frames before the failing one grew), `room` (into an empty dst of capacity output + 128 KiB, which never grows), `slice` (Rust: `decode_to_slice` into a buffer of that size, which allocates nothing; Go writes the `room` result, which must be the same. If the output does not fit the 128 KiB it has (a lying content size, say), `decode_to_slice` says `OutputLimit`, which has no Go class, so the Rust oracle writes the `room` result, but only where `room` failed or the content size passes the buffer; any other `OutputLimit` is compared and shows as a diff), `pre` (into a dst that holds `pre`, on the
same decoder after `dec`: a failed decode must give `pre` back), `size` (`ContentSize`: the number,
`none`, or the error) and `cancel` (`DecodeAllContext` with a cancelled context, Rust: the cancel
flag set) and `reuse` (one decoder decodes the two inputs before this one in name order, then this one into a nil dst; the result of the last is compared: the state a decode leaves behind must not show). Per `.czi` input: `run` (the whole destination, filled with 0xAA first, or nothing on
an error: Go leaves it undefined), `cancel` and `reuse` (one czi decoder decodes the two inputs before this one, then this one). Each is a record `<op> out <len>` with the output
bytes, or `<op> err <len>` with the error class. Go returns the bytes before an
error with it (`dst[:start]`, here empty or `pre`), and so does this crate, so an operation that
fails also writes an `<op> partial <len>` record with them; both sides write it, and they must be
equal.

## Error classes

| class | Go | Rust |
|---|---|---|
| `corrupt` | `ErrCorrupt` and every error wrapping it (incl. czi's `errHeader`, `errHiLo`, `errNoSize`, `errSizeLeft`) | `Error::Corrupt(_)` |
| `checksum` | `ErrChecksum` | `Error::Checksum` |
| `dictionary` | `ErrDictionary` | `Error::Dictionary` |
| `memory-limit` | `ErrMemoryLimit` | `Error::AllocLimit` |
| `window-too-large` | `ErrWindowTooLarge` | `Error::WindowTooLarge` |
| `mode` | czi `errMode` | `Error::Mode` |
| `cancelled` | `context.Canceled` | `Error::Cancelled` |
| (none) | `errOverlap` | cannot happen (borrow checker) |

`Error::OutputLimit` and `OutOfMemory` have no Go counterpart (Go has no output limit); they would
print as `other:<message>` and show up as a diff. `bad-input` is a `.czi` file that is short or
asks for a destination over 4 MiB, on both sides.

## Benchmark

`bench.sh` and `.github/workflows/bench.yml`: `oracle -bench N` and `examples/bench.rs` decode each
tile in `tests/data/tiles` N times and print the ns per decode, and again each `_hilo` tile as a czi
zstd1 payload (`czi-<name>`); `bench.sh` joins them into a ms/MB table by format and content. Both
sides reuse their decoder and a pre-sized buffer, as `BenchmarkDecodeAll` does. It prints the tables
twice: at the default parallelism, then with `GOMAXPROCS=1` and `BENCH_MAX_THREADS=1`
(`Limits::max_threads = 1`). `profile.sh` (the `profile` input of bench.yml) writes callgrind
profiles of both sides and the disassembly of the Rust sequence and Huffman loops.
