//! IconComposer-style compositing via CoreGraphics.
//!
//! macOS-26 `.icon` sized renditions aren't the bare layer — they are the
//! layer composited over a gradient background and clipped to the macOS
//! "squircle" icon shape. We reproduce that here with the same CoreGraphics
//! rasterizer Apple's actool uses (so the gradient/mask antialiasing matches),
//! drawing the already-rasterized layer (see [`crate::svg_raster`]) on top.
//!
//! Geometry was measured from `/usr/bin/actool` output: the icon shape is
//! inset 100/1024 of the canvas on each side and the corners follow the
//! `SQUIRCLE_N` superellipse (n≈5, fitted against Apple's `.car` alpha mask —
//! a circular-arc rounded rect cuts the corners ~8-12 px too deep). The
//! glass/specular/shadow treatments Apple applies on top are only partly
//! reproduced (no public algorithm); the gradient-squircle background is.

use std::ffi::CString;
use std::os::raw::{c_char, c_double, c_void};
use std::sync::OnceLock;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: c_double,
    y: c_double,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: c_double,
    height: c_double,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type FnDeviceRgb = unsafe extern "C" fn() -> *mut c_void;
type FnBitmapCreate =
    unsafe extern "C" fn(*mut c_void, usize, usize, usize, usize, *mut c_void, u32) -> *mut c_void;
type FnBitmapData = unsafe extern "C" fn(*mut c_void) -> *mut u8;
type FnPathCreateMutable = unsafe extern "C" fn() -> *mut c_void;
type FnPathAddPoint = unsafe extern "C" fn(*mut c_void, *const c_void, c_double, c_double);
type FnCtxPath = unsafe extern "C" fn(*mut c_void, *mut c_void);
type FnCtx = unsafe extern "C" fn(*mut c_void);
type FnGradCreate =
    unsafe extern "C" fn(*mut c_void, *const c_double, *const c_double, usize) -> *mut c_void;
type FnDrawLinear = unsafe extern "C" fn(*mut c_void, *mut c_void, CGPoint, CGPoint, u32);
type FnProviderCreate =
    unsafe extern "C" fn(*mut c_void, *const c_void, usize, *const c_void) -> *mut c_void;
type FnImageCreate = unsafe extern "C" fn(
    usize,
    usize,
    usize,
    usize,
    usize,
    *mut c_void,
    u32,
    *mut c_void,
    *const c_double,
    bool,
    u32,
) -> *mut c_void;
type FnDrawImage = unsafe extern "C" fn(*mut c_void, CGRect, *mut c_void);
type FnColorCreate = unsafe extern "C" fn(*mut c_void, *const c_double) -> *mut c_void;
type FnSetShadow = unsafe extern "C" fn(*mut c_void, CGSize, c_double, *mut c_void);
type FnSetRgbFill = unsafe extern "C" fn(*mut c_void, c_double, c_double, c_double, c_double);
type FnRelease = unsafe extern "C" fn(*mut c_void);

struct Syms {
    device_rgb: FnDeviceRgb,
    bitmap_create: FnBitmapCreate,
    bitmap_data: FnBitmapData,
    path_create_mutable: FnPathCreateMutable,
    path_move: FnPathAddPoint,
    path_line: FnPathAddPoint,
    path_close: FnRelease,
    ctx_add_path: FnCtxPath,
    ctx_clip: FnCtx,
    ctx_save: FnCtx,
    ctx_restore: FnCtx,
    ctx_fill_path: FnCtx,
    grad_create: FnGradCreate,
    draw_linear: FnDrawLinear,
    provider_create: FnProviderCreate,
    image_create: FnImageCreate,
    draw_image: FnDrawImage,
    color_create: FnColorCreate,
    set_shadow: FnSetShadow,
    set_rgb_fill: FnSetRgbFill,
    path_release: FnRelease,
    grad_release: FnRelease,
    provider_release: FnRelease,
    image_release: FnRelease,
    color_release: FnRelease,
    ctx_release: FnRelease,
    cs_release: FnRelease,
}
unsafe impl Sync for Syms {}
unsafe impl Send for Syms {}

