//! Asset catalog compiler.
//!
//! Orchestrates the compilation of xcassets into Assets.car, .icns, and
//! the partial info plist.

use crate::bom::BomWriter;
use crate::car::{self, RenditionKeyParts};
use crate::catalog::{AssetCatalog, Facet, IconImage};
use crate::icns;
use crate::packer::{self, PackedImage};
use anyhow::Result;
use byteorder::{LittleEndian, WriteBytesExt};
use indexmap::IndexMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Relative path from a Developer directory to the CoreUI framework used by
/// actool at runtime.
const XCODE_COREUI_INFO_PLIST: &str =
    "Platforms/MacOSX.platform/System/AssetRuntime/System/Library/PrivateFrameworks/CoreUI.framework/Resources/Info.plist";

/// Fallback host CoreUI framework path (older / non-Xcode layouts).
const SYSTEM_COREUI_INFO_PLIST: &str =
    "/System/Library/PrivateFrameworks/CoreUI.framework/Resources/Info.plist";

fn coreui_version_from_plist(path: &std::path::Path) -> Option<u32> {
    let value = plist::Value::from_file(path).ok()?;
    let dict = value.as_dictionary()?;
    let version_str = dict.get("CFBundleVersion")?.as_string()?;
    version_str.split('.').next()?.parse::<u32>().ok()
}

/// Try to read the CoreUI framework version that Apple actool would use.
/// On macOS this is the version bundled with the active Xcode toolchain
/// (under `Platforms/MacOSX.platform/System/AssetRuntime`), not the host
/// system CoreUI. Falls back to the system framework when the Xcode one is not
/// present.
fn detect_host_coreui_version() -> Option<u32> {
    // Honor DEVELOPER_DIR like xcrun does.
    let developer_dir = std::env::var_os("DEVELOPER_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            // Not set; ask xcode-select for the active developer directory.
            std::process::Command::new("xcode-select")
                .arg("-p")
                .output()
                .ok()
                .and_then(|out| {
                    if out.status.success() {
                        Some(PathBuf::from(String::from_utf8_lossy(&out.stdout,
                        ).trim()))
                    } else {
                        None
                    }
                })
        })?;

    let xcode_plist = developer_dir.join(XCODE_COREUI_INFO_PLIST);
    if let Some(v) = coreui_version_from_plist(&xcode_plist) {
        return Some(v);
    }

    coreui_version_from_plist(Path::new(SYSTEM_COREUI_INFO_PLIST))
}

/// Resolve the CoreUI version to write into the CAR header.
///
/// Priority:
/// 1. Explicit `--coreui-version` CLI flag.
/// 2. `ACTOOL_COREUI_VERSION` environment variable.
/// 3. Host CoreUI framework version on macOS.
/// 4. Sensible default based on the target platform.
pub fn resolve_coreui_version(_platform: &str, explicit: Option<u32>) -> u32 {
    if let Some(v) = explicit {
        return v;
    }
    if let Ok(v) = std::env::var("ACTOOL_COREUI_VERSION") {
        if let Ok(n) = v.parse::<u32>() {
            return n;
        }
    }
    if let Some(v) = detect_host_coreui_version() {
        return v;
    }
    // Default matching modern iOS/tvOS/macOS SDKs. Apple actool uses the
    // host CoreUI version when available, so this fallback only applies on
    // non-macOS hosts where no override is supplied.
    972
}

