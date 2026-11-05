//! iOS platform regression tests.
//!
//! Verifies the idiom-carrying catalog layout that `/usr/bin/actool
//! --platform iphoneos` emits: the iOS key format (with Idiom + Subtype
//! columns), CoreUI 975 / key-semantics 2 header, the `ios` deployment
//! platform string, per-idiom rendition keys, and the idiom filtering that
//! drops `mac`/`ios-marketing` from regular imagesets.

use actool::compiler;
use std::path::{Path, PathBuf};

fn workspace_tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from("tmp").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(path: &Path, w: u32, h: u32, rgba: [u8; 4]) {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
    img.save(path).unwrap();
}

/// Build an imageset carrying every idiom we care about, plus the two that
/// iOS imagesets must drop (`mac`, `ios-marketing`).
fn build_mixed_catalog(root: &Path) {
    let xc = root.join("Images.xcassets");
    let imageset = xc.join("Glyph.imageset");
    std::fs::create_dir_all(&imageset).unwrap();
    std::fs::write(
        xc.join("Contents.json"),
        r#"{"info":{"author":"xcode","version":1}}"#,
    )
    .unwrap();
    write_png(&imageset.join("p1.png"), 10, 10, [255, 0, 0, 255]);
    write_png(&imageset.join("p2.png"), 20, 20, [0, 255, 0, 255]);
    write_png(&imageset.join("p3.png"), 30, 30, [0, 0, 255, 255]);
    std::fs::write(
        imageset.join("Contents.json"),
        r#"{
          "images":[
            {"idiom":"iphone","scale":"1x","filename":"p1.png"},
            {"idiom":"iphone","scale":"2x","filename":"p2.png"},
            {"idiom":"iphone","scale":"3x","filename":"p3.png"},
            {"idiom":"ipad","scale":"1x","filename":"p1.png"},
            {"idiom":"ipad","scale":"2x","filename":"p2.png"},
            {"idiom":"mac","scale":"1x","filename":"p1.png"},
            {"idiom":"ios-marketing","scale":"1x","filename":"p1.png"}
          ],
          "info":{"author":"xcode","version":1}
        }"#,
    )
    .unwrap();
}

fn compile_ios(xcassets: &Path, out: &Path) {
    let plist = out.join("partial.plist");
    compiler::compile_catalog(
        &[xcassets.to_path_buf()],
        out,
        "iphoneos",
        "14.0",
        None,
        Some(&plist),
        None,
        None,
        "default",
        None,
        None,
        true,
        Some(975),
    )
    .expect("compile");
}

fn build_appicon_catalog(root: &Path, entries: &[(&str, &str, &str)]) {
    let xc = root.join("A.xcassets");
    let set = xc.join("AppIcon.appiconset");
    std::fs::create_dir_all(&set).unwrap();
    std::fs::write(
        xc.join("Contents.json"),
        r#"{"info":{"author":"xcode","version":1}}"#,
    )
    .unwrap();
    let mut images = String::from("{\"images\":[");
    for (i, (size, scale, idiom)) in entries.iter().enumerate() {
        let pt: f64 = size.split('x').next().unwrap().parse().unwrap();
        let px = (pt * scale[..1].parse::<f64>().unwrap()).round() as u32;
        let fname = format!("i{px}.png");
        write_png(&set.join(&fname), px, px, [64, 128, 192, 255]);
        if i > 0 {
            images.push(',');
        }
        images.push_str(&format!(
            r#"{{"size":"{size}","idiom":"{idiom}","filename":"{fname}","scale":"{scale}"}}"#
        ));
    }
    images.push_str("],\"info\":{\"author\":\"xcode\",\"version\":1}}");
    std::fs::write(set.join("Contents.json"), images).unwrap();
}