#[cfg(target_os = "macos")]
fn syms() -> Option<&'static Syms> {
    static CELL: OnceLock<Option<Syms>> = OnceLock::new();
    CELL.get_or_init(|| unsafe {
        let cg = dlopen(
            CString::new("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
                .unwrap()
                .as_ptr(),
            0x1,
        );
        if cg.is_null() {
            return None;
        }
        macro_rules! sym {
            ($name:expr, $ty:ty) => {{
                let n = CString::new($name).unwrap();
                let p = dlsym(cg, n.as_ptr());
                if p.is_null() {
                    return None;
                }
                std::mem::transmute::<_, $ty>(p)
            }};
        }
        Some(Syms {
            device_rgb: sym!("CGColorSpaceCreateDeviceRGB", FnDeviceRgb),
            bitmap_create: sym!("CGBitmapContextCreate", FnBitmapCreate),
            bitmap_data: sym!("CGBitmapContextGetData", FnBitmapData),
            path_create_mutable: sym!("CGPathCreateMutable", FnPathCreateMutable),
            path_move: sym!("CGPathMoveToPoint", FnPathAddPoint),
            path_line: sym!("CGPathAddLineToPoint", FnPathAddPoint),
            path_close: sym!("CGPathCloseSubpath", FnRelease),
            ctx_add_path: sym!("CGContextAddPath", FnCtxPath),
            ctx_clip: sym!("CGContextClip", FnCtx),
            ctx_save: sym!("CGContextSaveGState", FnCtx),
            ctx_restore: sym!("CGContextRestoreGState", FnCtx),
            ctx_fill_path: sym!("CGContextFillPath", FnCtx),
            grad_create: sym!("CGGradientCreateWithColorComponents", FnGradCreate),
            draw_linear: sym!("CGContextDrawLinearGradient", FnDrawLinear),
            provider_create: sym!("CGDataProviderCreateWithData", FnProviderCreate),
            image_create: sym!("CGImageCreate", FnImageCreate),
            draw_image: sym!("CGContextDrawImage", FnDrawImage),
            color_create: sym!("CGColorCreate", FnColorCreate),
            set_shadow: sym!("CGContextSetShadowWithColor", FnSetShadow),
            set_rgb_fill: sym!("CGContextSetRGBFillColor", FnSetRgbFill),
            path_release: sym!("CGPathRelease", FnRelease),
            grad_release: sym!("CGGradientRelease", FnRelease),
            provider_release: sym!("CGDataProviderRelease", FnRelease),
            image_release: sym!("CGImageRelease", FnRelease),
            color_release: sym!("CGColorRelease", FnRelease),
            ctx_release: sym!("CGContextRelease", FnRelease),
            cs_release: sym!("CGColorSpaceRelease", FnRelease),
        })
    })
    .as_ref()
}

#[cfg(not(target_os = "macos"))]
fn syms() -> Option<&'static Syms> {
    None
}

/// A two-stop linear gradient background. RGB components are device-RGB 0..1;
/// `start`/`stop` are normalized icon coordinates `[x, y]` with y measured
/// top-down (matching icon.json `orientation`).
pub struct GradientFill {
    pub start_rgb: [f64; 3],
    pub stop_rgb: [f64; 3],
    pub start: [f32; 2],
    pub stop: [f32; 2],
}

impl GradientFill {
    /// Sample the background gradient colour (device-RGB 0..1) at canvas pixel
    /// `(x, y)` for a `pixel_size²` rendition — the same projection
    /// `composite_icon` hands to CoreGraphics (clamped linear interpolation
    /// across the content rect). Used to reproduce the colour a frosted-glass
    /// layer multiplies, since the gradient is drawn under the layer stack.
    pub fn sample(&self, x: u32, y: u32, pixel_size: u32) -> [f64; 3] {
        self.sample_with_margin(x, y, pixel_size, MARGIN_RATIO)
    }

    /// As [`sample`](Self::sample), for a content rect inset by `margin_ratio`
    /// of the canvas on each side (0 for full-bleed iOS icons).
    pub fn sample_with_margin(
        &self,
        x: u32,
        y: u32,
        pixel_size: u32,
        margin_ratio: f64,
    ) -> [f64; 3] {
        let margin = pixel_size as f64 * margin_ratio;
        let content = (pixel_size as f64 - 2.0 * margin).max(1.0);
        // Normalize the pixel into the content rect, matching `to_ctx`.
        let nx = (x as f64 + 0.5 - margin) / content;
        // Pixel rows run top-down but `composite_icon` draws in a bottom-up
        // CoreGraphics context, where the gradient's first stop sits at high y.
        let ny = 1.0 - (y as f64 + 0.5 - margin) / content;
        let (sx, sy) = (self.start[0] as f64, self.start[1] as f64);
        let (ex, ey) = (self.stop[0] as f64, self.stop[1] as f64);
        let (dx, dy) = (ex - sx, ey - sy);
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= 0.0 {
            0.0
        } else {
            (((nx - sx) * dx + (ny - sy) * dy) / len2).clamp(0.0, 1.0)
        };
        [
            self.start_rgb[0] + (self.stop_rgb[0] - self.start_rgb[0]) * t,
            self.start_rgb[1] + (self.stop_rgb[1] - self.start_rgb[1]) * t,
            self.start_rgb[2] + (self.stop_rgb[2] - self.start_rgb[2]) * t,
        ]
    }
}

