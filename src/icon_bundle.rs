//! .icon bundle support (modern macOS icon.json + source image).

use crate::bom::BomWriter;
use crate::car::{self, MultisizeImageEntry, Rendition};
use crate::catalog::load_image_as_bgra;
use crate::icon_json::{Fill, IconJson};
use crate::name_hash::hash_name;
use byteorder::LittleEndian;

static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
use anyhow::Result;
use image::imageops::FilterType;
use std::fs;
use std::path::{Path, PathBuf};

/// One facet entry in the FACETKEYS tree: (facet_name, element, part, identifier).
type FacetEntry = (String, u16, Option<u16>, u16);

/// Bundle stem used as the prefix for asset facet names: e.g.
/// `<stem>_Assets/<layer_name>`. Matches Apple's actool naming.
fn bundle_facet_prefix(icon_path: &Path) -> String {
    icon_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Assets")
        .to_string()
}

/// Build a deterministic UUID-shaped string derived from `inputs`. Apple's
/// iconNxN_NSAppearanceName..._UUID-PID-HEX.png names embed a per-rendition
/// UUID; we don't need it to be cryptographic — just stable per input so
/// regenerating the same catalog yields the same byte stream.
fn deterministic_uuid(inputs: &[&str]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h1 = std::collections::hash_map::DefaultHasher::new();
    let mut h2 = std::collections::hash_map::DefaultHasher::new();
    "uuid-lo".hash(&mut h1);
    "uuid-hi".hash(&mut h2);
    for s in inputs {
        s.hash(&mut h1);
        s.hash(&mut h2);
    }
    let lo = h1.finish();
    let hi = h2.finish();
    let bytes: [u8; 16] = {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&lo.to_be_bytes());
        out[8..].copy_from_slice(&hi.to_be_bytes());
        out
    };
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Name a pre-rendered icon size the way Apple's actool does inside .car:
/// `iconNxN_<appearance>_<UUID>-<pid>-<hex>.png`. The pid+hex tail is a
/// stable hash-derived suffix; CoreUI keys renditions by attribute tuple,
/// not by name, so the precise format doesn't affect lookup.
fn pre_rendered_name(
    icon_name: &str,
    point_size: u32,
    scale: u32,
    appearance_name: &str,
) -> String {
    let scale_label = format!("{scale}x");
    let pt_label = format!("{point_size}");
    let uuid = deterministic_uuid(&[icon_name, &pt_label, &scale_label, appearance_name]);
    let tail_uuid =
        deterministic_uuid(&[icon_name, &pt_label, &scale_label, "tail"]);
    let tail_hex: String = tail_uuid.chars().filter(|c| *c != '-').collect();
    let pid = &tail_hex[0..5];
    let hex = &tail_hex[5..21];
    // Apple prefixes the pre-rendered name with the bundle stem (the
    // `--app-icon` name), not a literal "icon": feishin.icon → "feishin16x16…".
    // element-web's stem happens to be "icon", which is why the old literal
    // matched it.
    format!(
        "{icon_name}{point_size}x{point_size}_{appearance_name}_{uuid}-{pid}-{hex}.png"
    )
}

/// Sizes Apple's actool emits for `.icon` bundles: one rendition per point
/// size, all @2x except the 1024pt slot which is @1x. The 16/32/64 slots
/// get bundled into a packed atlas; the rest are stored inline.
const MACOS_ICON_SIZES: &[(u32, u32)] = &[
    (16, 2),
    (32, 2),
    (64, 2),
    (128, 2),
    (256, 2),
    (512, 2),
    (1024, 1),
];

/// Point sizes Apple atlases into ZZZZPackedAsset; the rest are inline.
const ATLAS_POINT_SIZES: &[u32] = &[16, 32, 64];

fn icon_dim2(point_size: u32) -> u16 {
    match point_size {
        16 => 1,
        32 => 2,
        64 => 3,
        128 => 4,
        256 => 5,
        512 => 6,
        1024 => 7,
        _ => 0,
    }
}

pub fn is_icon_bundle(path: &Path) -> bool {
    // Dispatch on `.icon` extension alone. `compile_icon_bundle` produces
    // a clean error when the path isn't a directory or icon.json is missing,
    // rather than silently falling through to the legacy xcassets path
    // (which emits an empty info.plist and no Assets.car).
    path.extension().and_then(|s| s.to_str()) == Some("icon")
}

/// CoreUI version at which Apple names the stock light/dark background
/// gradients `system-light`/`system-dark` instead of `Gradient-N`. Observed on
/// 1011 (Xcode 26); 975-era references use `Gradient-N`.
const SYSTEM_GRADIENT_NAMES_MIN_COREUI: u32 = 1000;

/// Lowest CoreUI version whose IconComposer code paths accept `.icon` catalogs.
const MIN_ICON_COREUI: u32 = 975;

/// Compile with the baseline (975-era) output; deterministic regardless of the
/// installed toolchain.
#[allow(clippy::too_many_arguments)]
pub fn compile_icon_bundle(
    icon_path: &Path,
    output_dir: &Path,
    platform: &str,
    min_deploy: &str,
    app_icon: Option<&str>,
    info_plist_path: Option<&Path>,
    accent_color: Option<&str>,
    standalone_icon_behavior: &str,
) -> Result<Vec<PathBuf>> {
    compile_icon_bundle_with_options(
        icon_path,
        output_dir,
        platform,
        min_deploy,
        app_icon,
        info_plist_path,
        accent_color,
        standalone_icon_behavior,
        &IconCompileOptions::default(),
    )
}

/// Toolchain-dependent inputs to an icon-bundle compile.
#[derive(Debug, Clone, Default)]
pub struct IconCompileOptions {
    /// CoreUI version of the toolchain being emulated (header version and
    /// version-dependent layout); `None` is the 975-era baseline.
    pub coreui_version: Option<u32>,
    /// `--target-device` values; empty means every device.
    pub target_devices: Vec<String>,
}

/// iOS refuses an `--app-icon` that doesn't name the bundle, where macOS
/// quietly emits an empty plist. Carries what actool still prints.
#[derive(Debug)]
pub struct NoMatchingAppIcon {
    pub name: String,
    pub document: PathBuf,
    pub output_files: Vec<PathBuf>,
}

impl std::fmt::Display for NoMatchingAppIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "None of the input catalogs contained a matching stickers icon set, app icon set, or icon stack named  \"{}\".",
            self.name
        )
    }
}

impl std::error::Error for NoMatchingAppIcon {}

/// Like [`compile_icon_bundle`], matching the toolchain described by `options`.
#[allow(clippy::too_many_arguments)]
pub fn compile_icon_bundle_with_options(
    icon_path: &Path,
    output_dir: &Path,
    platform: &str,
    min_deploy: &str,
    app_icon: Option<&str>,
    info_plist_path: Option<&Path>,
    accent_color: Option<&str>,
    standalone_icon_behavior: &str,
    options: &IconCompileOptions,
) -> Result<Vec<PathBuf>> {
    let coreui_version = options.coreui_version;
    let coreui_version = coreui_version.unwrap_or(MIN_ICON_COREUI).max(MIN_ICON_COREUI);
    fs::create_dir_all(output_dir)?;
    let bundle_stem = icon_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    // --app-icon controls the .icns filename and Info.plist value, but
    // Apple's actool always names the in-CAR facet after the bundle stem.
    let icon_name = app_icon.map(|s| s.to_string()).unwrap_or(bundle_stem.clone());

    let ios_platform = matches!(platform, "iphoneos" | "iphonesimulator");
    if ios_platform && icon_name != bundle_stem {
        let mut output_files = Vec::new();
        if let Some(path) = info_plist_path {
            write_empty_partial_plist(path)?;
            output_files.push(fs::canonicalize(path).unwrap_or(path.to_path_buf()));
        }
        return Err(NoMatchingAppIcon {
            name: icon_name,
            document: fs::canonicalize(icon_path).unwrap_or(icon_path.to_path_buf()),
            output_files,
        }
        .into());
    }

    let icon_json_path = icon_path.join("icon.json");
    let icon_json_text = fs::read_to_string(&icon_json_path).map_err(|e| {
        anyhow::anyhow!("could not read {}: {e}", icon_json_path.display())
    })?;
    let icon_json_value: serde_json::Value = serde_json::from_str(&icon_json_text)?;
    let parsed: IconJson = IconJson::parse(&icon_json_text)?;

    // Apple errors on `{}` (no `groups` key) but accepts `{"groups": []}`.
    // The serde-default Vec collapses both cases, so detect the distinction
    // on the raw JSON value before falling through to layer validation.
    if icon_json_value.get("groups").is_none() {
        anyhow::bail!("icon.json missing required `groups` field");
    }

    // Apple bails when any layer references a missing `image-name` or an
    // image file that doesn't resolve in `Assets/` or the bundle root.
    // Validate up front so we surface a clean error rather than silently
    // emitting an empty catalog.
    validate_layer_image_refs(icon_path, &icon_json_value)?;

    let source_images = find_all_source_images(icon_path, &icon_json_value);
    if source_images.is_empty() {
        return Ok(Vec::new());
    }
    let facet_prefix = bundle_facet_prefix(icon_path);
    let layer_assets = collect_layer_assets(icon_path, &parsed, &facet_prefix);
    // Apple emits a `<stem>/<group_name>` facet for every group even when
    // the group has no `name` field. Anonymous groups fall back to
    // "Group", then "Group 2", "Group 3", … so each facet stays unique
    // across multi-group bundles like ding_icon.
    let group_facet_names: Vec<String> = {
        let mut anon_seq: u32 = 0;
        parsed
            .groups
            .iter()
            .map(|g| {
                let n = match g.name.as_deref() {
                    Some(n) => n.to_string(),
                    None => {
                        anon_seq += 1;
                        if anon_seq == 1 {
                            "Group".to_string()
                        } else {
                            format!("Group {anon_seq}")
                        }
                    }
                };
                format!("{facet_prefix}/{n}")
            })
            .collect()
    };
    let (color_assets, mut gradient_assets) =
        fill_assets(&facet_prefix, parsed.fill.as_ref(), &parsed)
            .unwrap_or_else(|| (Vec::new(), Vec::new()));
    if coreui_version >= SYSTEM_GRADIENT_NAMES_MIN_COREUI {
        name_system_gradients(&facet_prefix, &color_assets, &mut gradient_assets);
    }

    let mut output_files: Vec<PathBuf> = Vec::new();

    // Route both PNG- and SVG-source `.icon` bundles through the same
    // IconComposer emit path. SVG layers are rasterized to PNGs in a
    // temp dir at 1024x1024 (full size for the layer asset) and at each
    // MACOS_ICON_SIZE (for the multisize + atlas pipeline).
    // Scratch dir for rasterized PNGs from SVG layers and the per-scale
    // resized images. Includes a monotonically incrementing counter so
    // concurrent compiles in the same process (e.g. parallel integration
    // tests) don't share the same path and tear each other down.
    let tmp_seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmpdir = std::env::temp_dir().join(format!(
        "actool_icon_{}_{}",
        std::process::id(),
        tmp_seq
    ));
    fs::create_dir_all(&tmpdir)?;
    let primary_source = &source_images[0];
    let primary_is_svg = is_svg_path(primary_source);

    // (filepath, pixel_size, scale) — fed to build_icon_car
    let mut icon_images: Vec<(PathBuf, u32, u32)> = Vec::new();
    if primary_is_svg {
        let svg_data = fs::read(primary_source)?;
        let (sw, sh) = crate::svg_raster::parse_svg_dimensions(&svg_data);
        if sw == 0 || sh == 0 {
            anyhow::bail!(
                "could not determine intrinsic size of {}",
                primary_source.display()
            );
        }
        for (point_size, scale) in MACOS_ICON_SIZES {
            let pixel_size = point_size * scale;
            let bgra =
                crate::svg_raster::rasterize_svg(&svg_data, *point_size, *point_size, *scale)?;
            let mut rgba = bgra.clone();
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            let img = image::RgbaImage::from_raw(pixel_size, pixel_size, rgba)
                .ok_or_else(|| anyhow::anyhow!("svg rasterization size mismatch"))?;
            let filename = format!("Icon{pixel_size}x{pixel_size}.png");
            let filepath = tmpdir.join(&filename);
            img.save(&filepath)?;
            icon_images.push((filepath, pixel_size, *scale));
        }
    } else {
        let src_img = image::open(primary_source)?.to_rgba8();
        for (point_size, scale) in MACOS_ICON_SIZES {
            let pixel_size = point_size * scale;
            let resized = image::imageops::resize(
                &src_img,
                pixel_size,
                pixel_size,
                FilterType::Lanczos3,
            );
            let filename = format!("Icon{pixel_size}x{pixel_size}.png");
            let filepath = tmpdir.join(&filename);
            resized.save(&filepath)?;
            icon_images.push((filepath, pixel_size, *scale));
        }
    }

    // SVG-sourced layers are emitted as Vector renditions (raw SVG) by
    // build_icon_car, so they pass through untouched here.
    // Apple emits the light+dark appearance-variant axis (a second set of
    // pre-rendered sized renditions + an alternate atlas keyed on attribute
    // 24 = 1) for a **single-group** icon that declares any `appearance: dark`
    // specialization — not only a top-level `fill-specializations`. KYA has
    // `fill-specializations: null` at the top level but a dark layer fill, and
    // Apple still emits 33 renditions with attr 24 = [0, 1]; gating on the
    // top-level block alone dropped its entire dark variant. **Multi-group**
    // icons never carry the axis even with dark content (transmission's 3 groups
    // and Rectangle's 2 → attr 24 absent in Apple's output) — their per-group
    // stacks are recomposed live instead. A `tinted` appearance never triggers
    // it (tint mode is a live system recolour, never a stored variant).
    let emit_variant_axis =
        parsed.groups.len() == 1 && json_has_dark_appearance(&icon_json_value);

    // Pre-render the full layer stack (all layers, positioned, with glass
    // shading / blend modes / opacity) at each icon size, aligned with
    // `icon_images` — per appearance, since blend modes and fills differ
    // between light and dark. The primary variant uses the light stack, the
    // alternate (when the variant axis is active) the dark stack.
    use crate::icon_effects::Appearance;
    // iOS icons are full-bleed: layers and the gradient span the whole canvas,
    // where macOS insets them into the 824/1024 content box.
    let ios = matches!(platform, "iphoneos" | "iphonesimulator");
    let base_scale = if ios { 1.0 } else { LAYER_BASE_SCALE };
    let light_layers = collect_stack_layers(icon_path, &parsed, Appearance::Light, base_scale);
    let dark_layers = collect_stack_layers(icon_path, &parsed, Appearance::Dark, base_scale);
    // Frosted-glass layers multiply the background gradient, so the stack
    // renderer needs the same fill the compositor will draw under it. The
    // solid-fill case uses the flat solid colour for the light background.
    let (light_fill, dark_fill) =
        resolve_background_fills(&parsed, &gradient_assets, &color_assets, base_scale);
    let mut light_stacks: Vec<Vec<u8>> = Vec::with_capacity(MACOS_ICON_SIZES.len());
    let mut dark_stacks: Vec<Vec<u8>> = Vec::with_capacity(MACOS_ICON_SIZES.len());
    let ios_images = if ios {
        Some(render_ios_composites(
            &light_layers,
            &dark_layers,
            light_fill.as_ref(),
            dark_fill.as_ref().or(light_fill.as_ref()),
        )?)
    } else {
        None
    };
    for (point_size, scale) in MACOS_ICON_SIZES.iter().filter(|_| !ios) {
        let px = point_size * scale;
        light_stacks.push(render_layer_stack(&light_layers, px, light_fill.as_ref())?);
        if emit_variant_axis {
            let dfill = dark_fill.as_ref().or(light_fill.as_ref());
            dark_stacks.push(render_layer_stack(&dark_layers, px, dfill)?);
        }
    }

    let car_path = output_dir.join("Assets.car");
    build_icon_car(
        &car_path,
        &facet_prefix,
        &icon_images,
        &layer_assets,
        &group_facet_names,
        &color_assets,
        &gradient_assets,
        &parsed.groups,
        &light_stacks,
        &dark_stacks,
        light_fill.as_ref(),
        dark_fill.as_ref(),
        emit_variant_axis,
        platform,
        min_deploy,
        coreui_version,
        ios_images.as_deref(),
    )?;
    output_files.push(fs::canonicalize(&car_path).unwrap_or(car_path));

    // Apple's empirical rule for `.icon` bundles: emit `<icon_name>.icns`
    // and a populated partial plist (CFBundleIconFile/CFBundleIconName)
    // iff `--app-icon` matches the bundle's filename stem case-sensitively.
    // Verified by toggling the stem against the flag — neither
    // `supported-platforms` nor `--standalone-icon-behavior` affects this.
    let names_match = icon_name == bundle_stem;
    if let Some(images) = ios_images.as_ref().filter(|_| names_match) {
        output_files.extend(write_ios_home_screen_icons(&icon_name, images, output_dir)?);
    } else if names_match && !ios {
        let icns_path = output_dir.join(format!("{icon_name}.icns"));
        let icns_images =
            composite_icns_sources(&icon_images, &light_stacks, light_fill.as_ref(), &tmpdir)?;
        crate::icns::create_icns(&icns_images, &icns_path)?;
        if icns_path.exists() {
            output_files.push(fs::canonicalize(&icns_path).unwrap_or(icns_path));
        }
    }

    let _ = fs::remove_dir_all(&tmpdir);
    let _ = accent_color;
    let _ = standalone_icon_behavior;

    if let Some(path) = info_plist_path {
        if names_match && ios {
            let targets = |d: &str| {
                options.target_devices.is_empty()
                    || options.target_devices.iter().any(|t| t == d)
            };
            crate::compiler::write_ios_icon_plist_files(
                path,
                &icon_name,
                &[format!("{icon_name}60x60")],
                &[format!("{icon_name}60x60"), format!("{icon_name}76x76")],
                (targets("iphone"), targets("ipad")),
            )?;
        } else if names_match {
            write_populated_partial_plist(path, &icon_name)?;
        } else {
            write_empty_partial_plist(path)?;
        }
        output_files.push(fs::canonicalize(path).unwrap_or(path.to_path_buf()));
    }
    Ok(output_files)
}

