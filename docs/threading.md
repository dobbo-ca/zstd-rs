# Threads

A zstd frame is sequential: each block can copy from the output of the blocks before it, so one
frame decodes on one thread, as in zstd-go, which uses no goroutines. The frames of one input are
independent (each starts with fresh repeat offsets and tables, and a match cannot reach before
its frame), so an input of several frames can decode them at the same time. That is what
`Decoder::decode_frames` (`src/frame.rs`, `decode_par`) does, when:

- `Limits::max_threads` is 2 or more;
- a scan of the frame and block headers (no decoding) splits the input into frames with nothing
  left over: two frames or more, each with a content size and no dictionary, no reserved block
  type and nothing cut short, and fewer than 1024 skippable frames in the input (the serial loop
  checks the cancel flag every 1024);
- the content sizes add up to 256 KiB or more (128 KiB, `PAR_MIN`, per thread), within
  `max_output`;
- the output fits the buffer without growing: the `decode_to_slice` buffer, the capacity of the
  `decode_all_into` `Vec` (zero-filled first, as the serial loop zero-fills it, checking the
  cancel flag), the czi `dst` or the czi scratch buffer kept from an earlier call.

Anything else runs the serial loop, which then gives the exact error. Otherwise the output is
split into one slice per frame, of its content size, and the frames into
`min(max_threads, frames, output / 128 KiB)` contiguous runs of about equal output; the caller's thread decodes
the last run and a scoped thread each of the others. Each frame is decoded by the serial
`decode_frame` into its slice, from the input at its start and with the output limit moved by
its start, so every check it makes sees the numbers the serial loop gives it, moved by the
frame's start. A run stops at its first error, and the call returns the error of the first run
that fails, which holds the first frame that fails: the serial loop's error. The output and the
error are the same for every `max_threads`, and zstd-go's: `tests/frames_parallel.rs` and the
`frames` fuzz target check the output and the error against the serial loop, and the `mf-*`
parity inputs check the output against zstd-go (a variant that fails is too small for the
parallel path there, so its error is compared on the serial path only).

Each extra thread needs a `Decoder` of its own (about 200 KiB of tables). The decoder makes these
helpers the first time it needs them and keeps them, so a reused decoder allocates no tables
after the first call; each call still allocates std's bookkeeping for the threads it spawns
(under 1 KiB a thread), which is charged to `max_alloc` only with a new helper. The tables of a new helper are charged to `max_alloc` with what the call
has charged so far; where they do not fit, fewer threads are used, down to the caller's alone.
The parallel path never grows the output, so the output's charges are the serial ones.

## Why a scope per call and no pool

The crate has no dependencies, and its threading uses only safe Rust. A pool of
long-lived threads that decodes into the caller's buffer needs to hand those threads a borrow of
the buffer that outlives the call as far as the type system knows, which in std-only Rust takes
`unsafe` (or `'static` copies of the input and output, which cost what the threads save).
`std::thread::scope` gives the borrow safely: each call spawns at most `max_threads - 1` threads
and joins them before it returns, so a call never uses more than `max_threads` threads and no
thread outlives it. Spawning a thread costs some tens of microseconds; at 128 KiB of output or
more per thread that is small against the decode, and smaller inputs stay on the caller's thread.
If the OS refuses a thread, the call decodes the input again on the serial path.
