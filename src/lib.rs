//! Zstandard (RFC 8878) decoding, tuned for WSI image tiles, with a decoder for CZI zstd0/zstd1
//! subblocks in [`czi`].
//!
//! Decode only: no dictionaries, no legacy formats, no streaming reader. The output of a decode
//! that succeeds, and the class of the error of one that fails, equal zstd-go's, with these
//! exceptions: [`Error::OutputLimit`] and [`Error::OutOfMemory`] have no Go counterpart (Go has no
//! output limit), and the guess of a frame without a content size is cut to `max_output`. A
//! failed decode appends nothing. Every entry point takes [`Limits`] and a cancel flag, the
//! constructors included: a decoder's fixed tables (about 200 KiB) must fit `max_alloc`.
//!
//! # Resource limits
//!
//! A caller in a small pod sets the limits and the call stays inside them:
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//! use zstd::Limits;
//!
//! // A frame with one raw block that holds "hello".
//! let frame = [
//!     0x28, 0xB5, 0x2F, 0xFD, 0x20, 0x05, 0x29, 0x00, 0x00, b'h', b'e', b'l', b'l', b'o',
//! ];
//! let limits = Limits {
//!     max_threads: 1,
//!     max_alloc: 64 << 20,
//!     ..Limits::default()
//! };
//! let cancel = AtomicBool::new(false);
//! let out = zstd::decode_all(&frame, &limits, &cancel)?;
//! assert_eq!(out, b"hello");
//! # Ok::<(), zstd::Error>(())
//! ```
//!
//! | field | default | unit | over the limit |
//! |---|---|---|---|
//! | [`max_output`](Limits::max_output) | 1 GiB | bytes appended to the output | [`Error::OutputLimit`] |
//! | [`max_alloc`](Limits::max_alloc) | 3 GiB | bytes allocated for the output buffer, charged as zstd-go charges its budget | [`Error::AllocLimit`] |
//! | [`max_threads`](Limits::max_threads) | [`std::thread::available_parallelism`], at least 1 | threads, the caller's included | never exceeded |
//!
//! `max_alloc` bounds the peak memory of one call: every growth of the output buffer is charged
//! its whole new size before it is allocated, and the charges are summed, so the sum is an upper
//! bound on the bytes held at any moment (see [`Limits::max_alloc`] for what is covered). It is
//! checked, not estimated: `tests/resources.rs` measures the peak with a counting allocator.
//!
//! Decoding a frame is sequential, as in zstd-go, which uses no goroutines. An input of several
//! frames with content sizes (256 KiB of output or more) whose output fits the buffer without
//! growing is decoded on up to `max_threads` threads, whole frames per thread: the caller's and
//! up to `max_threads - 1` spawned for the call. The output and the error are the serial ones (and
//! zstd-go's) for every `max_threads`; `max_threads = 1` never spawns. See `docs/threading.md`.
//!
//! The cancel flag is an [`AtomicBool`](std::sync::atomic::AtomicBool) every entry point takes. It
//! is checked at each block, every 128 KiB of output within a compressed block and of zero-filling,
//! while the frame checksum is computed and every 1024 skippable frames; a set flag is
//! [`Error::Cancelled`].
#![deny(
    unsafe_code,
    unsafe_op_in_unsafe_fn,
    clippy::undocumented_unsafe_blocks
)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
// The tables and bit readers are ported index by index from libzstd; iterator rewrites of those
// loops would not be clearer.
#![allow(
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::manual_memcpy
)]

mod bits;
pub mod czi;
mod error;
mod frame;
mod huf;
#[allow(unsafe_code)]
mod hufloop;
mod interleave;
mod limits;
mod literals;
mod seq;
#[allow(unsafe_code)]
mod seqloop;
mod xxhash;

use std::sync::atomic::AtomicBool;

use frame::Out;
use huf::HufTable;
use seq::SeqTables;

