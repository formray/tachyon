//! The Aurora colour field: three soft blobs inside the glass. Hue follows GPU load, warmth
//! follows temperature. Pure data + easing (no drawing), so it is unit-tested on any host.
//!
//! Lightness rule: the field only animates when a value moves past a hysteresis step; small
//! jitters are applied silently on the next data tick (which redraws anyway). An ease lasts
//! `EASE_MS` and is the only time Tachyon draws between data ticks.

pub type Rgb = (u8, u8, u8);

/// Duration of a colour transition.
pub const EASE_MS: u64 = 600;
/// Load must move this many percentage points before the field eases to the new value.
pub const LOAD_STEP: f32 = 10.0;
/// Temperature must move this many degrees before the field eases.
pub const TEMP_STEP: f32 = 3.0;
/// At most one ease starts per this many ms; a bigger change that arrives sooner waits for a
/// later tick (the field is a mood light, not a gauge, so a short delay is invisible).
pub const MIN_GAP_MS: u64 = 5000;

/// One blob: colour and intensity (0..1, multiplies the blob's peak alpha).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blob {
    pub c: [f32; 3],
    pub k: f32,
}

/// The three blobs, left to right: load (cool), load (deep), heat (warm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub b: [Blob; 3],
}

impl Field {
    pub fn lerp(&self, o: &Field, t: f32) -> Field {
        let mut r = *self;
        for i in 0..3 {
            for j in 0..3 {
                r.b[i].c[j] = self.b[i].c[j] + (o.b[i].c[j] - self.b[i].c[j]) * t;
            }
            r.b[i].k = self.b[i].k + (o.b[i].k - self.b[i].k) * t;
        }
        r
    }
    pub fn rgb(&self, i: usize) -> Rgb {
        let c = self.b[i].c;
        (c[0].round().clamp(0.0, 255.0) as u8, c[1].round().clamp(0.0, 255.0) as u8, c[2].round().clamp(0.0, 255.0) as u8)
    }
}

/// HSV (h in degrees, s/v 0..1) to RGB floats 0..255.
pub fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [(r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0]
}

/// Hue of the load blobs: azure at idle, through cyan-blue, to violet at full load.
pub fn load_hue(load: f32) -> f32 {
    215.0 + 60.0 * (load / 100.0).clamp(0.0, 1.0)
}

