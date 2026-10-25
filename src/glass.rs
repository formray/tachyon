//! Software "liquid glass" for layered windows: the capsule/panel shape is rasterised here into
//! a premultiplied BGRA surface with a soft drop shadow in the transparent padding, so the
//! window needs no DWM backdrop, frame or region. Pixels outside the shape and its shadow are
//! exactly alpha 0 (truly transparent and click-through under UpdateLayeredWindow).
//!
//! Pure Rust and host-independent, so the "no rectangle" guarantee is unit-tested.
use crate::aurora::Field;

/// Rounded rectangle in device pixels inside the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

impl Shape {
    /// Signed distance to the rounded rectangle (negative inside).
    pub fn sd(&self, px: f32, py: f32) -> f32 {
        let r = self.r.min(self.w / 2.0).min(self.h / 2.0).max(0.0);
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        let qx = (px - cx).abs() - (self.w / 2.0 - r);
        let qy = (py - cy).abs() - (self.h / 2.0 - r);
        let ox = qx.max(0.0);
        let oy = qy.max(0.0);
        (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
    }
}

/// Drop shadow: one tight contact layer and one soft key layer, both below the shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSpec {
    pub key_dy: f32,
    pub key_blur: f32,
    pub key_a: f32,
    pub near_dy: f32,
    pub near_blur: f32,
    pub near_a: f32,
}

/// Per-size cache: shape coverage and shadow alpha (0..255) for every surface pixel.
pub struct Geometry {
    pub w: usize,
    pub h: usize,
    pub shape: Shape,
    pub mask: Vec<u8>,
    pub shadow: Vec<u8>,
}

fn falloff(d: f32, blur: f32) -> f32 {
    if d <= 0.0 {
        1.0
    } else {
        let s = d / blur.max(0.5);
        (-2.6 * s * s).exp()
    }
}

impl Geometry {
    pub fn new(w: usize, h: usize, shape: Shape, sh: Option<ShadowSpec>) -> Self {
        let mut mask = vec![0u8; w * h];
        let mut shadow = vec![0u8; w * h];
        for y in 0..h {
            let py = y as f32 + 0.5;
            for x in 0..w {
                let px = x as f32 + 0.5;
                let d = shape.sd(px, py);
                let m = (0.5 - d).clamp(0.0, 1.0);
                mask[y * w + x] = (m * 255.0 + 0.5) as u8;
                if let Some(s) = sh {
                    if m < 1.0 {
                        let k = s.key_a * falloff(shape.sd(px, py - s.key_dy) + s.key_blur * 0.25, s.key_blur);
                        let n = s.near_a * falloff(shape.sd(px, py - s.near_dy) + s.near_blur * 0.25, s.near_blur);
                        let a = (1.0 - (1.0 - k) * (1.0 - n)) * (1.0 - m);
                        let v = (a * 255.0 + 0.5) as u8;
                        // the faintest tail is dropped so the padding is click-through almost everywhere
                        shadow[y * w + x] = if v < 3 { 0 } else { v };
                    }
                }
            }
        }
        Geometry { w, h, shape, mask, shadow }
    }
}

/// Where the blobs sit: a rectangle (device px) and per-blob centre/radii as fractions of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldLayout {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// (cx, cy, rx, ry) fractions of the rectangle, per blob
    pub blobs: [(f32, f32, f32, f32); 3],
    /// peak alpha of a blob at intensity 1
    pub peak: f32,
    /// per-blob extra multiplier (e.g. 0 to hide the load blobs on the rendering island)
    pub gain: [f32; 3],
}

/// Progress level (rendering island): a horizontal gradient from the shape's left edge to `end`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelFill {
    pub end: f32,
    pub c0: (u8, u8, u8),
    pub a0: f32,
    pub c1: (u8, u8, u8),
    pub a1: f32,
}

/// Glass body parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    /// glass tint (rgb, alpha)
    pub tint: (u8, u8, u8),
    pub tint_a: f32,
    pub field: Option<(Field, FieldLayout)>,
    pub level: Option<LevelFill>,
}