/// A drop shadow cast by the icon squircle. Drawn before the icon is clipped,
/// so it bleeds into the surrounding margin. See `icon_effects::shadow_geometry`
/// for the measured defaults.
pub struct ShadowParams {
    /// Straight (non-premultiplied) device-RGB colour + alpha.
    pub color: [f64; 4],
    /// Gaussian blur radius, pixels.
    pub blur: f64,
    /// Offset in pixels `(x, y)`; positive `y` is downward on screen.
    pub offset: [f64; 2],
}

/// Icon-shape geometry as a fraction of the canvas edge, measured from Apple's
/// 1024px output: 100px inset; corner shape is the `SQUIRCLE_N` superellipse.
const MARGIN_RATIO: f64 = 100.0 / 1024.0;

/// macOS app-icon shape is a squircle — a superellipse |x/a|ⁿ + |y/a|ⁿ = 1, not
/// a circular-arc rounded rect. Fitting Apple's `.car` alpha boundary (the icon
/// mask of scrumdinger's 1024px rendition) gives n ≈ 5.0 to ≈2 px, vs ≈7 px for
/// the best circular radius — a circular corner cuts ~8-12 px deeper into the
/// corner than Apple's, which showed up as a bright corner ring in the variant
/// GA8 diff. See `tools/fit_corner` / the corner-fit probe.
const SQUIRCLE_N: f64 = 5.0;

/// Build the icon-shape squircle as a polygon CGPath inscribed in `rect` (a
/// square). The superellipse is sampled densely enough that the polygon is
/// sub-pixel at the rendition size. Caller owns the returned path.
unsafe fn build_squircle_path(s: &Syms, rect: CGRect) -> *mut c_void {
    let a = rect.size.width / 2.0;
    let cx = rect.origin.x + a;
    let cy = rect.origin.y + a;
    // Curvature concentrates near the corners; sample by edge length so even
    // small renditions stay smooth there.
    let steps = ((rect.size.width as usize).max(256)).min(2048);
    let path = (s.path_create_mutable)();
    let e = 2.0 / SQUIRCLE_N;
    for i in 0..steps {
        let t = (i as f64) / (steps as f64) * std::f64::consts::TAU;
        let (st, ct) = t.sin_cos();
        let x = cx + a * ct.signum() * ct.abs().powf(e);
        let y = cy + a * st.signum() * st.abs().powf(e);
        if i == 0 {
            (s.path_move)(path, std::ptr::null(), x, y);
        } else {
            (s.path_line)(path, std::ptr::null(), x, y);
        }
    }
    (s.path_close)(path);
    path
}

