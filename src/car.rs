//! CAR (Core Asset Repository) format structures.
//!
//! CAR files use a BOM container with specific named blocks for asset
//! catalog data. CAR-internal structures use little-endian byte order;
//! the wrapping BOM structures are big-endian (handled in bom.rs).

use crate::{deepmap2, name_hash};
use byteorder::{LittleEndian, WriteBytesExt};
use std::io::Write;

// Keyformat tokens
pub const KEYFORMAT_ALL: &[u16] = &[7, 13, 1, 2, 3, 4, 17, 8, 9, 11, 12, 24];
pub const KEYFORMAT_OPTIONAL: &[u16] = &[4, 8, 9];

// iOS/tvOS catalogs carry Idiom (attr 15) and Subtype (attr 16) and omit the
// macOS-only Size/Layer columns. The base eight columns are emitted regardless
// of which idioms the renditions use; app icons additionally insert Dimension2
// then Dimension1 between Subtype and Identifier (see `compute_keyformat_ios`).
pub const KEYFORMAT_IOS: &[u16] = &[7, 13, 12, 15, 16, 17, 1, 2];

/// iOS key format with the Dimension2 (9) / Dimension1 (8) columns inserted
/// after Subtype when the renditions use them — app-icon size indices and
/// sprite-atlas counters respectively. Note the iOS order is 9 *then* 8, the
/// reverse of `KEYFORMAT_ALL`'s macOS `…8,9…`.
pub fn compute_keyformat_ios<R>(renditions: &[R], force_dim1: bool) -> Vec<u16>
where
    R: KeyformatRendition,
{
    let used_dim1 = force_dim1 || renditions.iter().any(|r| r.dim1() != 0);
    let used_dim2 = renditions.iter().any(|r| r.dim2() != 0);
    let mut kf = vec![7, 13, 12, 15, 16];
    if used_dim2 {
        kf.push(9);
    }
    if used_dim1 {
        kf.push(8);
    }
    kf.extend_from_slice(&[17, 1, 2]);
    kf
}

/// True for the device-family platforms whose catalogs encode idiom in the
/// rendition key (iOS/tvOS and their simulators). macOS keeps the legacy layout.
pub fn is_idiom_platform(platform: &str) -> bool {
    matches!(platform, "iphoneos" | "iphonesimulator" | "appletvos" | "appletvsimulator")
}

/// CoreUI deployment-platform string written into EXTENDED_METADATA. actool
/// records the device family ("ios" / "atv"), not the SDK name
/// ("iphoneos" / "appletvos").
pub fn deployment_platform_name(platform: &str) -> &str {
    match platform {
        "iphoneos" | "iphonesimulator" => "ios",
        "appletvos" | "appletvsimulator" => "atv",
        other => other,
    }
}

/// Dimension2 index used for the 1024pt source in Xcode 14+ single-size
/// iOS app icons. The single 1024 source is shared by both phone and pad
/// facets and uses index 1 rather than the multi-size marketing 1024 slot
/// (which is 8).
pub const IOS_ICON_DIM2_SINGLE_SIZE_1024: u16 = 1;

/// Numeric value for an asset-catalog `idiom` string, as encoded in rendition
/// key attribute 15. Values match CoreUI's `kCoreThemeIdiom*` enum.
pub fn idiom_value(idiom: &str) -> u16 {
    match idiom {
        "universal" => 0,
        "iphone" | "phone" => 1,
        "ipad" | "pad" => 2,
        "tv" | "appletv" => 3,
        "car" | "carplay" => 4,
        "watch" => 5,
        "ios-marketing" | "marketing" => 6,
        _ => 0,
    }
}

pub const DIRECTION_DEFAULT: u16 = 0;
pub const DIRECTION_RTL: u16 = 4;
pub const DIRECTION_LTR: u16 = 5;

pub const ELEMENT_UNIVERSAL: u16 = 85;
pub const ELEMENT_PACKED: u16 = 9;

pub const PART_ICON: u16 = 220;
pub const PART_ICON_MULTISIZE: u16 = 218;
pub const PART_REGULAR: u16 = 181;
pub const PART_TVOS_FLATTENED: u16 = 208;
pub const PART_TVOS_RADIOSITY: u16 = 209;
pub const PART_COLOR: u16 = 217;
pub const PART_SPRITE_ATLAS: u16 = 127;
// IconComposer (.icon) part IDs observed in Apple's actool output.
pub const PART_ICON_COMPOSER: u16 = 245;
pub const PART_ICON_GROUP: u16 = 246;
pub const PART_ICON_GRADIENT: u16 = 247;

pub const LAYOUT_PDF: u16 = 9;
pub const LAYOUT_ONE_PART_SCALE: u16 = 12;
pub const LAYOUT_RAW_DATA: u16 = 1000;
pub const LAYOUT_PACKED_IMAGE: u16 = 1003;
pub const LAYOUT_NAME_LIST: u16 = 1004;
pub const LAYOUT_METADATA: u16 = 1005;
pub const LAYOUT_COLOR: u16 = 1009;
pub const LAYOUT_MULTISIZE_IMAGE: u16 = 1010;
// IconComposer (macOS 26 / "liquid glass") renditions.
pub const LAYOUT_ICONSTACK: u16 = 1019;
pub const LAYOUT_ICON_GROUP: u16 = 1020;
pub const LAYOUT_GRADIENT: u16 = 1021;

pub const PIXELFMT_DATA: &[u8; 4] = b"ATAD";
pub const PIXELFMT_PDF: &[u8; 4] = b" FDP";
pub const PIXELFMT_SVG: &[u8; 4] = b" GVS";
pub const PIXELFMT_JPEG: &[u8; 4] = b"GEPJ";

pub fn colorspace_for_pixel_format(pixel_format: &[u8]) -> u32 {
    if pixel_format == b" 8AG" {
        2
    } else if pixel_format == b"61AG" {
        // GA16 (16-bit gray + 16-bit alpha) tracks Apple's extended-gray
        // colorspace ID 6 — verified empirically against scrumdinger's
        // alternate atlas + sized renditions.
        6
    } else {
        1
    }
}

fn parse_version(s: &str) -> (u32, u32) {
    let mut parts = s.split('.');
    let a = parts
        .next()
        .and_then(|x| x.parse::<u32>().ok())
        .unwrap_or(0);
    let b = parts
        .next()
        .and_then(|x| x.parse::<u32>().ok())
        .unwrap_or(0);
    (a, b)
}

fn min_lzfse_version(platform: &str) -> (u32, u32) {
    match platform {
        "macosx" => (10, 11),
        "iphoneos" | "appletvos" => (9, 0),
        "watchos" => (2, 0),
        _ => (10, 11),
    }
}

fn min_dmp2_version(platform: &str) -> (u32, u32) {
    match platform {
        "macosx" => (11, 0),
        "iphoneos" | "appletvos" => (14, 0),
        "watchos" => (7, 0),
        _ => (11, 0),
    }
}

pub fn min_pack_version(platform: &str) -> (u32, u32) {
    match platform {
        "macosx" => (10, 11),
        "iphoneos" | "appletvos" => (9, 0),
        "watchos" => (2, 0),
        _ => (10, 11),
    }
}

pub fn aligned_bytes_per_row(width: u32, _pixel_format: &[u8]) -> u32 {
    let exact = width * 4;
    ((exact + 31) / 32) * 32
}

pub fn compute_keyformat<R>(renditions: &[R], force_dim1: bool) -> Vec<u16>
where
    R: KeyformatRendition,
{
    let used_direction = renditions.iter().any(|r| r.direction() != 0);
    let used_dim1 = force_dim1 || renditions.iter().any(|r| r.dim1() != 0);
    let used_dim2 = renditions.iter().any(|r| r.dim2() != 0);
    let used_variant = renditions.iter().any(|r| r.variant() != 0);
    KEYFORMAT_ALL
        .iter()
        .copied()
        .filter(|t| match *t {
            4 => used_direction,
            8 => used_dim1,
            9 => used_dim2,
            // Attribute 24 ("variant") is Apple's appearance-specialization
            // axis — when no rendition has variant != 0, it's omitted. The
            // scrumdinger .icon fixture is the first to exercise it.
            24 => used_variant,
            _ => true,
        })
        .collect()
}

/// Accessor trait for the fields `compute_keyformat` consumes.
pub trait KeyformatRendition {
    fn direction(&self) -> u32;
    fn dim1(&self) -> u32;
    fn dim2(&self) -> u32;
    fn variant(&self) -> u32 {
        0
    }
}

pub fn make_carheader(rendition_count: u32) -> Vec<u8> {
    make_carheader_versioned(rendition_count, 972)
}

