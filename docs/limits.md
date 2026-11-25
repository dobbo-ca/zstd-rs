# Resource limits

Every entry point takes `&Limits` and a cancel flag.

| field | default | unit | over the limit |
|---|---|---|---|
| `max_output` | 1 GiB | bytes appended to the output | `Error::OutputLimit` |
| `max_alloc` | 3 GiB | bytes allocated for the output buffer (charged as below) | `Error::AllocLimit` |
| `max_threads` | `std::thread::available_parallelism()`, at least 1 | threads, the caller's included | never exceeded |

- `max_alloc` bounds the peak memory of one call. Each growth of the output buffer is charged its
  whole new size before it is allocated and the charges are summed, as zstd-go charges its
  `SetMemoryLimit` budget; the sum is an upper bound on the bytes held at any moment, so a call
  that would pass it fails with `AllocLimit` without allocating. An output of `n` bytes needs
  between `n` and about `3 n`. It covers every allocation that grows with the input in one call:
  the output `Vec` of `decode_all_into`, czi's scratch buffer and, in `decode_all`, the decoder's
  tables (about 200 KiB). A `Decoder` or `czi::Decoder` checks its tables against the budget once,
  in `new`, and a call on a reused decoder does not charge them again. Likewise, a reused
  `czi::Decoder` keeps its scratch buffer between calls (as zstd-go does): a call charges only
  what it grows, so the buffer kept from an earlier call is not charged again. A buffer the caller passes
  in, and `decode_to_slice`, which allocates nothing for the output, are not charged. Frames
  decoded in parallel (below) need a `Decoder` per extra thread: the decoder makes these helpers
  the first time and keeps them, and the tables of each new one (about 200 KiB) are charged with
  what the call has charged so far (`decode_all` always decodes serially: its `Vec` starts
  empty). Where they do not fit, the call uses fewer threads, down to the caller's alone, so the
  budget never makes a parallel decode fail. `tests/resources.rs` measures the peak with a counting allocator for
  several limits, including one that fits only some of the helpers. The bookkeeping of each
  spawned thread (under 1 KiB) is charged only with a new helper, not when a reused decoder's
  helpers run again.
- `max_threads`: decoding a frame is sequential and zstd-go uses no goroutines. The frames of one
  input are independent, so an input of two frames or more, all with a content size and 256 KiB
  of output or more in all, whose output fits the buffer without growing (a `decode_to_slice`
  buffer, the capacity of a `decode_all_into` `Vec`, a czi `dst` or the czi scratch buffer kept
  from an earlier call), is decoded on up to `max_threads` threads: the caller's and up to
  `max_threads - 1` spawned for the call, each decoding a contiguous run of whole frames
  ([threading.md](threading.md)). Nothing grows on that path, so `max_alloc` is charged as in
  the serial loop (plus the helpers' tables). The output and the error are the serial ones
  for every value; anything else (one frame, a frame without a content size, a buffer that
  must grow) decodes on the caller's thread, and `1` never spawns. `tests/resources.rs` checks
  that a call uses at most `max_threads` threads (the OS thread count, on Linux), and `tests/frames_parallel.rs` that every value
  gives the serial result.
- Cancellation: the flag is checked at each block (by every thread), every 128 KiB of output inside a compressed
  block (and of zero-filling a buffer), while the frame checksum is computed and every 1024
  skippable frames. A set flag is `Error::Cancelled`. As in zstd-go, which checks its context per
  block, a few skippable frames, or a frame that fails in its header, are not cancelled. A frame
  with a content size does not bound a block to 128 KiB of output (a few kilobytes of sequences
  can expand to gigabytes; its raw and RLE blocks are at most 2 MiB), hence the check inside a
  block.
- `max_output` caps the bytes appended: a frame whose content size would pass it fails before
  anything is allocated (`content_size` takes `Limits` and reports it the same way).

## How `max_alloc` is charged

Go had only the process-wide `SetMemoryLimit` (default: none, or half of `GOMEMLIMIT`), so the
defaults are finite: 1 GiB of output and 3 GiB of `max_alloc` (the 3 is the `3 n` above). The
charge is Go's: every reallocation of the output buffer adds its whole new size, and the sum may
not pass `max_alloc`. That counts the old buffer while it is copied, so the peak is within
`max_alloc`. A frame without a content size only guesses its size (32 times the input, then
doubling), and Go charges the guess, so does this crate: a 4 MiB output behind 128 MiB of input is
`AllocLimit` under a 16 MiB budget on both sides. Only `max_output`, which Go lacks, cuts the guess
(to `max_output` plus 32 bytes), so a one-byte output never costs a gigabyte. The buffer is grown
with `try_reserve_exact` (`Error::OutOfMemory`, not an abort), and is never sized from a header: a
frame that claims more than 4 input bytes per 128 KiB block is `Corrupt`, and a buffer grows as
the frame really decodes. A `Vec` that already has room is not charged for it, and growth starts
from its capacity as Go's from `cap(dst)`; its spare capacity is zero-filled only as far as the
output reaches, after the frame header has been checked. czi's scratch buffer holds the whole
frame and is charged the same way, per call; the decoder keeps it between calls, as Go does (the
growth of the frames that decoded, not of a frame that failed), and does not charge it again. When
the output of a zstd0/zstd1 payload passes `dst`, it moves to a buffer that grows from `dst`'s
size, each growth charged, and the first frame (in `dst`) costs nothing, as in Go, where the
growth starts from `cap(dst)`. A `Vec` that a failed `decode_all_into` leaves has the capacity Go's
returned slice has (the original, or what the frames before the failing one grew).
