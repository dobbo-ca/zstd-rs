#![allow(dead_code)]

use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use zstd::{Error, Limits};

#[path = "../../src/xxhash.rs"]
mod xxhash;

pub static NO_CANCEL: AtomicBool = AtomicBool::new(false);

pub fn path(rel: &str) -> String {
    format!("{}/tests/data/{rel}", env!("CARGO_MANIFEST_DIR"))
}

pub fn read(rel: &str) -> Vec<u8> {
    std::fs::read(path(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The file names in a tests/data directory, sorted.
pub fn list(rel: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(path(rel))
        .unwrap_or_else(|e| panic!("read_dir {rel}: {e}"))
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

pub fn decode(src: &[u8]) -> Result<Vec<u8>, Error> {
    zstd::decode_all(src, &Limits::default(), &NO_CANCEL)
}

pub fn hex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(s.len().is_multiple_of(2), "odd hex length");
    s.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

pub fn is_corrupt(e: &Error) -> bool {
    matches!(e, Error::Corrupt(_))
}

/// A frame of raw blocks with a 128 KiB window, as zstd-go's `rawFrame`.
pub fn raw_frame(data: &[u8], fcs: bool, checksum: bool, dict_id: u8) -> Vec<u8> {
    let orig = data;
    let mut data = data;
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD];
    let mut fhd = 0u8;
    if fcs {
        fhd |= 3 << 6; // 8-byte content size
    }
    if checksum {
        fhd |= 4;
    }
    if dict_id != 0 {
        fhd |= 1;
    }
    f.push(fhd);
    f.push((17 << 3) - (10 << 3)); // window 128 KiB
    if dict_id != 0 {
        f.push(dict_id);
    }
    if fcs {
        f.extend_from_slice(&(data.len() as u64).to_le_bytes());
    }
    let mut first = true;
    while first || !data.is_empty() {
        first = false;
        let n = data.len().min(128 << 10);
        let mut bh = (n as u32) << 3;
        if n == data.len() {
            bh |= 1;
        }
        f.extend_from_slice(&bh.to_le_bytes()[..3]);
        f.extend_from_slice(&data[..n]);
        data = &data[n..];
    }
    if checksum {
        f.extend_from_slice(&(xxhash::xxh64(orig) as u32).to_le_bytes());
    }
    f
}

/// xorshift64*, the tests' only source of pseudo-random data.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() >> 11) as usize % n
    }
}

static TMP: AtomicUsize = AtomicUsize::new(0);

/// Compresses `data` with the zstd CLI. `None` if the CLI is missing, unless `ZSTD_REQUIRE_CLI`
/// is set (CI), which makes that a failure. `ZSTD_SKIP_CLI` skips it (for a machine
/// where the CLI cannot run).
pub fn zstd_cli(data: &[u8], args: &[&str]) -> Option<Vec<u8>> {
    if std::env::var_os("ZSTD_SKIP_CLI").is_some() {
        return None;
    }
    let f = std::env::temp_dir().join(format!(
        "zstd-rs-{}-{}",
        std::process::id(),
        TMP.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&f, data).unwrap();
    let out = Command::new("zstd")
        .args(["-q", "-c", "-f"])
        .args(args)
        .arg(&f)
        .output();
    let _ = std::fs::remove_file(&f);
    match out {
        Ok(o) if o.status.success() => Some(o.stdout),
        Ok(o) => panic!("zstd {args:?}: {}", String::from_utf8_lossy(&o.stderr)),
        Err(e) => {
            assert!(
                std::env::var_os("ZSTD_REQUIRE_CLI").is_none(),
                "zstd CLI missing: {e}"
            );
            None
        }
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_known() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
