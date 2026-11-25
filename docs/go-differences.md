# Differences from zstd-go

## API

Differences from Go's API, besides the idiomatic shapes: `decode_to_slice` is Go's "dst with room"
(a frame with a content size needs `dst` to hold it, one without needs room for the block it is
decoding, up to 128 KiB, else `Error::OutputLimit`). The czi decoder decodes straight into `dst` when the content
size fits, as Go does (hi/lo frames go through a scratch buffer that is kept between calls).

## Decoding rules

A frame with a content size decodes exactly as libzstd's one-shot `ZSTD_decompress` does, which is
what libCZI calls; one without follows `ZSTD_decompressStream` (window at most 2^27 + 1). The
decode rules are zstd-go's. Skippable frames decode
to nothing.

## Threads

zstd-go decodes the frames of an input one after another and uses no goroutines. This crate
decodes the frames of an input of several frames on up to `Limits::max_threads` threads when
their output fits the buffer without growing ([threading.md](threading.md)). The output, the
error (the one of the first frame that fails) and the `max_alloc` charges of the output buffer
are Go's for every thread count; `max_threads = 1` is Go's serial loop.

## Errors

One enum, `zstd::Error`, with a variant for each Go error: `Corrupt(&str)` is `ErrCorrupt` and
every error that wraps it (czi's `errHeader`, `errHiLo`, `errNoSize`, `errSizeLeft` included; the
text says which), `Checksum`, `Dictionary`, `AllocLimit` (`ErrMemoryLimit`) and `WindowTooLarge`
are the `Err*` values, `Mode` is czi's `errMode`, and `Cancelled` is `ctx.Err()`. `OutputLimit`
and `OutOfMemory` have no Go counterpart. Go's `errOverlap` (`src` inside `dst`'s spare
capacity) cannot happen here: the borrow checker rules it out. A failed decode appends nothing to
`dst` (Go returns `dst[:start]`).
