#![no_main]
use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use zstd::{Decoder, Error, Limits};

const LIMITS: Limits = Limits {
    max_output: 1 << 26,
    max_alloc: 1 << 28,
    max_threads: 1,
};

static NO: AtomicBool = AtomicBool::new(false);

/// A skippable frame of 3 bytes.
const SKIP: [u8; 11] = [0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3];
/// A frame with a content size of 0: one empty raw block.
const EMPTY: [u8; 9] = [0x28, 0xB5, 0x2F, 0xFD, 0x20, 0, 1, 0, 0];

thread_local! {
    /// One reused decoder per thread count, so the helpers of the parallel one are reused too.
    static ONE: RefCell<Decoder> = RefCell::new(Decoder::new(&LIMITS).unwrap());
    static FOUR: RefCell<Decoder> = RefCell::new(Decoder::new(&LIMITS).unwrap());
}

/// The sum of the content sizes of the frames in `src`, from the frame and block headers (as the
/// parallel path scans them), where every frame has a content size and nothing is cut short.
fn fcs_sum(mut src: &[u8]) -> Option<usize> {
    let mut total = 0usize;
    while !src.is_empty() {
        let word = |i: usize| {
            src.get(i..i + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let n = if word(0)? & 0xFFFF_FFF0 == 0x184D_2A50 {
            8 + usize::try_from(word(4)?).ok()?
        } else {
            let fcs = zstd::content_size(src, &LIMITS).ok()??;
            total = total.checked_add(usize::try_from(fcs).ok()?)?;
            let d = usize::from(src[4]);
            // Magic, descriptor, window, dictionary id, content size.
            let mut at = 5
                + usize::from(d & 0x20 == 0)
                + [0, 1, 2, 4][d & 3]
                + [usize::from(d & 0x20 != 0), 2, 4, 8][d >> 6];
            loop {
                let b = src.get(at..at + 3)?;
                let bh = u32::from_le_bytes([b[0], b[1], b[2], 0]);
                at += 3 + if (bh >> 1) & 3 == 1 {
                    1
                } else {
                    (bh >> 3) as usize
                };
                if bh & 1 != 0 {
                    break;
                }
            }
            at + 4 * usize::from(d & 4 != 0)
        };
        src = src.get(n..)?;
    }
    Some(total)
}

/// `decode_to_slice` into a buffer of `room` bytes (its output on success: on an error the buffer
/// is undefined), then `decode_all_into` into a `Vec` with that capacity (its contents either way).
fn run(
    d: &RefCell<Decoder>,
    src: &[u8],
    room: usize,
    max_threads: usize,
) -> (Result<Vec<u8>, Error>, Result<(), Error>, Vec<u8>) {
    let limits = Limits {
        max_threads,
        ..LIMITS
    };
    let mut d = d.borrow_mut();
    let mut buf = vec![0xAA; room];
    let slice = d
        .decode_to_slice(&mut buf, src, &limits, &NO)
        .map(|n| buf[..n].to_vec());
    let mut v = Vec::with_capacity(room);
    let all = d.decode_all_into(&mut v, src, &limits, &NO);
    (slice, all, v)
}

/// The result at `max_threads = 4` is the one at 1.
fn check(src: &[u8], room: usize) {
    let one = ONE.with(|d| run(d, src, room, 1));
    let four = FOUR.with(|d| run(d, src, room, 4));
    assert!(one == four, "max_threads 4 differs from 1");
}

// The frames of one input decoded on several threads give the serial result, output and error:
// the input as it is, and three copies of it with a skippable frame and an empty frame between
// them, as they are, with a byte changed in the last copy, with a byte changed in the first copy
// too (two runs fail: the first one's error is the serial one) and cut in the last copy. The
// buffers have room for three outputs of the input (from the content sizes in its headers where
// it does not decode, up to 1 MiB), so the copies take the parallel path when they have content
// sizes and 256 KiB of output.
fuzz_target!(|src: &[u8]| {
    let room = match zstd::decode_all(src, &LIMITS, &NO) {
        Ok(v) => 3 * v.len() + 64,
        Err(_) => fcs_sum(src).map_or(1 << 16, |t| (3 * t.min(1 << 20) + 64).max(1 << 16)),
    }
    .min(LIMITS.max_output);
    check(src, room);
    if src.is_empty() || src.len() > 1 << 22 {
        return;
    }
    let cat = [src, &SKIP[..], src, &EMPTY[..], src].concat();
    check(&cat, room);
    let h = src
        .iter()
        .fold(0usize, |a, &b| a.wrapping_mul(31).wrapping_add(b.into()));
    let at = cat.len() - src.len() + h % src.len();
    let mut flip = cat.clone();
    flip[at] ^= 1 << (h % 8);
    check(&flip, room);
    flip[(h / 8 + 1) % src.len()] ^= 1 << (h / 64 % 8);
    check(&flip, room);
    check(&cat[..at], room);
});