/// Recursively copy a directory tree, preserving file permissions.
fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.as_ref().join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn compile_catalog(
    xcassets_paths: &[PathBuf],
    output_dir: &Path,
    platform: &str,
    min_deploy: &str,
    app_icon: Option<&str>,
    info_plist_path: Option<&Path>,
    accent_color: Option<&str>,
    widget_background_color: Option<&str>,
    standalone_icon_behavior: &str,
    include_languages: Option<Vec<String>>,
    development_region: Option<String>,
    plist_localizations: bool,
    coreui_version: Option<u32>,
) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(output_dir)?;
    let has_icon = app_icon.is_some();

    // Parse each provided asset catalog and merge their contents into a single
    // compilation unit, matching Apple actool behavior when multiple catalogs
    // are passed on the command line.
    let mut all_renditions: Vec<crate::car::Rendition> = Vec::new();
    let mut all_facets: IndexMap<String, crate::catalog::Facet> = IndexMap::new();
    let mut all_loose_jpegs: Vec<(String, PathBuf)> = Vec::new();
    let mut all_locales: HashSet<String> = HashSet::new();
    let mut all_appicon_images: Vec<crate::catalog::IconImage> = Vec::new();
    let mut all_icon_images: Vec<(PathBuf, u32, u32)> = Vec::new();
    let mut all_tvos_brandassets: Vec<PathBuf> = Vec::new();
    let mut all_tvos_top_shelf: Vec<(String, PathBuf)> = Vec::new();

    for path in xcassets_paths {
        let mut catalog = AssetCatalog::new(
            path.to_path_buf(),
            platform.to_string(),
            min_deploy.to_string(),
            app_icon.map(|s| s.to_string()),
            include_languages.clone(),
            development_region.clone(),
        );
        let (mut renditions, facets) = catalog.parse()?;
        all_renditions.append(&mut renditions);
        for (name, facet) in facets {
            all_facets.entry(name).or_insert(facet);
        }
        all_loose_jpegs.append(&mut catalog.loose_jpegs.clone());
        all_locales.extend(catalog.get_locales_used());
        all_appicon_images.append(&mut catalog.get_appicon_images()?);
        all_icon_images.append(&mut catalog.get_icon_images()?);
        all_tvos_brandassets.append(&mut catalog.tvos_brandassets.clone());
        all_tvos_top_shelf.append(&mut catalog.tvos_top_shelf_images.clone());
    }

    // Build a representative catalog object for methods that still need it.
    let _catalog = AssetCatalog::new(
        xcassets_paths[0].clone(),
        platform.to_string(),
        min_deploy.to_string(),
        app_icon.map(|s| s.to_string()),
        include_languages,
        development_region,
    );

    let (mut renditions, facets) = (all_renditions, all_facets);

    let deploy_ver: (u32, u32) = {
        let mut parts = min_deploy.split('.');
        let a: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let b: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (a, b)
    };
    let min_pack_ver = car::min_pack_version(platform);
    let (mut pack_groups, inline_indices) = if deploy_ver >= min_pack_ver {
        packer::group_for_packing(&renditions)
    } else {
        (Vec::new(), (0..renditions.len()).collect())
    };

    // Compute dynamic keyformat: include dim1 when atlases exceed distinct scales.
    let mut trial_atlas_count = 0;
    let mut trial_scales = std::collections::HashSet::new();
    for (_fmt, scale, idxs) in &pack_groups {
        trial_scales.insert(*scale);
        let imgs: Vec<PackedImage> = idxs
            .iter()
            .map(|i| {
                let r = &renditions[*i];
                let mut pi = PackedImage::new(
                    r.name.clone(),
                    r.identifier as u32,
                    r.width,
                    r.height,
                );
                pi.pixel_format = r.pixel_format;
                pi.scale = r.scale as u32;
                pi.part = r.part as u32;
                pi.dim2 = r.dim2 as u32;
                pi.idiom = r.idiom as u32;
                pi
            })
            .collect();
        trial_atlas_count += packer::pack_images_split(imgs, 262, 196).len();
    }
    let uses_dim1 = trial_atlas_count > trial_scales.len();
    // iOS catalogs use an idiom-carrying key format (with dim2/dim1 added for
    // app icons); macOS trims its key columns to the attributes actually used.
    let keyformat = if car::is_idiom_platform(platform) {
        car::compute_keyformat_ios(&renditions, uses_dim1)
    } else {
        car::compute_keyformat(&renditions, uses_dim1)
    };

    for rend in &mut renditions {
        rend.has_icon = has_icon;
        rend.keyformat = keyformat.clone();
        rend.min_deploy = min_deploy.to_string();
        rend.platform = platform.to_string();
    }

    let mut all_entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    // dim1 (atlas index) is counted within each (scale, idiom): Apple resets
    // it to 0 for the first atlas of every idiom at a scale, not once per scale.
    let mut dim1_by_scale: IndexMap<(u16, u16), u16> = IndexMap::new();

    // Sort pack groups: (scale ascending, then GA8 before BGRA) matching Python.
    // In Python: key=lambda g: (g[1], 0 if g[0] == b"BGRA" else 1), but
    // b"BGRA" (0x42) comes after b" 8AG" (0x20) lexically. The Python comment
    // says "BGRA sorts after GA8, so use reverse fmt order" — the lambda puts
    // BGRA first (0) then GA8 (1). That matches: BGRA before GA8 within a scale.
    pack_groups.sort_by_key(|(fmt, scale, _)| (*scale, if fmt == b"BGRA" { 0 } else { 1 }));

    for (fmt, scale, idxs) in &pack_groups {
        let sprite_atlas_id =
            idxs.first().map(|i| renditions[*i].sprite_atlas_id).unwrap_or(0);
        // All renditions in a pack group share an idiom (it's part of the
        // group key), so the first one names the group's idiom.
        let group_idiom = idxs.first().map(|i| renditions[*i].idiom).unwrap_or(0);

        let packed_imgs: Vec<PackedImage> = idxs
            .iter()
            .map(|i| {
                let r = &renditions[*i];
                let intent = if r.template_rendering_intent < 0 {
                    if r.is_template {
                        2
                    } else {
                        4
                    }
                } else {
                    r.template_rendering_intent
                };
                PackedImage {
                    name: r.name.clone(),
                    identifier: r.identifier as u32,
                    width: r.width,
                    height: r.height,
                    x: 0,
                    y: 0,
                    pixel_data: r.pixel_data.clone(),
                    pixel_format: r.pixel_format,
                    scale: r.scale as u32,
                    is_template: r.is_template,
                    template_rendering_intent: intent,
                    part: r.part as u32,
                    dim2: r.dim2 as u32,
                    appearance: r.appearance as u32,
                    direction: r.direction as u32,
                    idiom: r.idiom as u32,
                    is_svg_rasterization: r.is_svg_rasterization,
                    variant: r.variant as u32,
                }
            })
            .collect();

        let mut atlases = packer::pack_images_split(packed_imgs, 262, 196);
        for atlas in &mut atlases {
            let dim1_counter = *dim1_by_scale.get(&(*scale, group_idiom)).unwrap_or(&0);
            atlas.dim1 = dim1_counter as u32;
            atlas.render();

            let all_icons = atlas.images.iter().all(|i| i.part == car::PART_ICON as u32);
            let force_lzfse = fmt == b"BGRA" && all_icons;

            let atlas_name = if sprite_atlas_id != 0 {
                atlas.name().replace("ZZZZPackedAsset", "ZZZZExplicitlyPackedAsset")
            } else {
                atlas.name()
            };

            let atlas_idiom = atlas.images.first().map(|i| i.idiom as u16).unwrap_or(0);
            let atlas_key = car::make_rendition_key(
                RenditionKeyParts {
                    element: car::ELEMENT_PACKED,
                    part: car::PART_REGULAR,
                    identifier: sprite_atlas_id,
                    dim1: dim1_counter,
                    scale: *scale,
                    idiom: atlas_idiom,
                    ..Default::default()
                },
                &keyformat,
            );

            let atlas_csi = car::build_packed_asset_csi(
                &atlas_name,
                atlas.width,
                atlas.height,
                *scale,
                fmt,
                &atlas.pixel_data,
                min_deploy,
                platform,
                force_lzfse,
            );
            all_entries.push((atlas_key, atlas_csi));

            for img in &atlas.images {
                let ref_key = car::make_rendition_key(
                    RenditionKeyParts {
                        element: car::ELEMENT_UNIVERSAL,
                        part: img.part as u16,
                        identifier: img.identifier as u16,
                        dim2: img.dim2 as u16,
                        appearance: img.appearance as u16,
                        direction: img.direction as u16,
                        scale: *scale,
                        idiom: img.idiom as u16,
                        ..Default::default()
                    },
                    &keyformat,
                );
                let inlk_y = atlas.height - img.y - img.height;
                let ref_csi = car::build_packed_image_csi(
                    &img.name,
                    img.width,
                    img.height,
                    *scale,
                    fmt,
                    img.x,
                    inlk_y,
                    sprite_atlas_id,
                    dim1_counter,
                    {
                        let mut f = (img.template_rendering_intent as u32) << 2;
                        if img.is_svg_rasterization {
                            f |= 0x04;
                        }
                        f
                    },
                    img.idiom as u16,
                );
                all_entries.push((ref_key, ref_csi));
            }
            dim1_by_scale.insert((*scale, group_idiom), dim1_counter + 1);
        }
    }

    // Sprite atlas metadata renditions
    let mut atlas_sprites: IndexMap<u16, Vec<String>> = IndexMap::new();
    for (name, facet) in &facets {
        for rend in &renditions {
            if rend.sprite_atlas_id != 0 && rend.identifier == facet.identifier {
                let entry = atlas_sprites.entry(rend.sprite_atlas_id).or_default();
                if !entry.contains(name) {
                    entry.push(name.clone());
                }
                break;
            }
        }
    }
    for (atlas_id, mut sprite_names) in atlas_sprites {
        let meta_key = car::make_rendition_key(
            RenditionKeyParts {
                element: car::ELEMENT_PACKED,
                part: car::PART_SPRITE_ATLAS,
                identifier: atlas_id,
                scale: 1,
                ..Default::default()
            },
            &keyformat,
        );
        if all_entries.iter().any(|(k, _)| k == &meta_key) {
            continue;
        }
        sprite_names.sort();
        let meta_csi = car::build_sprite_atlas_metadata_csi(&sprite_names);
        all_entries.push((meta_key, meta_csi));
    }

    // Inline renditions
    for i in inline_indices {
        let r = &renditions[i];
        let key = r.build_rendition_key();
        let csi = r.build_csi();
        all_entries.push((key, csi));
    }

    all_entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut bom = BomWriter::new();
    // iOS catalogs declare key-semantics 2 (idiom-aware keys). macOS and tvOS
    // stay on the legacy key-semantics 1 pairing. The CoreUI format version
    // normally comes from the host CoreUI framework on macOS, so we mirror that
    // by detecting it; callers can override with --coreui-version or the
    // ACTOOL_COREUI_VERSION environment variable for reproducible cross-platform
    // builds.
    let coreui_ver = resolve_coreui_version(platform, coreui_version);
    let key_semantics = if platform == "iphoneos" || platform == "iphonesimulator" {
        2
    } else {
        1
    };
    bom.add_named_block(
        "CARHEADER",
        car::make_carheader_full(all_entries.len() as u32, coreui_ver, key_semantics),
    );
    bom.set_inline_key_size(Some(keyformat.len() * 2));
    bom.add_tree("RENDITIONS", &all_entries, 4096);
    bom.set_inline_key_size(None);

    let mut facetkey_entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut facet_names: Vec<_> = facets.keys().cloned().collect();
    facet_names.sort();
    for name in &facet_names {
        let f = &facets[name];
        let key_data = name.as_bytes().to_vec();
        let value_data = car::make_facetkey_value(f.element, f.part, f.identifier);
        facetkey_entries.push((key_data, value_data));
    }
    bom.add_tree("FACETKEYS", &facetkey_entries, 4096);

    let has_appearances = renditions.iter().any(|r| r.appearance != 0);
    if car::is_idiom_platform(platform) {
        // iOS always emits APPEARANCEKEYS, declaring the wildcard appearance
        // (UIAppearanceAny=0) even when no asset has a dark variant.
        let mut ap_entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut v_any = Vec::new();
        v_any.write_u16::<LittleEndian>(0).unwrap();
        ap_entries.push((b"UIAppearanceAny".to_vec(), v_any));
        if has_appearances {
            let mut v_dark = Vec::new();
            v_dark.write_u16::<LittleEndian>(1).unwrap();
            ap_entries.push((b"UIAppearanceDark".to_vec(), v_dark));
        }
        bom.add_tree("APPEARANCEKEYS", &ap_entries, 4096);
    } else if has_appearances {
        let mut ap_entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut v1 = Vec::new();
        v1.write_u16::<LittleEndian>(1).unwrap();
        ap_entries.push((b"NSAppearanceNameDarkAqua".to_vec(), v1));
        let mut v0 = Vec::new();
        v0.write_u16::<LittleEndian>(0).unwrap();
        ap_entries.push((b"NSAppearanceNameSystem".to_vec(), v0));
        bom.add_tree("APPEARANCEKEYS", &ap_entries, 4096);
    }

    bom.add_named_block("KEYFORMAT", car::make_keyformat(&keyformat));
    bom.add_named_block(
        "EXTENDED_METADATA",
        car::make_extended_metadata(platform, min_deploy),
    );

    let bitmap_entries = build_bitmapkeys(&facets, &all_entries, &keyformat);
    bom.add_raw_key_tree("BITMAPKEYS", &bitmap_entries, 1024);

    let produce_car = !all_entries.is_empty();
    let car_path = output_dir.join("Assets.car");
    if produce_car {
        bom.write(&car_path)?;
    }

    let mut output_files: Vec<PathBuf> = Vec::new();

    // Loose JPEG files for deployment targets below the per-platform
    // JPEG-in-CAR threshold. Named after the imageset stem with the
    // source file's extension preserved (matches host actool).
    for (imageset_stem, src) in &all_loose_jpegs {
        let ext = src
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("jpg");
        let dest = output_dir.join(format!("{imageset_stem}.{ext}"));
        fs::copy(src, &dest)?;
        output_files.push(fs::canonicalize(&dest).unwrap_or(dest));
    }

    if let Some(icon_name) = app_icon {
        if car::is_idiom_platform(platform) {
            // iOS home-screen icons are emitted as loose PNGs (one per idiom,
            // at @2x) alongside the CAR — not as an .icns bundle.
            for loose in ios_loose_icons(&all_appicon_images, icon_name) {
                let dest = output_dir.join(&loose.filename);
                let expected_px = loose.point_w * loose.scale;
                if let Some((src_w, src_h)) = png_dimensions(&loose.src) {
                    if src_w != expected_px || src_h != expected_px {
                        scale_png(&loose.src, &dest, expected_px, expected_px)?;
                    } else {
                        fs::copy(&loose.src, &dest)?;
                    }
                } else {
                    fs::copy(&loose.src, &dest)?;
                }
                output_files.push(fs::canonicalize(&dest).unwrap_or(dest));
            }
        } else if standalone_icon_behavior != "none" {
            let icons = all_icon_images;
            if !icons.is_empty() {
                let icns_path = output_dir.join(format!("{icon_name}.icns"));
                icns::create_icns(&icons, &icns_path)?;
                output_files.push(fs::canonicalize(&icns_path).unwrap_or(icns_path));
            }
        }
    }

    if produce_car {
        output_files.push(fs::canonicalize(&car_path).unwrap_or(car_path));
    }

    // tvOS app-icon brandassets must be emitted as a bundle directory in the
    // compiled output (alongside Assets.car) so the app bundle contains the
    // Home Screen / App Store icon stacks that App Store Connect validates.
    if platform == "appletvos" || platform == "appletvsimulator" {
        for src in &all_tvos_brandassets {
            let bundle_name = src
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let dest = output_dir.join(&bundle_name);
            copy_dir_all(src, &dest)?;
            output_files.push(fs::canonicalize(&dest).unwrap_or(dest));
        }
        // Top-shelf images are also expected as loose files named after their
        // imageset in the app bundle root.
        for (name, src) in &all_tvos_top_shelf {
            let ext = src
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("png");
            let dest = output_dir.join(format!("{name}.{ext}"));
            fs::copy(src, &dest)?;
            output_files.push(fs::canonicalize(&dest).unwrap_or(dest));
        }
    }

    if let Some(path) = info_plist_path {
        let locales = if plist_localizations {
            let mut v: Vec<String> = all_locales.into_iter().collect();
            v.sort();
            v
        } else {
            Vec::new()
        };
        if platform == "appletvos" || platform == "appletvsimulator" {
            if let Some(icon_name) = app_icon {
                write_tvos_icon_plist(path, icon_name)?;
            } else {
                write_info_plist(
                    path,
                    app_icon,
                    accent_color,
                    widget_background_color,
                    &locales,
                )?;
            }
        } else if let (true, Some(icon_name)) = (car::is_idiom_platform(platform), app_icon) {
            write_ios_icon_plist(path, icon_name, &all_appicon_images)?;
        } else {
            write_info_plist(
                path,
                app_icon,
                accent_color,
                widget_background_color,
                &locales,
            )?;
        }
        output_files.push(fs::canonicalize(path).unwrap_or(path.to_path_buf()));
    }

    Ok(output_files)
}

