//! VRAM forecast: a robust trend (Theil-Sen median slope) of used video memory, for the card
//! and for each process the driver reports, and a warning only when the trend is clear and the
//! card would fill up soon. Also flags idle VRAM hogs: background apps (browsers, chat, launchers)
//! holding a lot of video memory without using the GPU.
use crate::telemetry::Sample;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};

/// Seconds of history the trend looks at.
pub const SPAN_S: f64 = 120.0;
/// Minimum seconds of history before any forecast.
pub const MIN_SPAN_S: f64 = 30.0;
/// Only warn when the card would be full within this many seconds.
pub const HORIZON_S: f64 = 15.0 * 60.0;
/// A hog must hold this much VRAM...
pub const HOG_MB: f32 = 400.0;
/// ...while idle for this long.
pub const HOG_IDLE_S: f64 = 60.0;

/// Process names (lower case) that are background apps even when the category rules say "Other".
const BACKGROUND: [&str; 14] = [
    "discord.exe",
    "steamwebhelper.exe",
    "spotify.exe",
    "ms-teams.exe",
    "teams.exe",
    "slack.exe",
    "zoom.exe",
    "whatsapp.exe",
    "telegram.exe",
    "epicgameslauncher.exe",
    "msedgewebview2.exe",
    "battle.net.exe",
    "galaxyclient.exe",
    "nvidia overlay.exe",
];

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Trend {
    /// MB per second (median of pairwise slopes).
    pub slope_mbs: f64,
    /// Fraction of pairwise slopes with the same sign as the median: 1.0 = perfectly steady.
    pub agreement: f64,
    pub points: usize,
}

