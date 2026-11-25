#![no_main]
use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use zstd::czi::{BGR48, Decoder, GRAY16, ZSTD0, ZSTD1};
use zstd::{Error, Limits};

const LIMITS: Limits = Limits {
    max_output: 1 << 26,
    max_alloc: 1 << 27,
    max_threads: 1,
};

static NO: AtomicBool = AtomicBool::new(false);

thread_local! {
    static SHARED: RefCell<Decoder> = RefCell::new(Decoder::new(&Limits::default()).unwrap());
}

/// libCZI's decoder: decode everything, then pack, then cut or zero-fill to `dst.len()`.
fn ref_decode(dst: &mut [u8], src: &[u8], mode: u32, pixel_type: u32) -> Result<(), Error> {
    let mut src = src;
    let mut hilo = false;
    match mode {
        ZSTD0 => {}
        ZSTD1 => {
            let (n, h) = if src.len() > 1 && src[0] == 1 {
                (1, false)
            } else if src.len() > 3 && src[0] == 3 && src[1] == 1 {
                (3, src[2] & 1 != 0)
            } else {
                return Err(Error::Corrupt("header"));
            };
            if h && ((pixel_type != GRAY16 && pixel_type != BGR48) || !dst.len().is_multiple_of(2))
            {
                return Err(Error::Corrupt("hilo"));
            }
            src = &src[n..];
            hilo = h;
        }
        _ => return Err(Error::Mode),
    }
    let Some(fcs) = zstd::content_size(src, &LIMITS)? else {
        return Err(Error::Corrupt("no size"));
    };
    let mut blob = zstd::decode_all(src, &LIMITS, &NO)?;
    if blob.len() as u64 != fcs {
        return Err(Error::Corrupt("size left"));
    }
    if hilo {
        let half = blob.len() / 2;
        let mut p = vec![0; 2 * half];
        for i in 0..half {
            p[2 * i] = blob[i];
            p[2 * i + 1] = blob[half + i];
        }
        blob = p;
    }
    let k = blob.len().min(dst.len());
    dst[..k].copy_from_slice(&blob[..k]);
    dst[k..].fill(0);
    Ok(())
}

// data = [mode][pixel type][dst length: u16 LE][payload]. The czi decoder gives the bytes of the
// libCZI port (as zstd-go's FuzzDecode checks), or fails where it fails; the interleave it uses
// is the tier this CPU runs. One decoder is reused across inputs (as `FuzzDecode` does). As in
// Go, a reused decoder keeps the buffer of its last successful decode, so it has at least the room
// a new one has: where a new decoder succeeds, the reused one gives the same bytes; where it
// fails, the reused one may succeed (more room) or fail with another corrupt-input error.
fuzz_target!(|data: &[u8]| {
    let Some((&[mode, pt, a, b], src)) = data.split_first_chunk::<4>() else {
        return;
    };
    let n = usize::from(u16::from_le_bytes([a, b]));
    let (mut got, mut want) = (vec![0xAA; n], vec![0xAA; n]);
    let r = SHARED.with(|d| {
        d.borrow_mut()
            .decode(&mut got, src, mode.into(), pt.into(), &LIMITS, &NO)
    });
    let mut fresh = vec![0xAA; n];
    let r2 = Decoder::new(&Limits::default()).unwrap().decode(
        &mut fresh,
        src,
        mode.into(),
        pt.into(),
        &LIMITS,
        &NO,
    );
    if r2.is_ok() {
        assert!(
            r.is_ok(),
            "a new decoder succeeds, the reused one fails: {r:?}"
        );
        assert!(got == fresh, "output depends on the decoder's earlier use");
    }
    let w = ref_decode(&mut want, src, mode.into(), pt.into());
    assert_eq!(
        r2.is_ok(),
        w.is_ok(),
        "{r2:?} against the libCZI port {w:?}"
    );
    if r2.is_ok() {
        assert!(
            fresh == want,
            "output differs from the libCZI port ({n} bytes)"
        );
    }
});