/// Build a `<stem>_Assets/<layer_name>` facet entry for each layer that
/// references an image. Source paths are resolved against `<bundle>/Assets/`
/// first, then the bundle root. Layers without a resolvable image are
/// skipped silently — they don't correspond to a source asset.
fn collect_layer_assets(
    bundle: &Path,
    json: &IconJson,
    facet_prefix: &str,
) -> Vec<LayerAsset> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_group, layer) in json.iter_layers() {
        let Some(image_name) = layer.image_name.as_deref() else {
            continue;
        };
        // Both SVG and raster source layers flow through the same path.
        // SVG layers are rasterized to PNG via materialize_svg_layer_assets
        // before they reach load_image_as_bgra.
        let assets_path = bundle.join("Assets").join(image_name);
        let resolved = if assets_path.exists() {
            assets_path
        } else {
            let root_path = bundle.join(image_name);
            if !root_path.exists() {
                continue;
            }
            root_path
        };
        // Apple names the asset facet after the image-file stem (no
        // extension), not the layer's `name` field. Verified against
        // element-web (element.png → "element"), tagspaces (Image 2.png
        // → "Image 2") and KYA (AppIcon.svg → "AppIcon", layer.name = "Logo").
        let stem = std::path::Path::new(image_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(image_name);
        let facet_name = format!("{facet_prefix}_Assets/{stem}");
        if !seen.insert(facet_name.clone()) {
            continue;
        }
        out.push(LayerAsset {
            facet_name,
            source_path: resolved,
        });
    }
    out
}

/// Apple's actool errors when a layer has no `image-name` or references an
/// image that doesn't exist in `Assets/` or the bundle root. Mirror that
/// behaviour with explicit messages instead of silently dropping layers
/// (which is what `find_all_source_images` does).
fn validate_layer_image_refs(bundle: &Path, json: &serde_json::Value) -> Result<()> {
    let groups = match json.get("groups").and_then(|v| v.as_array()) {
        Some(g) => g,
        None => return Ok(()),
    };
    for group in groups {
        let Some(layers) = group.get("layers").and_then(|v| v.as_array()) else {
            continue;
        };
        for layer in layers {
            let layer_name = layer
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("(unnamed)");
            let image_name = match layer.get("image-name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => {
                    anyhow::bail!(
                        "the layer \"{layer_name}\" does not have an image name"
                    );
                }
            };
            let in_assets = bundle.join("Assets").join(image_name);
            let in_root = bundle.join(image_name);
            if !in_assets.exists() && !in_root.exists() {
                anyhow::bail!(
                    "the layer \"{layer_name}\" references an image named \
                     \"{image_name}\" that does not exist"
                );
            }
        }
    }
    Ok(())
}

/// The layer image at `scale = 1` is drawn into the icon's content area — the
/// 824/1024 squircle inset, measured from tagspaces (a non-glass positioned
/// layer). `position.scale` multiplies it and `translation-in-points` (in this
/// same scaled space) shifts it.
const LAYER_BASE_SCALE: f32 = 824.0 / 1024.0;

/// Grey floor an opaque-glass layer's blacks are lifted toward (measured on
/// KYA's cup body, lum ≈ 45/255).
const GLASS_FLOOR: f32 = 45.0 / 255.0;

/// Frosted-glass tint depth: a `layer-color`-shadow glass layer darkens the
/// background by `D·(1 − colour)` per channel — a uniform subtractive tint, not
/// a multiply. Measured constant (≈63.5/255) across background, colour, channel,
/// vertical position and every translucency value > 0 via a solid-slab probe
/// (`tools/probe_glass_tint.py`); overlapping tinted layers stack the
/// subtraction additively.
const GLASS_TINT_D: f32 = 63.5 / 255.0;

/// The "raised glass" look is a soft blur of the glass contribution's edges —
/// not an emboss/bevel (an edge-profile probe found a monotonic transition with
/// no rim). The feather is a Gaussian of σ ≈ 19 px at 1024, measured
/// size-independent (`tools/probe_glass_relief.py`), so it scales with the
/// rendition. (18 rather than 19 since the three-box approximation slightly
/// widens the effective σ — tuned so our edge width matches Apple's ≈48 px.)
const GLASS_BLUR_SIGMA: f32 = 18.0;

/// A neutral (non-tinted) frosted layer darkens the background it sits over by a
/// constant fraction — measured on Apple's scrumdinger GA8 as a flat ≈12.7/255
/// (≈5.1%) below the local background across the *entire* layer body, top to
/// bottom (`tools/layer_body` probe). The earlier `0.025 + 0.012·y` ramp was a
/// misfit. Apple's frosted glass flattens the layer's internal structure into a
/// uniform tint; our relief still preserves some of it (the layer-boundary
/// "bowl" and the stem), so a ±-ramp residual remains — that flattening is the
/// renderer-bound part. The flat level is the correct first-order match.
const GLASS_RELIEF_STRENGTH: f32 = 0.051;

/// Per-layer drop shadow (`shadow: layer-color`/`neutral`): a glass layer casts
/// a soft shadow onto the background, offset down, tinted subtractively by
/// `(1 − colour)` like the glass tint. Measured (`tools/probe_layer_shadow.py`):
/// peak ≈ 0.35·opacity for `layer-color`, ~0.07·opacity for `neutral`; blurred
/// (σ ≈ 16 px @ 1024) and offset down ≈ 9 px, all scaled to the rendition.
const SHADOW_PEAK_LAYERCOLOR: f32 = 0.49;
const SHADOW_PEAK_NEUTRAL: f32 = 0.10;
const SHADOW_SIGMA: f32 = 17.0;
const SHADOW_OFFSET_Y: f32 = 12.0;

/// In-place separable blur of a straight-RGBA buffer (Gaussian approximated by
/// three box passes), blurring in premultiplied space so transparent edges
/// don't bleed dark. `sigma` is in pixels; a no-op below ~1 px.
fn blur_rgba_premul(buf: &mut [u8], w: usize, sigma: f32) {
    let radius = sigma.round() as usize;
    if radius < 1 {
        return;
    }
    let n = w * w;
    // Straight RGBA → premultiplied f32 channels.
    let mut ch: [Vec<f32>; 4] =
        [vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]];
    for i in 0..n {
        let a = buf[i * 4 + 3] as f32 / 255.0;
        for c in 0..3 {
            ch[c][i] = (buf[i * 4 + c] as f32 / 255.0) * a;
        }
        ch[3][i] = a;
    }
    let mut tmp = vec![0.0f32; n];
    for c in &mut ch {
        for _ in 0..3 {
            box_blur_h(c, &mut tmp, w, radius);
            box_blur_v(c, &mut tmp, w, radius);
        }
    }
    // Premultiplied f32 → straight RGBA u8.
    for i in 0..n {
        let a = ch[3][i].clamp(0.0, 1.0);
        for c in 0..3 {
            let v = if a > 0.0004 { (ch[c][i] / a).clamp(0.0, 1.0) } else { 0.0 };
            buf[i * 4 + c] = (v * 255.0).round() as u8;
        }
        buf[i * 4 + 3] = (a * 255.0).round() as u8;
    }
}

/// One horizontal box-blur pass (radius `r`, edge-clamped), `src` → `src` via
/// scratch `tmp`, using a running sum for O(n).
fn box_blur_h(src: &mut [f32], tmp: &mut [f32], w: usize, r: usize) {
    let win = (2 * r + 1) as f32;
    for y in 0..w {
        let row = y * w;
        let mut sum = 0.0;
        for k in 0..=r {
            sum += src[row + k.min(w - 1)];
        }
        sum += src[row] * r as f32; // left edge clamp
        for x in 0..w {
            tmp[row + x] = sum / win;
            let add = (x + r + 1).min(w - 1);
            let sub = if x >= r { x - r } else { 0 };
            sum += src[row + add] - src[row + sub];
        }
    }
    src.copy_from_slice(tmp);
}

/// One vertical box-blur pass (radius `r`, edge-clamped).
fn box_blur_v(src: &mut [f32], tmp: &mut [f32], w: usize, r: usize) {
    let win = (2 * r + 1) as f32;
    for x in 0..w {
        let mut sum = 0.0;
        for k in 0..=r {
            sum += src[k.min(w - 1) * w + x];
        }
        sum += src[x] * r as f32;
        for y in 0..w {
            tmp[y * w + x] = sum / win;
            let add = (y + r + 1).min(w - 1);
            let sub = if y >= r { y - r } else { 0 };
            sum += src[add * w + x] - src[sub * w + x];
        }
    }
    src.copy_from_slice(tmp);
}

/// One layer in render order: its rasterizable source, glass flag, and the
/// affine placement (scale + translation in 1024-canvas pixels) resolved from
/// the group and layer `position`.
struct StackLayer {
    source: PathBuf,
    /// Frosted glass (glass + translucency enabled): drawn as a faint
    /// see-through relief rather than in its own colour.
    frosted: bool,
    /// Tinted frosted glass: the group's shadow is `layer-color`, so the glass
    /// keeps its own colour (multiplies the background) instead of collapsing to
    /// a neutral relief. Gated on the shadow kind — verified on a synthetic
    /// two-group fixture: flipping the shadow to `none`/`neutral` strips the
    /// colour to grey, and overlapping tinted groups stack their multiplies.
    tinted: bool,
    /// Opaque glass with a specular sheen: keeps its colour but gets a raised
    /// rim highlight (KYA's coffee cup). `glass` + specular, translucency off.
    specular: bool,
    /// Per-layer drop-shadow peak (0 = none) from the group's `shadow` kind ×
    /// opacity: the layer casts a soft offset-down shadow on the background.
    shadow_strength: f32,
    scale: f32,
    tx: f32,
    ty: f32,
    opacity: f32,
    blend: BlendMode,
    /// Native point size of the source (SVG viewBox / image dimensions), so a
    /// non-1024 / non-square layer keeps its aspect and isn't stretched.
    native_w: u32,
    native_h: u32,
}

/// Resolve each visible layer's source path, glass flag, placement, opacity and
/// blend mode for `appearance`, in document order, for the layer-stack
/// compositor.
fn collect_stack_layers(
    bundle: &Path,
    parsed: &IconJson,
    appearance: crate::icon_effects::Appearance,
    base_scale: f32,
) -> Vec<StackLayer> {
    use crate::icon_effects::resolve_icon_effects;
    let mut out = Vec::new();
    for group in &parsed.groups {
        let eff = resolve_icon_effects(group, appearance);
        // A "glass context" — the group has blur enabled or any
        // sibling layer is glass — makes every layer render as glass unless it
        // explicitly opts out (`glass: false`). scrumdinger's middle layer
        // omits `glass` yet Apple still frosts it.
        // Translucency alone does not: Chimera/srb2-legacy enable it with no
        // glass layer and Apple renders their art flat.
        let glass_context = eff.blur_material.is_some() || eff.layers.iter().any(|l| l.glass);
        let (gscale, gtx, gty) = group
            .position
            .as_ref()
            .map(|p| {
                let t = p.translation_in_points.unwrap_or([0.0, 0.0]);
                (p.scale.unwrap_or(1.0), t[0], t[1])
            })
            .unwrap_or((1.0, 0.0, 0.0));
        for (i, layer) in group.layers.iter().enumerate() {
            if layer.hidden == Some(true) {
                continue;
            }
            let Some(name) = layer.image_name.as_deref() else { continue };
            let assets = bundle.join("Assets").join(name);
            let path = if assets.exists() { assets } else { bundle.join(name) };
            if !path.exists() {
                continue;
            }
            let explicit = eff.layers.get(i).map(|l| l.glass).unwrap_or(false);
            let opted_out =
                layer.glass == Some(false) && layer.glass_specializations.is_none();
            let (lscale, ltx, lty) = layer
                .position
                .as_ref()
                .map(|p| {
                    let t = p.translation_in_points.unwrap_or([0.0, 0.0]);
                    (p.scale.unwrap_or(1.0), t[0], t[1])
                })
                .unwrap_or((1.0, 0.0, 0.0));
            let (opacity, blend) = eff
                .layers
                .get(i)
                .map(|l| (l.opacity, parse_blend(&l.blend_mode)))
                .unwrap_or((1.0, BlendMode::Normal));
            // Translucency decides the glass mode: enabled → frosted relief
            // (see-through), disabled → opaque, with a specular sheen when the
            // group's specular is on (KYA's cup).
            let is_glass = explicit || (glass_context && !opted_out);
            // Apple keeps colour bitmap glass layers crisp even with translucency
            // on (srb2: `glass: true`, translucency 0.3 → unblurred art in both
            // CAR and ICNS); the relief look was only measured on vector glyphs.
            let frosted = is_glass && eff.translucency.enabled && is_svg_path(&path);
            let tinted = frosted
                && eff.shadow.kind == crate::icon_effects::ShadowKind::LayerColor;
            let specular = is_glass && !eff.translucency.enabled && eff.specular;
            // A glass layer casts a per-layer drop shadow on the background when
            // its group requests one; the peak scales with the shadow opacity.
            use crate::icon_effects::ShadowKind;
            let shadow_strength = if is_glass {
                match eff.shadow.kind {
                    ShadowKind::LayerColor => SHADOW_PEAK_LAYERCOLOR * eff.shadow.opacity,
                    ShadowKind::Neutral => SHADOW_PEAK_NEUTRAL * eff.shadow.opacity,
                    ShadowKind::None => 0.0,
                }
            } else {
                0.0
            };
            let (native_w, native_h) = layer_native_size(&path);
            // Compose group∘layer, then map to canvas pixels by the base scale.
            out.push(StackLayer {
                source: path,
                frosted,
                tinted,
                specular,
                shadow_strength,
                scale: base_scale * gscale * lscale,
                tx: base_scale * (gscale * ltx + gtx),
                ty: base_scale * (gscale * lty + gty),
                opacity,
                blend,
                native_w,
                native_h,
            });
        }
    }
    // icon.json lists groups/layers front-to-back (index 0 is the topmost);
    // painter's order needs them back-to-front, so reverse.
    out.reverse();
    out
}

/// Rasterize a layer source (SVG or raster) to `pixel_size`², straight RGBA.
/// Whether a layer/source path is an SVG (case-insensitive extension).
fn is_svg_path(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().ends_with(".svg")
}