/// CARHEADER builder with explicit CoreUI version number. IconComposer
/// (.icon) catalogs must declare 975 or higher so CoreUI activates the
/// facet-resolution path that understands PART_ICON_COMPOSER/_GROUP and
/// the layered-image renditions; lower values cause silent lookup
/// failures (`imagesWithName:` returns empty arrays).
pub fn make_carheader_versioned(rendition_count: u32, coreui_version: u32) -> Vec<u8> {
    make_carheader_full(rendition_count, coreui_version, 1)
}

/// CARHEADER builder exposing the key-semantics field (offset 432). macOS and
/// .icon catalogs use 1; idiom platforms (iOS) declare 2, which tells CoreUI
/// the rendition keys carry an idiom column.
pub fn make_carheader_full(
    rendition_count: u32,
    coreui_version: u32,
    key_semantics: u32,
) -> Vec<u8> {
    let mut buf = vec![0u8; 436];
    buf[0..4].copy_from_slice(b"RATC");
    (&mut buf[4..8]).write_u32::<LittleEndian>(coreui_version).unwrap();
    (&mut buf[8..12]).write_u32::<LittleEndian>(17).unwrap();
    (&mut buf[12..16]).write_u32::<LittleEndian>(0).unwrap();
    (&mut buf[16..20])
        .write_u32::<LittleEndian>(rendition_count)
        .unwrap();
    let main_ver = format!("@(#)PROGRAM:CoreUI  PROJECT:CoreUI-{coreui_version}.1\n");
    let main_ver_bytes = main_ver.as_bytes();
    buf[20..20 + main_ver_bytes.len()].copy_from_slice(main_ver_bytes);
    let ver_str = b"IBCocoaTouchImageCatalogTool-17.0\n";
    buf[148..148 + ver_str.len()].copy_from_slice(ver_str);
    // uuid at 404 = zeros; checksum at 420 = zero.
    (&mut buf[424..428]).write_u32::<LittleEndian>(2).unwrap();
    (&mut buf[428..432]).write_u32::<LittleEndian>(1).unwrap();
    (&mut buf[432..436])
        .write_u32::<LittleEndian>(key_semantics)
        .unwrap();
    buf
}

pub fn make_extended_metadata(platform: &str, min_deploy: &str) -> Vec<u8> {
    let mut buf = vec![0u8; 1028];
    buf[0..4].copy_from_slice(b"META");
    let d = min_deploy.as_bytes();
    buf[260..260 + d.len()].copy_from_slice(d);
    let p = deployment_platform_name(platform).as_bytes();
    buf[516..516 + p.len()].copy_from_slice(p);
    let tool = b"actool";
    buf[772..772 + tool.len()].copy_from_slice(tool);
    buf
}

pub fn make_keyformat(attrs: &[u16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12 + attrs.len() * 4);
    buf.extend_from_slice(b"tmfk");
    buf.write_u32::<LittleEndian>(0).unwrap();
    buf.write_u32::<LittleEndian>(attrs.len() as u32).unwrap();
    for a in attrs {
        buf.write_u32::<LittleEndian>(*a as u32).unwrap();
    }
    buf
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RenditionKeyParts {
    pub appearance: u16,
    pub unknown13: u16,
    pub element: u16,
    pub part: u16,
    pub size: u16,
    pub direction: u16,
    pub identifier: u16,
    pub dim1: u16,
    pub dim2: u16,
    pub layer: u16,
    pub scale: u16,
    /// Attribute 15 — device idiom (universal=0, phone=1, pad=2, …). Only
    /// present in the key on idiom platforms (iOS); 0 elsewhere.
    pub idiom: u16,
    /// Attribute 16 — idiom subtype (e.g. plus-phone displays). 0 unless an
    /// asset declares a subtype.
    pub subtype: u16,
    /// Attribute 24 — Apple's appearance-specialization axis. 0 = primary,
    /// 1+ = alternate variants emitted when icon.json has top-level
    /// `fill-specializations`.
    pub variant: u16,
}

pub fn make_rendition_key(parts: RenditionKeyParts, keyformat: &[u16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(keyformat.len() * 2);
    for t in keyformat {
        let v = match *t {
            7 => parts.appearance,
            13 => parts.unknown13,
            1 => parts.element,
            2 => parts.part,
            3 => parts.size,
            4 => parts.direction,
            17 => parts.identifier,
            8 => parts.dim1,
            9 => parts.dim2,
            11 => parts.layer,
            12 => parts.scale,
            15 => parts.idiom,
            16 => parts.subtype,
            24 => parts.variant,
            _ => 0,
        };
        buf.write_u16::<LittleEndian>(v).unwrap();
    }
    buf
}

/// Standard APPEARANCEKEYS entries emitted by Apple's actool for IconComposer
/// (.icon) bundles. The values are the appearance attribute IDs that appear
/// in rendition keys when a rendition is specialized for that appearance.
pub fn make_appearancekeys_entries() -> Vec<(Vec<u8>, Vec<u8>)> {
    let entries: &[(&str, u16)] = &[
        ("NSAppearanceNameSystem", 0),
        ("NSAppearanceNameDarkAqua", 1),
        ("NSAppearanceNameAqua", 8),
        ("ISAppearanceTintable", 10),
    ];
    entries
        .iter()
        .map(|(name, id)| {
            let mut v = Vec::with_capacity(2);
            v.write_u16::<LittleEndian>(*id).unwrap();
            (name.as_bytes().to_vec(), v)
        })
        .collect()
}

/// APPEARANCEKEYS for idiom platforms (iOS), as Apple writes them for `.icon`
/// catalogs: the UIKit appearance names plus the Tintable mode.
pub fn make_ios_appearancekeys_entries() -> Vec<(Vec<u8>, Vec<u8>)> {
    [
        ("UIAppearanceAny", 0u16),
        ("UIAppearanceDark", 1),
        ("UIAppearanceLight", 4),
        ("ISAppearanceTintable", 10),
    ]
    .iter()
    .map(|(name, id)| (name.as_bytes().to_vec(), id.to_le_bytes().to_vec()))
    .collect()
}

pub fn make_facetkey_value(element: u16, part: Option<u16>, identifier: u16) -> Vec<u8> {
    let mut attrs: Vec<(u16, u16)> = Vec::new();
    attrs.push((1, element));
    if let Some(p) = part {
        attrs.push((2, p));
    }
    attrs.push((17, identifier));
    let mut buf = Vec::new();
    buf.write_u16::<LittleEndian>(0).unwrap();
    buf.write_u16::<LittleEndian>(0).unwrap();
    buf.write_u16::<LittleEndian>(attrs.len() as u16).unwrap();
    for (n, v) in attrs {
        buf.write_u16::<LittleEndian>(n).unwrap();
        buf.write_u16::<LittleEndian>(v).unwrap();
    }
    buf
}

fn compress_rle(pixel_data: &[u8], width: u32, height: u32, bpp: u32) -> Vec<u8> {
    let row_stride = (width * bpp) as usize;
    let bpp = bpp as usize;
    let width = width as usize;
    let height = height as usize;

    let mut header = Vec::with_capacity(12);
    header.write_u32::<LittleEndian>(bpp as u32).unwrap();
    header.write_u32::<LittleEndian>(width as u32).unwrap();
    header.write_u32::<LittleEndian>(height as u32).unwrap();

    let mut row_offsets = Vec::with_capacity(height);
    let mut encoded = Vec::new();
    let data_base = 12 + height * 4;
    let mut row_cache: std::collections::HashMap<Vec<u8>, u32> =
        std::collections::HashMap::new();

    for y in 0..height {
        let row_start = y * row_stride;
        let row = &pixel_data[row_start..row_start + row_stride];
        if let Some(&off) = row_cache.get(row) {
            row_offsets.push(off);
            continue;
        }
        let abs_off = (data_base + encoded.len()) as u32;
        row_cache.insert(row.to_vec(), abs_off);
        row_offsets.push(abs_off);

        let mut x = 0usize;
        while x < width {
            let px = &row[x * bpp..(x + 1) * bpp];
            let mut run_len = 1usize;
            while x + run_len < width
                && &row[(x + run_len) * bpp..(x + run_len + 1) * bpp] == px
            {
                run_len += 1;
            }
            if run_len >= 2 {
                encoded.write_u16::<LittleEndian>(run_len as u16).unwrap();
                encoded.write_u16::<LittleEndian>(0x8000).unwrap();
                encoded.extend_from_slice(px);
                x += run_len;
            } else {
                let lit_start = x;
                while x < width {
                    let next_px = &row[x * bpp..(x + 1) * bpp];
                    if x + 1 < width
                        && &row[(x + 1) * bpp..(x + 2) * bpp] == next_px
                    {
                        break;
                    }
                    x += 1;
                }
                let lit_count = (x - lit_start) as u16;
                encoded.write_u16::<LittleEndian>(lit_count).unwrap();
                encoded.write_u16::<LittleEndian>(0).unwrap();
                encoded.extend_from_slice(&row[lit_start * bpp..x * bpp]);
            }
        }
    }

    let mut out = header;
    for o in row_offsets {
        out.write_u32::<LittleEndian>(o).unwrap();
    }
    out.extend_from_slice(&encoded);
    out
}

fn lzfse_compress(input: &[u8]) -> Option<Vec<u8>> {
    // lzfse::encode_buffer needs a pre-sized output buffer; worst case size
    // is bounded by input size plus a small overhead.
    let cap = input.len() + 128;
    let mut out = vec![0u8; cap];
    let n = lzfse::encode_buffer(input, &mut out).ok()?;
    out.truncate(n);
    Some(out)
}

/// Returns the KCBC chunk stream and the number of chunks it holds.
fn compress_kcbc(pixel_data: &[u8], height: u32) -> Option<(Vec<u8>, u32)> {
    if pixel_data.is_empty() || height == 0 {
        return None;
    }
    let bpr = pixel_data.len() / height as usize;
    if bpr == 0 {
        return None;
    }
    let rows_per_chunk = if height >= 3 {
        (height / 3) as usize
    } else {
        height as usize
    };
    let mut out = Vec::new();
    let mut row = 0usize;
    let mut chunks = 0u32;
    let total = height as usize;
    while row < total {
        let n = rows_per_chunk.min(total - row);
        let chunk = &pixel_data[row * bpr..(row + n) * bpr];
        let compressed = lzfse_compress(chunk)?;
        out.write_u32::<LittleEndian>(0).unwrap();
        out.write_u32::<LittleEndian>(0).unwrap();
        out.write_u32::<LittleEndian>(n as u32).unwrap();
        out.write_u32::<LittleEndian>(compressed.len() as u32).unwrap();
        out.extend_from_slice(&compressed);
        row += n;
        chunks += 1;
        if row < total {
            out.extend_from_slice(b"KCBC");
        }
    }
    Some((out, chunks))
}

fn gzip_compress(data: &[u8]) -> Option<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).ok()?;
    enc.finish().ok()
}