fn compile_ios_icon(xcassets: &Path, out: &Path) {
    let plist = out.join("partial.plist");
    compiler::compile_catalog(
        &[xcassets.to_path_buf()],
        out,
        "iphoneos",
        "14.0",
        Some("AppIcon"),
        Some(&plist),
        None,
        None,
        "default",
        None,
        None,
        true,
        Some(975),
    )
    .expect("compile");
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

/// Parse the KEYFORMAT (`tmfk`) attribute list from a compiled CAR.
fn keyformat(car: &[u8]) -> Vec<u32> {
    let i = car
        .windows(4)
        .position(|w| w == b"tmfk")
        .expect("tmfk block");
    let n = read_u32_le(car, i + 8) as usize;
    (0..n).map(|k| read_u32_le(car, i + 12 + 4 * k)).collect()
}

#[test]
fn ios_imageset_emits_idiom_keyformat_and_header() {
    let root = workspace_tmp("ios_keyformat");
    build_mixed_catalog(&root);
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios(&root.join("Images.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");

    // Fixed iOS key format: Appearance, Localization, Scale, Idiom, Subtype,
    // Identifier, Element, Part.
    assert_eq!(keyformat(&car), vec![7, 13, 12, 15, 16, 17, 1, 2]);

    // CARHEADER: CoreUI 975, key-semantics 2.
    let h = car
        .windows(4)
        .position(|w| w == b"RATC")
        .expect("RATC block");
    assert_eq!(read_u32_le(&car, h + 4), 975, "coreui version");
    assert_eq!(read_u32_le(&car, h + 432), 2, "key semantics");

    // EXTENDED_METADATA records the device family, not the SDK name.
    let m = car
        .windows(4)
        .position(|w| w == b"META")
        .expect("META block");
    let platform: String = car[m + 516..m + 516 + 8]
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    assert_eq!(platform, "ios");
}

#[test]
fn ios_imageset_filters_mac_and_marketing_idioms() {
    let root = workspace_tmp("ios_filter");
    build_mixed_catalog(&root);
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios(&root.join("Images.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);
    let idiom_col = kf.iter().position(|t| *t == 15).expect("idiom column");

    // Walk RENDITIONS rendition keys (fixed inline key size = kf.len()*2) and
    // collect the idiom values actually present. `mac` (no value here) and
    // `ios-marketing` (=6) must have been dropped; only phone(1)/pad(2) remain.
    let key_size = kf.len() * 2;
    let mut idioms: Vec<u16> = Vec::new();
    // Rendition keys appear as the inline-key region of the RENDITIONS leaf;
    // scan for 16-byte aligned candidates whose scale column is 1..=3 and whose
    // idiom column is a small value, mirroring the probe in tools.
    let scale_col = kf.iter().position(|t| *t == 12).unwrap();
    for i in (0..car.len().saturating_sub(key_size)).step_by(2) {
        let cols: Vec<u16> = (0..kf.len())
            .map(|c| u16::from_le_bytes(car[i + c * 2..i + c * 2 + 2].try_into().unwrap()))
            .collect();
        let scale = cols[scale_col];
        let idiom = cols[idiom_col];
        // Heuristic: a real image rendition key has scale 1..=3, idiom 1..=2,
        // appearance 0 and localization 0.
        if (1..=3).contains(&scale) && (1..=2).contains(&idiom) && cols[0] == 0 && cols[1] == 0 {
            idioms.push(idiom);
        }
    }
    assert!(idioms.contains(&1), "expected an iphone (1) rendition");
    assert!(idioms.contains(&2), "expected an ipad (2) rendition");
    assert!(
        !idioms.contains(&6),
        "ios-marketing (6) must be dropped from imagesets"
    );
}

#[test]
fn ios_appicon_emits_loose_primary_pngs() {
    let root = workspace_tmp("ios_appicon_loose");
    build_appicon_catalog(
        &root,
        &[
            ("60x60", "2x", "iphone"),
            ("60x60", "3x", "iphone"),
            ("76x76", "2x", "ipad"),
            ("1024x1024", "1x", "ios-marketing"),
        ],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    // iPhone primary @2x and iPad primary @2x are emitted loose; the marketing
    // (1024) icon is CAR-only and gets no loose file.
    assert!(out.join("AppIcon60x60@2x.png").exists(), "iphone loose png");
    assert!(
        out.join("AppIcon76x76@2x~ipad.png").exists(),
        "ipad loose png"
    );
    assert!(!out.join("AppIcon.icns").exists(), "no icns on iOS");
}

#[test]
fn ios_appicon_partial_plist_has_cfbundleicons() {
    let root = workspace_tmp("ios_appicon_plist");
    build_appicon_catalog(
        &root,
        &[("60x60", "2x", "iphone"), ("76x76", "2x", "ipad")],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let plist = std::fs::read_to_string(out.join("partial.plist")).unwrap();
    assert!(plist.contains("<key>CFBundleIcons</key>"));
    assert!(plist.contains("<key>CFBundleIcons~ipad</key>"));
    assert!(plist.contains("<key>CFBundlePrimaryIcon</key>"));
    assert!(plist.contains("<string>AppIcon60x60</string>"));
    assert!(plist.contains("<string>AppIcon76x76</string>"));
    assert!(plist.contains("<key>CFBundleIconName</key>"));
    // legacy macOS keys must NOT appear on iOS
    assert!(!plist.contains("<key>CFBundleIconFile</key>"));
}

#[test]
fn ios_appicon_ipad_files_only_with_ipad_icons() {
    // iPhone-only set: CFBundleIcons~ipad carries just the name (no files).
    let root = workspace_tmp("ios_appicon_iphone_only");
    build_appicon_catalog(&root, &[("60x60", "2x", "iphone"), ("60x60", "3x", "iphone")]);
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let plist = std::fs::read_to_string(out.join("partial.plist")).unwrap();
    // The ~ipad dict exists but must not list AppIcon76x76 (no iPad icon).
    assert!(plist.contains("<key>CFBundleIcons~ipad</key>"));
    assert!(!plist.contains("AppIcon76x76"));
    assert!(!out.join("AppIcon76x76@2x~ipad.png").exists());
}

#[test]
fn ios_appicon_renditions_carry_device_idiom() {
    let root = workspace_tmp("ios_appicon_idiom");
    build_appicon_catalog(
        &root,
        &[
            ("60x60", "2x", "iphone"),
            ("60x60", "3x", "iphone"),
            ("1024x1024", "1x", "ios-marketing"),
        ],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);
    let idiom_col = kf.iter().position(|t| *t == 15).expect("idiom column");

    // App-icon image renditions encode their idiom: phone(1) and marketing(6).
    let mut seen = std::collections::HashSet::new();
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let cols: Vec<u16> = (0..kf.len())
            .map(|c| u16::from_le_bytes(car[i + c * 2..i + c * 2 + 2].try_into().unwrap()))
            .collect();
        if cols[0] == 0 && cols[1] == 0 && (cols[idiom_col] == 1 || cols[idiom_col] == 6) {
            seen.insert(cols[idiom_col]);
        }
    }
    assert!(seen.contains(&1), "expected a phone (1) icon rendition");
    assert!(seen.contains(&6), "expected a marketing (6) icon rendition");
}

#[test]
fn ios_appicon_keyformat_adds_dim2_and_renditions_dont_collide() {
    let root = workspace_tmp("ios_appicon_keyformat");
    // Five iPhone @2x sizes that share idiom+scale and previously collided on a
    // single key when Dimension2 was absent.
    build_appicon_catalog(
        &root,
        &[
            ("20x20", "2x", "iphone"),
            ("29x29", "2x", "iphone"),
            ("40x40", "2x", "iphone"),
            ("60x60", "2x", "iphone"),
            ("60x60", "3x", "iphone"),
        ],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);

    // App-icon key format carries Dimension2 (9) after Subtype; an imageset
    // (no icons) does not — see ios_imageset_emits_idiom_keyformat_and_header.
    assert!(kf.contains(&9), "app-icon keyformat must include dim2 (9)");
    let sub = kf.iter().position(|t| *t == 16).unwrap();
    let d2 = kf.iter().position(|t| *t == 9).unwrap();
    assert!(d2 == sub + 1, "dim2 must follow subtype");

    // The four distinct @2x point sizes must produce distinct Dimension2 values
    // (1,2,3,4) in their rendition keys — i.e. no collision.
    let d2_col = d2;
    let scale_col = kf.iter().position(|t| *t == 12).unwrap();
    let idiom_col = kf.iter().position(|t| *t == 15).unwrap();
    let mut d2_at_scale2 = std::collections::HashSet::new();
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let cols: Vec<u16> = (0..kf.len())
            .map(|c| u16::from_le_bytes(car[i + c * 2..i + c * 2 + 2].try_into().unwrap()))
            .collect();
        if cols[0] == 0 && cols[1] == 0 && cols[scale_col] == 2 && cols[idiom_col] == 1 {
            d2_at_scale2.insert(cols[d2_col]);
        }
    }
    for idx in [1u16, 2, 3, 4] {
        assert!(
            d2_at_scale2.contains(&idx),
            "expected distinct phone@2x dim2={idx}"
        );
    }
}

#[test]
fn ios_appicon_packs_into_atlases_with_dim1_and_keeps_idiom() {
    let root = workspace_tmp("ios_appicon_packing");
    build_appicon_catalog(
        &root,
        &[
            ("20x20", "2x", "iphone"),
            ("20x20", "3x", "iphone"),
            ("29x29", "2x", "iphone"),
            ("29x29", "3x", "iphone"),
            ("40x40", "2x", "iphone"),
            ("40x40", "3x", "iphone"),
            ("60x60", "2x", "iphone"),
            ("60x60", "3x", "iphone"),
            ("1024x1024", "1x", "ios-marketing"),
        ],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);

    // Packing multiple atlases per scale introduces Dimension1 (8); together
    // with Dimension2 (9) the app-icon key format matches host actool.
    assert!(kf.contains(&9), "keyformat must carry dim2");
    assert!(kf.contains(&8), "keyformat must carry dim1 once icons pack");

    // A packed atlas rendition (element = PACKED) must exist and carry the
    // phone idiom — packing must not drop the idiom (it keys the atlas).
    let el = kf.iter().position(|t| *t == 1).unwrap();
    let idiom_col = kf.iter().position(|t| *t == 15).unwrap();
    let mut packed_phone = false;
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let cols: Vec<u16> = (0..kf.len())
            .map(|c| u16::from_le_bytes(car[i + c * 2..i + c * 2 + 2].try_into().unwrap()))
            .collect();
        // ELEMENT_PACKED == 9; phone idiom == 1.
        if cols[el] == 9 && cols[idiom_col] == 1 && cols[0] == 0 && cols[1] == 0 {
            packed_phone = true;
        }
    }
    assert!(packed_phone, "expected a phone-idiom packed atlas key");
}

#[test]
fn ios_appicon_multisize_split_per_idiom() {
    let root = workspace_tmp("ios_appicon_msplit");
    build_appicon_catalog(
        &root,
        &[
            ("20x20", "2x", "iphone"),
            ("60x60", "2x", "iphone"),
            ("76x76", "2x", "ipad"),
            ("1024x1024", "1x", "ios-marketing"),
        ],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);
    let part_col = kf.iter().position(|t| *t == 2).unwrap();
    let idiom_col = kf.iter().position(|t| *t == 15).unwrap();

    // One MultiSized icon facet (part 218) per idiom: phone(1), pad(2),
    // marketing(6) — keyed by idiom rather than a single combined facet.
    let mut multisize_idioms = std::collections::HashSet::new();
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let cols: Vec<u16> = (0..kf.len())
            .map(|c| u16::from_le_bytes(car[i + c * 2..i + c * 2 + 2].try_into().unwrap()))
            .collect();
        if cols[part_col] == 218 && cols[0] == 0 && cols[1] == 0 {
            multisize_idioms.insert(cols[idiom_col]);
        }
    }
    for idiom in [1u16, 2, 6] {
        assert!(
            multisize_idioms.contains(&idiom),
            "expected a MultiSized facet for idiom {idiom}"
        );
    }
}

#[test]
fn ios_appicon_synthesizes_plus_phone_subtype_1792() {
    let root = workspace_tmp("ios_appicon_subtype");
    // A 60pt@3x iPhone icon triggers the synthesized 90pt@2x Plus-phone icon.
    build_appicon_catalog(
        &root,
        &[
            ("60x60", "2x", "iphone"),
            ("60x60", "3x", "iphone"),
            ("1024x1024", "1x", "ios-marketing"),
        ],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);
    let sub_col = kf.iter().position(|t| *t == 16).unwrap();
    let part_col = kf.iter().position(|t| *t == 2).unwrap();
    let d2_col = kf.iter().position(|t| *t == 9).unwrap();
    let scale_col = kf.iter().position(|t| *t == 12).unwrap();

    let mut multisize_1792 = false;
    let mut leaf_1792 = false;
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let c: Vec<u16> = (0..kf.len())
            .map(|x| u16::from_le_bytes(car[i + x * 2..i + x * 2 + 2].try_into().unwrap()))
            .collect();
        if c[0] != 0 || c[1] != 0 || c[sub_col] != 1792 {
            continue;
        }
        // Multisize facet (part 218, scale 1) and the 90pt leaf (part 220,
        // scale 2, dim2 index 7).
        if c[part_col] == 218 {
            multisize_1792 = true;
        }
        if c[part_col] == 220 && c[scale_col] == 2 && c[d2_col] == 7 {
            leaf_1792 = true;
        }
    }
    assert!(multisize_1792, "expected a subtype-1792 multisize facet");
    assert!(leaf_1792, "expected a subtype-1792 90pt leaf icon");
}

#[test]
fn ios_appicon_no_subtype_without_60pt_at_3x() {
    let root = workspace_tmp("ios_appicon_no_subtype");
    // No 60pt@3x -> no Plus-phone synthesis.
    build_appicon_catalog(
        &root,
        &[("60x60", "2x", "iphone"), ("1024x1024", "1x", "ios-marketing")],
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&root.join("A.xcassets"), &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);
    let sub_col = kf.iter().position(|t| *t == 16).unwrap();
    let part_col = kf.iter().position(|t| *t == 2).unwrap();
    let mut any_1792 = false;
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let c: Vec<u16> = (0..kf.len())
            .map(|x| u16::from_le_bytes(car[i + x * 2..i + x * 2 + 2].try_into().unwrap()))
            .collect();
        // Constrain to real icon/multisize renditions; the bare value 0x0700
        // (1792) also occurs in unrelated key/metadata bytes.
        if c[0] == 0
            && c[1] == 0
            && c[sub_col] == 1792
            && (c[part_col] == 218 || c[part_col] == 220)
        {
            any_1792 = true;
        }
    }
    assert!(!any_1792, "no subtype-1792 expected without a 60pt@3x icon");
}

