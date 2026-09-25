//! The Mammoth logo, rasterised in code (anti-aliased, any size).
//!
//! A rounded square with a blue→violet gradient holding three "lines of text" and a
//! caret. Dependency-free on purpose: `build.rs` includes this file to generate the
//! Windows `.ico` embedded in the executable.

const BRAND_A: [f32; 3] = [0x3d as f32, 0x7b as f32, 0xff as f32];
const BRAND_B: [f32; 3] = [0x8b as f32, 0x4d as f32, 0xf6 as f32];

/// Signed distance from `(px, py)` to a rounded rectangle centred at `(cx, cy)`.
fn rounded_rect(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - hx + r;
    let qy = (py - cy).abs() - hy + r;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// A horizontal pill from `x0` to `x1` centred on `y` with height `h`.
fn pill(px: f32, py: f32, x0: f32, x1: f32, y: f32, h: f32) -> f32 {
    rounded_rect(
        px,
        py,
        (x0 + x1) / 2.0,
        y,
        (x1 - x0) / 2.0,
        h / 2.0,
        h / 2.0,
    )
}

/// Colour (premultiplied-free RGBA, 0..1) of the logo at `(u, v)` in unit space.
fn sample(u: f32, v: f32, aa: f32) -> [f32; 4] {
    let cov = |d: f32| (0.5 - d / aa).clamp(0.0, 1.0);

    // Background: rounded square with a diagonal gradient and a soft top sheen.
    let bg = cov(rounded_rect(u, v, 0.5, 0.5, 0.46, 0.46, 0.2));
    if bg <= 0.0 {
        return [0.0; 4];
    }
    let t = ((u * 0.55 + v * 0.45) - 0.05).clamp(0.0, 1.0);
    let mut rgb = [0.0f32; 3];
    for i in 0..3 {
        rgb[i] = (BRAND_A[i] + (BRAND_B[i] - BRAND_A[i]) * t) / 255.0;
    }
    let sheen = ((0.45 - v) / 0.45).clamp(0.0, 1.0) * 0.12;
    for c in &mut rgb {
        *c = *c + (1.0 - *c) * sheen;
    }

    // Foreground: three text lines and a caret, in white.
    let h = 0.085;
    let fg = [
        cov(pill(u, v, 0.24, 0.76, 0.34, h)),
        cov(pill(u, v, 0.24, 0.56, 0.50, h)),
        cov(pill(u, v, 0.24, 0.68, 0.66, h)) * 0.75,
        cov(rounded_rect(u, v, 0.655, 0.50, 0.022, 0.095, 0.02)),
    ]
    .into_iter()
    .fold(0.0f32, f32::max);
    for c in &mut rgb {
        *c = *c + (1.0 - *c) * fg;
    }
    [rgb[0], rgb[1], rgb[2], bg]
}

/// Render the logo as straight-alpha RGBA8, `n`×`n` pixels, 4×4 supersampled.
pub fn render(n: usize) -> Vec<u8> {
    const SS: usize = 4;
    let mut out = vec![0u8; n * n * 4];
    let aa = 1.0 / n as f32;
    for y in 0..n {
        for x in 0..n {
            let mut acc = [0.0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / n as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / SS as f32) / n as f32;
                    let s = sample(u, v, aa);
                    // Accumulate premultiplied so edges blend correctly.
                    for i in 0..3 {
                        acc[i] += s[i] * s[3];
                    }
                    acc[3] += s[3];
                }
            }
            let k = (SS * SS) as f32;
            let a = acc[3] / k;
            let i = (y * n + x) * 4;
            if a > 0.0 {
                for c in 0..3 {
                    out[i + c] = ((acc[c] / k / a) * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

/// Encode several sizes of the logo as a Windows `.ico` (32-bit BMP entries).
#[allow(dead_code)]
pub fn ico(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&n| bmp_entry(n)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&n, img) in sizes.iter().zip(&images) {
        let dim = if n >= 256 { 0 } else { n as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

fn bmp_entry(n: usize) -> Vec<u8> {
    let rgba = render(n);
    let mask_row = n.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + n * n * 4 + mask_row * n);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(n as i32).to_le_bytes());
    out.extend_from_slice(&(2 * n as i32).to_le_bytes()); // colour + mask
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for y in (0..n).rev() {
        for x in 0..n {
            let i = (y * n + x) * 4;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out.extend(std::iter::repeat_n(0u8, mask_row * n));
    out
}