pub fn compress_data(
    pixel_data: &[u8],
    pixel_format: &[u8],
    width: u32,
    height: u32,
    min_deploy: &str,
    platform: &str,
    allow_dmp2: bool,
    dmp2_inline: bool,
    is_opaque: bool,
) -> Vec<u8> {
    let deploy_ver = parse_version(min_deploy);

    if allow_dmp2 {
        let dmp2_min = min_dmp2_version(platform);
        if deploy_ver >= dmp2_min && pixel_data.len() > 256 {
            if let Some(dmp2_data) =
                deepmap2::encode(pixel_data, pixel_format, width, height)
            {
                let celm_ver = if is_opaque { 2 } else { 0 };
                return deepmap2::make_celm_dmp2(
                    &dmp2_data,
                    pixel_format,
                    dmp2_inline,
                    celm_ver,
                );
            }
        }
    }

    let lzfse_min = min_lzfse_version(platform);
    if deploy_ver >= lzfse_min && pixel_data.len() > 256 {
        if let Some((kcbc, chunks)) = compress_kcbc(pixel_data, height) {
            let celm_ver: u32 = if is_opaque { 3 } else { 1 };
            let mut out = Vec::new();
            out.extend_from_slice(b"MLEC");
            out.write_u32::<LittleEndian>(celm_ver).unwrap();
            out.write_u32::<LittleEndian>(4).unwrap();
            out.write_u32::<LittleEndian>(chunks).unwrap();
            out.extend_from_slice(b"KCBC");
            out.extend_from_slice(&kcbc);
            return out;
        }
    }

    if pixel_data.len() >= 4096 {
        if let Some(gz) = gzip_compress(pixel_data) {
            if gz.len() < pixel_data.len() {
                let mut out = Vec::new();
                out.extend_from_slice(b"MLEC");
                out.write_u32::<LittleEndian>(0).unwrap();
                out.write_u32::<LittleEndian>(2).unwrap();
                out.write_u32::<LittleEndian>(gz.len() as u32).unwrap();
                out.extend_from_slice(&gz);
                return out;
            }
        }
    } else if height > 0 {
        let bpp = if pixel_format == b"BGRA" { 4 } else { 2 };
        let rle = compress_rle(pixel_data, width, height, bpp);
        if rle.len() < pixel_data.len() {
            let mut out = Vec::new();
            out.extend_from_slice(b"MLEC");
            out.write_u32::<LittleEndian>(0).unwrap();
            out.write_u32::<LittleEndian>(1).unwrap();
            out.write_u32::<LittleEndian>(rle.len() as u32).unwrap();
            out.extend_from_slice(&rle);
            return out;
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"MLEC");
    out.write_u32::<LittleEndian>(0).unwrap();
    out.write_u32::<LittleEndian>(0).unwrap();
    out.write_u32::<LittleEndian>(pixel_data.len() as u32).unwrap();
    out.extend_from_slice(pixel_data);
    out
}

pub fn make_csi_header(
    width: u32,
    height: u32,
    scale_factor: u32,
    pixel_format: &[u8],
    layout: u16,
    name: &str,
    rendition_flags: u32,
    colorspace_id: u32,
) -> Vec<u8> {
    let mut buf = vec![0u8; 184];
    buf[0..4].copy_from_slice(b"ISTC");
    (&mut buf[4..8]).write_u32::<LittleEndian>(1).unwrap();
    (&mut buf[8..12])
        .write_u32::<LittleEndian>(rendition_flags)
        .unwrap();
    (&mut buf[12..16]).write_u32::<LittleEndian>(width).unwrap();
    (&mut buf[16..20]).write_u32::<LittleEndian>(height).unwrap();
    (&mut buf[20..24])
        .write_u32::<LittleEndian>(scale_factor)
        .unwrap();
    buf[24..28].copy_from_slice(pixel_format);
    (&mut buf[28..32])
        .write_u32::<LittleEndian>(colorspace_id & 0xF)
        .unwrap();
    (&mut buf[32..36]).write_u32::<LittleEndian>(0).unwrap();
    (&mut buf[36..38]).write_u16::<LittleEndian>(layout).unwrap();
    (&mut buf[38..40]).write_u16::<LittleEndian>(0).unwrap();
    let name_bytes = name.as_bytes();
    let n = name_bytes.len().min(127);
    buf[40..40 + n].copy_from_slice(&name_bytes[..n]);
    buf
}

pub fn build_csi(
    width: u32,
    height: u32,
    scale_factor: u32,
    pixel_format: &[u8],
    layout: u16,
    name: &str,
    tlv_data: &[u8],
    rendition_data: &[u8],
    rendition_flags: u32,
    colorspace_id: u32,
    bitmaplist_unknown: u32,
) -> Vec<u8> {
    let mut header = make_csi_header(
        width,
        height,
        scale_factor,
        pixel_format,
        layout,
        name,
        rendition_flags,
        colorspace_id,
    );
    (&mut header[168..172])
        .write_u32::<LittleEndian>(tlv_data.len() as u32)
        .unwrap();
    (&mut header[172..176])
        .write_u32::<LittleEndian>(bitmaplist_unknown)
        .unwrap();
    (&mut header[176..180]).write_u32::<LittleEndian>(0).unwrap();
    (&mut header[180..184])
        .write_u32::<LittleEndian>(rendition_data.len() as u32)
        .unwrap();
    let mut out = Vec::with_capacity(header.len() + tlv_data.len() + rendition_data.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(tlv_data);
    out.extend_from_slice(rendition_data);
    out
}

fn tlv_header(tag: u32, len: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(8);
    h.write_u32::<LittleEndian>(tag).unwrap();
    h.write_u32::<LittleEndian>(len).unwrap();
    h
}

pub fn make_slices_tlv(width: u32, height: u32) -> Vec<u8> {
    let mut slice = Vec::new();
    slice.write_u32::<LittleEndian>(1).unwrap();
    slice.write_u32::<LittleEndian>(0).unwrap();
    slice.write_u32::<LittleEndian>(0).unwrap();
    slice.write_u32::<LittleEndian>(width).unwrap();
    slice.write_u32::<LittleEndian>(height).unwrap();
    let mut out = tlv_header(0x03E9, slice.len() as u32);
    out.extend(slice);
    out
}

pub fn make_metrics_tlv(width: u32, height: u32) -> Vec<u8> {
    let mut m = Vec::new();
    m.write_u32::<LittleEndian>(1).unwrap();
    for _ in 0..4 {
        m.write_u32::<LittleEndian>(0).unwrap();
    }
    m.write_u32::<LittleEndian>(width).unwrap();
    m.write_u32::<LittleEndian>(height).unwrap();
    let mut out = tlv_header(0x03EB, m.len() as u32);
    out.extend(m);
    out
}

pub fn make_blend_opacity_tlv() -> Vec<u8> {
    let mut d = Vec::new();
    d.write_u32::<LittleEndian>(0).unwrap();
    d.write_f32::<LittleEndian>(1.0).unwrap();
    let mut out = tlv_header(0x03EC, d.len() as u32);
    out.extend(d);
    out
}

pub fn make_color_blend_opacity_tlv() -> Vec<u8> {
    let mut d = Vec::new();
    d.write_u32::<LittleEndian>(0).unwrap();
    d.write_f32::<LittleEndian>(0.0).unwrap();
    let mut out = tlv_header(0x03EC, d.len() as u32);
    out.extend(d);
    out
}

pub fn make_exif_orientation_tlv(orientation: u32) -> Vec<u8> {
    let mut d = Vec::new();
    d.write_u32::<LittleEndian>(orientation).unwrap();
    let mut out = tlv_header(0x03EE, d.len() as u32);
    out.extend(d);
    out
}

pub fn make_bytes_per_row_tlv(width: u32, pixel_format: &[u8], aligned: bool) -> Vec<u8> {
    let bpr = if aligned {
        aligned_bytes_per_row(width, pixel_format)
    } else {
        width * 4
    };
    let mut d = Vec::new();
    d.write_u32::<LittleEndian>(bpr).unwrap();
    let mut out = tlv_header(0x03EF, d.len() as u32);
    out.extend(d);
    out
}

pub fn make_inlk_tlv(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    scale: u16,
    atlas_identifier: u16,
    atlas_dim1: u16,
    idiom: u16,
) -> Vec<u8> {
    let mut inlk = Vec::new();
    inlk.extend_from_slice(b"KLNI");
    inlk.write_u32::<LittleEndian>(0).unwrap();
    inlk.write_u32::<LittleEndian>(x).unwrap();
    inlk.write_u32::<LittleEndian>(y).unwrap();
    inlk.write_u32::<LittleEndian>(width).unwrap();
    inlk.write_u32::<LittleEndian>(height).unwrap();

    let mut attr = Vec::new();
    attr.write_u16::<LittleEndian>(0).unwrap();
    attr.write_u16::<LittleEndian>(1).unwrap();
    attr.write_u16::<LittleEndian>(ELEMENT_PACKED).unwrap();
    attr.write_u16::<LittleEndian>(2).unwrap();
    attr.write_u16::<LittleEndian>(PART_REGULAR).unwrap();
    if atlas_dim1 != 0 {
        attr.write_u16::<LittleEndian>(8).unwrap();
        attr.write_u16::<LittleEndian>(atlas_dim1).unwrap();
    }
    if atlas_identifier != 0 {
        attr.write_u16::<LittleEndian>(17).unwrap();
        attr.write_u16::<LittleEndian>(atlas_identifier).unwrap();
    }
    attr.write_u16::<LittleEndian>(12).unwrap();
    attr.write_u16::<LittleEndian>(scale).unwrap();
    // iOS atlases are keyed per idiom; the link must name the atlas idiom
    // (attr 15) or CUICatalog can't resolve the packed image to its atlas.
    if idiom != 0 {
        attr.write_u16::<LittleEndian>(15).unwrap();
        attr.write_u16::<LittleEndian>(idiom).unwrap();
    }
    attr.write_u16::<LittleEndian>(0).unwrap();

    inlk.write_u16::<LittleEndian>(12).unwrap();
    inlk.write_u16::<LittleEndian>(attr.len() as u16).unwrap();
    inlk.extend(attr);

    let mut out = tlv_header(0x03F2, inlk.len() as u32);
    out.extend(inlk);
    out
}

pub fn build_packed_image_csi(
    name: &str,
    width: u32,
    height: u32,
    scale: u16,
    pixel_format: &[u8],
    x: u32,
    y: u32,
    atlas_identifier: u16,
    atlas_dim1: u16,
    rendition_flags: u32,
    idiom: u16,
) -> Vec<u8> {
    let scale_factor = scale as u32 * 100;
    let mut tlv = Vec::new();
    tlv.extend(make_slices_tlv(width, height));
    tlv.extend(make_metrics_tlv(width, height));
    tlv.extend(make_inlk_tlv(
        x,
        y,
        width,
        height,
        scale,
        atlas_identifier,
        atlas_dim1,
        idiom,
    ));
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    let cs_id = colorspace_for_pixel_format(pixel_format);
    build_csi(
        width,
        height,
        scale_factor,
        pixel_format,
        LAYOUT_PACKED_IMAGE,
        name,
        &tlv,
        &[],
        rendition_flags,
        cs_id,
        1,
    )
}

pub fn build_packed_asset_csi(
    name: &str,
    width: u32,
    height: u32,
    scale: u16,
    pixel_format: &[u8],
    pixel_data: &[u8],
    min_deploy: &str,
    platform: &str,
    force_lzfse: bool,
) -> Vec<u8> {
    let scale_factor = scale as u32 * 100;
    let mut tlv = Vec::new();
    tlv.extend(make_slices_tlv(0, 0));
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    let use_dmp2 = !force_lzfse;
    let rend_data = compress_data(
        pixel_data,
        pixel_format,
        width,
        height,
        min_deploy,
        platform,
        use_dmp2,
        false,
        false,
    );
    let actual_comp = if rend_data.len() >= 12 {
        u32::from_le_bytes(rend_data[8..12].try_into().unwrap())
    } else {
        0
    };
    let bpr = if actual_comp == 11 {
        aligned_bytes_per_row(width, pixel_format)
    } else if height > 0 {
        (pixel_data.len() / height as usize) as u32
    } else {
        width * 4
    };
    tlv.extend(tlv_header(0x03EF, 4));
    tlv.write_u32::<LittleEndian>(bpr).unwrap();

    let cs_id = colorspace_for_pixel_format(pixel_format);
    build_csi(
        width,
        height,
        scale_factor,
        pixel_format,
        LAYOUT_NAME_LIST,
        name,
        &tlv,
        &rend_data,
        0,
        cs_id,
        1,
    )
}

/// Reference to a rendition embedded in iconstack / IconGroup TLVs.
/// Apple encodes each as a 12-byte (attr, value) tuple covering element,
/// part and identifier — the other rendition-key attributes default to 0.
pub struct LayerRef {
    pub part: u16,
    pub identifier: u16,
    /// Group-level rendering parameters, carried by a group's entry in the
    /// iconstack. `None` writes the legacy defaults.
    pub group_style: Option<GroupStyle>,
    /// Layer-level glass flag, carried by a layer's entry in an IconGroup.
    pub glass: bool,
    /// Layer placement `(x, y, w, h)` on the 1024pt canvas, carried by a
    /// layer's entry in an IconGroup. `None` falls back to the full canvas.
    pub frame: Option<(i32, i32, u32, u32)>,
    /// Layer opacity for the rendition's appearance, carried by a layer's
    /// entry in an IconGroup.
    pub opacity: f32,
}

#[derive(Clone, Copy)]
pub struct GroupStyle {
    pub translucency_enabled: bool,
    pub translucency: f32,
    /// Raw shadow-kind code; only neutral (3) has been observed in Apple's output.
    pub shadow_kind: u32,
    pub shadow_opacity: f32,
}

/// 0x3FE: count + 12 bytes per layer + 4-byte trailer, all zero in every
/// Apple catalog observed.
fn write_t3fe(layer_count: usize) -> Vec<u8> {
    let mut b = Vec::new();
    b.write_u32::<LittleEndian>(layer_count as u32).unwrap();
    b.extend(std::iter::repeat(0u8).take(layer_count * 12 + 4));
    let mut out = tlv_header(0x03FE, b.len() as u32);
    out.extend(b);
    out
}

fn write_layer_key_fragment(buf: &mut Vec<u8>, r: &LayerRef) {
    buf.write_u16::<LittleEndian>(1).unwrap();
    buf.write_u16::<LittleEndian>(ELEMENT_UNIVERSAL).unwrap();
    buf.write_u16::<LittleEndian>(2).unwrap();
    buf.write_u16::<LittleEndian>(r.part).unwrap();
    buf.write_u16::<LittleEndian>(17).unwrap();
    buf.write_u16::<LittleEndian>(r.identifier).unwrap();
}

/// iconstack rendition (LAYOUT_ICONSTACK=1019). Apple emits one per
/// non-system appearance (DarkAqua/Aqua/Tintable), each referencing the
/// stacked layers (gradient + group facets) that compose the icon for
/// that appearance.
pub fn build_iconstack_csi(name: &str, dim: u32, layers: &[LayerRef]) -> Vec<u8> {
    let mut t3f4 = Vec::new();
    t3f4.write_u32::<LittleEndian>(layers.len() as u32).unwrap();
    for layer in layers {
        t3f4.extend(std::iter::repeat(0u8).take(28));
        t3f4.write_f32::<LittleEndian>(1.0).unwrap();
        t3f4.write_u32::<LittleEndian>(16).unwrap();
        write_layer_key_fragment(&mut t3f4, layer);
    }
    t3f4.extend(std::iter::repeat(0u8).take(4));

    let mut t3fc = Vec::new();
    t3fc.write_u32::<LittleEndian>(layers.len() as u32).unwrap();
    t3fc.write_u32::<LittleEndian>(0).unwrap();
    for (i, _) in layers.iter().enumerate() {
        t3fc.write_u32::<LittleEndian>(if i == 0 { 0 } else { 2 }).unwrap();
        t3fc.write_u32::<LittleEndian>(0).unwrap();
        t3fc.write_u32::<LittleEndian>(1).unwrap();
        t3fc.write_u8(0).unwrap();
    }

    // 0x3FD: 4 (count) + N × 20-byte entries + 4-byte trailer. A group's entry
    // is [pad, translucency enabled, translucency value, shadow kind, shadow
    // opacity]; every other entry is zero.
    let mut t3fd = Vec::new();
    t3fd.write_u32::<LittleEndian>(layers.len() as u32).unwrap();
    for layer in layers {
        match (layer.part == PART_ICON_GROUP, layer.group_style) {
            (true, Some(st)) => {
                t3fd.write_u32::<LittleEndian>(0).unwrap();
                t3fd.write_u32::<LittleEndian>(st.translucency_enabled as u32).unwrap();
                t3fd.write_f32::<LittleEndian>(st.translucency).unwrap();
                t3fd.write_u32::<LittleEndian>(st.shadow_kind).unwrap();
                t3fd.write_f32::<LittleEndian>(st.shadow_opacity).unwrap();
            }
            (is_group, _) => {
                t3fd.extend(std::iter::repeat(0u8).take(16));
                t3fd.write_f32::<LittleEndian>(if is_group { 0.5 } else { 0.0 }).unwrap();
            }
        }
    }
    t3fd.extend(std::iter::repeat(0u8).take(4));

    let mut tlv = Vec::new();
    tlv.extend(tlv_header(0x03F4, t3f4.len() as u32));
    tlv.extend(t3f4);
    tlv.extend(tlv_header(0x03FC, t3fc.len() as u32));
    tlv.extend(t3fc);
    tlv.extend(tlv_header(0x03FD, t3fd.len() as u32));
    tlv.extend(t3fd);
    tlv.extend(write_t3fe(layers.len()));
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_layered_uti_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    let trailer = make_dwar_trailer();
    build_csi(
        dim,
        dim,
        100,
        PIXELFMT_DATA,
        LAYOUT_ICONSTACK,
        name,
        &tlv,
        &trailer,
        0,
        0,
        1,
    )
}

/// IconGroup rendition (LAYOUT_ICON_GROUP=1020). Apple emits one per
/// non-system appearance; each references the group's child layers
/// (typically a single image layer like `<stem>_Assets/element`).
/// `child_dim` is the natural pixel dimension of the underlying image
/// (Apple writes it twice into each entry — probably as render hints).
pub fn build_icongroup_csi(name: &str, child_dim: u32, layers: &[LayerRef]) -> Vec<u8> {
    let mut t3f4 = Vec::new();
    t3f4.write_u32::<LittleEndian>(layers.len() as u32).unwrap();
    for layer in layers {
        let (x, y, w, h) = layer.frame.unwrap_or((0, 0, child_dim, child_dim));
        t3f4.write_u64::<LittleEndian>(0).unwrap();
        t3f4.write_i32::<LittleEndian>(x).unwrap();
        t3f4.write_i32::<LittleEndian>(y).unwrap();
        t3f4.write_u32::<LittleEndian>(w).unwrap();
        t3f4.write_u32::<LittleEndian>(h).unwrap();
        t3f4.write_u32::<LittleEndian>(0).unwrap();
        t3f4.write_f32::<LittleEndian>(layer.opacity).unwrap();
        t3f4.write_u32::<LittleEndian>(16).unwrap();
        write_layer_key_fragment(&mut t3f4, layer);
    }
    t3f4.extend(std::iter::repeat(0u8).take(4));

    let mut t3fc = Vec::new();
    t3fc.write_u32::<LittleEndian>(layers.len() as u32).unwrap();
    t3fc.write_u32::<LittleEndian>(0).unwrap();
    for layer in layers {
        t3fc.write_u32::<LittleEndian>(layer.glass as u32).unwrap();
        t3fc.write_u32::<LittleEndian>(0).unwrap();
        t3fc.write_u32::<LittleEndian>(1).unwrap();
        t3fc.write_u8(0).unwrap();
    }

    // Same 20-byte-entry shape as iconstack 0x3FD, with image layers (the
    // child of a group) always carrying f32 = 0.0.
    let mut t3fd = Vec::new();
    t3fd.write_u32::<LittleEndian>(layers.len() as u32).unwrap();
    for _ in layers {
        t3fd.extend(std::iter::repeat(0u8).take(20));
    }
    t3fd.extend(std::iter::repeat(0u8).take(4));

    let mut tlv = Vec::new();
    tlv.extend(tlv_header(0x03F4, t3f4.len() as u32));
    tlv.extend(t3f4);
    tlv.extend(tlv_header(0x03FC, t3fc.len() as u32));
    tlv.extend(t3fc);
    tlv.extend(tlv_header(0x03FD, t3fd.len() as u32));
    tlv.extend(t3fd);
    tlv.extend(write_t3fe(layers.len()));
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    let trailer = make_dwar_trailer();
    build_csi(
        0,
        0,
        100,
        PIXELFMT_DATA,
        LAYOUT_ICON_GROUP,
        name,
        &tlv,
        &trailer,
        0,
        0,
        1,
    )
}

/// Length-prefixed UTI string ("public.layeredimage") wrapped as a 0x03ED
/// TLV. Apple emits this in iconstack but not IconGroup.
fn make_layered_uti_tlv() -> Vec<u8> {
    const UTI: &[u8] = b"public.layeredimage";
    let mut body = Vec::new();
    body.write_u32::<LittleEndian>(UTI.len() as u32 + 1).unwrap();
    body.write_u32::<LittleEndian>(0).unwrap();
    body.extend_from_slice(UTI);
    body.write_u8(0).unwrap();
    let mut out = tlv_header(0x03ED, body.len() as u32);
    out.extend(body);
    out
}

/// 12-byte rendition_data trailer used after layered TLVs: "DWAR" + 8 zeros.
fn make_dwar_trailer() -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"DWAR");
    t.extend(std::iter::repeat(0u8).take(8));
    t
}

