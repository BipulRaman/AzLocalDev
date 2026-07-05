//! Generates the app's icon in memory (a rounded-square gradient mark with a centered
//! "play" triangle - the emulator's run/engine symbol), so we don't need to ship/maintain
//! a separate .ico/.png asset file. Used for the system tray icon.
//!
//! Rendered supersampled (4x) and box-filtered back down to the requested size, so edges
//! (both the rounded-rect background and the triangle) come out smooth/anti-aliased
//! instead of the old pixel-art glyph's hard, blocky edges.

/// How many sub-pixels per axis to render before downsampling, for anti-aliasing.
const SUPERSAMPLE: u32 = 4;

/// Renders the icon as raw RGBA8 pixels. Returns `(rgba, size)`.
pub fn render_rgba(size: u32) -> (Vec<u8>, u32) {
    let ss = SUPERSAMPLE;
    let big = size * ss;
    let big_f = big as f32;
    let radius = big_f * 0.22;

    // brand gradient: indigo -> violet (matches the web UI's accent gradient)
    let c1 = (0x4f as f32, 0x46 as f32, 0xe5 as f32);
    let c2 = (0x7c as f32, 0x3a as f32, 0xed as f32);

    // Centered "play" triangle. Optically centered by nudging it slightly right of the
    // true center (a triangle's visual weight sits left of its bounding box), the same
    // trick real play-button icons use.
    let cx = big_f * 0.47;
    let cy = big_f * 0.5;
    let tri_h = big_f * 0.46;
    let tri_w = big_f * 0.40;
    let top = (cx - tri_w * 0.5, cy - tri_h * 0.5);
    let bottom = (cx - tri_w * 0.5, cy + tri_h * 0.5);
    let tip = (cx + tri_w * 0.5, cy);

    let mut big_rgba = vec![0u8; (big * big * 4) as usize];
    for y in 0..big {
        for x in 0..big {
            let idx = ((y * big + x) * 4) as usize;
            let xf = x as f32 + 0.5;
            let yf = y as f32 + 0.5;

            if !inside_rounded_rect(xf, yf, big_f, big_f, radius) {
                continue; // leave fully transparent
            }

            let t = ((x + y) as f32) / (2.0 * big_f);
            let mut r = lerp(c1.0, c2.0, t);
            let mut g = lerp(c1.1, c2.1, t);
            let mut b = lerp(c1.2, c2.2, t);

            if inside_triangle((xf, yf), top, bottom, tip) {
                r = 255.0;
                g = 255.0;
                b = 255.0;
            }

            big_rgba[idx] = r as u8;
            big_rgba[idx + 1] = g as u8;
            big_rgba[idx + 2] = b as u8;
            big_rgba[idx + 3] = 255;
        }
    }

    (downsample(&big_rgba, big, ss), size)
}

/// Box-filters a `big`x`big` RGBA buffer down by a factor of `ss` per axis.
fn downsample(src: &[u8], big: u32, ss: u32) -> Vec<u8> {
    let size = big / ss;
    let mut out = vec![0u8; (size * size * 4) as usize];
    let n = (ss * ss) as u32;
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let px = x * ss + sx;
                    let py = y * ss + sy;
                    let idx = ((py * big + px) * 4) as usize;
                    r += src[idx] as u32;
                    g += src[idx + 1] as u32;
                    b += src[idx + 2] as u32;
                    a += src[idx + 3] as u32;
                }
            }
            let idx = ((y * size + x) * 4) as usize;
            out[idx] = (r / n) as u8;
            out[idx + 1] = (g / n) as u8;
            out[idx + 2] = (b / n) as u8;
            out[idx + 3] = (a / n) as u8;
        }
    }
    out
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Rounded-rectangle hit test: is point `(x, y)` inside a `w`x`h` rect with corner radius `r`?
fn inside_rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    let in_left = x < r;
    let in_right = x > w - r;
    let in_top = y < r;
    let in_bottom = y > h - r;

    if (in_left || in_right) && (in_top || in_bottom) {
        let cx = if in_left { r } else { w - r };
        let cy = if in_top { r } else { h - r };
        let dx = x - cx;
        let dy = y - cy;
        return dx * dx + dy * dy <= r * r;
    }
    true
}

/// Point-in-triangle test via barycentric sign comparison.
fn inside_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    fn sign(p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) -> f32 {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    }
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}