fn rasterize_layer(path: &Path, w: u32, h: u32) -> Result<Vec<u8>> {
    if is_svg_path(path) {
        // CoreSVG renders premultiplied-first BGRA; unpremultiply to straight RGBA.
        let svg = fs::read(path)?;
        let bgra = crate::svg_raster::rasterize_svg(&svg, w, h, 1)?;
        let mut rgba = vec![0u8; bgra.len()];
        for (o, px) in bgra.chunks_exact(4).enumerate() {
            let a = px[3];
            let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
            rgba[o * 4] = un(px[2]);
            rgba[o * 4 + 1] = un(px[1]);
            rgba[o * 4 + 2] = un(px[0]);
            rgba[o * 4 + 3] = a;
        }
        Ok(rgba)
    } else {
        let mut img = image::open(path)?.to_rgba8();
        // Resample in premultiplied space: the RGB of transparent pixels is
        // arbitrary (often white) and would bleed into the antialiased edge.
        for p in img.pixels_mut() {
            let a = p[3] as u32;
            for c in 0..3 {
                p[c] = ((p[c] as u32 * a + 127) / 255) as u8;
            }
        }
        let mut resized = image::imageops::resize(&img, w, h, FilterType::Lanczos3);
        for p in resized.pixels_mut() {
            let a = p[3] as u32;
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                p[c] = ((p[c] as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
        Ok(resized.into_raw())
    }
}

/// The layer source's native point size (SVG viewBox / raster dimensions),
/// used to render it at the right aspect inside the 1024-pt canvas.
/// Per-group data for the iconstack/IconGroup renditions, resolved once per
/// appearance family (dark vs everything else).
struct GroupInfo {
    ident: u16,
    light_refs: Vec<car::LayerRef>,
    dark_refs: Vec<car::LayerRef>,
    light_style: car::GroupStyle,
    dark_style: car::GroupStyle,
}

/// A layer's placement on the 1024pt canvas as Apple records it in the
/// IconGroup: native size scaled by group × layer scale, centred and shifted by
/// the translations. Offsets floor and sizes round (srb2: 746×734 at 1.4 →
/// x −11, y −2, 1044×1028).
fn layer_canvas_frame(
    group: &crate::icon_json::Group,
    layer: &crate::icon_json::Layer,
    source: &Path,
) -> (i32, i32, u32, u32) {
    let placement = |p: Option<&crate::icon_json::Position>| {
        p.map(|p| {
            let t = p.translation_in_points.unwrap_or([0.0, 0.0]);
            (p.scale.unwrap_or(1.0), t[0], t[1])
        })
        .unwrap_or((1.0, 0.0, 0.0))
    };
    let (gs, gtx, gty) = placement(group.position.as_ref());
    let (ls, ltx, lty) = placement(layer.position.as_ref());
    let (nw, nh) = layer_native_size(source);
    let (w, h) = (nw as f32 * gs * ls, nh as f32 * gs * ls);
    let x = 512.0 + gs * ltx + gtx - w / 2.0;
    let y = 512.0 + gs * lty + gty - h / 2.0;
    (x.floor() as i32, y.floor() as i32, w.round() as u32, h.round() as u32)
}

fn layer_native_size(path: &Path) -> (u32, u32) {
    if is_svg_path(path) {
        if let Ok(svg) = fs::read(path) {
            let (w, h) = crate::svg_raster::parse_svg_dimensions(&svg);
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
        (1024, 1024)
    } else {
        image::image_dimensions(path).unwrap_or((1024, 1024))
    }
}

/// Separable layer blend modes (icon.json `blend-mode-specializations`). The
/// channel functions are the W3C/PDF separable blends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Darken,
    Lighten,
}

fn parse_blend(s: &str) -> BlendMode {
    match s {
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "soft-light" => BlendMode::SoftLight,
        "hard-light" => BlendMode::HardLight,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        _ => BlendMode::Normal,
    }
}

/// Blend one channel (backdrop `cb`, source `cs`, both 0..1).
fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    let hard = |cb: f32, cs: f32| {
        if cs <= 0.5 {
            2.0 * cb * cs
        } else {
            1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
        }
    };
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard(cs, cb),
        BlendMode::HardLight => hard(cb, cs),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
    }
}

/// Straight-alpha source-over of `src` onto `dst` with a separable blend mode
/// (W3C compositing). `Normal` reduces to plain "over".
fn composite_blend(dst: &mut [u8], src: &[u8], mode: BlendMode) {
    let sa = src[3] as f32 / 255.0;
    if sa <= 0.0 {
        return;
    }
    let da = dst[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return;
    }
    for c in 0..3 {
        let cs = src[c] as f32 / 255.0;
        let cb = dst[c] as f32 / 255.0;
        let bl = blend_channel(mode, cb, cs);
        // Co = (1-αb)·αs·Cs + αb·αs·B(Cb,Cs) + (1-αs)·αb·Cb, normalised by αo.
        let co = (1.0 - da) * sa * cs + da * sa * bl + (1.0 - sa) * da * cb;
        dst[c] = (co / oa * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Composite a group's layers into a single premultiplied-first BGRA buffer at
/// `pixel_size`². Glass layers are not drawn in their own colour — they are
/// merged into one coverage mask and rendered as Apple's frosted-glass relief:
/// a near-black overlay at low opacity that darkens toward the bottom (the
/// concave-sphere shading measured from scrumdinger, lum ≈ 232 at the centre,
/// 225 lower, vs a white background). Non-glass layers composite normally.
fn render_layer_stack(
    layers: &[StackLayer],
    pixel_size: u32,
    gradient: Option<&crate::icon_render::GradientFill>,
) -> Result<Vec<u8>> {
    let w = pixel_size as usize;
    let n = w * w;
    let mut rgba = vec![0u8; n * 4];
    let mut glass_cov = vec![0u8; n];
    // Per-pixel subtractive tint a *tinted* frosted layer (group shadow
    // `layer-color`) applies to the background: each adds `cov·D·(1 − colour)`
    // per channel, so a saturated slab darkens the channels where it's dark and
    // overlapping tinted layers stack additively (the purple overlap Apple
    // emits where blue meets red). `glass_tinted` marks pixels under any tinted
    // layer; a plain frosted layer (shadow none/neutral) instead contributes the
    // neutral relief darkening.
    let mut glass_sub = vec![[0.0f32; 3]; n];
    let mut glass_tinted = vec![false; n];
    let mut any_glass = false;
    // Per-layer drop shadow: each shadow-casting layer adds a subtractive,
    // `(1 − colour)`-tinted darkening at its coverage; the whole buffer is then
    // offset down + blurred and laid on the background under the layers.
    let mut shadow_dark = vec![[0.0f32; 3]; n];
    let mut any_shadow = false;
    let f = pixel_size as f32 / 1024.0;
    // Rasterizing a layer source decodes the file and Lanczos-resizes it — the
    // dominant per-layer cost. Multi-group icons flatten every group's layers
    // into one stack, so the same source is requested at the same size once per
    // group (50 groups × every size = hundreds of identical decodes). Cache by
    // (source, w, h) so each is rasterized once per render.
    let mut raster_cache: std::collections::HashMap<(PathBuf, u32, u32), Vec<u8>> =
        std::collections::HashMap::new();
    for layer in layers {
        // Render at the layer's native aspect, scaled by base·group·layer (so a
        // non-1024 / non-square SVG keeps its proportions), then blit centred +
        // translated per its resolved placement.
        let k = layer.scale * f;
        let rw = ((layer.native_w as f32 * k).round() as u32).max(1);
        let rh = ((layer.native_h as f32 * k).round() as u32).max(1);
        use std::collections::hash_map::Entry;
        let src = match raster_cache.entry((layer.source.clone(), rw, rh)) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(rasterize_layer(&layer.source, rw, rh)?),
        };
        let (rw, rh) = (rw as i64, rh as i64);
        let half = pixel_size as f32 / 2.0;
        let ox = (half + layer.tx * f - rw as f32 / 2.0).round() as i64;
        let oy = (half + layer.ty * f - rh as f32 / 2.0).round() as i64;
        for ly in 0..rh {
            let cy = oy + ly;
            if cy < 0 || cy >= w as i64 {
                continue;
            }
            for lx in 0..rw {
                let cx = ox + lx;
                if cx < 0 || cx >= w as i64 {
                    continue;
                }
                let si = ((ly * rw + lx) * 4) as usize;
                // Layer opacity scales the source alpha.
                let sa = (src[si + 3] as f32 * layer.opacity).round() as u8;
                if sa == 0 {
                    continue;
                }
                let ci = (cy * w as i64 + cx) as usize;
                if layer.shadow_strength > 0.0 {
                    any_shadow = true;
                    let cov = sa as f32 / 255.0;
                    for c in 0..3 {
                        let col = src[si + c] as f32 / 255.0;
                        shadow_dark[ci][c] += layer.shadow_strength * cov * (1.0 - col);
                    }
                }
                if layer.frosted {
                    any_glass = true;
                    glass_cov[ci] = glass_cov[ci].max(sa);
                    if layer.tinted {
                        // Coverage-weighted subtractive tint: add D·(1−colour)
                        // per channel, accumulating across overlapping layers.
                        let cov = sa as f32 / 255.0;
                        glass_tinted[ci] = true;
                        for c in 0..3 {
                            let col = src[si + c] as f32 / 255.0;
                            glass_sub[ci][c] += cov * GLASS_TINT_D * (1.0 - col);
                        }
                    }
                } else {
                    let mut dst =
                        [rgba[ci * 4], rgba[ci * 4 + 1], rgba[ci * 4 + 2], rgba[ci * 4 + 3]];
                    // Opaque glass lifts the layer's blacks toward a grey floor
                    // (≈45/255 on KYA's cup) — screen each channel with it.
                    let s = if layer.specular {
                        let lift = |c: u8| {
                            let c = c as f32 / 255.0;
                            ((c + GLASS_FLOOR - c * GLASS_FLOOR) * 255.0).round() as u8
                        };
                        [lift(src[si]), lift(src[si + 1]), lift(src[si + 2]), sa]
                    } else {
                        [src[si], src[si + 1], src[si + 2], sa]
                    };
                    composite_blend(&mut dst, &s, layer.blend);
                    rgba[ci * 4..ci * 4 + 4].copy_from_slice(&dst);
                }
            }
        }
    }
    if any_glass {
        // Build the glass contribution as its own straight-RGBA buffer, then
        // blur its edges (the soft "raised glass" look) before compositing it
        // over the layers.
        let mut glass_buf = vec![0u8; n * 4];
        let strength = GLASS_RELIEF_STRENGTH;
        for y in 0..w {
            for x in 0..w {
                let i = y * w + x;
                let cov = glass_cov[i] as f32 / 255.0;
                if cov <= 0.0 {
                    continue;
                }
                let a = (cov * 255.0).round() as u8;
                let glass_px = match gradient {
                    // We bake the resolved glass colour as an opaque pixel drawn
                    // over the same gradient the compositor uses, so it replaces
                    // the gradient with the glass result. A `layer-color`-tinted
                    // pixel subtracts the accumulated `D·(1−colour)` from the
                    // background (uniform, no vertical relief); a plain frosted
                    // pixel keeps the faint vertical relief darkening.
                    Some(g) => {
                        let bg = g.sample(x as u32, y as u32, pixel_size);
                        let mut out = [0u8; 4];
                        for c in 0..3 {
                            let v = if glass_tinted[i] {
                                bg[c] as f32 - glass_sub[i][c]
                            } else {
                                bg[c] as f32 * (1.0 - strength)
                            };
                            out[c] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                        }
                        out[3] = a;
                        out
                    }
                    // No gradient (raw-layer fallback): keep the neutral relief.
                    None => [0, 0, 0, (cov * strength * 255.0).round() as u8],
                };
                glass_buf[i * 4..i * 4 + 4].copy_from_slice(&glass_px);
            }
        }
        // Soft glass edge: feather by σ ≈ 19 px at 1024, scaled to this size.
        let sigma = GLASS_BLUR_SIGMA * pixel_size as f32 / 1024.0;
        blur_rgba_premul(&mut glass_buf, w, sigma);
        for i in 0..n {
            if glass_buf[i * 4 + 3] == 0 {
                continue;
            }
            let g = [
                glass_buf[i * 4],
                glass_buf[i * 4 + 1],
                glass_buf[i * 4 + 2],
                glass_buf[i * 4 + 3],
            ];
            let mut dst = [rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2], rgba[i * 4 + 3]];
            composite_blend(&mut dst, &g, BlendMode::Normal);
            rgba[i * 4..i * 4 + 4].copy_from_slice(&dst);
        }
    }
    // Per-layer drop shadow: offset the accumulated darkening down, blur it, and
    // subtract it from the background — but only on background pixels (where no
    // layer/glass is already opaque), so the casting layer stays on top.
    if any_shadow {
        if let Some(g) = gradient {
            let off = (SHADOW_OFFSET_Y * f).round() as usize;
            let mut chans: [Vec<f32>; 3] =
                [vec![0.0; n], vec![0.0; n], vec![0.0; n]];
            for y in 0..w {
                let sy = y.saturating_sub(off); // shift darkening downward
                for x in 0..w {
                    for c in 0..3 {
                        chans[c][y * w + x] = shadow_dark[sy * w + x][c];
                    }
                }
            }
            let radius = (SHADOW_SIGMA * f).round() as usize;
            if radius >= 1 {
                let mut tmp = vec![0.0f32; n];
                for c in &mut chans {
                    for _ in 0..3 {
                        box_blur_h(c, &mut tmp, w, radius);
                        box_blur_v(c, &mut tmp, w, radius);
                    }
                }
            }
            for y in 0..w {
                for x in 0..w {
                    let i = y * w + x;
                    if rgba[i * 4 + 3] >= 16 {
                        continue; // a layer/glass already covers this pixel
                    }
                    let dark = [chans[0][i], chans[1][i], chans[2][i]];
                    if dark[0] + dark[1] + dark[2] < 0.004 {
                        continue;
                    }
                    let bg = g.sample(x as u32, y as u32, pixel_size);
                    let mut out = [0u8; 4];
                    for c in 0..3 {
                        let v = (bg[c] as f32 - dark[c]).clamp(0.0, 1.0);
                        out[c] = (v * 255.0).round() as u8;
                    }
                    out[3] = 255;
                    rgba[i * 4..i * 4 + 4].copy_from_slice(&out);
                }
            }
        }
    }
    // Straight RGBA → premultiplied-first BGRA.
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let (r, g, b, a) = (rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2], rgba[i * 4 + 3]);
        let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        out[i * 4] = pm(b);
        out[i * 4 + 1] = pm(g);
        out[i * 4 + 2] = pm(r);
        out[i * 4 + 3] = a;
    }
    Ok(out)
}