/// Color rendition (LAYOUT_COLOR=1009) used by IconComposer asset fills.
/// `components` is whatever the colorspace expects: 2 floats for gray+alpha,
/// 5 for srgba, etc. Matches Apple's RLOC blob layout.
pub fn build_icon_color_csi(name: &str, colorspace_id: u32, components: &[f64]) -> Vec<u8> {
    let mut colr = Vec::new();
    colr.extend_from_slice(b"RLOC");
    colr.write_u32::<LittleEndian>(1).unwrap();
    colr.write_u32::<LittleEndian>(colorspace_id).unwrap();
    colr.write_u32::<LittleEndian>(components.len() as u32).unwrap();
    for c in components {
        colr.write_f64::<LittleEndian>(*c).unwrap();
    }
    let mut tlv = Vec::new();
    tlv.extend(make_color_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));
    build_csi(
        0,
        0,
        0,
        b"\x00\x00\x00\x00",
        LAYOUT_COLOR,
        name,
        &tlv,
        &colr,
        0,
        0,
        1,
    )
}

/// Gradient rendition (LAYOUT_GRADIENT=1021) used by IconComposer fills.
/// `geom` is (x0, y0, x1, y1) for the start/end points in [0,1] icon space.
/// Each stop is (position, color_facet_name) — the named color must exist
/// elsewhere in the catalog as an `icon_Assets/Color-N` rendition.
pub fn build_icon_gradient_csi(
    name: &str,
    geom: [f32; 4],
    stops: &[(f32, &str)],
    kind: u32,
) -> Vec<u8> {
    // `kind`: 0 = "automatic-gradient" style (Apple's term — single user
    // color, geom unused), 1 = linear two-stop background gradient.
    let mut grad = Vec::new();
    grad.extend_from_slice(b"ARGG");
    grad.write_u32::<LittleEndian>(stops.len() as u32).unwrap();
    grad.write_u32::<LittleEndian>(kind).unwrap();
    grad.write_u32::<LittleEndian>(0).unwrap();
    for f in geom.iter() {
        grad.write_f32::<LittleEndian>(*f).unwrap();
    }
    for (pos, color_name) in stops {
        grad.write_f32::<LittleEndian>(*pos).unwrap();
        let b = color_name.as_bytes();
        grad.write_u32::<LittleEndian>((b.len() + 1) as u32).unwrap();
        grad.extend_from_slice(b);
        grad.write_u8(0).unwrap();
    }
    let mut tlv = Vec::new();
    tlv.extend(make_color_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));
    build_csi(
        0,
        0,
        0,
        b"\x00\x00\x00\x00",
        LAYOUT_GRADIENT,
        name,
        &tlv,
        &grad,
        0,
        0,
        1,
    )
}