fn build_bitmapkeys(
    facets: &IndexMap<String, Facet>,
    rendition_entries: &[(Vec<u8>, Vec<u8>)],
    keyformat: &[u16],
) -> Vec<(u32, Vec<u8>)> {
    let wildcard_attrs: std::collections::HashSet<u16> = [1, 2, 7, 17].into_iter().collect();

    let mut id_keys: IndexMap<u16, Vec<Vec<u16>>> = IndexMap::new();
    let id_pos = keyformat.iter().position(|t| *t == 17);
    for (key_data, _csi) in rendition_entries {
        let n_vals = key_data.len() / 2;
        let vals: Vec<u16> = (0..n_vals)
            .map(|i| u16::from_le_bytes(key_data[i * 2..i * 2 + 2].try_into().unwrap()))
            .collect();
        if let Some(p) = id_pos {
            if p < vals.len() {
                let ident = vals[p];
                id_keys.entry(ident).or_default().push(vals);
            }
        }
    }

    let mut entries: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut sorted_facets: Vec<(&String, &Facet)> = facets.iter().collect();
    sorted_facets.sort_by_key(|(_, f)| f.identifier);
    for (_name, facet) in sorted_facets {
        if facet.identifier == 0 {
            continue;
        }
        let mut attr_masks: Vec<u32> = Vec::with_capacity(keyformat.len());
        let keys_for_id = id_keys.get(&facet.identifier).cloned().unwrap_or_default();
        for (i, attr_id) in keyformat.iter().enumerate() {
            if wildcard_attrs.contains(attr_id) {
                attr_masks.push(0xFFFFFFFF);
            } else {
                let mut bitmask: u32 = 0;
                for key_vals in &keys_for_id {
                    if i < key_vals.len() {
                        let v = key_vals[i];
                        if v < 32 {
                            bitmask |= 1u32 << v;
                        }
                    }
                }
                attr_masks.push(if bitmask == 0 { 1 } else { bitmask });
            }
        }

        let n_attrs = keyformat.len() as u32;
        let data_size = 4 + n_attrs * 4;
        let mut value = Vec::new();
        value.write_u32::<LittleEndian>(1).unwrap();
        value.write_u32::<LittleEndian>(0).unwrap();
        value.write_u32::<LittleEndian>(data_size).unwrap();
        value.write_u32::<LittleEndian>(n_attrs).unwrap();
        for mask in attr_masks {
            value.write_u32::<LittleEndian>(mask).unwrap();
        }
        entries.push((facet.identifier as u32, value));
    }
    entries
}

