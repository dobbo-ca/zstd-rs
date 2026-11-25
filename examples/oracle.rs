//! The Rust side of the parity check: `oracle <inputs> <outputs>` decodes every input like
//! `tests/parity/go/oracle` and writes one file per input, `<name>.res`: a record per operation, each a
//! header line `<op> <kind> <len>\n` and then `<len>` bytes. The kind is `out` (the bytes), or,
//! for a failed operation, `err` (the error class, no newline) and `partial` (the bytes before
//! the error). `ORACLE_MAX_INPUT` (bytes) skips larger inputs. The class table is in
//! `docs/parity.md`.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::{env, fs, io};
use zstd::{Error, Limits, czi};

/// What the Go oracle sets with `SetMemoryLimit`, unless `ORACLE_MEM_LIMIT` (bytes) says
/// otherwise, as it does there. Go has no output limit.
fn mem_limit() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| match env::var("ORACLE_MEM_LIMIT") {
        Ok(v) => v
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .unwrap_or_else(|| panic!("bad ORACLE_MEM_LIMIT {v:?}")),
        Err(_) => 256 << 20,
    })
}

/// `ORACLE_MAX_INPUT` (bytes): inputs over this size are skipped, as in the Go oracle.
fn max_input() -> Option<u64> {
    env::var("ORACLE_MAX_INPUT").ok().map(|v| {
        v.parse()
            .ok()
            .filter(|&n| n > 0)
            .unwrap_or_else(|| panic!("bad ORACLE_MAX_INPUT {v:?}"))
    })
}

/// The largest czi destination an input may ask for.
const MAX_DST: usize = 4 << 20;

fn class(e: &Error) -> String {
    match e {
        Error::Corrupt(_) => "corrupt".into(),
        Error::Checksum => "checksum".into(),
        Error::Dictionary => "dictionary".into(),
        Error::AllocLimit => "memory-limit".into(),
        Error::WindowTooLarge => "window-too-large".into(),
        Error::Mode => "mode".into(),
        Error::Cancelled => "cancelled".into(),
        e => format!("other:{e}"),
    }
}

/// The bytes of an operation and its error class: both can be there, as Go returns both.
type Emit<'a> = &'a mut dyn FnMut(&str, Vec<u8>, Option<String>);

/// Four threads whatever the machine has, so the inputs of several frames (`mf-*`) whose output
/// fits the buffer decode in parallel, and must give Go's serial result.
fn limits() -> Limits {
    Limits {
        max_output: usize::MAX,
        max_alloc: mem_limit(),
        max_threads: 4,
    }
}

/// Every operation on one frames input; see `run_frames` in `tests/parity/go/oracle/main.go`.
fn run_frames(emit: Emit, data: &[u8]) {
    let (limits, no, yes) = (limits(), AtomicBool::new(false), AtomicBool::new(true));
    let mut d = zstd::Decoder::new(&Limits::default()).unwrap();
    let mut b = Vec::new();
    let r = d.decode_all_into(&mut b, data, &limits, &no);
    // A dst with spare capacity: growth starts from its capacity, as Go's from cap(dst).
    let cap = if r.is_ok() { b.len() / 3 } else { 64 << 10 };
    let room = b.len() + (128 << 10);
    emit("dec", b, r.err().map(|e| class(&e)));
    let mut b = Vec::with_capacity(cap);
    let r = d.decode_all_into(&mut b, data, &limits, &no);
    emit("cap", b.clone(), r.err().map(|e| class(&e)));
    // The Vec again, as it was left: after a failure it has the capacity of Go's returned slice.
    b.clear();
    let r = d.decode_all_into(&mut b, data, &limits, &no);
    emit("again", b, r.err().map(|e| class(&e)));
    let mut b = Vec::with_capacity(room);
    let r = d.decode_all_into(&mut b, data, &limits, &no);
    let room_err = r.err().map(|e| class(&e));
    emit("room", b.clone(), room_err.clone());
    // `decode_to_slice` into a buffer of the same size, which never grows. It allocates nothing
    // and has no `max_alloc`, so where the output would not fit (`room` failed, or the content
    // size passes the buffer: a lying one, which `room` meets only later), `OutputLimit` stands
    // for the `room` result, which is what Go writes as `slice`. Any other result is compared,
    // and so is an `OutputLimit` where the output fits.
    let mut buf = vec![0; room];
    let too_big = matches!(zstd::content_size(data, &limits), Ok(Some(n)) if n > room as u64);
    match d.decode_to_slice(&mut buf, data, &limits, &no) {
        Ok(n) => emit("slice", buf[..n].to_vec(), None),
        Err(Error::OutputLimit) if room_err.is_some() || too_big => emit("slice", b, room_err),
        Err(e) => emit("slice", Vec::new(), Some(class(&e))),
    }
    let mut b = b"pre".to_vec();
    let r = d.decode_all_into(&mut b, data, &limits, &no);
    emit("pre", b, r.err().map(|e| class(&e)));
    match zstd::content_size(data, &limits) {
        Ok(None) => emit("size", b"none".to_vec(), None),
        Ok(Some(n)) => emit("size", n.to_string().into_bytes(), None),
        Err(e) => emit("size", Vec::new(), Some(class(&e))),
    }
    let mut b = Vec::new();
    let r = zstd::Decoder::new(&limits)
        .unwrap()
        .decode_all_into(&mut b, data, &limits, &yes);
    emit("cancel", b, r.err().map(|e| class(&e)));
}