pub use error::Error;
pub use frame::content_size;
// Only the differential fuzz target (cargo-fuzz sets `--cfg fuzzing`) calls the bare kernels.
#[cfg(fuzzing)]
pub use hufloop::huf_kernels;
#[cfg(fuzzing)]
pub use interleave::{interleave_scalar, interleave_simd};
pub use limits::Limits;
#[cfg(fuzzing)]
pub use seqloop::seqloop_modes;

pub(crate) const BLOCK_SIZE_MAX: usize = 128 << 10;
/// `ZSTD_decompressStream`'s default window limit, for frames without a content size.
pub(crate) const MAX_WINDOW: u64 = (1 << 27) + 1;
/// The sequence loop's copies may write this far past the current output position.
pub(crate) const WILDCOPY_SLACK: usize = 32;

/// The literals buffer of a [`Decoder`].
const LIT: usize = BLOCK_SIZE_MAX + WILDCOPY_SLACK;
/// What a [`Decoder`] holds for its whole life (about 200 KiB).
const TABLES: usize = LIT + size_of::<HufTable>() + size_of::<SeqTables>();

/// A reusable decoder. Reuse it across calls; it holds about 200 KiB of tables, and after a
/// parallel decode up to `max_threads - 1` helper decoders of about 200 KiB each, which it keeps
/// (see [`Limits::max_threads`]).
pub struct Decoder {
    /// Literals regenerated for the current block, plus wildcopy read slack.
    lit: Vec<u8>,
    huf: Box<HufTable>,
    seq: Box<SeqTables>,
    rep: [u64; 3],
    /// A Huffman table exists for treeless literals.
    lit_ok: bool,
    /// Sequence tables exist for repeat mode.
    fse_ok: bool,
    /// The writable extent of the output after the last frame that decoded in the last
    /// `decode_frames`, or at its start: what zstd-go's `DecodeAll` returns as the buffer when a
    /// later frame fails.
    kept: usize,
    /// The decoders of the threads that decode frames in parallel, made on first use and kept.
    helpers: Vec<Decoder>,
}

impl Decoder {
    /// A decoder. Its fixed tables (about 200 KiB) are allocated here and must fit
    /// `limits.max_alloc`; they are not charged again to a decode with this decoder.
    ///
    /// # Errors
    ///
    /// [`Error::AllocLimit`] if the tables do not fit `limits.max_alloc`, [`Error::OutOfMemory`]
    /// if the allocator refuses.
    pub fn new(limits: &Limits) -> Result<Self, Error> {
        if TABLES > limits.max_alloc {
            return Err(Error::AllocLimit);
        }
        let mut lit = Vec::new();
        lit.try_reserve_exact(LIT).map_err(|_| Error::OutOfMemory)?;
        lit.resize(LIT, 0);
        Ok(Self {
            lit,
            huf: HufTable::new(),
            seq: SeqTables::new(),
            rep: [1, 4, 8],
            lit_ok: false,
            fse_ok: false,
            kept: 0,
            helpers: Vec::new(),
        })
    }

    /// Appends the decoded frames in `src` to `dst`. A frame with a content size decodes as
    /// libzstd's one-shot `ZSTD_decompress` does; one without follows `ZSTD_decompressStream`'s
    /// limits. Skippable frames decode to nothing. On error `dst` is as it was.
    ///
    /// `limits.max_output` caps the bytes appended. `limits.max_alloc` caps the total size of the
    /// buffers allocated while `dst` grows (each reallocation's new size is charged, as in
    /// zstd-go, so the old buffer is counted while it is copied). Growth starts from the capacity
    /// of `dst`, as zstd-go's from `cap(dst)`; the spare capacity is zero-filled only as far as
    /// the output reaches, after the frame header has been checked. The cancel flag is checked at
    /// each block, every 128 KiB of output within a compressed block and of zero-filling, while
    /// the frame checksum is computed, and every 1024 skippable frames.
    ///
    /// # Errors
    ///
    /// [`Error::OutputLimit`] if the output would pass `limits.max_output`,
    /// [`Error::AllocLimit`] if the growth would pass `limits.max_alloc`,
    /// [`Error::Cancelled`] if `cancel` is set, [`Error::OutOfMemory`] if the allocator refuses,
    /// and the errors of a malformed frame ([`Error::Corrupt`], [`Error::Checksum`],
    /// [`Error::Dictionary`], [`Error::WindowTooLarge`]). Frames that fit the capacity of `dst`
    /// may decode on up to `limits.max_threads` threads (see [`Limits::max_threads`]), with the
    /// same result.
    pub fn decode_all_into(
        &mut self,
        dst: &mut Vec<u8>,
        src: &[u8],
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        self.decode_into(dst, src, limits, cancel, 0)
    }