fn find_all_source_images(bundle: &Path, json: &serde_json::Value) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(groups) = json.get("groups").and_then(|v| v.as_array()) {
        for group in groups {
            if let Some(layers) = group.get("layers").and_then(|v| v.as_array()) {
                for layer in layers {
                    let Some(name) = layer.get("image-name").and_then(|v| v.as_str())
                    else {
                        continue;
                    };
                    if !seen.insert(name.to_string()) {
                        continue;
                    }
                    let assets_path = bundle.join("Assets").join(name);
                    if assets_path.exists() {
                        out.push(assets_path);
                        continue;
                    }
                    let root_path = bundle.join(name);
                    if root_path.exists() {
                        out.push(root_path);
                    }
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_icon_car(
    car_path: &Path,
    icon_name: &str,
    icon_images: &[(PathBuf, u32, u32)],
    layer_assets: &[LayerAsset],
    group_facet_names: &[String],
    color_assets: &[ColorAsset],
    gradient_assets: &[GradientAsset],
    groups: &[crate::icon_json::Group],
    light_stacks: &[Vec<u8>],
    dark_stacks: &[Vec<u8>],
    light_fill: Option<&crate::icon_render::GradientFill>,
    dark_fill: Option<&crate::icon_render::GradientFill>,
    emit_variant_axis: bool,
    platform: &str,
    min_deploy: &str,
    coreui_version: u32,
    ios_images: Option<&[IosComposite]>,
) -> Result<()> {
    let ident = hash_name(icon_name);
    // The actual keyformat is computed from the renditions below — only the
    // attributes they exercise survive. .icon catalogs typically end up with
    // [7, 13, 1, 2, 3, 17, 9, 11, 12] (no direction, no dim1, no variant).
    let placeholder_kf: Vec<u16> = Vec::new();
    let mut renditions: Vec<Rendition> = Vec::new();

    // Each variant (0 = primary, 1 = alternate) gets its own packed_imgs
    // list so the packer routes them into separate atlases (the alternate
    // atlas gets gamut=1 in its name and attribute 24 = 1 in its key).
    let variants: &[u16] = if emit_variant_axis { &[0u16, 1] } else { &[0u16] };
    let mut packed_imgs_per_variant: Vec<Vec<crate::packer::PackedImage>> =
        variants.iter().map(|_| Vec::new()).collect();

    // The sized rendition is the layer composited over the icon's background
    // (resolved by the caller: the light gradient/solid for the primary variant,
    // the dark gradient for the alternate; verified by decoding Apple's GA8/GA16
    // renditions with libdm2's KCBC path). With no fill we fall back to the raw
    // layer.

    // The icon tile ALWAYS casts a constant margin drop shadow, independent of
    // the group `shadow` kind: Apple casts the same halo for `shadow: none`
    // (element-web) and an absent shadow (feishin) as for `layer-color` (KYA) —
    // all measure BOT α ≈37. (The group shadow drives the *per-layer* shadow
    // instead, in `render_layer_stack`.) A fixed neutral spec at opacity 0.5
    // reproduces Apple's halo (`shadow_geometry` tuned to it).
    use crate::icon_effects::{ShadowKind, ShadowSpec};
    let icon_shadow = Some(ShadowSpec { kind: ShadowKind::Neutral, opacity: 0.5 });
    let light_shadow = icon_shadow;
    let dark_shadow = icon_shadow;

    if let Some(images) = ios_images {
        // iOS stores one full-bleed 1024pt composite per idiom and appearance,
        // plus a single-entry multisize rendition per idiom (size index 1).
        for idiom in [1u16, 2] {
            for img in images {
                renditions.push(Rendition {
                    name: pre_rendered_name(icon_name, IOS_ICON_SIZE, 1, img.appearance_name),
                    identifier: ident,
                    element: car::ELEMENT_UNIVERSAL,
                    part: car::PART_ICON,
                    scale: 1,
                    width: IOS_ICON_SIZE,
                    height: IOS_ICON_SIZE,
                    pixel_data: img.bgra.clone(),
                    pixel_format: *b"BGRA",
                    layout: car::LAYOUT_ONE_PART_SCALE,
                    appearance: img.appearance,
                    idiom,
                    dim2: car::IOS_ICON_DIM2_SINGLE_SIZE_1024,
                    keyformat: placeholder_kf.clone(),
                    min_deploy: min_deploy.to_string(),
                    platform: platform.to_string(),
                    colorspace_id: car::colorspace_for_pixel_format(b"BGRA"),
                    template_rendering_intent: 0,
                    ..Rendition::default()
                });
            }
            let mut ms_rend = car::build_multisize_rendition(
                icon_name,
                ident,
                &[MultisizeImageEntry {
                    width: IOS_ICON_SIZE,
                    height: IOS_ICON_SIZE,
                    index: car::IOS_ICON_DIM2_SINGLE_SIZE_1024 as u32,
                }],
            );
            ms_rend.idiom = idiom;
            ms_rend.keyformat = placeholder_kf.clone();
            renditions.push(ms_rend);
        }
    } else {
        // Split images into atlas candidates (small sizes) and inline (large).
        // For each size load BGRA pixels, then dispatch by point size. When
        // `emit_variant_axis` is set, every sized rendition is duplicated for
        // the alternate variant (same pixels — the variant axis is structural;
        // CUICatalog reads it to pick which alternate to display per-appearance).
        for (idx, (img_path, pixel_size, scale)) in icon_images.iter().enumerate() {
            let point_size = pixel_size / scale;
            let dim2 = icon_dim2(point_size);
            for &variant in variants {
                // Use the pre-rendered multi-layer stack for this size and variant
                // (the alternate variant has its own dark-appearance stack); fall
                // back to the primary layer if absent.
                let stacks = if variant == 1 && !dark_stacks.is_empty() {
                    dark_stacks
                } else {
                    light_stacks
                };
                let (layer_bgra, w, h) = match stacks.get(idx) {
                    Some(stack) if stack.len() == (pixel_size * pixel_size * 4) as usize => {
                        (stack.clone(), *pixel_size, *pixel_size)
                    }
                    _ => {
                        let (b, w, h, _pf) = load_image_as_bgra(img_path, true)?;
                        (b, w, h)
                    }
                };
                // Composite the layer over the variant's background gradient,
                // clipped to the squircle. The alternate variant uses the dark
                // gradient when present.
                let fill = if variant == 1 {
                    dark_fill.or(light_fill)
                } else {
                    light_fill
                };
                let shadow = shadow_params(
                    if variant == 1 { dark_shadow } else { light_shadow },
                    *pixel_size,
                );
                let composited = match fill {
                    Some(f) => crate::icon_render::composite_icon(
                        *pixel_size,
                        f,
                        &layer_bgra,
                        shadow.as_ref(),
                    )
                    .unwrap_or_else(|| layer_bgra.clone()),
                    None => layer_bgra.clone(),
                };
                // With the variant axis the composite is stored as a grayscale
                // image (variant 0 → GA8 cspace 2, variant 1 → GA16 cspace 6);
                // otherwise it stays BGRA.
                let (pd, pf, cs_id): (Vec<u8>, [u8; 4], u32) = if emit_variant_axis {
                    if variant == 0 {
                        (crate::catalog::bgra_to_ga8_force(&composited), *b" 8AG", 2)
                    } else {
                        // The dark (alternate) variant is NOT a baked dark composite
                        // — Apple stores a flat near-white squircle tint that
                        // CUICatalog composites over the light icon. Build that tint
                        // (premult gray 59 / α 60 over the squircle, plus the icon's
                        // black drop shadow) instead of the dark composite.
                        let dark = fill
                            .and_then(|f| {
                                build_dark_variant_tint(*pixel_size, f, shadow.as_ref())
                            })
                            .unwrap_or_else(|| composited.clone());
                        (crate::catalog::bgra_to_ga16_force(&dark), *b"61AG", 6)
                    }
                } else {
                    (composited, *b"BGRA", car::colorspace_for_pixel_format(b"BGRA"))
                };
                let name = pre_rendered_name(
                    icon_name,
                    point_size,
                    *scale,
                    "NSAppearanceNameSystem",
                );
                if ATLAS_POINT_SIZES.contains(&point_size) {
                    let mut pi = crate::packer::PackedImage::new(
                        name.clone(),
                        ident as u32,
                        w,
                        h,
                    );
                    pi.pixel_data = pd;
                    pi.pixel_format = pf;
                    pi.scale = *scale;
                    pi.part = car::PART_ICON as u32;
                    pi.dim2 = dim2 as u32;
                    pi.variant = variant as u32;
                    packed_imgs_per_variant[variant as usize].push(pi);
                } else {
                    renditions.push(Rendition {
                        name,
                        identifier: ident,
                        element: car::ELEMENT_UNIVERSAL,
                        part: car::PART_ICON,
                        scale: *scale as u16,
                        width: w,
                        height: h,
                        pixel_data: pd,
                        pixel_format: pf,
                        layout: car::LAYOUT_ONE_PART_SCALE,
                        dim2,
                        variant,
                        keyformat: placeholder_kf.clone(),
                        min_deploy: min_deploy.to_string(),
                        platform: platform.to_string(),
                        colorspace_id: cs_id,
                        // Apple uses bitmapEncoding=0 (original) for the pre-rendered
                        // sized PNGs in .icon catalogs; the default (-1 → auto/4)
                        // sets the rendition_flags bit that makes CUICatalog look
                        // for a template variant that doesn't exist.
                        template_rendering_intent: 0,
                        ..Rendition::default()
                    });
                }
            }
        }

        // Pack 16/32/64 into ZZZZPackedAsset atlases — one atlas per variant
        // when the variant axis is active. Each atlas's gamut field controls
        // its name suffix and routes through to the rendition's attribute 24.
        for (variant_idx, packed_imgs) in packed_imgs_per_variant.into_iter().enumerate() {
            if packed_imgs.is_empty() {
                continue;
            }
            let pf = packed_imgs[0].pixel_format;
            let variant = variant_idx as u16;
            let mut atlases = crate::packer::pack_images_split(packed_imgs, 262, 196);
            for atlas in &mut atlases {
                atlas.gamut = variant_idx as u32;
                atlas.render();
                let atlas_name = atlas.name();
                // Apple uses LZFSE (not deepmap2) for icon atlases regardless of
                // pixel format — BGRA, GA8 (variant 0), and GA16 (variant 1) all
                // go through LZFSE.
                let force_lzfse = matches!(&pf, b"BGRA" | b" 8AG" | b"61AG");
                let atlas_csi = car::build_packed_asset_csi(
                    &atlas_name,
                    atlas.width,
                    atlas.height,
                    2,
                    &pf,
                    &atlas.pixel_data,
                    min_deploy,
                    platform,
                    force_lzfse,
                );
                renditions.push(Rendition {
                    name: atlas_name.clone(),
                    identifier: 0,
                    element: car::ELEMENT_PACKED,
                    part: car::PART_REGULAR,
                    scale: 2,
                    width: atlas.width,
                    height: atlas.height,
                    pixel_data: Vec::new(),
                    pixel_format: pf,
                    layout: car::LAYOUT_NAME_LIST,
                    variant,
                    keyformat: placeholder_kf.clone(),
                    min_deploy: min_deploy.to_string(),
                    platform: platform.to_string(),
                    colorspace_id: car::colorspace_for_pixel_format(&pf),
                    csi_override: Some(atlas_csi),
                    ..Rendition::default()
                });

                for img in &atlas.images {
                    let inlk_y = atlas.height - img.y - img.height;
                    let ref_csi = car::build_packed_image_csi(
                        &img.name,
                        img.width,
                        img.height,
                        img.scale as u16,
                        &pf,
                        img.x,
                        inlk_y,
                        0,
                        0,
                        0,
                        0,
                    );
                    renditions.push(Rendition {
                        name: img.name.clone(),
                        identifier: ident,
                        element: car::ELEMENT_UNIVERSAL,
                        part: car::PART_ICON,
                        scale: img.scale as u16,
                        dim2: img.dim2 as u16,
                        width: img.width,
                        height: img.height,
                        pixel_data: Vec::new(),
                        pixel_format: pf,
                        layout: car::LAYOUT_PACKED_IMAGE,
                        variant,
                        keyformat: placeholder_kf.clone(),
                        min_deploy: min_deploy.to_string(),
                        platform: platform.to_string(),
                        colorspace_id: car::colorspace_for_pixel_format(&pf),
                        csi_override: Some(ref_csi),
                        ..Rendition::default()
                    });
                }
            }
        }

        let mut ms_entries: Vec<MultisizeImageEntry> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (_, pixel_size, scale) in icon_images {
            let pt = pixel_size / scale;
            if seen.insert(pt) {
                ms_entries.push(MultisizeImageEntry {
                    width: pt,
                    height: pt,
                    index: icon_dim2(pt) as u32,
                });
            }
        }
        let mut ms_rend = car::build_multisize_rendition(icon_name, ident, &ms_entries);
        ms_rend.keyformat = placeholder_kf.clone();
        renditions.push(ms_rend);
    }

    // Apple's FACETKEYS entry for the main icon facet carries part = PART_ICON
    // (220), the part of its pre-rendered sized renditions. Apple itself flips
    // between 220 and PART_ICON_COMPOSER (245, the iconstack's part) from run
    // to run on identical input, so there is nothing to match.
    let mut facets: Vec<FacetEntry> = vec![(
        icon_name.to_string(),
        car::ELEMENT_UNIVERSAL,
        Some(car::PART_ICON),
        ident,
    )];
    // Group facets — `<stem>/<group_name>` — appear in Apple's FACETKEYS
    // alongside the main icon. We don't emit IconGroup renditions yet, but
    // registering the facet lets CoreUI enumerate them.
    for gname in group_facet_names {
        let gid = hash_name(gname);
        facets.push((
            gname.clone(),
            car::ELEMENT_UNIVERSAL,
            Some(car::PART_ICON_GROUP),
            gid,
        ));
    }
    for asset in layer_assets {
        let asset_ident = hash_name(&asset.facet_name);
        let is_svg = asset
            .source_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("svg"))
            .unwrap_or(false);
        if is_svg {
            // An SVG-sourced layer is stored as a Vector rendition holding the
            // raw SVG (named "image.svg"), not a rasterized bitmap. Apple keeps
            // the vector so the layer can be re-rendered at any scale.
            let svg_data = fs::read(&asset.source_path)?;
            let csi = car::build_svg_csi("image.svg", &svg_data);
            renditions.push(Rendition {
                name: "image.svg".to_string(),
                identifier: asset_ident,
                element: car::ELEMENT_UNIVERSAL,
                part: car::PART_REGULAR,
                scale: 1,
                width: 0,
                height: 0,
                pixel_data: Vec::new(),
                pixel_format: *car::PIXELFMT_SVG,
                layout: car::LAYOUT_PDF,
                keyformat: placeholder_kf.clone(),
                min_deploy: min_deploy.to_string(),
                platform: platform.to_string(),
                colorspace_id: 0,
                csi_override: Some(csi),
                ..Rendition::default()
            });
            facets.push((
                asset.facet_name.clone(),
                car::ELEMENT_UNIVERSAL,
                Some(car::PART_REGULAR),
                asset_ident,
            ));
            continue;
        }
        let (pd, w, h, pf) = load_image_as_bgra(&asset.source_path, false)?;
        // Apple's actool normalizes the in-CAR rendition name for a layer's
        // source image to "image.png" rather than the original filename.
        // CoreUI keys renditions by attribute tuple, not by name, so this is
        // metadata-only — but it matches Apple's catalog byte-for-byte for
        // the single-layer case.
        renditions.push(Rendition {
            name: "image.png".to_string(),
            identifier: asset_ident,
            element: car::ELEMENT_UNIVERSAL,
            part: car::PART_REGULAR,
            scale: 1,
            width: w,
            height: h,
            pixel_data: pd,
            pixel_format: pf,
            layout: car::LAYOUT_ONE_PART_SCALE,
            keyformat: placeholder_kf.clone(),
            min_deploy: min_deploy.to_string(),
            platform: platform.to_string(),
            colorspace_id: car::colorspace_for_pixel_format(&pf),
            // Apple stores layer source images as non-opaque (CELM ver=0)
            // even when the source image has alpha=255 everywhere, so the
            // layer composites with alpha against other stack layers.
            force_non_opaque: true,
            ..Rendition::default()
        });
        facets.push((
            asset.facet_name.clone(),
            car::ELEMENT_UNIVERSAL,
            Some(car::PART_REGULAR),
            asset_ident,
        ));
    }

    for color in color_assets {
        let cident = hash_name(&color.facet_name);
        let csi = car::build_icon_color_csi(
            &color.facet_name,
            color.colorspace_id,
            &color.components,
        );
        // Apple's Color rendition KEY has scale=1 (even though the CSI's own
        // scale_factor field is 0). CUICatalog filters lookups by scale=1
        // by default, so scale=0 keys are invisible to colorWithName:.
        renditions.push(Rendition {
            name: color.facet_name.clone(),
            identifier: cident,
            element: car::ELEMENT_UNIVERSAL,
            part: car::PART_COLOR,
            scale: 1,
            width: 0,
            height: 0,
            pixel_data: Vec::new(),
            pixel_format: *b"\0\0\0\0",
            layout: car::LAYOUT_COLOR,
            keyformat: placeholder_kf.clone(),
            min_deploy: min_deploy.to_string(),
            platform: platform.to_string(),
            colorspace_id: 0,
            csi_override: Some(csi),
            ..Rendition::default()
        });
        facets.push((
            color.facet_name.clone(),
            car::ELEMENT_UNIVERSAL,
            Some(car::PART_COLOR),
            cident,
        ));
    }

    for grad in gradient_assets {
        let gident = hash_name(&grad.facet_name);
        let stops: Vec<(f32, &str)> = grad
            .stops
            .iter()
            .map(|(p, name)| (*p, name.as_str()))
            .collect();
        let csi = car::build_icon_gradient_csi(
            &grad.facet_name,
            grad.geometry,
            &stops,
            grad.kind,
        );
        // Gradient KEYs follow the same scale=1 convention as Colors.
        renditions.push(Rendition {
            name: grad.facet_name.clone(),
            identifier: gident,
            element: car::ELEMENT_UNIVERSAL,
            part: car::PART_ICON_GRADIENT,
            scale: 1,
            width: 0,
            height: 0,
            pixel_data: Vec::new(),
            pixel_format: *b"\0\0\0\0",
            layout: car::LAYOUT_GRADIENT,
            keyformat: placeholder_kf.clone(),
            min_deploy: min_deploy.to_string(),
            platform: platform.to_string(),
            colorspace_id: 0,
            csi_override: Some(csi),
            ..Rendition::default()
        });
        facets.push((
            grad.facet_name.clone(),
            car::ELEMENT_UNIVERSAL,
            Some(car::PART_ICON_GRADIENT),
            gident,
        ));
    }

    // Apple emits iconstack + IconGroup per non-system appearance for .icon
    // bundles. Only meaningful when we have a group + at least one image
    // layer + gradients to back the appearance variants.
    let layered = !group_facet_names.is_empty()
        && !layer_assets.is_empty()
        && !gradient_assets.is_empty();
    if layered {
        let main_ident = hash_name(icon_name);
        let stack_name = format!("{icon_name}.iconstack");
        // Each group becomes a PART_ICON_GROUP facet whose IconGroup rendition
        // references that group's own image layers (PART_REGULAR). Previously
        // only the first group was wired, so a second group's facet had a
        // FACETKEYS entry but no rendition — absent from BITMAPKEYS, it made
        // CUICatalog return "no images" (Rectangle's Overlay facet). Build the
        // per-group ident + layer refs so every group resolves.
        use crate::icon_effects::Appearance;
        let group_infos: Vec<GroupInfo> = group_facet_names
            .iter()
            .zip(groups.iter())
            .map(|(facet, g)| {
                let for_appearance = |appearance: Appearance| {
                    let eff = crate::icon_effects::resolve_icon_effects(g, appearance);
                    // Apple lists a group's layers back-to-front, the reverse of
                    // icon.json, each with its opacity resolved for the appearance.
                    let refs: Vec<car::LayerRef> = g
                        .layers
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| l.hidden != Some(true))
                        .filter_map(|(i, l)| {
                            let img = l.image_name.as_deref()?;
                            let stem = std::path::Path::new(img)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or(img);
                            let facet = format!("{icon_name}_Assets/{stem}");
                            Some(car::LayerRef {
                                part: car::PART_REGULAR,
                                identifier: hash_name(&facet),
                                group_style: None,
                                glass: !(l.glass == Some(false)
                                    && l.glass_specializations.is_none()),
                                frame: layer_assets
                                    .iter()
                                    .find(|a| a.facet_name == facet)
                                    .map(|a| layer_canvas_frame(g, l, &a.source_path)),
                                opacity: eff.layers.get(i).map(|e| e.opacity).unwrap_or(1.0),
                            })
                        })
                        .rev()
                        .collect();
                    let style = car::GroupStyle {
                        translucency_enabled: eff.translucency.enabled,
                        translucency: eff.translucency.value,
                        shadow_kind: match eff.shadow.kind {
                            crate::icon_effects::ShadowKind::Neutral => 3,
                            _ => 0,
                        },
                        shadow_opacity: eff.shadow.opacity,
                    };
                    (refs, style)
                };
                let (light_refs, light_style) = for_appearance(Appearance::Light);
                let (dark_refs, dark_style) = for_appearance(Appearance::Dark);
                GroupInfo {
                    ident: hash_name(facet),
                    light_refs,
                    dark_refs,
                    light_style,
                    dark_style,
                }
            })
            .filter(|g| !g.light_refs.is_empty())
            .collect();
        // Map appearance ID -> gradient facet name. 1=DarkAqua uses the
        // second (dark) gradient; 8=Aqua and 10=Tintable use the first.
        let grad1_ident = hash_name(&gradient_assets[0].facet_name);
        let grad2_ident = hash_name(
            &gradient_assets
                .get(1)
                .map(|g| g.facet_name.clone())
                .unwrap_or_else(|| gradient_assets[0].facet_name.clone()),
        );
        let appearances: [u16; 3] = if ios_images.is_some() { [1, 4, 10] } else { [1, 8, 10] };
        for appearance in appearances {
            let grad_id = if appearance == 1 { grad2_ident } else { grad1_ident };
            // Stack the gradient at the bottom, then the groups back-to-front.
            // icon.json lists groups front-to-back (index 0 topmost), so the
            // painter's order reverses them.
            let mut stack_layers = vec![car::LayerRef {
                part: car::PART_ICON_GRADIENT,
                identifier: grad_id,
                group_style: None,
                glass: false,
                frame: None,
                opacity: 1.0,
            }];
            for info in group_infos.iter().rev() {
                stack_layers.push(car::LayerRef {
                    part: car::PART_ICON_GROUP,
                    identifier: info.ident,
                    group_style: Some(if appearance == 1 { info.dark_style } else { info.light_style }),
                    glass: false,
                    frame: None,
                    opacity: 1.0,
                });
            }
            let stack_csi = car::build_iconstack_csi(&stack_name, 1024, &stack_layers);
            renditions.push(Rendition {
                name: stack_name.clone(),
                identifier: main_ident,
                element: car::ELEMENT_UNIVERSAL,
                part: car::PART_ICON_COMPOSER,
                scale: 1,
                appearance,
                width: 1024,
                height: 1024,
                pixel_data: Vec::new(),
                pixel_format: *car::PIXELFMT_DATA,
                layout: car::LAYOUT_ICONSTACK,
                keyformat: placeholder_kf.clone(),
                min_deploy: min_deploy.to_string(),
                platform: platform.to_string(),
                colorspace_id: 0,
                csi_override: Some(stack_csi),
                ..Rendition::default()
            });

            for info in &group_infos {
                let layer_refs = if appearance == 1 { &info.dark_refs } else { &info.light_refs };
                let group_ident = &info.ident;
                let group_csi = car::build_icongroup_csi("IconGroup", 1024, layer_refs);
                renditions.push(Rendition {
                    name: "IconGroup".to_string(),
                    identifier: *group_ident,
                    element: car::ELEMENT_UNIVERSAL,
                    part: car::PART_ICON_GROUP,
                    scale: 1,
                    appearance,
                    width: 0,
                    height: 0,
                    pixel_data: Vec::new(),
                    pixel_format: *car::PIXELFMT_DATA,
                    layout: car::LAYOUT_ICON_GROUP,
                    keyformat: placeholder_kf.clone(),
                    min_deploy: min_deploy.to_string(),
                    platform: platform.to_string(),
                    colorspace_id: 0,
                    csi_override: Some(group_csi),
                    ..Rendition::default()
                });
            }
        }
    }

    let keyformat = if ios_images.is_some() {
        car::compute_keyformat_ios(&renditions, false)
    } else {
        car::compute_keyformat(&renditions, false)
    };
    for rend in &mut renditions {
        rend.keyformat = keyformat.clone();
    }

    let mut all_entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for rend in &renditions {
        let key = rend.build_rendition_key();
        let csi = rend.build_csi();
        all_entries.push((key, csi));
    }
    all_entries.sort_by(|a, b| a.0.cmp(&b.0));

    write_icon_car(
        car_path,
        &facets,
        &keyformat,
        &all_entries,
        platform,
        min_deploy,
        coreui_version,
        ios_images.is_some(),
    )
}

/// A layer's source image, emitted as a `<stem>_Assets/<layer_name>` facet
/// referencing an inline rendition of the image bytes.
pub struct LayerAsset {
    pub facet_name: String,
    pub source_path: PathBuf,
}

/// A solid color asset extracted from icon.json `fill` / `fill-specializations`.
struct ColorAsset {
    facet_name: String,
    colorspace_id: u32,
    components: Vec<f64>,
}

/// A gradient asset extracted from icon.json `fill` specs. Stops reference
/// Color assets by facet name (e.g. "icon_Assets/Color-2").
struct GradientAsset {
    facet_name: String,
    geometry: [f32; 4],
    stops: Vec<(f32, String)>,
    /// 0 = single-color (radial-style; Apple emits this for the user
    /// color in `automatic-gradient`), 1 = linear top-to-bottom.
    kind: u32,
}

/// Treat a Color's stored components as device-RGB for compositing. Gray /
/// extended-gray (2 components) expand to a neutral triple; 4-component
/// spaces (srgb / p3) pass their RGB through (p3→sRGB primaries differ, but
/// the gray-axis backgrounds we composite are unaffected).
/// Drop the alpha from a parsed colour-spec's components and expand to RGB:
/// `[gray, a]` → `[g, g, g]`, `[r, g, b, a]` → `[r, g, b]`. `None` for any
/// other arity.
fn comps_to_rgb(comps: &[f64]) -> Option<[f64; 3]> {
    match comps {
        [g, _a] => Some([*g, *g, *g]),
        [r, g, b, _a] => Some([*r, *g, *b]),
        _ => None,
    }
}

fn color_to_rgb(c: &ColorAsset) -> [f64; 3] {
    comps_to_rgb(&c.components).unwrap_or([0.0, 0.0, 0.0])
}

/// Build per-size CoreGraphics drop-shadow parameters from a resolved
/// `ShadowSpec`. `None` when there is no shadow. The colour alpha is set so
/// that, once Gaussian-blurred over the squircle edge, the halo peaks near the
/// measured `PEAK_ALPHA`; neutral and layer-color shadows are both approximated
/// as black (dark-mode backgrounds make the distinction negligible).
fn shadow_params(
    spec: Option<crate::icon_effects::ShadowSpec>,
    pixel_size: u32,
) -> Option<crate::icon_render::ShadowParams> {
    use crate::icon_effects::shadow_geometry::{BLUR_RATIO, OFFSET_Y_RATIO, PEAK_ALPHA};
    use crate::icon_effects::ShadowKind;
    let spec = spec?;
    if spec.kind == ShadowKind::None || spec.opacity <= 0.0 {
        return None;
    }
    let alpha = (2.0 * PEAK_ALPHA * spec.opacity as f64).min(1.0);
    Some(crate::icon_render::ShadowParams {
        color: [0.0, 0.0, 0.0, alpha],
        blur: pixel_size as f64 * BLUR_RATIO,
        offset: [0.0, pixel_size as f64 * OFFSET_Y_RATIO],
    })
}

/// Apple's `.icns` holds the same squircle-clipped, shadowed composite as the
/// light CAR renditions, not the raw primary layer. Falls back to the source
/// image for any size whose stack or compositor is unavailable.
fn composite_icns_sources(
    icon_images: &[(PathBuf, u32, u32)],
    light_stacks: &[Vec<u8>],
    light_fill: Option<&crate::icon_render::GradientFill>,
    tmpdir: &Path,
) -> Result<Vec<(PathBuf, u32, u32)>> {
    use crate::icon_effects::{ShadowKind, ShadowSpec};
    let spec = Some(ShadowSpec { kind: ShadowKind::Neutral, opacity: 0.5 });
    let mut out = Vec::with_capacity(icon_images.len());
    for (idx, (path, px, scale)) in icon_images.iter().enumerate() {
        let composited = match (light_fill, light_stacks.get(idx)) {
            (Some(fill), Some(stack)) => crate::icon_render::composite_icon(
                *px,
                fill,
                stack,
                shadow_params(spec, *px).as_ref(),
            ),
            _ => None,
        };
        let Some(bgra) = composited else {
            out.push((path.clone(), *px, *scale));
            continue;
        };
        let mut rgba = Vec::with_capacity(bgra.len());
        for p in bgra.chunks_exact(4) {
            let a = p[3] as u32;
            let un = |c: u8| -> u8 {
                if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 }
            };
            rgba.extend_from_slice(&[un(p[2]), un(p[1]), un(p[0]), p[3]]);
        }
        let img = image::RgbaImage::from_raw(*px, *px, rgba)
            .ok_or_else(|| anyhow::anyhow!("icns composite size mismatch"))?;
        let file = tmpdir.join(format!("Icns{px}x{px}.png"));
        img.save(&file)?;
        out.push((file, *px, *scale));
    }
    Ok(out)
}

/// One pre-rendered iOS 1024pt app-icon variant.
struct IosComposite {
    /// CoreUI appearance name used in the rendition name.
    appearance_name: &'static str,
    /// Appearance attribute (UIAppearanceAny=0, Dark=1, Tintable=10).
    appearance: u16,
    /// Opaque premultiplied BGRA, 1024².
    bgra: Vec<u8>,
}

const IOS_ICON_SIZE: u32 = 1024;

/// Composite a premultiplied layer stack over a full-bleed gradient (no
/// squircle, margin or shadow — iOS masks the icon at runtime). The result is
/// fully opaque.
fn composite_fullbleed(
    stack: &[u8],
    pixel_size: u32,
    fill: Option<&crate::icon_render::GradientFill>,
) -> Vec<u8> {
    let w = pixel_size as usize;
    let mut out = vec![0u8; w * w * 4];
    for y in 0..w {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let bg = match fill {
                Some(f) => f.sample_with_margin(x as u32, y as u32, pixel_size, 0.0),
                None => [1.0, 1.0, 1.0],
            };
            let a = stack[i + 3] as f32 / 255.0;
            // BGRA, premultiplied: dst = src + bg·(1 − a).
            for c in 0..3 {
                let layer = stack[i + c] as f32;
                let back = bg[2 - c] as f32 * 255.0;
                out[i + c] = (layer + back * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = 255;
        }
    }
    out
}

/// Render the Any / Dark / Tintable variants Apple stores for an iOS icon.
/// Tintable is the dark stack's luminance over black (the system re-tints it).
fn render_ios_composites(
    light_layers: &[StackLayer],
    dark_layers: &[StackLayer],
    light_fill: Option<&crate::icon_render::GradientFill>,
    dark_fill: Option<&crate::icon_render::GradientFill>,
) -> Result<Vec<IosComposite>> {
    let size = IOS_ICON_SIZE;
    let any_stack = render_layer_stack(light_layers, size, light_fill)?;
    let dark_stack = render_layer_stack(dark_layers, size, dark_fill)?;
    let any = composite_fullbleed(&any_stack, size, light_fill);
    let dark = composite_fullbleed(&dark_stack, size, dark_fill);
    let black = crate::icon_render::GradientFill {
        start_rgb: [0.0; 3],
        stop_rgb: [0.0; 3],
        start: [0.5, 0.0],
        stop: [0.5, 1.0],
    };
    let mut tint = composite_fullbleed(&dark_stack, size, Some(&black));
    for px in tint.chunks_exact_mut(4) {
        let luma = 0.114 * px[0] as f32 + 0.587 * px[1] as f32 + 0.299 * px[2] as f32;
        let g = luma.round().clamp(0.0, 255.0) as u8;
        px[..3].fill(g);
    }
    Ok(vec![
        IosComposite { appearance_name: "UIAppearanceAny", appearance: 0, bgra: any },
        IosComposite { appearance_name: "UIAppearanceDark", appearance: 1, bgra: dark },
        IosComposite { appearance_name: "ISAppearanceTintable", appearance: 10, bgra: tint },
    ])
}

/// Loose home-screen PNGs actool writes next to Assets.car for iOS:
/// `<name>60x60@2x.png` (iPhone) and `<name>76x76@2x~ipad.png` (iPad), cut
/// down from the Any composite.
fn write_ios_home_screen_icons(
    icon_name: &str,
    images: &[IosComposite],
    output_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let Some(any) = images.iter().find(|i| i.appearance == 0) else {
        return Ok(Vec::new());
    };
    let rgba: Vec<u8> = any
        .bgra
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], p[3]])
        .collect();
    let full = image::RgbaImage::from_raw(IOS_ICON_SIZE, IOS_ICON_SIZE, rgba)
        .ok_or_else(|| anyhow::anyhow!("ios icon size mismatch"))?;
    let mut out = Vec::new();
    for (file, px) in [
        (format!("{icon_name}60x60@2x.png"), 120),
        (format!("{icon_name}76x76@2x~ipad.png"), 152),
    ] {
        let path = output_dir.join(file);
        image::imageops::resize(&full, px, px, FilterType::Lanczos3).save(&path)?;
        out.push(fs::canonicalize(&path).unwrap_or(path));
    }
    Ok(out)
}