/// Theil-Sen estimate over (t seconds, value) points. At most `max_pts` are used (evenly
/// thinned) so the cost stays O(max_pts^2).
pub fn theil_sen(pts: &[(f64, f64)], max_pts: usize) -> Option<Trend> {
    if pts.len() < 3 {
        return None;
    }
    let step = pts.len().div_ceil(max_pts.max(3));
    let p: Vec<(f64, f64)> = pts.iter().step_by(step.max(1)).copied().collect();
    let mut slopes = Vec::with_capacity(p.len() * p.len() / 2);
    for i in 0..p.len() {
        for j in i + 1..p.len() {
            let dt = p[j].0 - p[i].0;
            if dt > 0.0 {
                slopes.push((p[j].1 - p[i].1) / dt);
            }
        }
    }
    if slopes.is_empty() {
        return None;
    }
    slopes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = slopes[slopes.len() / 2];
    let same = slopes.iter().filter(|s| (**s > 0.0) == (m > 0.0) && **s != 0.0).count();
    Some(Trend { slope_mbs: m, agreement: same as f64 / slopes.len() as f64, points: p.len() })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hog {
    pub name: String,
    pub category: String,
    pub vram_mb: f32,
    pub idle_secs: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Forecast {
    pub used_mb: Option<f32>,
    pub total_mb: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend: Option<Trend>,
    /// Seconds until full at the current rate, when confident.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secs_to_full: Option<f64>,
    /// The process growing fastest, with its slope in MB/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub growing: Option<(String, f64)>,
    /// One plain sentence when there is something to warn about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub hogs: Vec<Hog>,
}

impl Forecast {
    /// Plain sentence about idle hogs, e.g. "chrome.exe (Browser) holds 1.2 GB of VRAM without using the GPU."
    pub fn hog_sentence(&self) -> Option<String> {
        let h = self.hogs.first()?;
        let more = match self.hogs.len() {
            1 => String::new(),
            2 => format!(" So does {}.", self.hogs[1].name),
            n => format!(" {} other background apps do too.", n - 1),
        };
        Some(format!("{} ({}) holds {:.1} GB of VRAM without using the GPU.{more}", h.name, h.category, h.vram_mb / 1024.0))
    }
}

/// "~3 min", "under a minute", "~1 h 10 min".
pub fn fmt_eta(secs: f64) -> String {
    if secs < 60.0 {
        return "under a minute".into();
    }
    let m = (secs / 60.0).round() as u64;
    if m < 60 { format!("~{m} min") } else { format!("~{} h {} min", m / 60, m % 60) }
}

struct ProcTrack {
    name: String,
    category: String,
    pts: VecDeque<(f64, f64)>,
    idle_since: Option<f64>,
    last_seen: f64,
    last_mb: f32,
}

#[derive(Default)]
pub struct Forecaster {
    total: VecDeque<(f64, f64)>,
    procs: HashMap<u32, ProcTrack>,
    t0: Option<i64>,
    confident_n: u32,
    calm_n: u32,
    warning_on: bool,
    last: Forecast,
    tick: u32,
    /// Warning horizon in seconds; None = `HORIZON_S`, Some(0) = no warnings.
    horizon: Option<f64>,
}

fn is_background(name: &str, category: &str) -> bool {
    let n = name.to_ascii_lowercase();
    category.eq_ignore_ascii_case("Browser") || BACKGROUND.contains(&n.as_str())
}

impl Forecaster {
    /// Add a sample; the forecast is re-evaluated every `every` samples (cheap either way).
    pub fn push(&mut self, s: &Sample, every: u32) -> &Forecast {
        let t0 = *self.t0.get_or_insert(s.t_ms);
        let t = (s.t_ms - t0) as f64 / 1000.0;
        if let Some(u) = s.vram_mb {
            self.total.push_back((t, u as f64));
        }
        while self.total.front().is_some_and(|(a, _)| t - a > SPAN_S) {
            self.total.pop_front();
        }
        for p in &s.procs {
            let tr = self.procs.entry(p.pid).or_insert_with(|| ProcTrack {
                name: p.name.clone(),
                category: p.category.clone(),
                pts: VecDeque::new(),
                idle_since: None,
                last_seen: t,
                last_mb: 0.0,
            });
            tr.last_seen = t;
            if let Some(mb) = p.vram_mb {
                tr.pts.push_back((t, mb as f64));
                tr.last_mb = mb;
            }
            while tr.pts.front().is_some_and(|(a, _)| t - a > SPAN_S) {
                tr.pts.pop_front();
            }
            let busy = p.sm.unwrap_or(0) > 2 || p.enc.unwrap_or(0) > 2 || p.dec.unwrap_or(0) > 2;
            tr.idle_since = if busy { None } else { tr.idle_since.or(Some(t)) };
        }
        self.procs.retain(|_, p| t - p.last_seen < 10.0);
        self.tick = self.tick.wrapping_add(1);
        if self.tick.is_multiple_of(every.max(1)) {
            self.evaluate(s, t);
        } else {
            self.last.used_mb = s.vram_mb;
            self.last.total_mb = s.vram_total_mb;
        }
        &self.last
    }

    /// Warn when VRAM would be full within `minutes` (0 = never warn). Applies from the next evaluation.
    pub fn set_horizon_minutes(&mut self, minutes: u32) {
        self.horizon = Some(minutes as f64 * 60.0);
    }

    fn horizon_s(&self) -> f64 {
        self.horizon.unwrap_or(HORIZON_S)
    }

    pub fn current(&self) -> &Forecast {
        &self.last
    }

    fn evaluate(&mut self, s: &Sample, t: f64) {
        let mut f = Forecast { used_mb: s.vram_mb, total_mb: s.vram_total_mb, ..Default::default() };
        let span = self.total.back().zip(self.total.front()).map(|(b, a)| b.0 - a.0).unwrap_or(0.0);
        let pts: Vec<(f64, f64)> = self.total.iter().copied().collect();
        f.trend = if span >= MIN_SPAN_S { theil_sen(&pts, 60) } else { None };
        // fastest-growing process (only where the driver reports per-process VRAM)
        f.growing = self
            .procs
            .values()
            .filter(|p| p.pts.len() >= 10)
            .filter_map(|p| {
                let v: Vec<(f64, f64)> = p.pts.iter().copied().collect();
                theil_sen(&v, 40).filter(|tr| tr.slope_mbs > 1.0 && tr.agreement >= 0.7).map(|tr| (p.name.clone(), tr.slope_mbs))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let confident = match (f.trend, s.vram_mb, s.vram_total_mb) {
            (Some(tr), Some(u), Some(tot)) if tr.slope_mbs >= 5.0 && tr.agreement >= 0.8 && tot > u => {
                let secs = (tot - u) as f64 / tr.slope_mbs;
                (secs <= self.horizon_s()).then_some(secs)
            }
            _ => None,
        };
        // hysteresis: two confident evaluations to warn, two calm ones to stop
        if confident.is_some() {
            self.confident_n += 1;
            self.calm_n = 0;
        } else {
            self.calm_n += 1;
            self.confident_n = 0;
        }
        if self.confident_n >= 2 {
            self.warning_on = true;
        } else if self.calm_n >= 2 {
            self.warning_on = false;
        }
        if let (true, Some(secs)) = (self.warning_on, confident.or(self.last.secs_to_full)) {
            f.secs_to_full = Some(secs);
            let who = f.growing.as_ref().map(|(n, mbs)| format!(" ({n} is growing by {mbs:.0} MB/s)")).unwrap_or_default();
            f.warning = Some(format!("At this rate VRAM will be full in {}{who}.", fmt_eta(secs)));
        }
        let mut hogs: Vec<Hog> = self
            .procs
            .values()
            .filter(|p| p.last_mb >= HOG_MB && is_background(&p.name, &p.category))
            .filter_map(|p| {
                let idle = t - p.idle_since?;
                (idle >= HOG_IDLE_S).then(|| Hog {
                    name: p.name.clone(),
                    category: p.category.clone(),
                    vram_mb: p.last_mb,
                    idle_secs: idle,
                })
            })
            .collect();
        hogs.sort_by(|a, b| b.vram_mb.partial_cmp(&a.vram_mb).unwrap_or(std::cmp::Ordering::Equal));
        f.hogs = hogs;
        self.last = f;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::Proc;

    fn s(t: i64, used: f32, procs: Vec<Proc>) -> Sample {
        Sample { t_ms: t * 1000, vram_mb: Some(used), vram_total_mb: Some(16000.0), procs, ..Default::default() }
    }

    fn py(mb: f32, sm: u8) -> Proc {
        Proc { pid: 7, name: "python.exe".into(), category: "AI/agent".into(), vram_mb: Some(mb), sm: Some(sm), ..Default::default() }
    }

    #[test]
    fn theil_sen_ignores_outliers() {
        let mut pts: Vec<(f64, f64)> = (0..60).map(|i| (i as f64, 1000.0 + 10.0 * i as f64)).collect();
        pts[20].1 = 9000.0; // a spike
        pts[40].1 = 0.0; // a dropout
        let tr = theil_sen(&pts, 60).unwrap();
        assert!((tr.slope_mbs - 10.0).abs() < 0.01, "{tr:?}");
        assert!(tr.agreement > 0.9);
        assert!(theil_sen(&pts[..2], 60).is_none());
    }

    #[test]
    fn warns_on_a_steady_climb_with_eta_and_process() {
        let mut f = Forecaster::default();
        // 10 GB used, +20 MB/s: 6000 MB free -> 300 s = ~5 min
        for t in 0..90 {
            let u = 10000.0 + 20.0 * t as f32;
            f.push(&s(t, u, vec![py(u - 2000.0, 90)]), 5);
        }
        let fc = f.current();
        let w = fc.warning.as_deref().expect("warning");
        assert!(w.starts_with("At this rate VRAM will be full in ~"), "{w}");
        assert!(w.contains("(python.exe is growing by 20 MB/s)"), "{w}");
        let secs = fc.secs_to_full.unwrap();
        assert!((secs - (16000.0 - (10000.0 + 20.0 * 85.0)) / 20.0).abs() < 30.0, "{secs}");
    }

    #[test]
    fn no_warning_for_noise_flat_or_far_away() {
        // noisy but flat
        let mut f = Forecaster::default();
        for t in 0..120 {
            let u = 9000.0 + if t % 2 == 0 { 400.0 } else { -400.0 };
            f.push(&s(t, u, vec![]), 5);
        }
        assert!(f.current().warning.is_none());
        // slow climb: full in hours, not minutes
        let mut f = Forecaster::default();
        for t in 0..120 {
            f.push(&s(t, 4000.0 + 0.5 * t as f32, vec![]), 5);
        }
        assert!(f.current().warning.is_none());
        // too little history
        let mut f = Forecaster::default();
        for t in 0..20 {
            f.push(&s(t, 10000.0 + 50.0 * t as f32, vec![]), 5);
        }
        assert!(f.current().warning.is_none());
    }

    #[test]
    fn warning_clears_after_the_climb_stops() {
        let mut f = Forecaster::default();
        for t in 0..90 {
            f.push(&s(t, 10000.0 + 20.0 * t as f32, vec![]), 5);
        }
        assert!(f.current().warning.is_some());
        for t in 90..300 {
            f.push(&s(t, 11800.0, vec![]), 5);
        }
        assert!(f.current().warning.is_none());
    }

    #[test]
    fn idle_background_hogs() {
        let chrome = |sm| Proc {
            pid: 9,
            name: "chrome.exe".into(),
            category: "Browser".into(),
            vram_mb: Some(1200.0),
            sm: Some(sm),
            ..Default::default()
        };
        let discord = |mb| Proc {
            pid: 10,
            name: "Discord.exe".into(),
            category: "Other".into(),
            vram_mb: Some(mb),
            sm: Some(0),
            ..Default::default()
        };
        let mut f = Forecaster::default();
        for t in 0..90 {
            f.push(&s(t, 8000.0, vec![chrome(0), discord(500.0), py(5000.0, 0)]), 5);
        }
        let fc = f.current();
        let names: Vec<&str> = fc.hogs.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["chrome.exe", "Discord.exe"], "python is not a background app");
        assert_eq!(fc.hog_sentence().unwrap(), "chrome.exe (Browser) holds 1.2 GB of VRAM without using the GPU. So does Discord.exe.");
        // a browser that is actually using the GPU is not a hog
        let mut f = Forecaster::default();
        for t in 0..90 {
            f.push(&s(t, 8000.0, vec![chrome(if t % 10 == 0 { 30 } else { 0 })]), 5);
        }
        assert!(f.current().hogs.is_empty());
    }

    #[test]
    fn eta_wording() {
        assert_eq!(fmt_eta(30.0), "under a minute");
        assert_eq!(fmt_eta(185.0), "~3 min");
        assert_eq!(fmt_eta(4200.0), "~1 h 10 min");
    }

    #[test]
    fn horizon_from_settings() {
        // full in ~5 min: a 3-minute horizon stays quiet, 0 turns warnings off, 10 warns
        let run = |m: Option<u32>| {
            let mut f = Forecaster::default();
            if let Some(m) = m {
                f.set_horizon_minutes(m);
            }
            for t in 0..90 {
                let u = 10000.0 + 20.0 * t as f32;
                f.push(&s(t, u, vec![py(u - 2000.0, 90)]), 5);
            }
            f.current().warning.is_some()
        };
        assert!(run(None) && run(Some(10)));
        assert!(!run(Some(3)) && !run(Some(0)));
    }
}
