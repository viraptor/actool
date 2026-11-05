//! Parity regressions for `.icon` bundles against Apple's actool (Xcode 26+):
//! the iOS output set, CoreUI-1000+ palette/facet naming, and the macOS
//! `.icns` composition. Bundles are synthetic; assertions are on outputs that
//! Apple's own runs showed to be deterministic.

use actool::icon_bundle::{
    compile_icon_bundle, compile_icon_bundle_with_options, IconCompileOptions, NoMatchingAppIcon,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

const MODERN_COREUI: u32 = 1011;

fn tmpdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "actool_platform_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A bundle whose only layer is fully transparent, so anything visible in the
/// output comes from the fill and the compositor.
fn make_bundle(stem: &str, icon_json: &str) -> (PathBuf, PathBuf) {
    let parent = tmpdir();
    let bundle = parent.join(format!("{stem}.icon"));
    std::fs::create_dir_all(bundle.join("Assets")).unwrap();
    image::RgbaImage::from_pixel(256, 256, image::Rgba([0, 0, 0, 0]))
        .save(bundle.join("Assets/main.png"))
        .unwrap();
    std::fs::write(bundle.join("icon.json"), icon_json).unwrap();
    let out = parent.join("out");
    std::fs::create_dir_all(&out).unwrap();
    (bundle, out)
}

const LINEAR_WHITE_TO_BLUE: &str = r#"{
  "fill": {"linear-gradient": ["extended-gray:1.0,1.0", "display-p3:0.0,0.0,1.0,1.0"]},
  "groups": [{"layers": [{"image-name": "main.png", "name": "main"}],
              "shadow": {"kind": "neutral", "opacity": 0.5},
              "translucency": {"enabled": true, "value": 0.3}}]
}"#;

fn compile(
    stem: &str,
    json: &str,
    platform: &str,
    app_icon: &str,
    options: &IconCompileOptions,
) -> (anyhow::Result<Vec<PathBuf>>, PathBuf) {
    let (bundle, out) = make_bundle(stem, json);
    let plist = out.join("p.plist");
    let res = compile_icon_bundle_with_options(
        &bundle,
        &out,
        platform,
        "17.0",
        Some(app_icon),
        Some(&plist),
        None,
        "default",
        options,
    );
    (res, out)
}

fn modern() -> IconCompileOptions {
    IconCompileOptions { coreui_version: Some(MODERN_COREUI), target_devices: Vec::new() }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn car_bytes(out: &Path) -> Vec<u8> {
    std::fs::read(out.join("Assets.car")).unwrap()
}

#[test]
fn linear_gradient_with_white_first_stop_reuses_the_anchor_color() {
    let (res, out) = compile("Dedup", LINEAR_WHITE_TO_BLUE, "macosx", "Dedup", &modern());
    res.unwrap();
    let car = car_bytes(&out);
    // Color-1 is the white anchor, the blue stop, and the two dark-background
    // grays: four colors, and the white stop adds none.
    assert!(contains(&car, "Dedup_Assets/Color-4"));
    assert!(!contains(&car, "Dedup_Assets/Color-5"));
}

#[test]
fn modern_coreui_names_the_dark_background_gradient() {
    let (res, out) = compile("Naming", LINEAR_WHITE_TO_BLUE, "macosx", "Naming", &modern());
    res.unwrap();
    let car = car_bytes(&out);
    assert!(contains(&car, "Naming_Assets/Gradient-1"));
    assert!(contains(&car, "Naming_Assets/system-dark"));
    assert!(!contains(&car, "Naming_Assets/Gradient-2"));
}

#[test]
fn baseline_coreui_keeps_numbered_gradients() {
    let (bundle, out) = make_bundle("Legacy", LINEAR_WHITE_TO_BLUE);
    compile_icon_bundle(&bundle, &out, "macosx", "11.0", Some("Legacy"), None, None, "default")
        .unwrap();
    let car = car_bytes(&out);
    assert!(contains(&car, "Legacy_Assets/Gradient-2"));
    assert!(!contains(&car, "Legacy_Assets/system-dark"));
}

#[cfg(target_os = "macos")]
#[test]
fn icns_composites_the_fill_behind_the_layers() {
    let (res, out) = compile("Bg", LINEAR_WHITE_TO_BLUE, "macosx", "Bg", &modern());
    res.unwrap();
    let icns = std::fs::read(out.join("Bg.icns")).unwrap();
    // ic07: 128px PNG entry.
    let pos = icns.windows(4).position(|w| w == b"ic07").unwrap();
    let len = u32::from_be_bytes(icns[pos + 4..pos + 8].try_into().unwrap()) as usize;
    let img = image::load_from_memory(&icns[pos + 8..pos + len]).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (128, 128));
    // The layer is transparent, so an opaque centre can only be the gradient;
    // the squircle mask leaves the corner clear.
    assert_eq!(img.get_pixel(64, 64)[3], 255);
    assert_eq!(img.get_pixel(0, 0)[3], 0);
}