/// Premultiplied-gray and alpha of Apple's flat dark-variant tint at full
/// squircle coverage (measured constant on feishin/scrumdinger GA16: a
/// near-white tint, premult gray ≈59, α ≈60 — `tools/compare_variant_renditions.py`).
const DARK_TINT_GRAY: f32 = 59.0;
const DARK_TINT_ALPHA: f32 = 60.0;

/// Build the dark (alternate) variant rendition: not a baked dark composite but
/// Apple's flat semi-transparent squircle tint (premultiplied near-white at
/// `DARK_TINT_GRAY`/`DARK_TINT_ALPHA`) composited over the icon's black drop
/// shadow. CUICatalog overlays this on the light icon to produce dark mode.
/// Returns premultiplied-first BGRA (what `bgra_to_ga16_force` consumes).
fn build_dark_variant_tint(
    pixel_size: u32,
    fill: &crate::icon_render::GradientFill,
    shadow: Option<&crate::icon_render::ShadowParams>,
) -> Option<Vec<u8>> {
    let n = (pixel_size * pixel_size) as usize;
    let empty = vec![0u8; n * 4];
    // Squircle coverage (alpha only); shadow adds the margin halo.
    let mask = crate::icon_render::composite_icon(pixel_size, fill, &empty, None)?;
    let shadowed = match shadow {
        Some(s) => crate::icon_render::composite_icon(pixel_size, fill, &empty, Some(s))?,
        None => mask.clone(),
    };
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let m = mask[i * 4 + 3] as f32 / 255.0; // squircle coverage
        // Black drop shadow restricted to the margin (outside the squircle).
        let sa = (shadowed[i * 4 + 3] as f32 / 255.0) * (1.0 - m);
        let tint_a = (DARK_TINT_ALPHA / 255.0) * m;
        let out_a = tint_a + sa * (1.0 - tint_a); // tint over black shadow
        let pg = (DARK_TINT_GRAY * m).round().clamp(0.0, 255.0) as u8;
        let a = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        out[i * 4] = pg;
        out[i * 4 + 1] = pg;
        out[i * 4 + 2] = pg;
        out[i * 4 + 3] = a;
    }
    Some(out)
}