    /// `decode_all_into` with `charged` bytes already counted against `limits.max_alloc`.
    fn decode_into(
        &mut self,
        dst: &mut Vec<u8>,
        src: &[u8],
        limits: &Limits,
        cancel: &AtomicBool,
        charged: usize,
    ) -> Result<(), Error> {
        let start = dst.len();
        let ext = dst.capacity();
        match self.decode_frames(
            &mut Out::Grow(dst, charged, ext),
            start,
            src,
            limits,
            cancel,
            &mut |_: &[u8]| {},
        ) {
            Ok(end) => {
                dst.truncate(end);
                Ok(())
            }
            Err(e) => {
                // As zstd-go returns the buffer of the last frame that decoded: the next decode
                // into this `Vec` starts from that capacity, not from what the failed frame grew.
                dst.truncate(start);
                dst.shrink_to(self.kept);
                Err(e)
            }
        }
    }

    /// Decodes the frames in `src` into the start of `dst` and returns the number of bytes
    /// written: no zero-filling and, when it decodes on the caller's thread alone (see
    /// [`Limits::max_threads`]), no allocation, for a buffer the caller reuses. A frame with a
    /// content size needs `dst` to hold it; one without needs room for the block it is decoding
    /// (a compressed block, up to 128 KiB) past the output so far. Output that does not fit is
    /// [`Error::OutputLimit`]. On error `dst` is undefined. `limits.max_output` caps the bytes
    /// reported: a compressed block is checked after it is written, so on [`Error::OutputLimit`]
    /// up to one block (128 KiB) of `dst` may be written past it. It allocates nothing for the
    /// output, so `max_alloc` applies only to the tables of the decoders that frames decoded in
    /// parallel need the first time (see [`Limits::max_threads`]); each parallel call also
    /// allocates std's bookkeeping for the threads it spawns (under 1 KiB a thread), which is
    /// not charged when the decoder's helpers already exist. The cancel flag is checked at
    /// each block and every 128 KiB of output within a compressed block, while the frame checksum
    /// is computed, and every 1024 skippable frames.
    ///
    /// # Errors
    ///
    /// [`Error::OutputLimit`] if the output does not fit `dst` or passes `limits.max_output`,
    /// [`Error::Cancelled`] if `cancel` is set, and the errors of a malformed frame
    /// ([`Error::Corrupt`], [`Error::Checksum`], [`Error::Dictionary`],
    /// [`Error::WindowTooLarge`]).
    pub fn decode_to_slice(
        &mut self,
        dst: &mut [u8],
        src: &[u8],
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<usize, Error> {
        self.decode_frames(
            &mut Out::Fixed(dst),
            0,
            src,
            limits,
            cancel,
            &mut |_: &[u8]| {},
        )
    }
}

/// Decodes the frames in `src` with a new [`Decoder`] and returns the output; see
/// [`Decoder::decode_all_into`]. The decoder's tables (about 200 KiB) are charged to
/// `limits.max_alloc` together with the output buffer's growth, so the budget covers the whole
/// call.
///
/// # Errors
///
/// As [`Decoder::decode_all_into`], and [`Error::AllocLimit`] if the tables alone do not fit
/// `limits.max_alloc`.
pub fn decode_all(src: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Vec<u8>, Error> {
    let mut dst = Vec::new();
    Decoder::new(limits)?.decode_into(&mut dst, src, limits, cancel, TABLES)?;
    Ok(dst)
}
