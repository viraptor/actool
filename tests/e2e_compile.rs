//! End-to-end integration tests for `actool compile`.
//!
//! Compiles the reference catalog (tests/ref_samples/Catalog.xcassets) and
//! checks the output structure. Pixel-byte equivalence with the Python
//! implementation is confirmed in the commit history (sha256 287ef254…).

use actool::{bom::BomWriter, car, catalog::AssetCatalog, compiler};
use std::path::PathBuf;

fn ref_catalog() -> PathBuf {
    PathBuf::from("tests/ref_samples/Catalog.xcassets")
}

fn workspace_tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from("tmp").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn compile_reference_catalog_produces_outputs() {
    if !ref_catalog().exists() {
        eprintln!("Skipping: reference catalog missing");
        return;
    }
    let out = workspace_tmp("rust_e2e_full");
    let plist = out.join("AppIcon.Info.plist");
    let files = compiler::compile_catalog(
        &[ref_catalog()],
        &out,
        "macosx",
        "11.0",
        Some("AppIcon"),
        Some(&plist),
        None,
        None,
        "default",
        None,
        None,
        true,
        None,
    )
    .expect("compile");
    assert!(files.iter().any(|p| p.ends_with("Assets.car")));
    assert!(files.iter().any(|p| p.ends_with("AppIcon.icns")));
    assert!(files.iter().any(|p| p.ends_with("AppIcon.Info.plist")));
    // CAR must start with BOMStore
    let car_bytes = std::fs::read(out.join("Assets.car")).expect("read car");
    assert_eq!(&car_bytes[..8], b"BOMStore");
    // Plist contains the icon name
    let plist_txt = std::fs::read_to_string(&plist).expect("read plist");
    assert!(plist_txt.contains("<string>AppIcon</string>"));
}

#[test]
fn compile_reference_catalog_without_icon() {
    if !ref_catalog().exists() {
        return;
    }
    let out = workspace_tmp("rust_e2e_no_icon");
    let files = compiler::compile_catalog(
        &[ref_catalog()],
        &out,
        "macosx",
        "11.0",
        None,
        None,
        None,
        None,
        "default",
        None,
        None,
        true,
        None,
    )
    .expect("compile");
    assert!(files.iter().any(|p| p.ends_with("Assets.car")));
    assert!(!files.iter().any(|p| p.ends_with(".icns")));
    assert!(!files.iter().any(|p| p.ends_with(".plist")));
}

#[test]
fn version_matches_expected_fields() {
    // Placeholder — the CLI hard-codes these constants. Keeping this as a
    // compile-time sanity check that the library still exposes the pieces
    // the CLI needs.
    let hdr = car::make_carheader(0);
    assert_eq!(&hdr[..4], b"RATC");
    let _ = BomWriter::new();
}

#[test]
fn catalog_parse_does_not_crash() {
    if !ref_catalog().exists() {
        return;
    }
    let mut catalog = AssetCatalog::new(
        ref_catalog(),
        "macosx".to_string(),
        "11.0".to_string(),
        Some("AppIcon".to_string()),
        None,
        None,
    );
    let (renditions, facets) = catalog.parse().expect("parse");
    assert!(!renditions.is_empty(), "expected renditions");
    assert!(!facets.is_empty(), "expected facets");
    // AppIcon facet should be present
    assert!(facets.contains_key("AppIcon"));
}