/// Resolve a GradientAsset into a renderable background fill, looking up each
/// stop's RGB in the palette. A single-stop gradient fills both ends with the
/// same color.
fn resolve_gradient_fill(
    grad: &GradientAsset,
    colors: &[ColorAsset],
    base_scale: f32,
) -> Option<crate::icon_render::GradientFill> {
    let rgb = |facet: &str| -> Option<[f64; 3]> {
        colors.iter().find(|c| c.facet_name == facet).map(color_to_rgb)
    };
    let start_rgb = rgb(&grad.stops.first()?.1)?;
    let stop_rgb = rgb(&grad.stops.last()?.1)?;
    let g = grad.geometry;
    // Apple renders the icon background with the first stop at the TOP, last at
    // the bottom, regardless of how the stored geometry orders its endpoints —
    // feishin's [0.5,1.0]→[0.5,0.3] and scrumdinger/automatic [0.5,0.0]→[0.5,1.0]
    // both render first-stop-on-top. The orientation only picks direction, NOT
    // the span: Apple always draws the gradient across the FULL content box.
    // feishin's literal y-range is 0.7 (1.0→0.3), but its rendered gradient
    // spans the whole box just like scrumdinger's 1.0 range — measured on the
    // background strip, the literal 0.7 span made ours too steep (slope 64 vs
    // Apple's 48 gray/yfrac) and clip to white, off by up to 8.7/luma; spanning
    // the full box drops it to ≈0.1. So anchor the endpoints to the box extremes
    // (top→bottom), not the orientation magnitudes. All observed icon gradients
    // are vertical (x constant), so keep x from the orientation.
    let (top_y, bot_y) = (1.0f32, 0.0f32);
    // Apple lays the gradient axis inside the same content box layers use: the
    // normalized [0,1] orientation is inset by LAYER_BASE_SCALE about the
    // centre, so the full-box top→bottom gradient spans canvas y ≈ [181,843],
    // not the full squircle [100,924]. Measured on a black→white probe (gradient
    // span 662 px = 824·LAYER_BASE_SCALE, centred); applies to x too.
    let inset = |p: f32| 0.5 + (p - 0.5) * base_scale;
    Some(crate::icon_render::GradientFill {
        start_rgb,
        stop_rgb,
        start: [inset(g[0]), inset(top_y)],
        stop: [inset(g[2]), inset(bot_y)],
    })
}

/// A flat (single-colour) background fill — both gradient stops the same RGB.
fn flat_fill(rgb: [f64; 3]) -> crate::icon_render::GradientFill {
    crate::icon_render::GradientFill {
        start_rgb: rgb,
        stop_rgb: rgb,
        start: [0.5, 0.0],
        stop: [0.5, 1.0],
    }
}

/// The flat background colour for a `fill: {"solid": "<spec>"}` icon. Apple's
/// light rendition paints this solid colour (not the dark `Gradient-1`, which
/// is the dark-mode background), so it must drive the light composite directly.
fn solid_fill_color(parsed: &IconJson) -> Option<[f64; 3]> {
    let Some(Fill::Structured(v)) = parsed.fill.as_ref() else { return None };
    let spec = v.get("solid").and_then(|x| x.as_str())?;
    let (_cspace, comps) = parse_color_spec(spec)?;
    comps_to_rgb(&comps)
}

/// Resolve the (light, dark) background fills the compositor draws under the
/// layer stack. For a `solid` fill the light background is the flat solid
/// colour and the (dark-mode) `Gradient-1` is the dark fill; otherwise
/// `Gradient-1` is light and `Gradient-2` (if any) is dark.
fn resolve_background_fills(
    parsed: &IconJson,
    gradient_assets: &[GradientAsset],
    color_assets: &[ColorAsset],
    base_scale: f32,
) -> (Option<crate::icon_render::GradientFill>, Option<crate::icon_render::GradientFill>) {
    if let Some(rgb) = solid_fill_color(parsed) {
        let dark = gradient_assets.first().and_then(|g| resolve_gradient_fill(g, color_assets, base_scale));
        return (Some(flat_fill(rgb)), dark);
    }
    let light = gradient_assets.first().and_then(|g| resolve_gradient_fill(g, color_assets, base_scale));
    let dark = gradient_assets.get(1).and_then(|g| resolve_gradient_fill(g, color_assets, base_scale));
    (light, dark)
}

/// Apple's default palette for `fill: "automatic"`. Empirically observed in
/// actool output: a special white anchor color, light-mode bg gradient
/// (Color-2 → Color-3), and dark-mode bg gradient (Color-4 → Color-5).
///
/// Apple's actool stores the gray channel as `f64(f32(v))` — i.e. promotes
/// from a 32-bit float to a 64-bit field. We round-trip through f32 so the
/// emitted CSI bytes match Apple's exactly.
fn automatic_fill_assets(facet_prefix: &str) -> (Vec<ColorAsset>, Vec<GradientAsset>) {
    let n = |s: &str| format!("{facet_prefix}_Assets/{s}");
    let g = |v: f32| (v as f32) as f64;
    let colors = vec![
        ColorAsset {
            facet_name: n("Color-1"),
            colorspace_id: 6,
            components: vec![g(1.0), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-2"),
            colorspace_id: 2,
            components: vec![g(1.0), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-3"),
            colorspace_id: 2,
            components: vec![g(0.925), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-4"),
            colorspace_id: 2,
            components: vec![g(0.192), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-5"),
            colorspace_id: 2,
            components: vec![g(0.078), g(1.0)],
        },
    ];
    let gradients = vec![
        GradientAsset {
            facet_name: n("Gradient-1"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, n("Color-2")), (1.0, n("Color-3"))],
            kind: 1,
        },
        GradientAsset {
            facet_name: n("Gradient-2"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, n("Color-4")), (1.0, n("Color-5"))],
            kind: 1,
        },
    ];
    (colors, gradients)
}

/// Determine whether `fill` is the "automatic" keyword (the only fill shape
/// we currently generate Color/Gradient renditions for). Returns true for an
/// explicit `"automatic"` string AND for an absent fill — Apple's actool
/// treats both the same way.
fn fill_is_automatic(fill: Option<&Fill>) -> bool {
    match fill {
        None => true,
        Some(Fill::Keyword(k)) => k == "automatic",
        Some(Fill::Structured(_)) => false,
    }
}

/// Whether the icon.json declares any `appearance: dark` specialization
/// anywhere — top-level `fill-specializations` or nested in a group/layer
/// (`*-specializations`). This is Apple's trigger for the light+dark variant
/// axis: a bundle with dark content gets a second (dark) set of renditions
/// keyed on attr 24 = 1. Walks the raw JSON so it catches every specialization
/// array without enumerating each typed field. `tinted` entries are ignored —
/// tint mode is a live system recolour, never a stored variant. Visibility
/// swaps (`opacity-`/`hidden-specializations`, e.g. Chimera's dark-only layer)
/// are ignored too: Apple composes those live and emits no variant axis.
fn json_has_dark_appearance(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Object(map) => {
            if map.get("appearance").and_then(|a| a.as_str()) == Some("dark") {
                return true;
            }
            map.iter().any(|(k, child)| {
                k != "opacity-specializations"
                    && k != "hidden-specializations"
                    && json_has_dark_appearance(child)
            })
        }
        serde_json::Value::Array(items) => items.iter().any(json_has_dark_appearance),
        _ => false,
    }
}

/// A parsed Apple color-spec string like `srgb:0.97288,0.97288,0.97288,1.0`
/// or `extended-gray:0.84536,1.0`. Returns (colorspace_id, components).
fn parse_color_spec(spec: &str) -> Option<(u32, Vec<f64>)> {
    let (name, rest) = spec.split_once(':')?;
    // Apple rounds color-spec literals to 3 decimal places before promoting
    // to f32 and storing as f64 — verified empirically: spec "0.97288"
    // becomes f32(0.973) = 0.9729999…, not f32(0.97288). Mirror that.
    let comps: Vec<f64> = rest
        .split(',')
        .map(|s| {
            let f: f64 = s.trim().parse().ok()?;
            let rounded = (f * 1000.0).round() / 1000.0;
            Some(rounded as f32 as f64)
        })
        .collect::<Option<Vec<_>>>()?;
    // Empirical colorspace ids from Apple's RLOC blobs:
    //   srgb:r,g,b,a              → cspace=1 (4 components)
    //   gray:gray,alpha           → cspace=2 (2 components)
    //   display-p3:r,g,b,a        → cspace=3 (4 components)  [classhub]
    //   extended-srgb:r,g,b,a     → cspace=4 (4 components)  [recipe-scraper]
    //   extended-gray:gray,alpha  → cspace=6 (2 components)
    let cspace = match name {
        "srgb" => 1,
        "gray" => 2,
        "display-p3" => 3,
        "extended-srgb" => 4,
        "extended-gray" => 6,
        _ => return None,
    };
    Some((cspace, comps))
}

/// Apple's palette for a `fill: {"solid": "<spec>"}` icon. Empirically
/// observed for tagspaces (srgb solid): 4 Colors + 1 Gradient.
/// Color-1: fixed white anchor (cspace=6)
/// Color-2: the user-provided color (cspace from spec)
/// Color-3: 0.192 extended-gray (dark mode mid-gray)
/// Color-4: 0.078 extended-gray (dark mode bottom)
/// Gradient-1: Color-3 → Color-4 (linear top-to-bottom, dark mode bg)
fn solid_fill_assets(
    facet_prefix: &str,
    spec: &str,
) -> Option<(Vec<ColorAsset>, Vec<GradientAsset>)> {
    let (user_cspace, user_components) = parse_color_spec(spec)?;
    let n = |s: &str| format!("{facet_prefix}_Assets/{s}");
    let g = |v: f32| (v as f32) as f64;
    let colors = vec![
        ColorAsset {
            facet_name: n("Color-1"),
            colorspace_id: 6,
            components: vec![g(1.0), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-2"),
            colorspace_id: user_cspace,
            components: user_components,
        },
        ColorAsset {
            facet_name: n("Color-3"),
            colorspace_id: 2,
            components: vec![g(0.192), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-4"),
            colorspace_id: 2,
            components: vec![g(0.078), g(1.0)],
        },
    ];
    let gradients = vec![GradientAsset {
        facet_name: n("Gradient-1"),
        geometry: [0.5, 0.0, 0.5, 1.0],
        stops: vec![(0.0, n("Color-3")), (1.0, n("Color-4"))],
        kind: 1,
    }];
    Some((colors, gradients))
}

/// Apple's palette for `fill: {"linear-gradient": ["<stop0>", "<stop1>"]}` —
/// observed in classhub (display-p3 stops) and recipe-scraper (extended-srgb
/// stops). Identical structure to the "automatic" palette: 5 Colors + 2
/// linear Gradients, but the two USER-PROVIDED stops fill Color-2/Color-3
/// in their declared colorspace and back the light-mode Gradient-1.
fn linear_gradient_fill_assets(
    facet_prefix: &str,
    stops_spec: &[&str],
) -> Option<(Vec<ColorAsset>, Vec<GradientAsset>)> {
    if stops_spec.len() < 2 {
        return None;
    }
    let (cs0, c0) = parse_color_spec(stops_spec[0])?;
    let (cs1, c1) = parse_color_spec(stops_spec[1])?;
    let g = |v: f32| (v as f32) as f64;
    let mut colors = vec![ColorAsset {
        facet_name: format!("{facet_prefix}_Assets/Color-1"),
        colorspace_id: 6,
        components: vec![g(1.0), g(1.0)],
    }];
    // Apple deduplicates by value: a white first stop is the Color-1 anchor
    // itself (srb2: 4 Colors), and the dark-background grays follow the stops.
    let n0 = palette_add_color(&mut colors, facet_prefix, cs0, c0, 0);
    let n1 = palette_add_color(&mut colors, facet_prefix, cs1, c1, 0);
    let d0 = palette_add_color(&mut colors, facet_prefix, 2, vec![g(0.192), g(1.0)], 0);
    let d1 = palette_add_color(&mut colors, facet_prefix, 2, vec![g(0.078), g(1.0)], 0);
    let gradients = vec![
        GradientAsset {
            facet_name: format!("{facet_prefix}_Assets/Gradient-1"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, n0), (1.0, n1)],
            kind: 1,
        },
        GradientAsset {
            facet_name: format!("{facet_prefix}_Assets/Gradient-2"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, d0), (1.0, d1)],
            kind: 1,
        },
    ];
    Some((colors, gradients))
}

/// Rename the stock background gradients the way CoreUI 1000+ does: the gray
/// pairs Apple uses for the light/dark system backgrounds become
/// `system-light`/`system-dark`; everything else keeps a `Gradient-N` name,
/// renumbered in order. A duplicate stock pair keeps its numbered name.
fn name_system_gradients(
    facet_prefix: &str,
    colors: &[ColorAsset],
    gradients: &mut [GradientAsset],
) {
    let gray = |name: &str| -> Option<f64> {
        let c = colors.iter().find(|c| c.facet_name == name)?;
        (c.colorspace_id == 2).then(|| c.components[0])
    };
    let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
    let mut used: Vec<&'static str> = Vec::new();
    let mut numbered = 0;
    for grad in gradients.iter_mut() {
        let stock = if grad.kind == 1 && grad.stops.len() == 2 {
            match (gray(&grad.stops[0].1), gray(&grad.stops[1].1)) {
                (Some(t), Some(b)) if near(t, 1.0) && near(b, 0.925) => Some("system-light"),
                (Some(t), Some(b)) if near(t, 0.192) && near(b, 0.078) => Some("system-dark"),
                _ => None,
            }
        } else {
            None
        };
        match stock {
            Some(name) if !used.contains(&name) => {
                used.push(name);
                grad.facet_name = format!("{facet_prefix}_Assets/{name}");
            }
            _ => {
                numbered += 1;
                grad.facet_name = format!("{facet_prefix}_Assets/Gradient-{numbered}");
            }
        }
    }
}

/// Apple's palette for `fill: {"automatic-gradient": "<color spec>"}` —
/// observed in ding_icon. Same 4 base Colors as the solid case, but
/// emits TWO gradients: Gradient-1 is a single-stop "user-color"
/// gradient (kind=0) pointing at Color-2, and Gradient-2 is the
/// standard dark-mode background (Color-3 → Color-4).
fn automatic_gradient_fill_assets(
    facet_prefix: &str,
    spec: &str,
) -> Option<(Vec<ColorAsset>, Vec<GradientAsset>)> {
    let (user_cspace, user_components) = parse_color_spec(spec)?;
    let n = |s: &str| format!("{facet_prefix}_Assets/{s}");
    let g = |v: f32| (v as f32) as f64;
    let colors = vec![
        ColorAsset {
            facet_name: n("Color-1"),
            colorspace_id: 6,
            components: vec![g(1.0), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-2"),
            colorspace_id: user_cspace,
            components: user_components,
        },
        ColorAsset {
            facet_name: n("Color-3"),
            colorspace_id: 2,
            components: vec![g(0.192), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-4"),
            colorspace_id: 2,
            components: vec![g(0.078), g(1.0)],
        },
    ];
    let gradients = vec![
        GradientAsset {
            facet_name: n("Gradient-1"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, n("Color-2"))],
            kind: 0,
        },
        GradientAsset {
            facet_name: n("Gradient-2"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, n("Color-3")), (1.0, n("Color-4"))],
            kind: 1,
        },
    ];
    Some((colors, gradients))
}

/// Apple's palette for `fill: "system-dark"` — observed in KYA:
/// 3 colors (white anchor + mid-dark + deep-dark) plus TWO identical
/// dark gradients. Per-layer fills are appended afterwards as Color-N+1.
fn system_dark_fill_assets(facet_prefix: &str) -> (Vec<ColorAsset>, Vec<GradientAsset>) {
    let n = |s: &str| format!("{facet_prefix}_Assets/{s}");
    let g = |v: f32| (v as f32) as f64;
    let colors = vec![
        ColorAsset {
            facet_name: n("Color-1"),
            colorspace_id: 6,
            components: vec![g(1.0), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-2"),
            colorspace_id: 2,
            components: vec![g(0.192), g(1.0)],
        },
        ColorAsset {
            facet_name: n("Color-3"),
            colorspace_id: 2,
            components: vec![g(0.078), g(1.0)],
        },
    ];
    let gradients = vec![
        GradientAsset {
            facet_name: n("Gradient-1"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, n("Color-2")), (1.0, n("Color-3"))],
            kind: 1,
        },
        GradientAsset {
            facet_name: n("Gradient-2"),
            geometry: [0.5, 0.0, 0.5, 1.0],
            stops: vec![(0.0, n("Color-2")), (1.0, n("Color-3"))],
            kind: 1,
        },
    ];
    (colors, gradients)
}


/// Resolve a fill-specialization keyword `value` to its background gray pair
/// (top stop, bottom stop). "system-light"/"system-dark" are fixed; bare
/// "automatic" resolves by the entry's appearance (dark → dark pair, else
/// the light pair). Returns None for keywords we don't model.
fn keyword_bg_pair(keyword: &str, appearance: Option<&str>) -> Option<(f64, f64)> {
    match keyword {
        "system-light" => Some((1.0, 0.925)),
        "system-dark" => Some((0.192, 0.078)),
        "automatic" => {
            if appearance == Some("dark") {
                Some((0.192, 0.078))
            } else {
                Some((1.0, 0.925))
            }
        }
        _ => None,
    }
}

/// Add a Color, deduplicating by (colorspace, components). Returns the facet
/// name of the existing or newly-created Color so gradients can reference it.
/// Add a Color, deduplicating against existing entries at index `dedup_from`
/// and later. Solids pass `dedup_from = 0` (dedup against the whole palette);
/// gradient-stop / keyword colours pass the base-palette length so they don't
/// collapse onto a hardcoded base colour — Apple keeps e.g. transmission's
/// Color-12 (0.078) distinct from the automatic-gradient's Color-4 (also 0.078).
fn palette_add_color(
    colors: &mut Vec<ColorAsset>,
    facet_prefix: &str,
    colorspace_id: u32,
    components: Vec<f64>,
    dedup_from: usize,
) -> String {
    if let Some(existing) = colors[dedup_from.min(colors.len())..]
        .iter()
        .find(|c| c.colorspace_id == colorspace_id && c.components == components)
    {
        return existing.facet_name.clone();
    }
    let idx = colors.len() + 1;
    let name = format!("{facet_prefix}_Assets/Color-{idx}");
    colors.push(ColorAsset {
        facet_name: name.clone(),
        colorspace_id,
        components,
    });
    name
}

/// Add a linear Gradient, deduplicating by (geometry, stops). A layer-level
/// keyword fill that resolves to a background pair already emitted (e.g.
/// scrumdinger's redundant dark "automatic") collapses to nothing here.
fn palette_add_gradient(
    gradients: &mut Vec<GradientAsset>,
    facet_prefix: &str,
    geometry: [f32; 4],
    stops: Vec<(f32, String)>,
) {
    if gradients
        .iter()
        .any(|g| g.kind == 1 && g.geometry == geometry && g.stops == stops)
    {
        return;
    }
    let idx = gradients.len() + 1;
    gradients.push(GradientAsset {
        facet_name: format!("{facet_prefix}_Assets/Gradient-{idx}"),
        geometry,
        stops,
        kind: 1,
    });
}

/// Read a gradient `orientation` object into geometry [start.x, start.y,
/// stop.x, stop.y]. Defaults to a top-to-bottom gradient when absent.
fn parse_orientation(obj: &serde_json::Map<String, serde_json::Value>) -> [f32; 4] {
    let default = [0.5f32, 0.0, 0.5, 1.0];
    let Some(orient) = obj.get("orientation") else {
        return default;
    };
    let pt = |key: &str, dx: f32, dy: f32| -> (f32, f32) {
        let p = orient.get(key);
        let x = p
            .and_then(|o| o.get("x"))
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(dx);
        let y = p
            .and_then(|o| o.get("y"))
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(dy);
        (x, y)
    };
    let (sx, sy) = pt("start", 0.5, 0.0);
    let (ex, ey) = pt("stop", 0.5, 1.0);
    [sx, sy, ex, ey]
}

/// Fold one fill-specialization `value` into the running palette. Handles the
/// three value shapes Apple emits: a keyword ("system-light"/"system-dark"/
/// "automatic") → a two-stop gray background gradient; a structured
/// `{linear-gradient: [...], orientation}` → its stops + an oriented gradient;
/// and `{solid: "<spec>"}` → a single color.
fn process_fill_value(
    value: &serde_json::Value,
    appearance: Option<&str>,
    facet_prefix: &str,
    colors: &mut Vec<ColorAsset>,
    gradients: &mut Vec<GradientAsset>,
    grad_floor: usize,
) {
    let g = |v: f64| (v as f32) as f64;
    if let Some(s) = value.as_str() {
        if let Some((top, bottom)) = keyword_bg_pair(s, appearance) {
            let c0 = palette_add_color(colors, facet_prefix, 2, vec![g(top), g(1.0)], grad_floor);
            let c1 = palette_add_color(colors, facet_prefix, 2, vec![g(bottom), g(1.0)], grad_floor);
            palette_add_gradient(
                gradients,
                facet_prefix,
                [0.5, 0.0, 0.5, 1.0],
                vec![(0.0, c0), (1.0, c1)],
            );
        }
        return;
    }
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(arr) = obj.get("linear-gradient").and_then(|x| x.as_array()) {
        let specs: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
        if specs.len() < 2 {
            return;
        }
        let geometry = parse_orientation(obj);
        let mut names = Vec::with_capacity(specs.len());
        for spec in &specs {
            let Some((cspace, comps)) = parse_color_spec(spec) else {
                return;
            };
            names.push(palette_add_color(colors, facet_prefix, cspace, comps, grad_floor));
        }
        let last = (names.len() - 1) as f32;
        let stops: Vec<(f32, String)> = names
            .into_iter()
            .enumerate()
            .map(|(i, n)| (i as f32 / last, n))
            .collect();
        palette_add_gradient(gradients, facet_prefix, geometry, stops);
    } else if let Some(solid) = obj.get("solid").and_then(|x| x.as_str()) {
        // Solids deduplicate against the whole palette (recipe-scraper's layer
        // solid collapses onto its base gradient stop).
        if let Some((cspace, comps)) = parse_color_spec(solid) {
            palette_add_color(colors, facet_prefix, cspace, comps, 0);
        }
    }
}

/// Apple's palette for a top-level `fill-specializations` block. The white
/// anchor Color-1 is always first; then each top-level specialization is
/// folded in document order, followed by every layer's fill and
/// fill-specializations. Reverse-engineered against feishin (custom p3
/// gradient + system-dark + layer gradient → 8 Colors / 3 Gradients) and
/// scrumdinger (system-light + automatic → 5 Colors / 2 Gradients).
fn fill_specializations_assets(
    facet_prefix: &str,
    json: &IconJson,
) -> (Vec<ColorAsset>, Vec<GradientAsset>) {
    let g = |v: f32| (v as f32) as f64;
    let mut colors = vec![ColorAsset {
        facet_name: format!("{facet_prefix}_Assets/Color-1"),
        colorspace_id: 6,
        components: vec![g(1.0), g(1.0)],
    }];
    let mut gradients: Vec<GradientAsset> = Vec::new();
    // The whole top-spec palette is one fold, so gradient colours deduplicate
    // against everything (floor 0).
    if let Some(specs) = json.fill_specializations.as_ref() {
        for sp in specs {
            let appearance = sp.get("appearance").and_then(|a| a.as_str());
            if let Some(v) = sp.get("value") {
                process_fill_value(v, appearance, facet_prefix, &mut colors, &mut gradients, 0);
            }
        }
    }
    append_layer_fills(facet_prefix, json, &mut colors, &mut gradients, 0);
    (colors, gradients)
}

/// Fold every layer's `fill` and `fill-specializations` into the palette via
/// [`process_fill_value`] — adding the colours (deduplicated) and, for
/// linear-gradient layer fills, their gradients (per appearance entry, in
/// document order). Used by both the top-level fill-specializations path and
/// the plain-`fill` paths so a multi-group icon's per-layer gradients (e.g.
/// transmission's ArrowLines / OuterEdge) are emitted like Apple's.
fn append_layer_fills(
    facet_prefix: &str,
    json: &IconJson,
    colors: &mut Vec<ColorAsset>,
    gradients: &mut Vec<GradientAsset>,
    grad_floor: usize,
) {
    for (_group, layer) in json.iter_layers() {
        if let Some(Fill::Structured(v)) = layer.fill.as_ref() {
            process_fill_value(v, None, facet_prefix, colors, gradients, grad_floor);
        }
        if let Some(specs) = layer.fill_specializations.as_ref() {
            for sp in specs {
                let appearance = sp.get("appearance").and_then(|a| a.as_str());
                if let Some(v) = sp.get("value") {
                    process_fill_value(v, appearance, facet_prefix, colors, gradients, grad_floor);
                }
            }
        }
    }
}

/// Try to derive Color/Gradient assets from an arbitrary fill spec.
/// Returns None when the spec shape is unrecognized — caller falls back
/// to no palette and the catalog stays self-consistent.
fn fill_assets(
    facet_prefix: &str,
    fill: Option<&Fill>,
    parsed: &IconJson,
) -> Option<(Vec<ColorAsset>, Vec<GradientAsset>)> {
    // A top-level `fill-specializations` block drives its own appearance-keyed
    // palette, distinct from the single-`fill` shapes below.
    if parsed
        .fill_specializations
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Some(fill_specializations_assets(facet_prefix, parsed));
    }
    if fill_is_automatic(fill) {
        let (mut colors, mut gradients) = automatic_fill_assets(facet_prefix);
        let base = colors.len();
            append_layer_fills(facet_prefix, parsed, &mut colors, &mut gradients, base);
        return Some((colors, gradients));
    }
    let fill_val = fill?;
    match fill_val {
        Fill::Keyword(k) if k == "system-dark" => {
            let (mut colors, mut gradients) = system_dark_fill_assets(facet_prefix);
            let base = colors.len();
            append_layer_fills(facet_prefix, parsed, &mut colors, &mut gradients, base);
            Some((colors, gradients))
        }
        Fill::Structured(v) => {
            if let Some(spec) = v.get("solid").and_then(|s| s.as_str()) {
                let (mut colors, mut gradients) = solid_fill_assets(facet_prefix, spec)?;
                let base = colors.len();
            append_layer_fills(facet_prefix, parsed, &mut colors, &mut gradients, base);
                return Some((colors, gradients));
            }
            if let Some(spec) = v.get("automatic-gradient").and_then(|s| s.as_str()) {
                let (mut colors, mut gradients) =
                    automatic_gradient_fill_assets(facet_prefix, spec)?;
                let base = colors.len();
            append_layer_fills(facet_prefix, parsed, &mut colors, &mut gradients, base);
                return Some((colors, gradients));
            }
            if let Some(arr) = v.get("linear-gradient").and_then(|s| s.as_array()) {
                let specs: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
                let (mut colors, mut gradients) =
                    linear_gradient_fill_assets(facet_prefix, &specs)?;
                let base = colors.len();
            append_layer_fills(facet_prefix, parsed, &mut colors, &mut gradients, base);
                return Some((colors, gradients));
            }
            None
        }
        _ => None,
    }
}