pub fn build_color_csi(
    name: &str,
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
    colorspace_id: u32,
) -> Vec<u8> {
    let mut colr = Vec::new();
    colr.extend_from_slice(b"RLOC");
    colr.write_u32::<LittleEndian>(1).unwrap();
    colr.write_u32::<LittleEndian>(colorspace_id & 0xFF).unwrap();
    colr.write_u32::<LittleEndian>(4).unwrap();
    colr.write_f64::<LittleEndian>(red).unwrap();
    colr.write_f64::<LittleEndian>(green).unwrap();
    colr.write_f64::<LittleEndian>(blue).unwrap();
    colr.write_f64::<LittleEndian>(alpha).unwrap();

    let mut tlv = Vec::new();
    tlv.extend(make_color_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    build_csi(
        0,
        0,
        0,
        b"\x00\x00\x00\x00",
        LAYOUT_COLOR,
        name,
        &tlv,
        &colr,
        0,
        0,
        1,
    )
}

pub fn build_sprite_atlas_metadata_csi(sprite_names: &[String]) -> Vec<u8> {
    let mut tlv = Vec::new();
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    if !sprite_names.is_empty() {
        let mut contents = Vec::new();
        contents
            .write_u32::<LittleEndian>(sprite_names.len() as u32)
            .unwrap();
        contents.write_u32::<LittleEndian>(0).unwrap();
        for sn in sprite_names {
            let b = sn.as_bytes();
            contents.write_u32::<LittleEndian>(b.len() as u32).unwrap();
            contents.extend_from_slice(b);
        }
        tlv.extend(tlv_header(0x03F5, contents.len() as u32));
        tlv.extend(contents);
    }

    build_csi(
        0,
        0,
        100,
        b"\x00\x00\x00\x00",
        LAYOUT_METADATA,
        "CoreStructuredImage",
        &tlv,
        &[],
        0,
        0,
        1,
    )
}

pub fn build_data_csi(raw_data: &[u8]) -> Vec<u8> {
    let mut rawd = Vec::new();
    rawd.extend_from_slice(b"DWAR");
    rawd.write_u32::<LittleEndian>(0).unwrap();
    rawd.write_u32::<LittleEndian>(raw_data.len() as u32).unwrap();
    rawd.extend_from_slice(raw_data);

    let mut tlv = Vec::new();
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    build_csi(
        0,
        0,
        0,
        PIXELFMT_DATA,
        LAYOUT_RAW_DATA,
        "CoreStructuredImage",
        &tlv,
        &rawd,
        0,
        1,
        1,
    )
}

pub fn build_pdf_csi(filename: &str, pdf_data: &[u8]) -> Vec<u8> {
    let mut rawd = Vec::new();
    rawd.extend_from_slice(b"DWAR");
    rawd.write_u32::<LittleEndian>(0).unwrap();
    rawd.write_u32::<LittleEndian>(pdf_data.len() as u32).unwrap();
    rawd.extend_from_slice(pdf_data);

    let mut tlv = Vec::new();
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    build_csi(
        0,
        0,
        0,
        PIXELFMT_PDF,
        LAYOUT_PDF,
        filename,
        &tlv,
        &rawd,
        0x04,
        0,
        1,
    )
}

/// Extract (width, height) from a JPEG's SOFn marker. Returns (0, 0) if
/// the file isn't a recognizable JPEG.
pub fn jpeg_dimensions(data: &[u8]) -> (u32, u32) {
    if data.len() < 4 || &data[..2] != [0xFF, 0xD8].as_ref() {
        return (0, 0);
    }
    let mut i = 2;
    while i + 3 < data.len() {
        if data[i] != 0xFF {
            return (0, 0);
        }
        // skip fill bytes
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            return (0, 0);
        }
        let marker = data[i];
        i += 1;
        // Standalone markers without a length field
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            continue;
        }
        if i + 2 > data.len() {
            return (0, 0);
        }
        let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        // SOF markers: 0xC0-0xCF except DHT (0xC4), JPG (0xC8), DAC (0xCC)
        let is_sof = (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC;
        if is_sof {
            if i + 7 > data.len() {
                return (0, 0);
            }
            let h = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
            let w = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            return (w, h);
        }
        i += seg_len;
    }
    (0, 0)
}