/// Target field for a load (%) and temperature (°C). `amber`/`red` are the temperature thresholds.
/// Colours are deliberately deep (not pastel) so white text stays readable over a bright desktop.
pub fn target(load: Option<f32>, temp: Option<f32>, amber: f32, red: f32) -> Field {
    let l = load.unwrap_or(0.0).clamp(0.0, 100.0);
    let t = l / 100.0;
    let h = load_hue(l);
    // cool blob: cyan -> azure; deep blob: blue -> violet (both move with load)
    let a = Blob { c: hsv(188.0 + 22.0 * t, 1.0, 0.62), k: 0.40 + 0.60 * t };
    let b = Blob { c: hsv(h, 0.74, 0.80), k: 0.30 + 0.70 * t };
    // warmth: invisible when cool, amber from ~45 °C, red at the red threshold
    let tc = temp.unwrap_or(0.0);
    let start = 45.0f32.min(amber - 10.0);
    let w = ((tc - start) / (red - start).max(1.0)).clamp(0.0, 1.0);
    let hot = ((tc - amber) / (red - amber).max(1.0)).clamp(0.0, 1.0);
    let c = Blob { c: hsv(26.0 - 20.0 * hot, 0.84, 0.86), k: w };
    Field { b: [a, b, c] }
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Field state with hysteresis and a time-based ease (time in ms, supplied by the caller).
#[derive(Clone, Debug)]
pub struct Aurora {
    /// last values the field was aimed at (hysteresis anchors)
    load: Option<f32>,
    temp: Option<f32>,
    from: Field,
    to: Field,
    start_ms: u64,
    animating: bool,
    started_once: bool,
}

impl Aurora {
    pub fn new(load: Option<f32>, temp: Option<f32>, amber: f32, red: f32) -> Self {
        let f = target(load, temp, amber, red);
        Self { load, temp, from: f, to: f, start_ms: 0, animating: false, started_once: false }
    }

    /// Feed a data tick. Returns true if a new ease started (the caller then runs a short timer).
    pub fn update(&mut self, load: Option<f32>, temp: Option<f32>, amber: f32, red: f32, now_ms: u64) -> bool {
        let moved = |a: Option<f32>, b: Option<f32>, step: f32| match (a, b) {
            (Some(a), Some(b)) => (a - b).abs() >= step,
            (None, None) => false,
            _ => true,
        };
        let big = moved(load, self.load, LOAD_STEP) || moved(temp, self.temp, TEMP_STEP);
        if !big {
            // small jitter: no animation and no change (the field is a mood, not a gauge)
            return false;
        }
        if self.started_once && now_ms.saturating_sub(self.start_ms) < MIN_GAP_MS {
            return false;
        }
        self.started_once = true;
        let cur = self.current(now_ms).0;
        self.load = load;
        self.temp = temp;
        self.from = cur;
        self.to = target(load, temp, amber, red);
        self.start_ms = now_ms;
        self.animating = true;
        true
    }

    /// Field at `now_ms` and whether the ease is still running.
    pub fn current(&self, now_ms: u64) -> (Field, bool) {
        if !self.animating {
            return (self.to, false);
        }
        let t = now_ms.saturating_sub(self.start_ms) as f32 / EASE_MS as f32;
        if t >= 1.0 { (self.to, false) } else { (self.from.lerp(&self.to, ease(t)), true) }
    }

    /// Advance; call on each ease frame. Returns the field and whether more frames are needed.
    pub fn step(&mut self, now_ms: u64) -> (Field, bool) {
        let (f, more) = self.current(now_ms);
        if !more {
            self.animating = false;
        }
        (f, more)
    }

    pub fn animating(&self) -> bool {
        self.animating
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_follows_load_and_warmth_follows_temperature() {
        assert!(load_hue(0.0) < load_hue(50.0) && load_hue(50.0) < load_hue(100.0));
        let cool = target(Some(10.0), Some(40.0), 80.0, 83.0);
        let busy = target(Some(95.0), Some(40.0), 80.0, 83.0);
        assert!(busy.b[0].k > cool.b[0].k && busy.b[1].k > cool.b[1].k);
        assert_eq!(cool.b[2].k, 0.0, "no ember when cool");
        let warm = target(Some(50.0), Some(74.0), 80.0, 83.0);
        let hot = target(Some(50.0), Some(83.0), 80.0, 83.0);
        assert!(warm.b[2].k > 0.5 && hot.b[2].k == 1.0);
        // hotter = redder (less green)
        assert!(hot.b[2].c[1] < warm.b[2].c[1]);
        // busy load is violet-ish: more red than the idle azure
        assert!(busy.b[1].c[0] > cool.b[1].c[0]);
    }

    #[test]
    fn small_jitter_never_animates() {
        let mut a = Aurora::new(Some(80.0), Some(70.0), 80.0, 83.0);
        for (i, l) in [81.0, 79.0, 88.0, 72.5, 80.0].iter().enumerate() {
            assert!(!a.update(Some(*l), Some(72.5), 80.0, 83.0, i as u64 * 1000));
            assert!(!a.animating());
        }
    }

    #[test]
    fn big_change_eases_over_600_ms_then_stops() {
        let mut a = Aurora::new(Some(10.0), Some(40.0), 80.0, 83.0);
        let from = a.current(0).0;
        assert!(a.update(Some(90.0), Some(40.0), 80.0, 83.0, 1000));
        let (mid, more) = a.step(1300);
        assert!(more && mid != from);
        assert!(mid.b[0].k > from.b[0].k);
        let (end, more) = a.step(1000 + EASE_MS);
        assert!(!more && !a.animating());
        assert_eq!(end, target(Some(90.0), Some(40.0), 80.0, 83.0));
        // eases are rate-limited: a big change right after one waits, then eases from where it is
        let mut b = Aurora::new(Some(10.0), None, 80.0, 83.0);
        assert!(b.update(Some(90.0), None, 80.0, 83.0, 0));
        assert!(!b.update(Some(20.0), None, 80.0, 83.0, 1000), "too soon");
        let here = b.current(MIN_GAP_MS).0;
        assert!(b.update(Some(20.0), None, 80.0, 83.0, MIN_GAP_MS));
        assert_eq!(b.current(MIN_GAP_MS).0, here);
    }

    #[test]
    fn missing_values_are_handled() {
        let mut a = Aurora::new(None, None, 80.0, 83.0);
        assert!(!a.update(None, None, 80.0, 83.0, 0));
        assert!(a.update(Some(50.0), None, 80.0, 83.0, 10));
    }
}