const STEP: usize = 6;
const CUT2: f32 = 1.69;
/// exp(-2.5 * CUT2)
const CUT_V: f32 = 0.014_63;

/// Draw shadow + glass body (tint, aurora field, progress level) into `out` (premultiplied BGRA,
/// `g.w * g.h * 4` bytes). Every byte is written: outside the shape and shadow it is 0,0,0,0.
pub fn compose_body(g: &Geometry, body: &Body, out: &mut [u8]) {
    let (w, h) = (g.w, g.h);
    debug_assert_eq!(out.len(), w * h * 4);
    // low-resolution field grid (premultiplied rgb + alpha), bilinear-sampled per pixel
    let gw = w / STEP + 2;
    let gh = h / STEP + 2;
    let grid: Vec<[f32; 4]> = match &body.field {
        Some((f, l)) => {
            let mut v = vec![[0f32; 4]; gw * gh];
            let rgb: [[f32; 3]; 3] = [f.b[0].c, f.b[1].c, f.b[2].c];
            for gy in 0..gh {
                for gx in 0..gw {
                    let (px, py) = ((gx * STEP) as f32, (gy * STEP) as f32);
                    let mut acc = [0f32; 4];
                    let mut keep = 1.0f32;
                    // the field ends softly at the bottom of its rectangle (CSS: inset bottom 0 + blur)
                    let bottom = l.y + l.h;
                    let vfade = if py <= bottom { 1.0 } else { (1.0 - (py - bottom) / (18.0 + 0.08 * l.h)).max(0.0) };
                    if vfade <= 0.0 {
                        continue;
                    }
                    for (i, rgb_i) in rgb.iter().enumerate() {
                        let (cx, cy, rx, ry) = l.blobs[i];
                        let dx = (px - (l.x + cx * l.w)) / (rx * l.w).max(1.0);
                        let dy = (py - (l.y + cy * l.h)) / (ry * l.h).max(1.0);
                        // gaussian, cut off at 1.3 radii so far pixels skip the field entirely
                        let d2 = dx * dx + dy * dy;
                        if d2 >= CUT2 {
                            continue;
                        }
                        let fall = ((-2.5 * d2).exp() - CUT_V) / (1.0 - CUT_V);
                        let a = (l.peak * f.b[i].k * l.gain[i] * fall * vfade).clamp(0.0, 1.0);
                        if a < 0.002 {
                            continue;
                        }
                        // successive "over" of each blob
                        for j in 0..3 {
                            acc[j] = rgb_i[j] * a + acc[j] * (1.0 - a);
                        }
                        keep *= 1.0 - a;
                    }
                    acc[3] = 1.0 - keep;
                    v[gy * gw + gx] = acc;
                }
            }
            v
        }
        None => vec![],
    };
    let ta = body.tint_a.clamp(0.0, 1.0);
    let tint = [body.tint.0 as f32 * ta, body.tint.1 as f32 * ta, body.tint.2 as f32 * ta];
    // plain glass pixel (fully inside, no field, no level): the common case, written as a constant
    let plain = [tint[2].round() as u8, tint[1].round() as u8, tint[0].round() as u8, (ta * 255.0).round() as u8];
    // grid cells whose four corners carry no field are skipped
    let cell_empty: Vec<bool> = if grid.is_empty() {
        vec![]
    } else {
        let mut v = vec![true; gw * gh];
        for gy in 0..gh - 1 {
            for gx in 0..gw - 1 {
                let a = |x: usize, y: usize| grid[y * gw + x][3];
                v[gy * gw + gx] = a(gx, gy).max(a(gx + 1, gy)).max(a(gx, gy + 1)).max(a(gx + 1, gy + 1)) < 0.002;
            }
        }
        v
    };
    let sx = g.shape.x;
    let level_end = body.level.map(|l| l.end).unwrap_or(f32::MIN);
    let mut row = vec![[0f32; 4]; if grid.is_empty() { 0 } else { gw }];
    for y in 0..h {
        let fy = y as f32 / STEP as f32;
        let gy0 = (fy as usize).min(gh - 2);
        let ty = fy - gy0 as f32;
        // vertical interpolation once per row; per pixel only the horizontal lerp is left
        for (gx, r) in row.iter_mut().enumerate() {
            let (a, b) = (&grid[gy0 * gw + gx], &grid[(gy0 + 1) * gw + gx]);
            for j in 0..4 {
                r[j] = a[j] + (b[j] - a[j]) * ty;
            }
        }
        for x in 0..w {
            let i = y * w + x;
            let o = &mut out[i * 4..i * 4 + 4];
            let m = g.mask[i];
            if m == 0 {
                o.copy_from_slice(&[0, 0, 0, g.shadow[i]]);
                continue;
            }
            let no_field = grid.is_empty() || cell_empty[gy0 * gw + (x / STEP).min(gw - 2)];
            if m == 255 && no_field && (x as f32 + 0.5) >= level_end {
                o.copy_from_slice(&plain);
                continue;
            }
            let s = g.shadow[i] as f32 / 255.0;
            let mut c = tint;
            let mut a = ta;
            if !no_field {
                let fx = x as f32 / STEP as f32;
                let gx0 = (fx as usize).min(gw - 2);
                let tx = fx - gx0 as f32;
                let (p0, p1) = (&row[gx0], &row[gx0 + 1]);
                let mut f = [0f32; 4];
                for j in 0..4 {
                    f[j] = p0[j] + (p1[j] - p0[j]) * tx;
                }
                // f rgb is already alpha-weighted (premultiplied)
                for j in 0..3 {
                    c[j] = f[j] + c[j] * (1.0 - f[3]);
                }
                a = f[3] + a * (1.0 - f[3]);
            }
            if let Some(l) = &body.level {
                let px = x as f32 + 0.5;
                if px < l.end {
                    let t = ((px - sx) / (l.end - sx).max(1.0)).clamp(0.0, 1.0);
                    let la = l.a0 + (l.a1 - l.a0) * t;
                    let lc = [
                        l.c0.0 as f32 + (l.c1.0 as f32 - l.c0.0 as f32) * t,
                        l.c0.1 as f32 + (l.c1.1 as f32 - l.c0.1 as f32) * t,
                        l.c0.2 as f32 + (l.c1.2 as f32 - l.c0.2 as f32) * t,
                    ];
                    for j in 0..3 {
                        c[j] = lc[j] * la + c[j] * (1.0 - la);
                    }
                    a = la + a * (1.0 - la);
                }
            }
            let mf = m as f32 / 255.0;
            let a = a * mf;
            let oa = a + s * (1.0 - a);
            // BGRA, premultiplied
            // `as u8` saturates; +0.5 rounds (f32::round is a libm call)
            o[0] = (c[2] * mf + 0.5) as u8;
            o[1] = (c[1] * mf + 0.5) as u8;
            o[2] = (c[0] * mf + 0.5) as u8;
            o[3] = (oa * 255.0 + 0.5) as u8;
        }
    }
}