#[test]
fn ios_appicon_single_size() {
    let root = workspace_tmp("ios_appicon_single_size");
    let xc = root.join("A.xcassets");
    let set = xc.join("AppIcon.appiconset");
    std::fs::create_dir_all(&set).unwrap();
    std::fs::write(
        xc.join("Contents.json"),
        r#"{"info":{"author":"xcode","version":1}}"#,
    )
    .unwrap();
    write_png(&set.join("Icon.png"), 1024, 1024, [64, 128, 192, 255]);
    std::fs::write(
        set.join("Contents.json"),
        r#"{
          "images":[{"filename":"Icon.png","idiom":"universal","platform":"ios","size":"1024x1024"}],
          "info":{"author":"xcode","version":1}
        }"#,
    )
    .unwrap();

    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&xc, &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);
    assert_eq!(kf, vec![7, 13, 12, 15, 16, 9, 17, 1, 2]);

    // Modern iOS actool uses CoreUI 975 / key-semantics 2 even for single-size
    // app icons.
    let h = car.windows(4).position(|w| w == b"RATC").expect("CARHEADER");
    assert_eq!(read_u32_le(&car, h + 4), 975, "single-size CoreUI version");
    assert_eq!(read_u32_le(&car, h + 432), 2, "single-size key semantics");

    let scale_col = kf.iter().position(|t| *t == 12).unwrap();
    let idiom_col = kf.iter().position(|t| *t == 15).unwrap();
    let d2_col = kf.iter().position(|t| *t == 9).unwrap();
    let part_col = kf.iter().position(|t| *t == 2).unwrap();

    let mut seen = std::collections::HashSet::new();
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let c: Vec<u16> = (0..kf.len())
            .map(|x| u16::from_le_bytes(car[i + x * 2..i + x * 2 + 2].try_into().unwrap()))
            .collect();
        if c[0] == 0
            && c[1] == 0
            && c[scale_col] == 1
            && c[d2_col] == 1
            && c[part_col] == 220
        {
            seen.insert(c[idiom_col]);
        }
    }
    assert!(seen.contains(&1), "expected phone idiom rendition");
    assert!(seen.contains(&2), "expected pad idiom rendition");

    // Loose home-screen PNGs are emitted (scaled from the 1024 source).
    assert!(out.join("AppIcon60x60@2x.png").exists());
    assert!(out.join("AppIcon76x76@2x~ipad.png").exists());

    let plist = std::fs::read_to_string(out.join("partial.plist")).unwrap();
    assert!(plist.contains("<key>CFBundleIcons</key>"));
    assert!(plist.contains("<key>CFBundleIcons~ipad</key>"));
    assert!(plist.contains("<string>AppIcon60x60</string>"));
    assert!(plist.contains("<string>AppIcon76x76</string>"));
}