/// One czi input: `[mode][pixel type][dst length u32 LE][payload]`. The output is the dst (filled
/// with 0xAA first), or nothing on an error. `None` is a short input or a destination over
/// `MAX_DST`.
fn czi_once(
    d: &mut czi::Decoder,
    data: &[u8],
    cancel: &AtomicBool,
) -> Option<Result<Vec<u8>, Error>> {
    let (&[mode, pt, a, b, c, e], src) = data.split_first_chunk::<6>()?;
    let n = u32::from_le_bytes([a, b, c, e]) as usize;
    if n > MAX_DST {
        return None;
    }
    let mut dst = vec![0xAAu8; n];
    let r = d.decode(
        &mut dst,
        src,
        u32::from(mode),
        u32::from(pt),
        &limits(),
        cancel,
    );
    Some(r.map(|()| dst))
}

fn emit_result(emit: Emit, op: &str, r: Result<Vec<u8>, Error>) {
    match r {
        Ok(dst) => emit(op, dst, None),
        Err(e) => emit(op, Vec::new(), Some(class(&e))),
    }
}

fn run_czi(emit: Emit, data: &[u8]) {
    for (op, cancelled) in [("run", false), ("cancel", true)] {
        let cancel = AtomicBool::new(cancelled);
        match czi_once(
            &mut czi::Decoder::new(&Limits::default()).unwrap(),
            data,
            &cancel,
        ) {
            Some(r) => emit_result(emit, op, r),
            None => return emit("run", Vec::new(), Some("bad-input".into())),
        }
    }
}

/// How many earlier inputs of the same kind one reused decoder decodes before an input in the
/// `reuse` operation (`runReuse` in `tests/parity/go/oracle/main.go`).
const REUSE_DEPTH: usize = 2;

/// Decodes the earlier inputs and then `data` with one decoder: the state a decode leaves behind
/// must not show.
fn run_reuse(emit: Emit, prev: &[Vec<u8>], data: &[u8], czi_input: bool) {
    let (limits, no) = (limits(), AtomicBool::new(false));
    if czi_input {
        let mut d = czi::Decoder::new(&Limits::default()).unwrap();
        for p in prev {
            let _ = czi_once(&mut d, p, &no);
        }
        match czi_once(&mut d, data, &no) {
            Some(r) => emit_result(emit, "reuse", r),
            None => emit("reuse", Vec::new(), Some("bad-input".into())),
        }
        return;
    }
    let mut d = zstd::Decoder::new(&Limits::default()).unwrap();
    for p in prev {
        let _ = d.decode_all_into(&mut Vec::new(), p, &limits, &no);
    }
    let mut b = Vec::new();
    let r = d.decode_all_into(&mut b, data, &limits, &no);
    emit("reuse", b, r.err().map(|e| class(&e)));
}

fn main() -> io::Result<()> {
    // The tier the run is meant to cover must be there: a node without AVX2 would pass on scalar.
    let scalar = env::var_os("ZSTD_SIMD").is_some_and(|v| v == "scalar");
    // On x86_64 the sequence loop has a BMI2 build, which must be there too.
    #[cfg(target_arch = "x86_64")]
    let (tier, seqloop, have) = (
        "avx2",
        "bmi2",
        std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("bmi1")
            && std::arch::is_x86_feature_detected!("bmi2"),
    );
    #[cfg(target_arch = "aarch64")]
    let (tier, seqloop, have) = (
        "neon",
        "plain",
        std::arch::is_aarch64_feature_detected!("neon"),
    );
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let (tier, seqloop, have) = ("none", "plain", true);
    if scalar {
        eprintln!("tier: scalar, seqloop: plain");
    } else {
        eprintln!("tier: {tier}, seqloop: {seqloop}");
    }
    if !scalar && !have {
        eprintln!(
            "{tier} (or the {seqloop} sequence loop) not detected and ZSTD_SIMD is not scalar"
        );
        std::process::exit(1);
    }
    let args: Vec<String> = env::args().collect();
    let [_, input, output] = args.as_slice() else {
        eprintln!("usage: oracle <inputs> <outputs>");
        std::process::exit(2);
    };
    let (input, output) = (Path::new(input), Path::new(output));
    let max_input = max_input();
    fs::create_dir_all(output)?;
    // In name order, as Go's `os.ReadDir` gives it: the `reuse` operation depends on it.
    let mut entries = fs::read_dir(input)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    let (mut prev_zst, mut prev_czi): (Vec<Vec<u8>>, Vec<Vec<u8>>) = (Vec::new(), Vec::new());
    for entry in entries {
        if entry.file_type()?.is_dir() {
            continue;
        }
        if max_input.is_some_and(|m| entry.metadata().is_ok_and(|md| md.len() > m)) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let data = fs::read(&path)?;
        let mut res: Vec<u8> = Vec::new();
        let mut emit = |op: &str, bytes: Vec<u8>, err: Option<String>| {
            let mut record = |kind: &str, b: &[u8]| {
                res.extend_from_slice(format!("{op} {kind} {}\n", b.len()).as_bytes());
                res.extend_from_slice(b);
            };
            match err {
                None => record("out", &bytes),
                Some(c) => {
                    record("err", c.as_bytes());
                    record("partial", &bytes); // what came before the error
                }
            }
        };
        let (is_czi, prev) = match path.extension().and_then(|e| e.to_str()) {
            Some("zst") => {
                run_frames(&mut emit, &data);
                (false, &mut prev_zst)
            }
            Some("czi") => {
                run_czi(&mut emit, &data);
                (true, &mut prev_czi)
            }
            _ => continue,
        };
        run_reuse(&mut emit, prev, &data, is_czi);
        fs::write(output.join(format!("{name}.res")), res)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        prev.push(data);
        if prev.len() > REUSE_DEPTH {
            prev.remove(0);
        }
    }
    Ok(())
}