/// Composite a sized rendition: the gradient background and the supplied layer
/// (canvas-sized premultiplied-first BGRA), both clipped to the icon squircle,
/// optionally preceded by a drop shadow. Returns premultiplied-first BGRA of
/// `pixel_size²`, or `None` if CoreGraphics is unavailable.
pub fn composite_icon(
    pixel_size: u32,
    gradient: &GradientFill,
    layer_bgra: &[u8],
    shadow: Option<&ShadowParams>,
) -> Option<Vec<u8>> {
    let s = syms()?;
    let size = pixel_size as usize;
    if layer_bgra.len() != size * size * 4 {
        return None;
    }
    let margin = pixel_size as f64 * MARGIN_RATIO;
    let content = pixel_size as f64 - 2.0 * margin;

    unsafe {
        let cs = (s.device_rgb)();
        const PREMUL_FIRST: u32 = 2;
        const LE32: u32 = 2 << 12;
        let info = PREMUL_FIRST | LE32;
        let ctx = (s.bitmap_create)(
            std::ptr::null_mut(),
            size,
            size,
            8,
            size * 4,
            cs,
            info,
        );
        if ctx.is_null() {
            (s.cs_release)(cs);
            return None;
        }

        let rect = CGRect {
            origin: CGPoint { x: margin, y: margin },
            size: CGSize { width: content, height: content },
        };
        let path = build_squircle_path(s, rect);

        // Drop shadow: fill the squircle opaque with the shadow set, so the
        // blurred copy bleeds into the margin. The opaque fill inside is then
        // overpainted by the gradient. Done before clipping (the shadow lives
        // outside the shape).
        if let Some(sh) = shadow {
            (s.ctx_save)(ctx);
            let color = (s.color_create)(cs, sh.color.as_ptr());
            // Context is bottom-up, so a downward screen offset is negative y.
            let off = CGSize { width: sh.offset[0], height: -sh.offset[1] };
            (s.set_shadow)(ctx, off, sh.blur, color);
            (s.set_rgb_fill)(ctx, 1.0, 1.0, 1.0, 1.0);
            (s.ctx_add_path)(ctx, path);
            (s.ctx_fill_path)(ctx);
            (s.color_release)(color);
            (s.ctx_restore)(ctx);
        }

        // Clip everything else to the squircle icon shape.
        (s.ctx_add_path)(ctx, path);
        (s.ctx_clip)(ctx);

        // Background gradient — two opaque stops.
        let comps: [c_double; 8] = [
            gradient.start_rgb[0],
            gradient.start_rgb[1],
            gradient.start_rgb[2],
            1.0,
            gradient.stop_rgb[0],
            gradient.stop_rgb[1],
            gradient.stop_rgb[2],
            1.0,
        ];
        let locs: [c_double; 2] = [0.0, 1.0];
        let grad = (s.grad_create)(cs, comps.as_ptr(), locs.as_ptr(), 2);
        // icon.json orientation y and this context share a top-down y axis
        // (y=1 is the top edge), matching the layer draw — map straight through.
        let to_ctx = |p: [f32; 2]| CGPoint {
            x: margin + p[0] as f64 * content,
            y: margin + p[1] as f64 * content,
        };
        // DrawsBeforeStartLocation | DrawsAfterEndLocation = 3 (extend ends).
        (s.draw_linear)(ctx, grad, to_ctx(gradient.start), to_ctx(gradient.stop), 3);
        (s.grad_release)(grad);

        // Layer on top, drawn at full canvas size. Our rasterized BGRA buffer
        // and the gradient already share the context's orientation, so the
        // CGImage lands aligned without an extra flip.
        let provider = (s.provider_create)(
            std::ptr::null_mut(),
            layer_bgra.as_ptr() as *const c_void,
            layer_bgra.len(),
            std::ptr::null(),
        );
        let img = (s.image_create)(
            size,
            size,
            8,
            32,
            size * 4,
            cs,
            info,
            provider,
            std::ptr::null(),
            false,
            0,
        );
        if !img.is_null() {
            let full = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize { width: size as f64, height: size as f64 },
            };
            (s.ctx_save)(ctx);
            (s.draw_image)(ctx, full, img);
            (s.ctx_restore)(ctx);
            (s.image_release)(img);
        }
        (s.provider_release)(provider);

        let mut data =
            std::slice::from_raw_parts((s.bitmap_data)(ctx), size * size * 4).to_vec();
        (s.path_release)(path);
        (s.ctx_release)(ctx);
        (s.cs_release)(cs);
        apply_icon_lighting(&mut data, size);
        Some(data)
    }
}