fn write_info_plist(
    path: &Path,
    app_icon: Option<&str>,
    accent_color: Option<&str>,
    widget_background_color: Option<&str>,
    localizations: &[String],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut lines = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.to_string(),
        r#"<plist version="1.0">"#.to_string(),
        "<dict>".to_string(),
    ];
    if let Some(name) = app_icon {
        lines.push("\t<key>CFBundleIconFile</key>".to_string());
        lines.push(format!("\t<string>{name}</string>"));
        lines.push("\t<key>CFBundleIconName</key>".to_string());
        lines.push(format!("\t<string>{name}</string>"));
    }
    if let Some(n) = accent_color {
        lines.push("\t<key>NSAccentColorName</key>".to_string());
        lines.push(format!("\t<string>{n}</string>"));
    }
    if let Some(n) = widget_background_color {
        lines.push("\t<key>NSWidgetBackgroundColorName</key>".to_string());
        lines.push(format!("\t<string>{n}</string>"));
    }
    if !localizations.is_empty() {
        lines.push("\t<key>CFBundleLocalizations</key>".to_string());
        lines.push("\t<array>".to_string());
        for loc in localizations {
            lines.push(format!("\t\t<string>{loc}</string>"));
        }
        lines.push("\t</array>".to_string());
    }
    lines.push("</dict>".to_string());
    lines.push("</plist>".to_string());
    lines.push(String::new());
    fs::write(path, lines.join("\n"))?;
    Ok(())
}