#[test]
fn ios_appicon_filters_mac_idioms_from_mixed_set() {
    // A real Xcode project often keeps iOS and macOS app icons in the same
    // .appiconset. `/usr/bin/actool --platform iphoneos` ignores the mac
    // entries; our compiler must do the same.
    let root = workspace_tmp("ios_appicon_mac_filter");
    let xc = root.join("A.xcassets");
    let set = xc.join("AppIcon.appiconset");
    std::fs::create_dir_all(&set).unwrap();
    std::fs::write(
        xc.join("Contents.json"),
        r#"{"info":{"author":"xcode","version":1}}"#,
    )
    .unwrap();

    // iOS single-size source plus a handful of macOS icon sizes.
    write_png(&set.join("icon-ios-1024.png"), 1024, 1024, [64, 128, 192, 255]);
    write_png(&set.join("icon-mac-16.png"), 16, 16, [255, 0, 0, 255]);
    write_png(&set.join("icon-mac-16@2x.png"), 32, 32, [255, 0, 0, 255]);
    write_png(&set.join("icon-mac-32.png"), 32, 32, [255, 0, 0, 255]);
    write_png(&set.join("icon-mac-32@2x.png"), 64, 64, [255, 0, 0, 255]);

    std::fs::write(
        set.join("Contents.json"),
        r#"{
          "images":[
            {"filename":"icon-ios-1024.png","idiom":"universal","platform":"ios","size":"1024x1024"},
            {"filename":"icon-mac-16.png","idiom":"mac","scale":"1x","size":"16x16"},
            {"filename":"icon-mac-16@2x.png","idiom":"mac","scale":"2x","size":"16x16"},
            {"filename":"icon-mac-32.png","idiom":"mac","scale":"1x","size":"32x32"},
            {"filename":"icon-mac-32@2x.png","idiom":"mac","scale":"2x","size":"32x32"}
          ],
          "info":{"author":"xcode","version":1}
        }"#,
    )
    .unwrap();

    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    compile_ios_icon(&xc, &out);

    let car = std::fs::read(out.join("Assets.car")).expect("car");
    let kf = keyformat(&car);
    let scale_col = kf.iter().position(|t| *t == 12).unwrap();
    let idiom_col = kf.iter().position(|t| *t == 15).unwrap();
    let d2_col = kf.iter().position(|t| *t == 9).unwrap();
    let part_col = kf.iter().position(|t| *t == 2).unwrap();

    // Single-size iOS should produce exactly two Icon Image renditions,
    // one for phone (1) and one for pad (2). The mac entries must not leak
    // in as idiom 0 / universal renditions.
    let mut seen_idioms = std::collections::HashSet::new();
    for i in (0..car.len().saturating_sub(kf.len() * 2)).step_by(2) {
        let c: Vec<u16> = (0..kf.len())
            .map(|x| u16::from_le_bytes(car[i + x * 2..i + x * 2 + 2].try_into().unwrap()))
            .collect();
        if c[0] == 0
            && c[1] == 0
            && c[scale_col] == 1
            && c[d2_col] == 1
            && c[part_col] == 220
        {
            seen_idioms.insert(c[idiom_col]);
        }
    }
    assert_eq!(seen_idioms, std::collections::HashSet::from([1u16, 2u16]));
}