#[inline]
fn srgb_to_lin(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn lin_to_srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Approximate signed distance (px) to the icon superellipse |x/a|ⁿ+|y/a|ⁿ=1,
/// negative inside. A superellipse has no closed-form SDF, so use the first-order
/// estimate `(g−1)/|∇g|` where `g=(|x/a|ⁿ+|y/a|ⁿ)^(1/n)` — exact on the boundary
/// and accurate within the thin band the edge lighting touches. Must match the
/// clip shape (`build_squircle_path`), or the highlight lands off the corners.
#[inline]
fn squircle_sdf(px: f64, py: f64, center: f64, half: f64, n: f64) -> f64 {
    let nx = ((px - center).abs() / half).max(1e-9);
    let ny = ((py - center).abs() / half).max(1e-9);
    let denom = nx.powf(n) + ny.powf(n);
    let g = denom.powf(1.0 / n);
    // |∇g| in pixel units (chain rule through the /half normalization).
    let pow = denom.powf(1.0 / n - 1.0);
    let dgx = pow * nx.powf(n - 1.0) / half;
    let dgy = pow * ny.powf(n - 1.0) / half;
    let gl = (dgx * dgx + dgy * dgy).sqrt().max(1e-12);
    (g - 1.0) / gl
}

/// Apple's icon-frame "glass-tile" lighting: a soft white light added along the
/// inner edge of the squircle, brightest top-left, fading inward over ~16 px.
/// Measured (`tools/probe_icon_lighting.py`) as an additive light in *linear*
/// space (≈constant across fills); top/left L0 ≈ 0.34, bottom/right ≈ 0.23, with
/// a linear depth falloff. Applied to the opaque interior of the composited icon
/// (premultiplied-first BGRA; interior α = 255, so RGB is straight there).
fn apply_icon_lighting(data: &mut [u8], size: usize) {
    let sz = size as f64;
    let center = sz / 2.0;
    let half = (sz - 2.0 * sz * MARGIN_RATIO) / 2.0;
    let depth = 16.5 * sz / 1024.0;
    if depth < 1.0 {
        return;
    }
    const L0_BASE: f64 = 0.286;
    const L0_DIR: f64 = 0.083;
    let sqrt2 = std::f64::consts::SQRT_2;
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            if data[i + 3] < 250 {
                continue;
            }
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let dist = -squircle_sdf(px, py, center, half, SQUIRCLE_N);
            if dist < 0.0 || dist >= depth {
                continue;
            }
            // Outward normal via central difference of the SDF.
            let gx = squircle_sdf(px + 1.0, py, center, half, SQUIRCLE_N)
                - squircle_sdf(px - 1.0, py, center, half, SQUIRCLE_N);
            let gy = squircle_sdf(px, py + 1.0, center, half, SQUIRCLE_N)
                - squircle_sdf(px, py - 1.0, center, half, SQUIRCLE_N);
            let gl = (gx * gx + gy * gy).sqrt().max(1e-6);
            let dir = (-gx / gl - gy / gl) / sqrt2; // top-left → +1
            let l0 = L0_BASE + L0_DIR * dir;
            let dl = l0 * (1.0 - dist / depth);
            for c in 0..3 {
                let lin = srgb_to_lin(data[i + c] as f64 / 255.0) + dl;
                data[i + c] = (lin_to_srgb(lin.min(1.0)) * 255.0).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_masks_to_squircle() {
        let size = 256u32;
        // Fully transparent layer → output is just the gradient, squircle-clipped.
        let layer = vec![0u8; (size * size * 4) as usize];
        let fill = GradientFill {
            start_rgb: [0.5, 0.5, 0.5],
            stop_rgb: [0.9, 0.9, 0.9],
            start: [0.5, 0.0],
            stop: [0.5, 1.0],
        };
        let Some(out) = composite_icon(size, &fill, &layer, None) else {
            // CoreGraphics unavailable (non-macOS CI) — nothing to assert.
            return;
        };
        assert_eq!(out.len(), (size * size * 4) as usize);
        let alpha = |x: u32, y: u32| out[((y * size + x) * 4 + 3) as usize];
        // Corner (well outside the inset squircle) is clipped away.
        assert_eq!(alpha(2, 2), 0, "corner must be transparent");
        // Center is inside the icon shape and opaque.
        assert_eq!(alpha(size / 2, size / 2), 255, "center must be opaque");
    }

    #[test]
    fn icon_lighting_brightens_inner_edge() {
        // Flat mid-grey fill: the glass-tile lighting must brighten the inner
        // edge (top-left strongest) and leave the centre alone.
        let size = 256u32;
        let layer = vec![0u8; (size * size * 4) as usize];
        let fill = GradientFill {
            start_rgb: [0.55, 0.55, 0.55],
            stop_rgb: [0.55, 0.55, 0.55],
            start: [0.5, 0.0],
            stop: [0.5, 1.0],
        };
        let Some(out) = composite_icon(size, &fill, &layer, None) else {
            return; // CoreGraphics unavailable
        };
        let g = |x: u32, y: u32| out[((y * size + x) * 4 + 1) as usize] as i32;
        let margin = (size as f64 * MARGIN_RATIO).round() as u32;
        let centre = g(size / 2, size / 2);
        let top_edge = g(size / 2, margin + 1);
        let bot_edge = g(size / 2, size - margin - 2);
        assert!(top_edge > centre + 5, "top inner edge should brighten ({top_edge} vs {centre})");
        assert!(bot_edge > centre + 3, "bottom inner edge should brighten ({bot_edge} vs {centre})");
        assert!(top_edge >= bot_edge, "top should be ≥ bottom (light from top)");
    }
}
