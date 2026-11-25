#![forbid(unsafe_code)]

use std::fmt;

/// One variant per Go error. Go's errors wrap `ErrCorrupt` for everything wrong with the input;
/// the text says what, and is not part of the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `ErrCorrupt` and every error that wraps it (including czi's).
    Corrupt(&'static str),
    /// `ErrChecksum`.
    Checksum,
    /// `ErrDictionary`.
    Dictionary,
    /// `ErrMemoryLimit`: the output buffer would pass `Limits::max_alloc`.
    AllocLimit,
    /// `ErrWindowTooLarge`.
    WindowTooLarge,
    /// czi: the compression mode is not zstd0 or zstd1 (Go's `errMode`).
    Mode,
    /// The output would pass `Limits::max_output`. Go had no such limit.
    OutputLimit,
    /// The allocator refused a size that is within the limits.
    OutOfMemory,
    /// The cancel flag was set (Go: `ctx.Err()`).
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt(s) => write!(f, "zstd: corrupt input: {s}"),
            Self::Checksum => f.write_str("zstd: checksum mismatch"),
            Self::Dictionary => f.write_str("zstd: dictionaries are not supported"),
            Self::AllocLimit => f.write_str("zstd: memory limit exceeded"),
            Self::WindowTooLarge => f.write_str("zstd: window too large"),
            Self::Mode => f.write_str("czi: compression mode is not zstd0 or zstd1"),
            Self::OutputLimit => f.write_str("zstd: output limit exceeded"),
            Self::OutOfMemory => f.write_str("zstd: out of memory"),
            Self::Cancelled => f.write_str("zstd: cancelled"),
        }
    }
}

impl std::error::Error for Error {}