/// The iOS home-screen ("primary") app-icon point size for an idiom — the size
/// listed in CFBundleIconFiles and emitted as a loose PNG. Smaller idiom sizes
/// (notification/settings/spotlight) and the marketing icon are CAR-only.
fn ios_primary_size(idiom: &str) -> Option<u32> {
    match idiom {
        "iphone" => Some(60),
        "ipad" => Some(76),
        _ => None,
    }
}

struct LooseIcon {
    filename: String,
    src: PathBuf,
    point_w: u32,
    scale: u32,
}

/// Loose home-screen PNGs actool drops next to the CAR: the @2x primary icon
/// for each idiom present (`AppIcon60x60@2x.png`, `AppIcon76x76@2x~ipad.png`).
fn ios_loose_icons(icons: &[IconImage], name: &str) -> Vec<LooseIcon> {
    let mut out = Vec::new();
    for idiom in ["iphone", "ipad"] {
        let Some(primary) = ios_primary_size(idiom) else {
            continue;
        };
        let Some(img) = icons
            .iter()
            .find(|i| i.idiom == idiom && i.point_w == primary && i.scale == 2)
        else {
            continue;
        };
        let ext = img
            .path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("png");
        let suffix = if idiom == "ipad" { "~ipad" } else { "" };
        out.push(LooseIcon {
            filename: format!("{name}{primary}x{primary}@2x{suffix}.{ext}"),
            src: img.path.clone(),
            point_w: primary,
            scale: 2,
        });
    }
    out
}

