//! The decodecorpus and klauspost fixtures, and the synthetic tiles (ports TestDecodecorpus and
//! TestKlauspostFixtures; the tiles come from Go's benchmark, which derives their size from the
//! file name).

mod common;
use common::*;

/// The `<name>.zst` / `<name>.orig` pairs of a directory.
fn pairs(dir: &str) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let names = list(dir);
    let zsts: Vec<&str> = names
        .iter()
        .filter_map(|n| n.strip_suffix(".zst"))
        .collect();
    let origs = names.iter().filter(|n| n.ends_with(".orig")).count();
    assert_eq!(
        zsts.len(),
        origs,
        "{dir}: .zst and .orig files differ in number"
    );
    zsts.iter()
        .map(|n| {
            (
                n.to_string(),
                read(&format!("{dir}/{n}.zst")),
                read(&format!("{dir}/{n}.orig")),
            )
        })
        .collect()
}

#[test]
fn decodecorpus() {
    let pairs = pairs("decodecorpus");
    assert_eq!(pairs.len(), 304);
    let failed: Vec<&str> = pairs
        .iter()
        .filter(|(_, zst, orig)| decode(zst).as_ref() != Ok(orig))
        .map(|(n, ..)| n.as_str())
        .collect();
    assert!(
        failed.is_empty(),
        "{}/{} decodecorpus frames failed: {failed:?}",
        failed.len(),
        pairs.len()
    );
}

/// Labeled by what the zstd 1.5.7 CLI does with each file: ours must agree, with the same bytes or
/// both rejecting.
#[test]
fn klauspost_good() {
    let pairs = pairs("klauspost/good");
    assert!(!pairs.is_empty());
    for (name, zst, orig) in pairs {
        let got = decode(&zst)
            .unwrap_or_else(|e| panic!("{name}: libzstd decodes this, we rejected it: {e}"));
        assert!(got == orig, "{name}: decoded bytes differ from libzstd's");
    }
}

#[test]
fn klauspost_bad() {
    let names: Vec<String> = list("klauspost/bad")
        .into_iter()
        .filter(|n| n.ends_with(".zst"))
        .collect();
    assert_eq!(names.len(), 33);
    for name in names {
        assert!(
            decode(&read(&format!("klauspost/bad/{name}"))).is_err(),
            "{name}: libzstd rejects this, we accepted it"
        );
    }
}

/// `<format>_<size>_<content>_z<level>[_hilo].zst`: gray16 is 2 bytes per pixel, bgr24 3.
#[test]
fn tiles() {
    let names = list("tiles");
    assert_eq!(names.len(), 24);
    for name in names {
        let parts: Vec<&str> = name.trim_end_matches(".zst").split('_').collect();
        let size: usize = parts[1].parse().unwrap();
        let bpp = if parts[0] == "gray16" { 2 } else { 3 };
        let got = decode(&read(&format!("tiles/{name}"))).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(got.len(), size * size * bpp, "{name}");
    }
}
