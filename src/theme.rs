//! Aurora theme (v5): colours of the glass, the text, and the contrast rules that keep the text
//! readable. Tachyon draws its own glass (no DWM backdrop), so the worst case is a pure white
//! desktop seen through the tint plus the brightest point of the colour field.

pub type Rgb = (u8, u8, u8);

// accents (graphics: lines, chips, progress)
pub const CYAN: Rgb = (92, 225, 255);
pub const VIOLET: Rgb = (169, 139, 255);
pub const AMBER: Rgb = (255, 176, 72);
pub const RED: Rgb = (255, 86, 106);
pub const NEUTRAL: Rgb = (150, 170, 200);

// text
pub const TEXT: Rgb = (248, 250, 253);
/// Labels, units, secondary lines.
pub const TEXT2: Rgb = (218, 226, 240);
/// Readable text versions of the status accents.
pub const T_CYAN: Rgb = (170, 238, 255);
pub const T_AMBER: Rgb = (255, 216, 150);
pub const T_RED: Rgb = (255, 190, 198);
pub const T_VIOLET: Rgb = (220, 210, 255);
pub const T_NEUTRAL: Rgb = (200, 214, 236);
/// 1 px soft text shadow (alpha).
pub const TEXT_SHADOW_A: f32 = 0.42;

// glass
/// "tint" glass: deep, slightly blue smoked glass.
pub const TINT: Rgb = (14, 16, 30);
pub const TINT_A: f32 = 0.72;
/// When Windows "Transparency effects" is off: almost opaque.
pub const TINT_OPAQUE_A: f32 = 0.95;
/// "blur" glass: lighter, because the desktop behind is already blurred (and darkened) by Windows.
pub const BLUR_TINT: Rgb = (22, 24, 40);
pub const BLUR_TINT_A: f32 = 0.50;
/// Peak alpha of one colour-field blob at full intensity.
pub const FIELD_PEAK: f32 = 0.52;
/// Rows/chips: white veil.
pub const ROW_A: f32 = 0.08;
pub const DIVIDER_A: f32 = 0.14;

/// `top` with alpha `a` over `bottom`.
pub fn over(top: Rgb, a: f32, bottom: Rgb) -> Rgb {
    let m = |t: u8, b: u8| (t as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    (m(top.0, bottom.0), m(top.1, bottom.1), m(top.2, bottom.2))
}

/// Lighter version of a tag colour (chip gradients).
pub fn text_tint(c: Rgb) -> Rgb {
    over((255, 255, 255), 0.4, c)
}

/// "#RRGGBB" -> colour (grey when malformed).
pub fn hex(s: &str) -> Rgb {
    let h = s.trim().trim_start_matches('#');
    match u32::from_str_radix(h, 16) {
        Ok(v) if h.len() == 6 => ((v >> 16) as u8, (v >> 8) as u8, v as u8),
        _ => (124, 139, 161),
    }
}

/// WCAG relative luminance.
pub fn luminance(c: Rgb) -> f32 {
    let ch = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
}

/// WCAG contrast ratio.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The glass over a given desktop colour, with a field blob of colour `f` at full peak.
pub fn glass_over(desktop: Rgb, f: Option<Rgb>) -> Rgb {
    let g = over(TINT, TINT_A, desktop);
    match f {
        Some(c) => over(c, FIELD_PEAK, g),
        None => g,
    }
}

/// Every colour the field can reach at full intensity (load 0..100 %, cool to red-hot).
pub fn field_extremes() -> Vec<Rgb> {
    let mut v = vec![];
    for load in [0.0, 25.0, 50.0, 75.0, 100.0] {
        let f = crate::aurora::target(Some(load), Some(95.0), 80.0, 83.0);
        for i in 0..3 {
            v.push(f.rgb(i));
        }
    }
    v
}

pub const TEXT_COLOURS: [(&str, Rgb); 7] = [
    ("TEXT", TEXT),
    ("TEXT2", TEXT2),
    ("T_CYAN", T_CYAN),
    ("T_AMBER", T_AMBER),
    ("T_RED", T_RED),
    ("T_VIOLET", T_VIOLET),
    ("T_NEUTRAL", T_NEUTRAL),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wcag_reference_values() {
        assert!((contrast((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 0.01);
        assert!((contrast((119, 119, 119), (255, 255, 255)) - 4.48).abs() < 0.02);
    }

    #[test]
    fn primary_text_reads_on_a_white_desktop_even_under_the_brightest_field() {
        let white = (255, 255, 255);
        let plain = glass_over(white, None);
        for (name, c) in TEXT_COLOURS {
            assert!(contrast(c, plain) >= 4.5, "{name} on plain glass over white: {:.2}", contrast(c, plain));
        }
        for f in field_extremes() {
            let bg = glass_over(white, Some(f));
            assert!(contrast(TEXT, bg) >= 4.5, "TEXT over field {f:?} on white: {:.2}", contrast(TEXT, bg));
            // secondary text is helped by its 1 px shadow; 3:1 is the floor on a pure white desktop
            assert!(contrast(TEXT2, bg) >= 3.0, "TEXT2 over field {f:?} on white: {:.2}", contrast(TEXT2, bg));
        }
    }

    #[test]
    fn all_text_is_at_least_4_5_to_1_on_a_dark_or_mid_desktop() {
        for desk in [(0, 0, 0), (60, 60, 70), (128, 128, 128)] {
            for f in field_extremes() {
                let bg = glass_over(desk, Some(f));
                for (name, c) in TEXT_COLOURS {
                    assert!(contrast(c, bg) >= 4.5, "{name} over {f:?} on {desk:?}: {:.2}", contrast(c, bg));
                }
            }
        }
    }
}