/// Read the width/height of a PNG file without decoding pixels.
fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut decoder = png::Decoder::new(reader);
    let info = decoder.read_header_info().ok()?;
    Some((info.width, info.height))
}

/// Scale a PNG image to the requested dimensions and write it as an RGBA PNG
/// with sRGB and EXIF chunks, matching Apple actool's loose icon output.
fn scale_png(src: &Path, dest: &Path, width: u32, height: u32) -> Result<()> {
    let img = image::open(src)?;
    let resized = img.resize(width, height, image::imageops::FilterType::Lanczos3);
    let rgba = resized.to_rgba8();

    let exif = build_exif_dimensions(width, height);
    let file = fs::File::create(dest)?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    writer.write_chunk(png::chunk::ChunkType(*b"eXIf"), &exif)?;
    writer.write_image_data(rgba.as_raw())?;
    Ok(())
}

/// Build the minimal EXIF blob Apple actool embeds in loose app-icon PNGs:
/// ColorSpace = sRGB, PixelXDimension, PixelYDimension.
fn build_exif_dimensions(width: u32, height: u32) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut buf = Vec::new();
    // TIFF header (big-endian)
    buf.extend_from_slice(b"MM");
    buf.write_u16::<BigEndian>(0x002a).unwrap();
    buf.write_u32::<BigEndian>(8).unwrap(); // IFD0 offset

    // IFD0: one entry pointing to the ExifIFD.
    buf.write_u16::<BigEndian>(1).unwrap();
    buf.write_u16::<BigEndian>(0x8769).unwrap(); // ExifIFD pointer
    buf.write_u16::<BigEndian>(4).unwrap();      // LONG
    buf.write_u32::<BigEndian>(1).unwrap();
    buf.write_u32::<BigEndian>(26).unwrap();     // ExifIFD offset
    buf.write_u32::<BigEndian>(0).unwrap();      // next IFD

    // ExifIFD at offset 26: ColorSpace, PixelXDimension, PixelYDimension.
    buf.write_u16::<BigEndian>(3).unwrap();
    // ColorSpace = 1 (sRGB)
    buf.write_u16::<BigEndian>(0xa001).unwrap();
    buf.write_u16::<BigEndian>(3).unwrap(); // SHORT
    buf.write_u32::<BigEndian>(1).unwrap();
    buf.write_u16::<BigEndian>(1).unwrap();
    buf.write_u16::<BigEndian>(0).unwrap(); // padding
    // PixelXDimension
    buf.write_u16::<BigEndian>(0xa002).unwrap();
    buf.write_u16::<BigEndian>(4).unwrap(); // LONG
    buf.write_u32::<BigEndian>(1).unwrap();
    buf.write_u32::<BigEndian>(width).unwrap();
    // PixelYDimension
    buf.write_u16::<BigEndian>(0xa003).unwrap();
    buf.write_u16::<BigEndian>(4).unwrap(); // LONG
    buf.write_u32::<BigEndian>(1).unwrap();
    buf.write_u32::<BigEndian>(height).unwrap();
    buf.write_u32::<BigEndian>(0).unwrap(); // next IFD

    buf
}