/// Premultiplied source-over: `dst = src + dst * (1 - src.a)`.
pub fn over(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
        let sa = s[3] as u32;
        if sa == 0 {
            continue;
        }
        if sa == 255 {
            d.copy_from_slice(s);
            continue;
        }
        let k = 255 - sa;
        for j in 0..4 {
            d[j] = (s[j] as u32 + (d[j] as u32 * k + 127) / 255).min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aurora::target;

    fn island() -> (Geometry, Body) {
        let pad = 16.0;
        let shape = Shape { x: pad, y: pad, w: 300.0, h: 46.0, r: 23.0 };
        let sh = ShadowSpec { key_dy: 5.0, key_blur: 12.0, key_a: 0.30, near_dy: 1.0, near_blur: 3.0, near_a: 0.22 };
        let g = Geometry::new(332, 78, shape, Some(sh));
        let f = target(Some(84.0), Some(74.0), 80.0, 83.0);
        let l = FieldLayout {
            x: pad - 30.0,
            y: pad - 14.0,
            w: 360.0,
            h: 74.0,
            blobs: [(0.18, 0.55, 0.40, 0.70), (0.55, 0.40, 0.45, 0.80), (0.92, 0.60, 0.30, 0.70)],
            peak: 0.5,
            gain: [1.0; 3],
        };
        (g, Body { tint: (14, 16, 28), tint_a: 0.7, field: Some((f, l)), level: None })
    }

    #[test]
    fn signed_distance() {
        let s = Shape { x: 0.0, y: 0.0, w: 100.0, h: 40.0, r: 20.0 };
        assert!(s.sd(50.0, 20.0) < -19.0);
        assert!((s.sd(50.0, -5.0) - 5.0).abs() < 1e-4);
        // the corner of the bounding box is outside the capsule
        assert!(s.sd(0.5, 0.5) > 5.0);
    }

    #[test]
    fn no_rectangle_corners_and_far_padding_are_fully_transparent() {
        let (g, b) = island();
        let mut out = vec![7u8; g.w * g.h * 4];
        compose_body(&g, &b, &mut out);
        let px = |x: usize, y: usize| &out[(y * g.w + x) * 4..(y * g.w + x) * 4 + 4];
        // the four corners of the window rectangle and the capsule's own bounding-box corners
        for (x, y) in [(0, 0), (g.w - 1, 0), (0, g.h - 1), (g.w - 1, g.h - 1), (16, 16), (315, 16)] {
            assert_eq!(px(x, y), &[0, 0, 0, 0], "pixel {x},{y} must be transparent");
        }
        // inside the capsule: opaque-ish glass
        assert!(px(166, 39)[3] > 150);
        // shadow below the capsule is soft: some alpha, black (premultiplied rgb = 0)
        let s = px(166, 16 + 46 + 4);
        assert!(s[3] > 0 && s[3] < 120 && s[0] == 0, "{s:?}");
        // premultiplied invariant everywhere
        for p in out.as_chunks::<4>().0 {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3], "{p:?}");
        }
        // most of the padding is exactly transparent (click-through)
        let clear = out.as_chunks::<4>().0.iter().zip(&g.mask).filter(|(p, m)| **m == 0 && p[3] == 0).count();
        let pad_px = g.mask.iter().filter(|m| **m == 0).count();
        assert!(clear * 10 > pad_px * 4, "{clear} of {pad_px}");
    }

    #[test]
    fn shadow_is_never_drawn_under_the_glass() {
        let (g, _) = island();
        for (m, s) in g.mask.iter().zip(&g.shadow) {
            if *m == 255 {
                assert_eq!(*s, 0);
            }
        }
    }

    #[test]
    fn field_tints_the_glass_and_level_fills_to_progress() {
        let (g, mut b) = island();
        let mut plain = vec![0u8; g.w * g.h * 4];
        let mut tinted = plain.clone();
        let f = b.field;
        b.field = None;
        compose_body(&g, &b, &mut plain);
        b.field = f;
        compose_body(&g, &b, &mut tinted);
        assert_ne!(plain, tinted);
        b.field = None;
        b.level = Some(LevelFill { end: 16.0 + 150.0, c0: (60, 220, 255), a0: 0.1, c1: (170, 120, 255), a1: 0.6 });
        let mut lv = vec![0u8; g.w * g.h * 4];
        compose_body(&g, &b, &mut lv);
        let i = |x: usize| (39 * g.w + x) * 4;
        assert_ne!(lv[i(150)..i(150) + 4], plain[i(150)..i(150) + 4], "inside the level");
        assert_eq!(lv[i(250)..i(250) + 4], plain[i(250)..i(250) + 4], "beyond the level");
    }

    #[test]
    fn over_is_premultiplied_source_over() {
        let mut d = vec![0, 0, 200, 200, 10, 10, 10, 255];
        over(&mut d, &[0, 0, 0, 0, 100, 0, 0, 128]);
        assert_eq!(&d[..4], &[0, 0, 200, 200]);
        assert_eq!(d[4], 100 + 5);
        assert_eq!(d[7], 255);
    }
}