/// Build a CSI for a JPEG image rendition (layout 12, pixfmt `GEPJ`).
///
/// JPEG bytes are stored raw inside a DWAR container — CoreUI decodes
/// the JPEG at render time. Unlike PDF/SVG, JPEG uses layout 12 with
/// real Slices/Metrics TLVs (width/height from the SOFn marker).
pub fn build_jpeg_csi(filename: &str, jpeg_data: &[u8]) -> Vec<u8> {
    let (width, height) = jpeg_dimensions(jpeg_data);
    let mut rawd = Vec::new();
    rawd.extend_from_slice(b"DWAR");
    rawd.write_u32::<LittleEndian>(0).unwrap();
    rawd.write_u32::<LittleEndian>(jpeg_data.len() as u32).unwrap();
    rawd.extend_from_slice(jpeg_data);

    let mut tlv = Vec::new();
    tlv.extend(make_slices_tlv(width, height));
    tlv.extend(make_metrics_tlv(width, height));
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    build_csi(
        0,
        0,
        100,
        PIXELFMT_JPEG,
        LAYOUT_ONE_PART_SCALE,
        filename,
        &tlv,
        &rawd,
        0x10,
        0,
        1,
    )
}

pub fn build_svg_csi(filename: &str, svg_data: &[u8]) -> Vec<u8> {
    let (stored, is_compressed): (Vec<u8>, u32) = match lzfse_compress(svg_data) {
        Some(c) if c.len() < svg_data.len() => (c, 1),
        _ => (svg_data.to_vec(), 0),
    };
    let mut rawd = Vec::new();
    rawd.extend_from_slice(b"DWAR");
    rawd.write_u32::<LittleEndian>(is_compressed).unwrap();
    rawd.write_u32::<LittleEndian>(stored.len() as u32).unwrap();
    rawd.extend_from_slice(&stored);

    let mut tlv = Vec::new();
    tlv.extend(make_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    build_csi(
        0,
        0,
        0,
        PIXELFMT_SVG,
        LAYOUT_PDF,
        filename,
        &tlv,
        &rawd,
        0x04,
        0,
        1,
    )
}

#[derive(Debug, Clone)]
pub struct Rendition {
    pub name: String,
    pub identifier: u16,
    pub element: u16,
    pub part: u16,
    pub scale: u16,
    pub width: u32,
    pub height: u32,
    pub pixel_data: Vec<u8>,
    pub pixel_format: [u8; 4],
    pub layout: u16,
    pub dim1: u16,
    pub dim2: u16,
    pub appearance: u16,
    pub direction: u16,
    /// Device idiom (key attribute 15). 0 = universal; only encoded into the
    /// rendition key on idiom platforms (iOS).
    pub idiom: u16,
    /// Idiom subtype (key attribute 16). 0 unless the asset declares one.
    pub subtype: u16,
    pub is_template: bool,
    /// bitmapEncoding: -1=auto, 0=original, 4=automatic, 2=template
    pub template_rendering_intent: i32,
    pub colorspace_id: u32,
    pub locale: String,
    pub sprite_atlas_id: u16,
    pub is_svg_rasterization: bool,
    pub has_icon: bool,
    pub keyformat: Vec<u16>,
    pub min_deploy: String,
    pub platform: String,
    pub csi_override: Option<Vec<u8>>,
    /// Force CELM encoding to ver=0 (non-opaque) regardless of the actual
    /// pixel alpha. Apple's actool stores .icon layer source images this
    /// way so they composite with alpha against other stack layers, even
    /// when the source is fully opaque.
    pub force_non_opaque: bool,
    /// Attribute 24 — appearance-variant axis. 0 = primary (most
    /// renditions), 1 = alternate variant emitted alongside the primary
    /// when icon.json declares top-level `fill-specializations`. When any
    /// rendition has variant != 0, attribute 24 is added to KEYFORMAT.
    pub variant: u16,
}

impl Default for Rendition {
    fn default() -> Self {
        Self {
            name: String::new(),
            identifier: 0,
            element: ELEMENT_UNIVERSAL,
            part: PART_REGULAR,
            scale: 1,
            width: 0,
            height: 0,
            pixel_data: Vec::new(),
            pixel_format: *b"BGRA",
            layout: LAYOUT_ONE_PART_SCALE,
            dim1: 0,
            dim2: 0,
            appearance: 0,
            direction: 0,
            idiom: 0,
            subtype: 0,
            is_template: false,
            template_rendering_intent: -1,
            colorspace_id: 1,
            locale: String::new(),
            sprite_atlas_id: 0,
            is_svg_rasterization: false,
            has_icon: true,
            keyformat: Vec::new(),
            min_deploy: "10.11".to_string(),
            platform: "macosx".to_string(),
            csi_override: None,
            force_non_opaque: false,
            variant: 0,
        }
    }
}

impl KeyformatRendition for Rendition {
    fn direction(&self) -> u32 {
        self.direction as u32
    }
    fn dim1(&self) -> u32 {
        self.dim1 as u32
    }
    fn dim2(&self) -> u32 {
        self.dim2 as u32
    }
    fn variant(&self) -> u32 {
        self.variant as u32
    }
}

fn check_opaque(pixel_data: &[u8], pixel_format: &[u8], width: u32, height: u32) -> bool {
    let width = width as usize;
    let height = height as usize;
    if pixel_format == b"BGRA" {
        let bpr = width * 4;
        for row in 0..height {
            for col in 0..width {
                if pixel_data[row * bpr + col * 4 + 3] != 255 {
                    return false;
                }
            }
        }
        true
    } else if pixel_format == b" 8AG" {
        let bpr = width * 2;
        for row in 0..height {
            for col in 0..width {
                if pixel_data[row * bpr + col * 2 + 1] != 255 {
                    return false;
                }
            }
        }
        true
    } else {
        false
    }
}

impl Rendition {
    pub fn build_rendition_key(&self) -> Vec<u8> {
        let locale_id = if self.locale.is_empty() {
            0
        } else {
            name_hash::hash_name(&self.locale)
        };
        let parts = RenditionKeyParts {
            appearance: self.appearance,
            unknown13: locale_id,
            element: self.element,
            part: self.part,
            size: 0,
            direction: self.direction,
            identifier: self.identifier,
            dim1: self.dim1,
            dim2: self.dim2,
            layer: 0,
            scale: self.scale,
            idiom: self.idiom,
            subtype: self.subtype,
            variant: self.variant,
        };
        make_rendition_key(parts, &self.keyformat)
    }

    pub fn build_csi(&self) -> Vec<u8> {
        if let Some(over) = &self.csi_override {
            return over.clone();
        }
        let scale_factor = self.scale as u32 * 100;

        let is_tvos_icon =
            self.part == PART_TVOS_FLATTENED || self.part == PART_TVOS_RADIOSITY;

        let mut tlv = Vec::new();
        if is_tvos_icon {
            // tvOS brandasset flattened/radiosity images carry the same slice,
            // blend, orientation and bytes-per-row TLVs as regular scaled
            // bitmaps, but omit the metrics TLV that Apple does not emit here.
            tlv.extend(make_slices_tlv(self.width, self.height));
            tlv.extend(make_blend_opacity_tlv());
            tlv.extend(make_exif_orientation_tlv(1));
        } else if self.layout == LAYOUT_ONE_PART_SCALE {
            tlv.extend(make_slices_tlv(self.width, self.height));
            tlv.extend(make_metrics_tlv(self.width, self.height));
            tlv.extend(make_blend_opacity_tlv());
            tlv.extend(make_exif_orientation_tlv(1));
        }

        let mut rend_data = Vec::new();
        if !self.pixel_data.is_empty() {
            let deploy_ver = parse_version(&self.min_deploy);
            let lzfse_min = min_lzfse_version(&self.platform);
            let use_aligned = deploy_ver >= lzfse_min;
            // Apple compresses xcassets BGRA *and* GA8 renditions with deepmap2
            // when its encoder finds it beneficial — verified on iina, where
            // both Apple and we land on the same 7-deepmap2 / 110-lzfse split for
            // BGRA (the Accelerate `vImageDeepmap2` heuristic mirrors Apple's),
            // and Apple deepmap2's the two large grayscale GA8 icons (speed-dark)
            // that we used to force through KCBC. The `.icon` variant axis is the
            // exception: there Apple uses KCBC for *every* GA8/GA16 (feishin: 0
            // dmp2), so PART_ICON stays off deepmap2. tvOS brandasset flattened
            // and radiosity images also stay off deepmap2; the latter uses the
            // special "blurred" compression (comp=6) instead.
            let use_dmp2 = self.part != PART_ICON
                && !is_tvos_icon
                && (&self.pixel_format == b"BGRA" || &self.pixel_format == b" 8AG");

            let mut pixel_data = self.pixel_data.clone();
            if use_aligned {
                let dmp2_min = min_dmp2_version(&self.platform);
                let will_try_dmp2 =
                    use_dmp2 && deploy_ver >= dmp2_min && pixel_data.len() > 256;
                let actual_bpp: u32 = if &self.pixel_format == b"BGRA" { 4 } else { 2 };
                let padded_bpr = if will_try_dmp2 {
                    aligned_bytes_per_row(self.width, &self.pixel_format)
                } else {
                    ((self.width * actual_bpp + 31) / 32) * 32
                };
                let exact_bpr = self.width * actual_bpp;
                if padded_bpr != exact_bpr && self.height > 0 {
                    let mut padded =
                        Vec::with_capacity((padded_bpr * self.height) as usize);
                    let pad = padded_bpr - exact_bpr;
                    for row in 0..self.height as usize {
                        let s = row * exact_bpr as usize;
                        padded.extend_from_slice(&pixel_data[s..s + exact_bpr as usize]);
                        padded.extend(std::iter::repeat(0u8).take(pad as usize));
                    }
                    pixel_data = padded;
                }
            }

            let opaque = !self.force_non_opaque
                && check_opaque(&self.pixel_data, &self.pixel_format, self.width, self.height);
            rend_data = compress_data(
                &pixel_data,
                &self.pixel_format,
                self.width,
                self.height,
                &self.min_deploy,
                &self.platform,
                use_dmp2,
                false,
                opaque,
            );
            let actual_comp = if rend_data.len() >= 12 {
                u32::from_le_bytes(rend_data[8..12].try_into().unwrap())
            } else {
                0
            };
            let bpr = if actual_comp == 11 {
                aligned_bytes_per_row(self.width, &self.pixel_format)
            } else if use_aligned {
                let actual_bpp: u32 = if &self.pixel_format == b"BGRA" { 4 } else { 2 };
                ((self.width * actual_bpp + 31) / 32) * 32
            } else {
                self.width * 4
            };
            tlv.extend(tlv_header(0x03EF, 4));
            tlv.write_u32::<LittleEndian>(bpr).unwrap();
        }

        let mut intent = self.template_rendering_intent;
        if intent < 0 {
            intent = if self.is_template { 2 } else { 4 };
        }
        let mut flags = (intent as u32) << 2;
        if self.is_svg_rasterization {
            flags |= 0x04;
        }

        build_csi(
            self.width,
            self.height,
            scale_factor,
            &self.pixel_format,
            self.layout,
            &self.name,
            &tlv,
            &rend_data,
            flags,
            self.colorspace_id,
            if self.pixel_data.is_empty() { 0 } else { 1 },
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MultisizeImageEntry {
    pub width: u32,
    pub height: u32,
    pub index: u32,
}

pub fn build_multisize_rendition(
    name: &str,
    identifier: u16,
    entries: &[MultisizeImageEntry],
) -> Rendition {
    let mut msis_entries = Vec::new();
    for e in entries {
        msis_entries.write_u32::<LittleEndian>(e.width).unwrap();
        msis_entries.write_u32::<LittleEndian>(e.height).unwrap();
        msis_entries.write_u32::<LittleEndian>(e.index).unwrap();
    }
    let mut msis = Vec::new();
    msis.extend_from_slice(b"SISM");
    msis.write_u32::<LittleEndian>(1).unwrap();
    msis.write_u32::<LittleEndian>(entries.len() as u32).unwrap();
    msis.extend(msis_entries);

    let mut tlv = Vec::new();
    tlv.extend(make_color_blend_opacity_tlv());
    tlv.extend(make_exif_orientation_tlv(1));

    let csi = build_csi(
        0,
        0,
        0,
        b"\x00\x00\x00\x00",
        LAYOUT_MULTISIZE_IMAGE,
        name,
        &tlv,
        &msis,
        0,
        0,
        1,
    );

    let mut rend = Rendition {
        name: name.to_string(),
        identifier,
        element: ELEMENT_UNIVERSAL,
        part: PART_ICON_MULTISIZE,
        scale: 1,
        width: 0,
        height: 0,
        layout: LAYOUT_MULTISIZE_IMAGE,
        pixel_format: [0, 0, 0, 0],
        colorspace_id: 0,
        template_rendering_intent: 0,
        ..Default::default()
    };
    rend.csi_override = Some(csi);
    rend
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carheader_layout() {
        let hdr = make_carheader(5);
        assert_eq!(hdr.len(), 436);
        assert_eq!(&hdr[..4], b"RATC");
        assert_eq!(u32::from_le_bytes(hdr[16..20].try_into().unwrap()), 5);
        assert_eq!(u32::from_le_bytes(hdr[424..428].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(hdr[428..432].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(hdr[432..436].try_into().unwrap()), 1);
    }

    #[test]
    fn keyformat_block() {
        let kf = make_keyformat(&[7, 13, 1, 2, 3, 17, 11, 12]);
        assert_eq!(&kf[..4], b"tmfk");
        assert_eq!(u32::from_le_bytes(kf[8..12].try_into().unwrap()), 8);
    }

    #[test]
    fn rendition_key_shape() {
        let p = RenditionKeyParts {
            appearance: 0,
            unknown13: 0,
            element: 85,
            part: 181,
            size: 0,
            direction: 0,
            identifier: 7,
            dim1: 0,
            dim2: 0,
            layer: 0,
            scale: 2,
            idiom: 0,
            subtype: 0,
            variant: 0,
        };
        let k = make_rendition_key(p, &[7, 13, 1, 2, 3, 17, 11, 12]);
        assert_eq!(k.len(), 16);
        // position of "identifier" (token 17) is index 5 → bytes 10-12
        assert_eq!(u16::from_le_bytes(k[10..12].try_into().unwrap()), 7);
    }

    #[test]
    fn compute_keyformat_trims_unused() {
        struct R {
            direction: u32,
            dim1: u32,
            dim2: u32,
        }
        impl KeyformatRendition for R {
            fn direction(&self) -> u32 {
                self.direction
            }
            fn dim1(&self) -> u32 {
                self.dim1
            }
            fn dim2(&self) -> u32 {
                self.dim2
            }
        }
        let rs = vec![
            R {
                direction: 0,
                dim1: 0,
                dim2: 0,
            },
            R {
                direction: 0,
                dim1: 0,
                dim2: 0,
            },
        ];
        let kf = compute_keyformat(&rs, false);
        // None of the optional tokens (4, 8, 9) should appear
        for t in &[4, 8, 9] {
            assert!(!kf.contains(t), "expected {t} to be absent");
        }
    }

    #[test]
    fn compress_data_uncompressed_small() {
        let out = compress_data(
            &vec![0u8; 16],
            b"BGRA",
            2,
            2,
            "10.11",
            "macosx",
            false,
            false,
            false,
        );
        // 16 bytes of zeros shouldn't compress smaller than themselves
        assert_eq!(&out[..4], b"MLEC");
        let comp = u32::from_le_bytes(out[8..12].try_into().unwrap());
        assert_eq!(comp, 0);
    }

    #[test]
    fn compress_data_lzfse_at_10_11() {
        let out = compress_data(
            &vec![0u8; 1024],
            b"BGRA",
            16,
            16,
            "10.11",
            "macosx",
            false,
            false,
            false,
        );
        let comp = u32::from_le_bytes(out[8..12].try_into().unwrap());
        assert_eq!(comp, 4); // KCBC LZFSE
    }

    #[test]
    fn compress_data_dmp2_at_11_0_when_allowed() {
        if !deepmap2::is_available() {
            return;
        }
        let out = compress_data(
            &vec![0u8; 1024],
            b"BGRA",
            16,
            16,
            "11.0",
            "macosx",
            true,
            false,
            false,
        );
        let comp = u32::from_le_bytes(out[8..12].try_into().unwrap());
        assert_eq!(comp, 11);
    }

    #[test]
    fn build_color_csi_layout() {
        let out = build_color_csi("Accent", 1.0, 0.5, 0.0, 1.0, 1);
        assert_eq!(&out[..4], b"ISTC");
        let layout = u16::from_le_bytes(out[36..38].try_into().unwrap());
        assert_eq!(layout, LAYOUT_COLOR);
    }

    #[test]
    fn build_multisize_rendition_has_csi_override() {
        let r = build_multisize_rendition(
            "AppIcon",
            7,
            &[MultisizeImageEntry {
                width: 16,
                height: 16,
                index: 0,
            }],
        );
        assert!(r.csi_override.is_some());
        let csi = r.csi_override.as_ref().unwrap();
        // Rendition data contains "SISM" marker.
        assert!(csi.windows(4).any(|w| w == b"SISM"));
    }

    #[test]
    fn kcbc_header_counts_every_chunk() {
        // 256 rows split into 85-row chunks leaves a 1-row tail: four chunks,
        // not the three a fixed header would claim.
        let rows = 256u32;
        let pixels: Vec<u8> = (0..rows * 256 * 4).map(|i| (i % 251) as u8).collect();
        let out = compress_data(&pixels, b"BGRA", 256, rows, "11.0", "macosx", false, false, false);
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()), 4, "KCBC compression");
        assert_eq!(u32::from_le_bytes(out[12..16].try_into().unwrap()), 4, "chunk count");
        let kcbc_tags = out.windows(4).filter(|w| *w == b"KCBC").count();
        assert_eq!(kcbc_tags, 4);

        let divisible = compress_data(&pixels[..255 * 256 * 4], b"BGRA", 256, 255, "11.0", "macosx", false, false, false);
        assert_eq!(u32::from_le_bytes(divisible[12..16].try_into().unwrap()), 3);
    }

    fn word(tlv: &[u8], tag: u32) -> Vec<u8> {
        let mut p = 0;
        while p + 8 <= tlv.len() {
            let t = u32::from_le_bytes(tlv[p..p + 4].try_into().unwrap());
            let l = u32::from_le_bytes(tlv[p + 4..p + 8].try_into().unwrap()) as usize;
            if t == tag {
                return tlv[p + 8..p + 8 + l].to_vec();
            }
            p += 8 + l;
        }
        panic!("tag {tag:#x} missing");
    }

    fn csi_tlv(csi: &[u8]) -> &[u8] {
        let tlv_len = u32::from_le_bytes(csi[168..172].try_into().unwrap()) as usize;
        &csi[184..184 + tlv_len]
    }

    fn layer(part: u16, id: u16) -> LayerRef {
        LayerRef { part, identifier: id, group_style: None, glass: true, frame: None, opacity: 1.0 }
    }

    #[test]
    fn iconstack_group_entry_carries_translucency_and_shadow() {
        let mut group = layer(PART_ICON_GROUP, 7);
        group.group_style = Some(GroupStyle {
            translucency_enabled: true,
            translucency: 0.3,
            shadow_kind: 3,
            shadow_opacity: 0.5,
        });
        let csi = build_iconstack_csi("s.iconstack", 1024, &[layer(PART_ICON_GRADIENT, 1), group]);
        let t3fd = word(csi_tlv(&csi), 0x3FD);
        // count, gradient entry (20 zero bytes), then the group entry.
        assert_eq!(t3fd.len(), 48);
        assert!(t3fd[4..24].iter().all(|b| *b == 0));
        let entry = &t3fd[24..44];
        assert_eq!(u32::from_le_bytes(entry[4..8].try_into().unwrap()), 1);
        assert_eq!(f32::from_le_bytes(entry[8..12].try_into().unwrap()), 0.3);
        assert_eq!(u32::from_le_bytes(entry[12..16].try_into().unwrap()), 3);
        assert_eq!(f32::from_le_bytes(entry[16..20].try_into().unwrap()), 0.5);
        // 0x3FE: count + 12 zero bytes per layer + 4-byte trailer.
        assert_eq!(word(csi_tlv(&csi), 0x3FE).len(), 4 + 2 * 12 + 4);
    }

    #[test]
    fn icongroup_entry_records_frame_opacity_and_glass() {
        let mut l = layer(PART_REGULAR, 9);
        l.frame = Some((-11, -2, 1044, 1028));
        l.opacity = 0.0;
        l.glass = false;
        let csi = build_icongroup_csi("IconGroup", 1024, &[l]);
        let tlv = csi_tlv(&csi);
        let t3f4 = word(tlv, 0x3F4);
        let rd = |o: usize| i32::from_le_bytes(t3f4[o..o + 4].try_into().unwrap());
        assert_eq!((rd(12), rd(16), rd(20), rd(24)), (-11, -2, 1044, 1028));
        assert_eq!(f32::from_le_bytes(t3f4[32..36].try_into().unwrap()), 0.0);
        let t3fc = word(tlv, 0x3FC);
        assert_eq!(u32::from_le_bytes(t3fc[8..12].try_into().unwrap()), 0, "glass off");
        assert_eq!(word(tlv, 0x3FE).len(), 4 + 12 + 4);
    }
}
