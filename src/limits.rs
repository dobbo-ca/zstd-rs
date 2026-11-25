#![forbid(unsafe_code)]

/// Caps on one decode call. Go had no byte limits except `SetMemoryLimit`; the defaults are finite.
/// Build one with `Limits { max_alloc: 64 << 20, ..Limits::default() }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most bytes a call may append to the output. More is
    /// [`Error::OutputLimit`](crate::Error::OutputLimit). Default 1 GiB.
    pub max_output: usize,
    /// The budget for the memory a call allocates for the output buffer, in bytes. Default 3 GiB.
    ///
    /// It is charged as zstd-go charges its `SetMemoryLimit`: each reallocation of the output
    /// buffer is charged its whole new size and the charges are summed, so the old buffer is
    /// counted while it is copied. That sum is an upper bound on the bytes the buffer holds at
    /// any moment (the peak), never below it: an output of `n` bytes needs between `n` and about
    /// `3 n` (the growth is geometric). The charge is made before the allocation, so a call
    /// that would pass the budget fails without allocating.
    ///
    /// What it covers: every allocation that grows with the input in one call. That is the
    /// output `Vec` of `decode_all_into` (growth from its capacity on, as zstd-go's from
    /// `cap(dst)`), czi's scratch buffer for a frame that does not fit `dst`, and, in
    /// [`decode_all`](crate::decode_all), the decoder's fixed tables (about 200 KiB) as well.
    /// A `Decoder` or `czi::Decoder` built with `new` checks its tables against the budget once
    /// and keeps them (and, for czi, the scratch buffer of the last call) between calls; a call
    /// on a reused decoder does not charge them again. A buffer the caller passes in, and
    /// `decode_to_slice`, which allocates nothing for the output, are not charged. Frames decoded
    /// in parallel (see `max_threads`) need a decoder per extra thread: the decoder makes them
    /// the first time and keeps them, and each one's tables (about 200 KiB) are charged with what
    /// the call has charged so far; where they do not fit, fewer threads are used, down to one.
    /// That path never grows the output, so the charges of the output are the serial ones.
    /// Everything else is fixed in size, so it is not charged; std's bookkeeping for each thread
    /// a call spawns (under 1 KiB) is charged only with a new helper, not when kept ones run.
    ///
    /// More is [`Error::AllocLimit`](crate::Error::AllocLimit), Go's `ErrMemoryLimit`.
    pub max_alloc: usize,
    /// The most OS threads one call uses, the calling thread included. Default
    /// [`std::thread::available_parallelism`], at least 1; 1 never spawns.
    ///
    /// Decoding a zstd frame is sequential, and zstd-go uses no goroutines. The frames of one
    /// input are independent, though: an input of two frames or more, all with content sizes and
    /// 256 KiB of output or more in all, whose output fits the buffer without growing (a
    /// `decode_to_slice` buffer, the capacity of a `decode_all_into` `Vec`, a czi `dst` or
    /// scratch buffer), is decoded on up to `max_threads` threads: the caller's, and up to
    /// `max_threads - 1` spawned for the call in a `std::thread::scope`, each decoding a run of
    /// whole frames. The output and the error are the serial ones (zstd-go's) whatever this is.
    /// Anything else decodes on the caller's thread. See `docs/threading.md`.
    pub max_threads: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_output: 1 << 30,
            max_alloc: 3 << 30,
            max_threads: std::thread::available_parallelism().map_or(1, usize::from),
        }
    }
}
