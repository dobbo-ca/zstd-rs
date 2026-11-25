#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::AtomicBool;
use zstd::Limits;

// content_size does not panic, and an input it rejects (from 5 bytes, where the decoder looks at
// it too) does not decode.
fuzz_target!(|data: &[u8]| {
    let cs = zstd::content_size(data, &Limits::default());
    if cs.is_err() && data.len() >= 5 {
        let r = zstd::decode_all(data, &Limits::default(), &AtomicBool::new(false));
        assert!(r.is_err(), "content_size failed but the input decodes");
    }
});
