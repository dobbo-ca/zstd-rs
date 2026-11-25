# zstd-rs

A Zstandard (RFC 8878) decoder tuned for WSI image tiles, with a decoder for CZI zstd0/zstd1
subblocks, in Rust. A port of [dobbo-ca/zstd-go](https://github.com/dobbo-ca/zstd-go)
(`tests/parity/go/upstream/` holds a snapshot of its sources). Decode only: no
dictionaries, no legacy formats, no streaming reader. Pure Rust, std only; `unsafe` only in the
kernel modules ([docs/safety.md](docs/safety.md)).

```toml
zstd = { git = "https://github.com/dobbo-ca/zstd-rs", rev = "<sha>" }
```

## Quick start

```rust
use zstd::Limits;
use std::sync::atomic::AtomicBool;

// Every call takes the limits it runs under: here at most 64 MiB of output buffer, one thread.
let limits = Limits { max_threads: 1, max_alloc: 64 << 20, ..Limits::default() };
let cancel = AtomicBool::new(false);

let out = zstd::decode_all(frames, &limits, &cancel)?;

// Reuse a Decoder (about 200 KiB of tables, plus about 200 KiB per extra thread after a
// parallel decode) and append to a buffer you own.
let mut d = zstd::Decoder::new(&limits)?;
d.decode_all_into(&mut buf, frames, &limits, &cancel)?;
// Or decode into a buffer you reuse, with no zero-filling, and no allocation on one thread
// (a parallel decode allocates its thread spawns): the number of bytes.
let n = d.decode_to_slice(&mut tile, frames, &limits, &cancel)?;
let size = zstd::content_size(frames, &limits)?; // Option<u64>

// CZI subblocks, compression mode 5 (zstd0) or 6 (zstd1), into a bitmap at minimal stride.
zstd::czi::Decoder::new(&limits)?.decode(&mut bitmap, payload, zstd::czi::ZSTD1, pixel_type, &limits, &cancel)?;
```

## Resource limits

Every entry point takes `&Limits` and a cancel flag (`Error::Cancelled` when set).

| field | default | unit | over the limit |
|---|---|---|---|
| `max_output` | 1 GiB | bytes appended to the output | `Error::OutputLimit` |
| `max_alloc` | 3 GiB | bytes allocated for the output buffer | `Error::AllocLimit` |
| `max_threads` | `std::thread::available_parallelism()`, at least 1 | threads, the caller's included | never exceeded |

- `max_alloc` is the enforced peak memory of one call, all threads together: every growth, and
  the tables of a new decoder for an extra thread, is charged before it is allocated.
- `max_threads = 1` spawns no thread. A frame is sequential, but an input of several frames
  (256 KiB of output or more) whose output fits the buffer without growing decodes on up to
  `max_threads` threads, the caller's included, with the serial output and error. The threads
  are scoped to the call; there is no thread pool ([docs/threading.md](docs/threading.md)).
- Cancellation: every thread checks the flag at each block.

Details: [docs/limits.md](docs/limits.md).

## Docs

- [docs/limits.md](docs/limits.md): limits, cancellation and how `max_alloc` is charged
- [docs/threading.md](docs/threading.md): frames decoded in parallel
- [docs/go-differences.md](docs/go-differences.md): API differences, decoding rules, errors
- [docs/safety.md](docs/safety.md): `unsafe`, SIMD tiers and kernels
- [docs/testing.md](docs/testing.md): tests and CI
- [docs/parity.md](docs/parity.md): Go parity job
- [docs/fuzzing.md](docs/fuzzing.md): fuzz targets (`cargo fuzz --fuzz-dir tests/fuzz`)
- [docs/benchmarks.md](docs/benchmarks.md): benchmark and results
- [docs/performance.md](docs/performance.md): consumer build profile, reuse, PGO and BOLT

## Licence

Apache-2.0, see LICENSE. NOTICE has the libzstd licence for the ported code.