fn write_icon_car(
    car_path: &Path,
    facets: &[FacetEntry],
    keyformat: &[u16],
    all_entries: &[(Vec<u8>, Vec<u8>)],
    platform: &str,
    min_deploy: &str,
    coreui_version: u32,
    ios: bool,
) -> Result<()> {
    let mut bom = BomWriter::new();
    // CoreUI must be 975 or newer so the IconComposer code paths activate —
    // older values cause silent `imagesWithName:` empty results even when
    // FACETKEYS / RENDITIONS are byte-identical to Apple's output.
    bom.add_named_block(
        "CARHEADER",
        if ios {
            car::make_carheader_full(all_entries.len() as u32, coreui_version, 2)
        } else {
            car::make_carheader_versioned(all_entries.len() as u32, coreui_version)
        },
    );
    // The named-block ORDER below matches Apple's actool exactly. CUICatalog
    // appears to scan named blocks during initWithURL: in BOM order and
    // expects RENDITIONS to register early, before the auxiliary trees.
    bom.set_inline_key_size(Some(keyformat.len() * 2));
    bom.add_tree("RENDITIONS", all_entries, 4096);
    bom.set_inline_key_size(None);

    let mut facetkey_entries: Vec<(Vec<u8>, Vec<u8>)> = facets
        .iter()
        .map(|(name, element, part, ident)| {
            (
                name.as_bytes().to_vec(),
                car::make_facetkey_value(*element, *part, *ident),
            )
        })
        .collect();
    facetkey_entries.sort_by(|a, b| a.0.cmp(&b.0));
    bom.set_compact_variable_trees(coreui_version >= SYSTEM_GRADIENT_NAMES_MIN_COREUI);
    bom.add_tree("FACETKEYS", &facetkey_entries, 4096);

    let mut appearance_entries = if ios {
        car::make_ios_appearancekeys_entries()
    } else {
        car::make_appearancekeys_entries()
    };
    appearance_entries.sort_by(|a, b| a.0.cmp(&b.0));
    bom.add_tree("APPEARANCEKEYS", &appearance_entries, 4096);

    bom.add_named_block("KEYFORMAT", car::make_keyformat(keyformat));
    bom.add_named_block(
        "EXTENDED_METADATA",
        car::make_extended_metadata(platform, min_deploy),
    );

    let bitmap_entries = build_bitmapkeys(all_entries, keyformat);
    bom.add_raw_key_tree("BITMAPKEYS", &bitmap_entries, 1024);
    bom.write(car_path)?;
    Ok(())
}