/// CFBundleIconFiles base name (`AppIcon60x60`) for an idiom's primary icon,
/// or empty when the catalog has no home-screen icon for that idiom.
fn ios_icon_files(icons: &[IconImage], name: &str, idiom: &str) -> Vec<String> {
    match ios_primary_size(idiom) {
        Some(primary) if icons.iter().any(|i| i.idiom == idiom && i.point_w == primary) => {
            vec![format!("{name}{primary}x{primary}")]
        }
        _ => Vec::new(),
    }
}

/// iOS partial Info.plist: a `CFBundleIcons` (iPhone primary) and
/// `CFBundleIcons~ipad` dict, each carrying `CFBundleIconName` and — when the
/// idiom has home-screen icons — `CFBundleIconFiles`. The iPad list inherits
/// the iPhone primaries (iPad falls back to them), matching host actool.
fn write_ios_icon_plist(path: &Path, name: &str, icons: &[IconImage]) -> Result<()> {
    let iphone_files = ios_icon_files(icons, name, "iphone");
    // iPad lists its own primary (preceded by the iPhone fallback) only when it
    // actually ships an iPad icon; with no iPad icons the dict carries just the
    // name, matching host actool.
    let ipad_primary = ios_icon_files(icons, name, "ipad");
    let ipad_files = if ipad_primary.is_empty() {
        Vec::new()
    } else {
        let mut v = iphone_files.clone();
        v.extend(ipad_primary);
        v
    };

    write_ios_icon_plist_files(path, name, &iphone_files, &ipad_files, (true, true))
}