#[test]
fn ios_emits_home_screen_pngs_and_no_icns() {
    let (res, out) = compile("Phone", LINEAR_WHITE_TO_BLUE, "iphoneos", "Phone", &modern());
    res.unwrap();
    assert!(!out.join("Phone.icns").exists());
    let phone = image::open(out.join("Phone60x60@2x.png")).unwrap().to_rgba8();
    let pad = image::open(out.join("Phone76x76@2x~ipad.png")).unwrap().to_rgba8();
    assert_eq!(phone.dimensions(), (120, 120));
    assert_eq!(pad.dimensions(), (152, 152));
    // Full-bleed: iOS applies the mask itself, so the corner is opaque.
    assert_eq!(phone.get_pixel(0, 0)[3], 255);
}

#[test]
fn ios_car_carries_idiom_appearances_and_no_atlas() {
    let (res, out) = compile("Phone", LINEAR_WHITE_TO_BLUE, "iphoneos", "Phone", &modern());
    res.unwrap();
    let car = car_bytes(&out);
    for name in ["UIAppearanceAny", "UIAppearanceDark", "UIAppearanceLight", "ISAppearanceTintable"] {
        assert!(contains(&car, name), "missing appearance {name}");
    }
    assert!(!contains(&car, "ZZZZPackedAsset"));
}

fn plist_for(devices: &[&str]) -> String {
    let options = IconCompileOptions {
        coreui_version: Some(MODERN_COREUI),
        target_devices: devices.iter().map(|d| d.to_string()).collect(),
    };
    let (res, out) = compile("Phone", LINEAR_WHITE_TO_BLUE, "iphoneos", "Phone", &options);
    res.unwrap();
    std::fs::read_to_string(out.join("p.plist")).unwrap()
}

#[test]
fn ios_plist_names_only_the_targeted_devices() {
    let names = |p: &str| p.matches("<key>CFBundleIconName</key>").count();
    assert_eq!(names(&plist_for(&[])), 2);
    assert_eq!(names(&plist_for(&["iphone", "ipad"])), 2);
    let phone_only = plist_for(&["iphone"]);
    assert_eq!(names(&phone_only), 1);
    assert!(phone_only.find("CFBundleIconName").unwrap() < phone_only.find("~ipad").unwrap());
    let pad_only = plist_for(&["ipad"]);
    assert_eq!(names(&pad_only), 1);
    assert!(pad_only.find("CFBundleIconName").unwrap() > pad_only.find("~ipad").unwrap());
}

#[test]
fn ios_rejects_an_app_icon_that_does_not_name_the_bundle() {
    let (res, out) = compile("Phone", LINEAR_WHITE_TO_BLUE, "iphoneos", "Other", &modern());
    let err = res.unwrap_err();
    let missing = err.downcast_ref::<NoMatchingAppIcon>().expect("typed error");
    assert_eq!(
        missing.to_string(),
        "None of the input catalogs contained a matching stickers icon set, app icon set, or icon stack named  \"Other\"."
    );
    assert!(!out.join("Assets.car").exists());
    let plist = std::fs::read_to_string(out.join("p.plist")).unwrap();
    assert!(plist.contains("<dict/>"));
}

#[test]
fn macos_still_accepts_a_mismatched_app_icon() {
    let (res, out) = compile("Phone", LINEAR_WHITE_TO_BLUE, "macosx", "Other", &modern());
    res.unwrap();
    assert!(out.join("Assets.car").exists());
    assert!(!out.join("Other.icns").exists());
}