/// Build BITMAPKEYS entries. CUICatalog uses these to resolve `imagesWithName:`
/// — without them, every facet lookup returns an empty array. Each entry
/// maps a facet identifier (raw u32 key) to a 52-byte attribute-mask blob:
///   u32 version=1, u32 zero, u32 size=40, u32 attr_count=keyformat.len(),
///   then keyformat.len() × i32 masks. Attributes that vary across renditions
///   (appearance, element, part, identifier) are always -1; the rest get a
///   bitmask of the values seen across renditions sharing that identifier.
fn build_bitmapkeys(
    entries: &[(Vec<u8>, Vec<u8>)],
    keyformat: &[u16],
) -> Vec<(u32, Vec<u8>)> {
    use byteorder::WriteBytesExt;
    use std::collections::BTreeMap;
    let identifier_pos = keyformat.iter().position(|&a| a == 17);
    let mut per_ident: BTreeMap<u16, Vec<Vec<u16>>> = BTreeMap::new();
    for (key, _) in entries {
        if key.len() < keyformat.len() * 2 {
            continue;
        }
        let mut attrs = Vec::with_capacity(keyformat.len());
        for i in 0..keyformat.len() {
            let v = u16::from_le_bytes([key[i * 2], key[i * 2 + 1]]);
            attrs.push(v);
        }
        let Some(ip) = identifier_pos else { continue };
        let ident = attrs[ip];
        if ident == 0 {
            continue;
        }
        per_ident.entry(ident).or_default().push(attrs);
    }
    let mut out: Vec<(u32, Vec<u8>)> = Vec::new();
    for (ident, rows) in per_ident {
        // 4 bytes for the attr count + 4 bytes per attribute mask.
        let body_size = 4 + 4 * keyformat.len() as u32;
        let mut buf = Vec::with_capacity(16 + body_size as usize);
        buf.write_u32::<LittleEndian>(1).unwrap();
        buf.write_u32::<LittleEndian>(0).unwrap();
        buf.write_u32::<LittleEndian>(body_size).unwrap();
        buf.write_u32::<LittleEndian>(keyformat.len() as u32).unwrap();
        for &attr in keyformat {
            // Apple always emits -1 for these "variable" attrs even when
            // every rendition in the facet has the same value.
            let always_variable = matches!(attr, 7 | 1 | 2 | 17);
            if always_variable {
                buf.write_i32::<LittleEndian>(-1).unwrap();
                continue;
            }
            let attr_pos = keyformat.iter().position(|a| *a == attr).unwrap();
            let mut mask: u32 = 0;
            for row in &rows {
                let v = row[attr_pos];
                if v < 32 {
                    mask |= 1u32 << v;
                }
            }
            if mask == 0 {
                mask = 1;
            }
            buf.write_u32::<LittleEndian>(mask).unwrap();
        }
        out.push((ident as u32, buf));
    }
    out.sort_by_key(|(k, _)| *k);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_appearance_detection() {
        // Nested dark spec (KYA-style: layer fill-specializations) is detected.
        let kya = serde_json::json!({
            "fill": {"automatic-gradient": "extended-gray:0.86,1.0"},
            "groups": [{"layers": [{"fill-specializations": [
                {"value": {"solid": "extended-gray:0.17,1.0"}},
                {"appearance": "dark", "value": {"solid": "extended-gray:0.87,1.0"}},
                {"appearance": "tinted", "value": {"solid": "gray:0.94,1.0"}}
            ]}]}]
        });
        assert!(json_has_dark_appearance(&kya));
        // tinted-only (Rectangle-style) is NOT a dark appearance.
        let tinted_only = serde_json::json!({
            "groups": [{"layers": [{"fill-specializations": [
                {"appearance": "tinted", "value": {"solid": "gray:0.9,1.0"}}
            ]}]}]
        });
        assert!(!json_has_dark_appearance(&tinted_only));
        // No specializations at all.
        let plain = serde_json::json!({"fill": "automatic", "groups": [{"layers": []}]});
        assert!(!json_has_dark_appearance(&plain));
    }

    #[test]
    fn pre_rendered_name_matches_apple_pattern() {
        let n = pre_rendered_name("feishin", 16, 2, "NSAppearanceNameSystem");
        assert!(n.starts_with("feishin16x16_NSAppearanceNameSystem_"));
        assert!(n.ends_with(".png"));
        // Pattern: prefix + UUID(8-4-4-4-12) + - + 5 hex + - + 16 hex + .png
        let body = n
            .trim_start_matches("feishin16x16_NSAppearanceNameSystem_")
            .trim_end_matches(".png");
        let parts: Vec<&str> = body.split('-').collect();
        assert_eq!(parts.len(), 7, "expected UUID-PID-HEX shape, got {n:?}");
    }

    #[test]
    fn pre_rendered_name_is_deterministic() {
        let a = pre_rendered_name("Icon", 32, 2, "NSAppearanceNameSystem");
        let b = pre_rendered_name("Icon", 32, 2, "NSAppearanceNameSystem");
        assert_eq!(a, b);
    }

    #[test]
    fn pre_rendered_name_differs_by_size_and_scale() {
        let a = pre_rendered_name("Icon", 32, 1, "NSAppearanceNameSystem");
        let b = pre_rendered_name("Icon", 32, 2, "NSAppearanceNameSystem");
        let c = pre_rendered_name("Icon", 64, 2, "NSAppearanceNameSystem");
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    fn palette(json: &str) -> (Vec<ColorAsset>, Vec<GradientAsset>) {
        let parsed = crate::icon_json::IconJson::parse(json).unwrap();
        fill_specializations_assets("X", &parsed)
    }

    fn write_solid_png(path: &Path, rgba: [u8; 4]) {
        image::RgbaImage::from_pixel(64, 64, image::Rgba(rgba))
            .save(path)
            .unwrap();
    }

    #[test]
    fn glass_layer_becomes_desaturated_relief() {
        // A fully-opaque red square rendered as glass loses its colour and
        // becomes a faint near-black overlay (premultiplied → 0,0,0 at low α).
        let dir = std::env::temp_dir().join(format!("glass_t_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let red = dir.join("red.png");
        write_solid_png(&red, [255, 0, 0, 255]);
        let layers = vec![StackLayer { source: red, frosted: true, tinted: false, specular: false, shadow_strength: 0.0, scale: 1.0, tx: 0.0, ty: 0.0, opacity: 1.0, blend: BlendMode::Normal, native_w: 64, native_h: 64 }];
        let out = render_layer_stack(&layers, 64, None).unwrap();
        let i = (32 * 64 + 32) * 4; // centre, premul-first BGRA
        let (b, g, r, a) = (out[i], out[i + 1], out[i + 2], out[i + 3]);
        assert!((5..70).contains(&a), "glass alpha should be low, got {a}");
        assert_eq!((b, g, r), (0, 0, 0), "colour stripped to premultiplied black");
    }

    #[test]
    fn non_glass_layer_keeps_colour() {
        let dir = std::env::temp_dir().join(format!("noglass_t_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let red = dir.join("red2.png");
        write_solid_png(&red, [255, 0, 0, 255]);
        let layers = vec![StackLayer { source: red, frosted: false, tinted: false, specular: false, shadow_strength: 0.0, scale: 1.0, tx: 0.0, ty: 0.0, opacity: 1.0, blend: BlendMode::Normal, native_w: 64, native_h: 64 }];
        let out = render_layer_stack(&layers, 64, None).unwrap();
        let i = (32 * 64 + 32) * 4; // premul-first BGRA, opaque red
        assert_eq!((out[i], out[i + 1], out[i + 2], out[i + 3]), (0, 0, 255, 255));
    }

    #[test]
    fn tinted_glass_subtracts_and_stacks() {
        // Tinted frosted glass (group shadow `layer-color`) darkens the
        // background by D·(1−colour) per channel; two overlapping tinted layers
        // stack the subtraction additively. A frosted-but-untinted layer leaves
        // the background un-tinted (neutral relief only).
        let dir = std::env::temp_dir().join(format!("tint_t_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let blue = dir.join("blue.png");
        let red = dir.join("red3.png");
        write_solid_png(&blue, [0, 51, 229, 255]);
        write_solid_png(&red, [229, 25, 0, 255]);
        // Constant grey 0.6 gradient so the sampled background is ≈153 anywhere.
        let grad = crate::icon_render::GradientFill {
            start_rgb: [0.6, 0.6, 0.6],
            stop_rgb: [0.6, 0.6, 0.6],
            start: [0.5, 0.0],
            stop: [0.5, 1.0],
        };
        // Full-canvas glass (native 1024 at scale 1) so the sampled centre is
        // deep interior — past the soft edge blur — and reflects the pure tint.
        let mk = |src: &Path, tinted: bool| StackLayer {
            source: src.to_path_buf(),
            frosted: true,
            tinted,
            specular: false,
            shadow_strength: 0.0,
            scale: 1.0,
            tx: 0.0,
            ty: 0.0,
            opacity: 1.0,
            blend: BlendMode::Normal,
            native_w: 1024,
            native_h: 1024,
        };
        let i = (128 * 256 + 128) * 4; // centre at 256², premul-first BGRA
        // One tinted blue layer over grey 153: out = 153 − 63.5·(1−col).
        // R(col 0) ≈ 89, B(col 0.9) ≈ 147 — darkened, blue ordering preserved.
        let single = render_layer_stack(&[mk(&blue, true)], 256, Some(&grad)).unwrap();
        let (b1, g1, r1) = (single[i], single[i + 1], single[i + 2]);
        assert!(b1 > g1 && g1 > r1, "blue tint ordering B>G>R, got {r1},{g1},{b1}");
        assert!((80..100).contains(&r1) && (140..155).contains(&b1),
            "subtractive tint magnitude off: {r1},{g1},{b1}");
        // Blue over red: stacked subtraction → far darker on every channel.
        let stacked =
            render_layer_stack(&[mk(&blue, true), mk(&red, true)], 256, Some(&grad)).unwrap();
        assert!(
            stacked[i] < b1 && stacked[i + 1] < g1 && stacked[i + 2] < r1,
            "overlap must be darker than either single tint on all channels"
        );
        // Untinted frosted layer leaves the grey background (only relief).
        let neutral = render_layer_stack(&[mk(&blue, false)], 256, Some(&grad)).unwrap();
        let (b, g, r) = (neutral[i], neutral[i + 1], neutral[i + 2]);
        assert!(
            b.abs_diff(g) < 6 && g.abs_diff(r) < 6 && g > 130,
            "untinted glass stays grey, got {r},{g},{b}"
        );
    }

    #[test]
    fn layer_keeps_native_aspect() {
        // A 2:1 native layer must blit a 2:1 region, not a square — at scale 1
        // and the 824/1024 base it's 824×412 wide, centred.
        let dir = std::env::temp_dir().join(format!("aspect_t_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let red = dir.join("wide.png");
        write_solid_png(&red, [255, 0, 0, 255]);
        let layers = vec![StackLayer {
            source: red,
            frosted: false,
            tinted: false,
            specular: false,
            shadow_strength: 0.0,
            scale: LAYER_BASE_SCALE,
            tx: 0.0,
            ty: 0.0,
            opacity: 1.0,
            blend: BlendMode::Normal,
            native_w: 256,
            native_h: 128,
        }];
        let out = render_layer_stack(&layers, 1024, None).unwrap();
        let opaque = |x: usize, y: usize| out[(y * 1024 + x) * 4 + 3] > 0;
        let mut xs = (0, 0);
        let mut ys = (0, 0);
        for x in 0..1024 {
            if opaque(x, 512) {
                if xs.0 == 0 {
                    xs.0 = x;
                }
                xs.1 = x;
            }
        }
        for y in 0..1024 {
            if opaque(512, y) {
                if ys.0 == 0 {
                    ys.0 = y;
                }
                ys.1 = y;
            }
        }
        let aspect = (xs.1 - xs.0) as f32 / (ys.1 - ys.0) as f32;
        assert!((aspect - 2.0).abs() < 0.1, "expected ~2:1, got {aspect:.2}");
    }

    #[test]
    fn blend_channel_math() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(near(blend_channel(BlendMode::Screen, 0.5, 0.5), 0.75));
        assert!(near(blend_channel(BlendMode::Multiply, 0.5, 0.5), 0.25));
        assert!(near(blend_channel(BlendMode::Darken, 0.3, 0.7), 0.3));
        assert!(near(blend_channel(BlendMode::Lighten, 0.3, 0.7), 0.7));
        assert_eq!(blend_channel(BlendMode::Normal, 0.3, 0.7), 0.7);
        // soft-light leaves a 0.5 source as the backdrop (identity).
        assert!(near(blend_channel(BlendMode::SoftLight, 0.4, 0.5), 0.4));
    }

    #[test]
    fn composite_blend_normal_is_over() {
        let mut dst = [0, 0, 255, 255];
        composite_blend(&mut dst, &[255, 0, 0, 255], BlendMode::Normal);
        assert_eq!(dst, [255, 0, 0, 255]);
        // Half-alpha red over opaque blue → halfway.
        let mut dst2 = [0, 0, 255, 255];
        composite_blend(&mut dst2, &[255, 0, 0, 128], BlendMode::Normal);
        assert!((120..=135).contains(&dst2[0]) && (120..=135).contains(&dst2[2]));
    }

    #[test]
    fn composite_blend_screen_lightens() {
        let mut dst = [128, 128, 128, 255];
        composite_blend(&mut dst, &[128, 128, 128, 255], BlendMode::Screen);
        // screen(0.502, 0.502) ≈ 0.752 → ~192
        assert!((185..=198).contains(&dst[0]), "got {}", dst[0]);
    }

    #[test]
    fn fill_specializations_feishin_palette() {
        // Top-level custom p3 gradient + system-dark, then a layer with a
        // default solid, a dark gradient (whose 2nd stop dedups to Color-2),
        // and a tinted solid → Apple's 8 Colors / 3 Gradients.
        let (colors, gradients) = palette(
            r#"{
              "fill-specializations":[
                {"value":{"linear-gradient":[
                    "display-p3:0.87416,0.87416,0.87416,1.0",
                    "display-p3:0.99575,0.99575,0.99575,1.0"],
                  "orientation":{"start":{"x":0.5,"y":1},"stop":{"x":0.5,"y":0.3}}}},
                {"appearance":"dark","value":"system-dark"}
              ],
              "groups":[{"layers":[{"image-name":"f.svg","name":"f",
                "fill-specializations":[
                  {"value":{"solid":"extended-gray:0.0,1.0"}},
                  {"appearance":"dark","value":{"linear-gradient":[
                      "display-p3:0.78674,0.78674,0.78674,1.0",
                      "display-p3:0.87416,0.87416,0.87416,1.0"],
                    "orientation":{"start":{"x":0.5,"y":1},"stop":{"x":0.5,"y":0}}}},
                  {"appearance":"tinted","value":{"solid":"gray:1.0,1.0"}}
                ]}]}]
            }"#,
        );
        assert_eq!(colors.len(), 8, "feishin should have 8 colors");
        assert_eq!(gradients.len(), 3, "feishin should have 3 gradients");
        // Colorspaces: anchor(6), 2×p3(3), 2×gray(2), ext-gray(6), p3(3), gray(2)
        let cs: Vec<u32> = colors.iter().map(|c| c.colorspace_id).collect();
        assert_eq!(cs, vec![6, 3, 3, 2, 2, 6, 3, 2]);
        // The layer dark-gradient's second stop dedups onto Color-2.
        assert_eq!(gradients[2].stops[0].1, "X_Assets/Color-7");
        assert_eq!(gradients[2].stops[1].1, "X_Assets/Color-2");
        // Gradient-1 carries the JSON orientation (top→0.3), not the default.
        assert_eq!(gradients[0].geometry, [0.5, 1.0, 0.5, 0.3]);
    }

    #[test]
    fn fill_specializations_scrumdinger_palette() {
        // system-light + dark automatic, plus a redundant layer dark automatic
        // that dedups entirely → 5 Colors / 2 Gradients (no Gradient-3).
        let (colors, gradients) = palette(
            r#"{
              "fill-specializations":[
                {"value":"system-light"},
                {"appearance":"dark","value":"automatic"}
              ],
              "groups":[{"layers":[{"image-name":"1.png","name":"1",
                "fill-specializations":[{"appearance":"dark","value":"automatic"}]}]}]
            }"#,
        );
        assert_eq!(colors.len(), 5, "scrumdinger should have 5 colors");
        assert_eq!(gradients.len(), 2, "redundant layer automatic must dedup");
        let cs: Vec<u32> = colors.iter().map(|c| c.colorspace_id).collect();
        assert_eq!(cs, vec![6, 2, 2, 2, 2]);
    }

    #[test]
    fn visibility_specializations_do_not_trigger_the_variant_axis() {
        // Chimera swaps which layer shows in dark; Apple emits no dark axis.
        let swap = serde_json::json!({
            "groups": [{"layers": [
                {"opacity-specializations": [{"value": 0}, {"appearance": "dark", "value": 1}]},
                {"hidden-specializations": [{"appearance": "dark", "value": true}]}
            ]}]
        });
        assert!(!json_has_dark_appearance(&swap));
        let fill = serde_json::json!({
            "groups": [{"layers": [{"fill-specializations": [{"appearance": "dark", "value": "automatic"}]}]}]
        });
        assert!(json_has_dark_appearance(&fill));
    }

    #[test]
    fn layer_canvas_frame_matches_apple_for_a_scaled_layer() {
        let dir = std::env::temp_dir().join(format!("frame_t_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("head.png");
        image::RgbaImage::new(746, 734).save(&png).unwrap();
        let json = r#"{"groups":[{"layers":[{"image-name":"head.png","position":{"scale":1.4,"translation-in-points":[0,0]}}]}]}"#;
        let parsed = crate::icon_json::IconJson::parse(json).unwrap();
        let g = &parsed.groups[0];
        assert_eq!(layer_canvas_frame(g, &g.layers[0], &png), (-11, -2, 1044, 1028));
    }

    fn named_palette(colors: &[(&str, u32, [f64; 2])], grads: &[(&str, [&str; 2])]) -> (Vec<ColorAsset>, Vec<GradientAsset>) {
        let c = colors
            .iter()
            .map(|(n, cs, v)| ColorAsset { facet_name: format!("P_Assets/{n}"), colorspace_id: *cs, components: v.to_vec() })
            .collect();
        let g = grads
            .iter()
            .map(|(n, s)| GradientAsset {
                facet_name: format!("P_Assets/{n}"),
                geometry: [0.5, 0.0, 0.5, 1.0],
                stops: vec![(0.0, format!("P_Assets/{}", s[0])), (1.0, format!("P_Assets/{}", s[1]))],
                kind: 1,
            })
            .collect();
        (c, g)
    }

    #[test]
    fn stock_gradients_are_named_and_the_rest_renumbered() {
        let (colors, mut grads) = named_palette(
            &[("Color-1", 6, [1.0, 1.0]), ("Color-2", 2, [1.0, 1.0]), ("Color-3", 2, [0.925, 1.0]),
              ("Color-4", 2, [0.192, 1.0]), ("Color-5", 2, [0.078, 1.0]), ("Color-6", 2, [0.5, 1.0])],
            &[("Gradient-1", ["Color-2", "Color-3"]), ("Gradient-2", ["Color-4", "Color-5"]),
              ("Gradient-3", ["Color-3", "Color-6"])],
        );
        name_system_gradients("P", &colors, &mut grads);
        let names: Vec<_> = grads.iter().map(|g| g.facet_name.as_str()).collect();
        assert_eq!(names, ["P_Assets/system-light", "P_Assets/system-dark", "P_Assets/Gradient-1"]);
    }

    #[test]
    fn a_repeated_stock_gradient_keeps_a_numbered_name() {
        let (colors, mut grads) = named_palette(
            &[("Color-1", 2, [0.192, 1.0]), ("Color-2", 2, [0.078, 1.0])],
            &[("Gradient-1", ["Color-1", "Color-2"]), ("Gradient-2", ["Color-1", "Color-2"])],
        );
        name_system_gradients("P", &colors, &mut grads);
        assert_eq!(grads[0].facet_name, "P_Assets/system-dark");
        assert_eq!(grads[1].facet_name, "P_Assets/Gradient-1");
    }

    #[test]
    fn linear_gradient_palette_deduplicates_a_white_first_stop() {
        let (colors, gradients) =
            linear_gradient_fill_assets("L", &["extended-gray:1.0,1.0", "display-p3:0.0,0.0,1.0,1.0"]).unwrap();
        assert_eq!(colors.len(), 4);
        assert_eq!(gradients[0].stops[0].1, "L_Assets/Color-1");
        assert_eq!(gradients[0].stops[1].1, "L_Assets/Color-2");
        let (colors, _) =
            linear_gradient_fill_assets("L", &["display-p3:0.9,1.0,0.9,1.0", "display-p3:0.0,0.0,1.0,1.0"]).unwrap();
        assert_eq!(colors.len(), 5);
    }
}

fn write_populated_partial_plist(path: &Path, icon_name: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let body = format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "\t<key>CFBundleIconFile</key>\n",
            "\t<string>{name}</string>\n",
            "\t<key>CFBundleIconName</key>\n",
            "\t<string>{name}</string>\n",
            "</dict>\n",
            "</plist>\n",
        ),
        name = icon_name,
    );
    fs::write(path, body)?;
    Ok(())
}

fn write_empty_partial_plist(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let body = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n",
        "<dict/>\n",
        "</plist>\n",
    );
    fs::write(path, body)?;
    Ok(())
}