/// Write the iOS partial plist from already-resolved `CFBundleIconFiles`
/// lists (an empty list omits the key). `with_name` says, per idiom
/// (iPhone, iPad), whether the dict also carries `CFBundleIconName`.
pub fn write_ios_icon_plist_files(
    path: &Path,
    name: &str,
    iphone_files: &[String],
    ipad_files: &[String],
    with_name: (bool, bool),
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut lines = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.to_string(),
        r#"<plist version="1.0">"#.to_string(),
        "<dict>".to_string(),
    ];
    let mut emit_primary = |key: &str, files: &[String], with_name: bool| {
        lines.push(format!("\t<key>{key}</key>"));
        lines.push("\t<dict>".to_string());
        lines.push("\t\t<key>CFBundlePrimaryIcon</key>".to_string());
        lines.push("\t\t<dict>".to_string());
        if !files.is_empty() {
            lines.push("\t\t\t<key>CFBundleIconFiles</key>".to_string());
            lines.push("\t\t\t<array>".to_string());
            for f in files {
                lines.push(format!("\t\t\t\t<string>{f}</string>"));
            }
            lines.push("\t\t\t</array>".to_string());
        }
        if with_name {
            lines.push("\t\t\t<key>CFBundleIconName</key>".to_string());
            lines.push(format!("\t\t\t<string>{name}</string>"));
        }
        lines.push("\t\t</dict>".to_string());
        lines.push("\t</dict>".to_string());
    };
    emit_primary("CFBundleIcons", iphone_files, with_name.0);
    emit_primary("CFBundleIcons~ipad", ipad_files, with_name.1);
    lines.push("</dict>".to_string());
    lines.push("</plist>".to_string());
    lines.push(String::new());
    fs::write(path, lines.join("\n"))?;
    Ok(())
}

/// tvOS partial Info.plist: a `CFBundleIcons` dict whose primary icon is the
/// icon name as a string. Unlike iOS, tvOS does not list icon files or emit
/// a separate `CFBundleIcons~ipad` key.
fn write_tvos_icon_plist(path: &Path, name: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let lines = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.to_string(),
        r#"<plist version="1.0">"#.to_string(),
        "<dict>".to_string(),
        "\t<key>CFBundleIcons</key>".to_string(),
        "\t<dict>".to_string(),
        "\t\t<key>CFBundlePrimaryIcon</key>".to_string(),
        format!("\t\t<string>{name}</string>"),
        "\t</dict>".to_string(),
        "</dict>".to_string(),
        "</plist>".to_string(),
        String::new(),
    ];
    fs::write(path, lines.join("\n"))?;
    Ok(())
}
