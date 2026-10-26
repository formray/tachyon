//! Bottleneck explainer: plain rules over the last few seconds of samples that say, in one
//! sentence, what is holding the work back. No AI, no extra sampling: it reads the samples the
//! monitor already has. A new verdict must win several evaluations in a row before it replaces
//! the one on screen, so the sentence does not flicker.
use crate::telemetry::Sample;
use serde::Serialize;
use std::collections::VecDeque;

/// Seconds of samples one verdict looks at.
pub const WINDOW: usize = 10;
/// Evaluations a new verdict must win in a row before it is shown.
pub const HOLD: u32 = 4;
/// While the verdict stays the same, its sentence (with its numbers) is refreshed this often,
/// so the text is not re-laid out every second.
pub const REFRESH: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Idle,
    Light,
    Mixed,
    GpuBound,
    PowerLimit,
    Decode,
    Encode,
    CpuThread,
    CpuAll,
    Vram,
    Thermal,
    Pcie,
}

impl Kind {
    /// Short label for lists (history, menus).
    pub fn label(self) -> &'static str {
        match self {
            Kind::Idle => "idle",
            Kind::Light => "light load",
            Kind::Mixed => "no clear limit",
            Kind::GpuBound => "GPU-bound",
            Kind::PowerLimit => "GPU-bound at power cap",
            Kind::Decode => "video decode",
            Kind::Encode => "video encode",
            Kind::CpuThread => "single CPU thread",
            Kind::CpuAll => "CPU-bound",
            Kind::Vram => "VRAM pressure",
            Kind::Thermal => "thermal throttling",
            Kind::Pcie => "PCIe transfers",
        }
    }
    /// True when this is a problem worth mentioning without being asked.
    pub fn is_problem(self) -> bool {
        matches!(self, Kind::Decode | Kind::Encode | Kind::CpuThread | Kind::CpuAll | Kind::Vram | Kind::Thermal | Kind::Pcie)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    pub kind: Kind,
    /// 0..1
    pub confidence: f32,
    /// One plain sentence.
    pub sentence: String,
    /// The process most involved, e.g. "python.exe (AI/agent)".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// The numbers behind it, one short line each.
    pub evidence: Vec<String>,
}

impl Verdict {
    /// "sure", "likely" or "possibly".
    pub fn confidence_word(&self) -> &'static str {
        if self.confidence >= 0.75 {
            "sure"
        } else if self.confidence >= 0.5 {
            "likely"
        } else {
            "possibly"
        }
    }
}

/// Averages over a window. Missing values are skipped; a metric never seen is None.
#[derive(Debug, Default, Clone)]
pub struct Agg {
    pub n: usize,
    pub util: Option<f32>,
    pub dec: Option<f32>,
    pub enc: Option<f32>,
    pub cpu: Option<f32>,
    pub core_max: Option<f32>,
    pub vram_frac: Option<f32>,
    pub vram_mb: Option<f32>,
    pub vram_total_mb: Option<f32>,
    pub power_frac: f32,
    pub thermal_frac: f32,
    pub temp_max: Option<f32>,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    /// max(rx, tx) as a fraction of the link capacity
    pub pcie_frac: Option<f32>,
    pub pcie_mbs: Option<f32>,
    pub pcie_rx: Option<f32>,
    pub pcie_tx: Option<f32>,
    /// The PCIe link in the latest sample: (gen, width, best gen, best width).
    pub link: Option<(Option<u8>, Option<u8>, Option<u8>, Option<u8>)>,
}

impl Agg {
    /// The link is narrower than the slot allows, or below its generation while the GPU is busy
    /// (idle downshifts are normal power saving and not mentioned).
    pub fn link_short(&self) -> Option<String> {
        let (g, w, gm, wm) = self.link?;
        let narrow = matches!((w, wm), (Some(w), Some(m)) if w < m);
        let slow = matches!((g, gm), (Some(g), Some(m)) if g < m) && self.util.is_some_and(|u| u >= crate::pcie::BUSY_UTIL as f32);
        let t = |a: Option<u8>, b: Option<u8>| crate::pcie::link_text(a.map(u32::from), b.map(u32::from));
        (narrow || slow).then(|| format!("{} instead of the {} this GPU and slot can do", t(g, w), t(gm, wm)))
    }
}

fn mean(it: impl Iterator<Item = Option<f32>>) -> Option<f32> {
    let (mut s, mut n) = (0.0, 0);
    for v in it.flatten() {
        s += v;
        n += 1;
    }
    (n > 0).then(|| s / n as f32)
}

pub fn aggregate(w: &[Sample]) -> Agg {
    let n = w.len();
    let nf = n.max(1) as f32;
    let last = w.last();
    Agg {
        n,
        util: mean(w.iter().map(|s| s.util.map(f32::from))),
        dec: mean(w.iter().map(|s| s.dec.map(f32::from))),
        enc: mean(w.iter().map(|s| s.enc.map(f32::from))),
        cpu: mean(w.iter().map(|s| s.cpu)),
        core_max: mean(w.iter().map(|s| s.cpu_core_max)),
        vram_frac: w.iter().filter_map(|s| s.vram_frac()).reduce(f32::max),
        vram_mb: last.and_then(|s| s.vram_mb),
        vram_total_mb: last.and_then(|s| s.vram_total_mb),
        power_frac: w.iter().filter(|s| s.power_throttled()).count() as f32 / nf,
        thermal_frac: w.iter().filter(|s| s.thermal_throttled()).count() as f32 / nf,
        temp_max: w.iter().filter_map(|s| s.temp.map(f32::from)).reduce(f32::max),
        power_w: mean(w.iter().map(|s| s.power_w)),
        power_limit_w: last.and_then(|s| s.power_limit_w),
        pcie_frac: mean(w.iter().map(|s| match (s.pcie_rx, s.pcie_tx, s.pcie_max) {
            (rx, tx, Some(m)) if m > 0.0 && (rx.is_some() || tx.is_some()) => Some(rx.unwrap_or(0.0).max(tx.unwrap_or(0.0)) / m),
            _ => None,
        })),
        pcie_mbs: mean(w.iter().map(|s| match (s.pcie_rx, s.pcie_tx) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0).max(b.unwrap_or(0.0))),
        })),
        pcie_rx: mean(w.iter().map(|s| s.pcie_rx)),
        pcie_tx: mean(w.iter().map(|s| s.pcie_tx)),
        link: last
            .filter(|s| s.pcie_gen.is_some() || s.pcie_width.is_some())
            .map(|s| (s.pcie_gen, s.pcie_width, s.pcie_gen_max, s.pcie_width_max)),
    }
}

/// The process with the highest value of `key` in the latest sample, as "name (category)".
fn top_proc(w: &[Sample], key: impl Fn(&crate::telemetry::Proc) -> f32) -> Option<String> {
    let s = w.last()?;
    let p = s.procs.iter().filter(|p| key(p) > 0.0).max_by(|a, b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal))?;
    Some(if p.category.is_empty() { p.name.clone() } else { format!("{} ({})", p.name, p.category) })
}

fn gb(mb: f32) -> String {
    format!("{:.1}", mb / 1024.0)
}

/// Evidence lines (the numbers behind a verdict), for "Why is it slow?".
pub fn evidence(a: &Agg) -> Vec<String> {
    let pc = |v: Option<f32>| v.map(|x| format!("{x:.0}%")).unwrap_or_else(|| "—".into());
    let mut e = vec![format!("GPU {} · decoder {} · encoder {}", pc(a.util), pc(a.dec), pc(a.enc))];
    if a.cpu.is_some() {
        e.push(format!("CPU {} · busiest core {}", pc(a.cpu), pc(a.core_max)));
    }
    if let (Some(u), Some(t)) = (a.vram_mb, a.vram_total_mb) {
        e.push(format!("VRAM {} of {} GB", gb(u), gb(t)));
    }
    let mut lim = format!("Power cap {:.0}% of the time · thermal {:.0}%", a.power_frac * 100.0, a.thermal_frac * 100.0);
    if let Some(t) = a.temp_max {
        lim += &format!(" · peak {t:.0} °C");
    }
    e.push(lim);
    if let (Some(rx), Some(tx)) = (a.pcie_rx, a.pcie_tx) {
        let mut l = format!("PCIe in {:.1} / out {:.1} GB/s", rx / 1024.0, tx / 1024.0);
        if let Some(f) = a.pcie_frac {
            l += &format!(" ({:.0}% of the link)", f * 100.0);
        }
        e.push(l);
    }
    if let Some(short) = a.link_short() {
        e.push(format!("PCIe link {short}"));
    } else if let Some((g, w, _, _)) = a.link {
        e.push(format!("PCIe link {}", crate::pcie::link_text(g.map(u32::from), w.map(u32::from))));
    }
    e
}

/// One verdict for a window of samples (oldest first). Rules are checked in priority order;
/// the first that applies wins.
pub fn assess(w: &[Sample]) -> Option<Verdict> {
    if w.len() < 3 {
        return None;
    }
    let a = aggregate(w);
    let util = a.util?;
    let pc = |v: f32| format!("{v:.0}%");
    let clamp = |c: f32| c.clamp(0.2, 0.95);
    let v = |kind, conf: f32, sentence: String, process: Option<String>| Verdict {
        kind,
        confidence: clamp(conf),
        sentence,
        process,
        evidence: evidence(&a),
    };
    let by_vram = || top_proc(w, |p| p.vram_mb.unwrap_or(0.0));
    let by_sm = || top_proc(w, |p| p.sm.map(f32::from).unwrap_or(0.0)).or_else(by_vram);
    // 1. VRAM nearly full (and maybe spilling into system memory over PCIe)
    if let Some(f) = a.vram_frac.filter(|f| *f >= 0.95) {
        let (u, t) = (a.vram_mb.unwrap_or(0.0), a.vram_total_mb.unwrap_or(0.0));
        let spill = a.pcie_frac.is_some_and(|p| p >= 0.35) && util < 85.0;
        let s = if spill {
            format!(
                "Video memory is full ({} of {} GB) and data is streaming over PCIe: it is probably spilling into system memory, which is much slower.",
                gb(u),
                gb(t)
            )
        } else {
            format!(
                "Video memory is nearly full ({} of {} GB): expect stutter or slowdowns. Closing apps that hold VRAM will help.",
                gb(u),
                gb(t)
            )
        };
        return Some(v(Kind::Vram, 0.55 + (f - 0.95) * 8.0 + if spill { 0.15 } else { 0.0 }, s, by_vram()));
    }
    // 2. thermal throttling
    if a.thermal_frac >= 0.5 {
        let t = a.temp_max.map(|t| format!(" at {t:.0} °C")).unwrap_or_default();
        return Some(v(
            Kind::Thermal,
            0.5 + 0.45 * a.thermal_frac,
            format!("The GPU is slowing itself down to stay cool (thermal limit{t}). Better airflow or a lower power limit would help."),
            by_sm(),
        ));
    }
    // 3. video decode / encode engines saturated while the shader cores wait
    if let Some(d) = a.dec.filter(|d| *d >= 80.0 && util < 60.0) {
        return Some(v(
            Kind::Decode,
            0.55 + (d - 80.0) / 50.0 + (60.0 - util) / 150.0,
            format!("Waiting on video decoding: the decoder is at {} while the GPU cores sit at {}.", pc(d), pc(util)),
            top_proc(w, |p| p.dec.map(f32::from).unwrap_or(0.0)),
        ));
    }
    if let Some(e) = a.enc.filter(|e| *e >= 85.0 && util < 70.0) {
        return Some(v(
            Kind::Encode,
            0.55 + (e - 85.0) / 40.0 + (70.0 - util) / 150.0,
            format!("Limited by video encoding: the encoder is at {} while the GPU cores sit at {}.", pc(e), pc(util)),
            top_proc(w, |p| p.enc.map(f32::from).unwrap_or(0.0)),
        ));
    }
    // 4. CPU: one thread maxed, or all cores busy, while the GPU waits
    if let (Some(c), Some(m)) = (a.cpu, a.core_max) {
        if c >= 90.0 && util < 70.0 {
            return Some(v(
                Kind::CpuAll,
                0.5 + (c - 90.0) / 25.0 + (70.0 - util) / 140.0,
                format!("The CPU is the limit: {} busy across all cores while the GPU waits at {}.", pc(c), pc(util)),
                by_sm(),
            ));
        }
        if m >= 92.0 && c < 60.0 && util < 70.0 {
            let who = by_sm().map(|p| format!(" The app feeding the GPU is {p}.")).unwrap_or_default();
            return Some(v(
                Kind::CpuThread,
                0.45 + (m - 92.0) / 20.0 + (70.0 - util) / 120.0,
                format!(
                    "One CPU core is maxed out ({}) while the GPU waits at {}: the work is held back by a single thread.{who}",
                    pc(m),
                    pc(util)
                ),
                by_sm(),
            ));
        }
    }
    // 5. PCIe link close to its limit while the GPU waits
    if let (Some(f), Some(mbs)) = (a.pcie_frac, a.pcie_mbs) {
        if f >= 0.7 && util < 75.0 {
            let short = a
                .link_short()
                .map(|s| format!(" The link itself runs at {s}: check the slot, riser or BIOS PCIe setting."))
                .unwrap_or_default();
            return Some(v(
                Kind::Pcie,
                0.45 + (f - 0.7) + (75.0 - util) / 150.0 + if short.is_empty() { 0.0 } else { 0.1 },
                format!(
                    "Data transfers over PCIe are the bottleneck ({:.1} GB/s, near this link's limit) while the GPU waits at {}.{short}",
                    mbs / 1024.0,
                    pc(util)
                ),
                by_vram(),
            ));
        }
    }
    // 6. GPU-bound (normal for renders), possibly at the power cap
    if util >= 85.0 && a.power_frac >= 0.5 {
        let p = match (a.power_w, a.power_limit_w) {
            (Some(w_), Some(l)) => format!(" ({w_:.0} of {l:.0} W)"),
            _ => String::new(),
        };
        return Some(v(
            Kind::PowerLimit,
            0.55 + a.power_frac * 0.3,
            format!("The GPU is the limit and runs at its power cap{p}; clocks dip to stay under it. That is normal for heavy work."),
            by_sm(),
        ));
    }
    if util >= 90.0 {
        return Some(v(
            Kind::GpuBound,
            0.6 + (util - 90.0) / 25.0,
            format!("The GPU is the limit: busy {} of the time. That is what you want for a render.", pc(util)),
            by_sm(),
        ));
    }
    if util >= 40.0 {
        let cpu = a.cpu.map(|c| format!(", CPU {}", pc(c))).unwrap_or_default();
        return Some(v(
            Kind::Mixed,
            0.35,
            format!("No single limit stands out (GPU {}{cpu}). The app may be waiting on disk, network or its own pacing.", pc(util)),
            by_sm(),
        ));
    }
    if util >= 10.0 {
        return Some(v(Kind::Light, 0.6, format!("Nothing is holding things back: the GPU is lightly loaded ({}).", pc(util)), by_sm()));
    }
    Some(v(Kind::Idle, 0.9, "The GPU is idle.".into(), None))
}

/// Keeps the window and applies hysteresis.
#[derive(Default)]
pub struct Explainer {
    w: VecDeque<Sample>,
    shown: Option<Verdict>,
    pending: Option<(Kind, u32)>,
    age: u32,
}

impl Explainer {
    /// Add a sample and re-evaluate. Returns the verdict to show (stable).
    pub fn push(&mut self, s: &Sample) -> Option<&Verdict> {
        self.w.push_back(s.clone());
        while self.w.len() > WINDOW {
            self.w.pop_front();
        }
        let v = assess(self.w.make_contiguous());
        match (v, &self.shown) {
            (None, _) => {}
            (Some(v), Some(cur)) if v.kind == cur.kind => {
                self.pending = None;
                self.age += 1;
                if self.age >= REFRESH || v.confidence_word() != cur.confidence_word() {
                    self.shown = Some(v);
                    self.age = 0;
                }
            }
            (Some(v), cur) => {
                let n = match self.pending {
                    Some((k, n)) if k == v.kind => n + 1,
                    _ => 1,
                };
                if cur.is_none() || n >= HOLD {
                    self.shown = Some(v);
                    self.pending = None;
                    self.age = 0;
                } else {
                    self.pending = Some((v.kind, n));
                }
            }
        }
        self.shown.as_ref()
    }

    pub fn current(&self) -> Option<&Verdict> {
        self.shown.as_ref()
    }

    /// A fresh verdict on the latest window, ignoring hysteresis (for "Why is it slow?").
    pub fn now(&mut self) -> Option<Verdict> {
        assess(self.w.make_contiguous())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::telemetry::Proc;

    /// A busy baseline sample; tests change what they need.
    pub fn base() -> Sample {
        Sample {
            util: Some(50),
            dec: Some(0),
            enc: Some(0),
            cpu: Some(20.0),
            cpu_core_max: Some(40.0),
            vram_mb: Some(8000.0),
            vram_total_mb: Some(16000.0),
            throttle: Some(0),
            temp: Some(65),
            power_w: Some(200.0),
            power_limit_w: Some(360.0),
            pcie_rx: Some(500.0),
            pcie_tx: Some(200.0),
            pcie_max: Some(31504.0),
            procs: vec![
                Proc {
                    pid: 1,
                    name: "python.exe".into(),
                    category: "AI/agent".into(),
                    vram_mb: Some(7000.0),
                    sm: Some(45),
                    enc: None,
                    dec: Some(0),
                },
                Proc {
                    pid: 2,
                    name: "chrome.exe".into(),
                    category: "Browser".into(),
                    vram_mb: Some(600.0),
                    sm: Some(1),
                    enc: None,
                    dec: Some(0),
                },
            ],
            ..Default::default()
        }
    }

    fn trace(n: usize, f: impl Fn(usize, &mut Sample)) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let mut s = base();
                s.t_ms = i as i64 * 1000;
                f(i, &mut s);
                s
            })
            .collect()
    }

    fn kind(w: &[Sample]) -> Kind {
        assess(w).unwrap().kind
    }

    #[test]
    fn idle_light_mixed_and_gpu_bound() {
        assert_eq!(kind(&trace(10, |_, s| s.util = Some(2))), Kind::Idle);
        assert_eq!(kind(&trace(10, |_, s| s.util = Some(25))), Kind::Light);
        assert_eq!(kind(&trace(10, |_, s| s.util = Some(60))), Kind::Mixed);
        let v = assess(&trace(10, |_, s| s.util = Some(97))).unwrap();
        assert_eq!(v.kind, Kind::GpuBound);
        assert!(v.sentence.contains("97%"), "{}", v.sentence);
        assert_eq!(v.process.as_deref(), Some("python.exe (AI/agent)"));
        assert!(assess(&trace(2, |_, _| {})).is_none(), "needs 3 samples");
    }

    #[test]
    fn decode_bound_names_the_decoding_process() {
        let w = trace(10, |_, s| {
            s.util = Some(32);
            s.dec = Some(93);
            s.procs[0].dec = Some(90);
        });
        let v = assess(&w).unwrap();
        assert_eq!(v.kind, Kind::Decode);
        assert!(v.sentence.starts_with("Waiting on video decoding: the decoder is at 93%"), "{}", v.sentence);
        assert_eq!(v.process.as_deref(), Some("python.exe (AI/agent)"));
        assert!(v.confidence >= 0.75, "{}", v.confidence);
        // high decode with busy shaders is not a decode bottleneck
        assert_eq!(
            kind(&trace(10, |_, s| {
                s.util = Some(95);
                s.dec = Some(93)
            })),
            Kind::GpuBound
        );
    }

    #[test]
    fn cpu_single_thread_and_all_cores() {
        let v = assess(&trace(10, |_, s| {
            s.util = Some(45);
            s.cpu = Some(14.0);
            s.cpu_core_max = Some(99.0);
        }))
        .unwrap();
        assert_eq!(v.kind, Kind::CpuThread);
        assert!(v.sentence.contains("One CPU core is maxed out (99%)") && v.sentence.contains("waits at 45%"), "{}", v.sentence);
        assert_eq!(
            kind(&trace(10, |_, s| {
                s.util = Some(40);
                s.cpu = Some(96.0);
                s.cpu_core_max = Some(100.0)
            })),
            Kind::CpuAll
        );
        // a busy core with a busy GPU is fine
        assert_eq!(
            kind(&trace(10, |_, s| {
                s.util = Some(95);
                s.cpu_core_max = Some(100.0)
            })),
            Kind::GpuBound
        );
    }

    #[test]
    fn vram_thermal_power_and_pcie() {
        let v = assess(&trace(10, |_, s| s.vram_mb = Some(15800.0))).unwrap();
        assert_eq!(v.kind, Kind::Vram);
        assert!(v.sentence.contains("15.4 of 15.6 GB"), "{}", v.sentence);
        let spill = assess(&trace(10, |_, s| {
            s.vram_mb = Some(15900.0);
            s.pcie_rx = Some(14000.0);
        }))
        .unwrap();
        assert!(spill.sentence.contains("spilling into system memory"), "{}", spill.sentence);
        assert_eq!(
            kind(&trace(10, |i, s| {
                s.util = Some(99);
                s.temp = Some(86);
                s.throttle = Some(if i % 4 == 0 { 0 } else { 0x20 })
            })),
            Kind::Thermal
        );
        let p = assess(&trace(10, |_, s| {
            s.util = Some(98);
            s.power_w = Some(355.0);
            s.throttle = Some(0x4)
        }))
        .unwrap();
        assert_eq!(p.kind, Kind::PowerLimit);
        assert!(p.sentence.contains("(355 of 360 W)"), "{}", p.sentence);
        assert_eq!(
            kind(&trace(10, |_, s| {
                s.util = Some(40);
                s.pcie_rx = Some(26000.0)
            })),
            Kind::Pcie
        );
        // no link info: no PCIe verdict
        assert_eq!(
            kind(&trace(10, |_, s| {
                s.util = Some(40);
                s.pcie_rx = Some(26000.0);
                s.pcie_max = None
            })),
            Kind::Mixed
        );
    }

    #[test]
    fn pcie_link_below_its_best_is_evidence_only_under_load() {
        let link = |s: &mut Sample, g: u8, w: u8| {
            s.pcie_gen = Some(g);
            s.pcie_width = Some(w);
            s.pcie_gen_max = Some(5);
            s.pcie_width_max = Some(16);
        };
        // idle at Gen1: normal power saving, just the link
        let v = assess(&trace(10, |_, s| {
            s.util = Some(2);
            link(s, 1, 16)
        }))
        .unwrap();
        assert!(v.evidence.iter().any(|e| e == "PCIe link Gen1 x16"), "{:?}", v.evidence);
        // busy at Gen3: degraded
        let v = assess(&trace(10, |_, s| {
            s.util = Some(97);
            link(s, 3, 16)
        }))
        .unwrap();
        assert!(v.evidence.iter().any(|e| e == "PCIe link Gen3 x16 instead of the Gen5 x16 this GPU and slot can do"), "{:?}", v.evidence);
        // narrower than the slot, at any load, and named in a PCIe verdict
        let v = assess(&trace(10, |_, s| {
            s.util = Some(40);
            s.pcie_rx = Some(26000.0);
            link(s, 5, 8)
        }))
        .unwrap();
        assert_eq!(v.kind, Kind::Pcie);
        assert!(v.sentence.contains("runs at Gen5 x8 instead of the Gen5 x16"), "{}", v.sentence);
    }

    #[test]
    fn missing_metrics_degrade_gracefully() {
        let w = trace(10, |_, s| {
            *s = Sample { util: Some(97), ..Default::default() };
        });
        let v = assess(&w).unwrap();
        assert_eq!(v.kind, Kind::GpuBound);
        assert!(v.process.is_none());
        let w = trace(10, |_, s| *s = Sample::default());
        assert!(assess(&w).is_none(), "no GPU load at all: no verdict");
    }

    #[test]
    fn hysteresis_does_not_flicker() {
        let mut e = Explainer::default();
        let mut shown = vec![];
        // a GPU-bound job with single-second dips that would read as "mixed"
        for i in 0..60 {
            let mut s = base();
            s.util = Some(if i % 5 == 0 { 60 } else { 98 });
            shown.push(e.push(&s).map(|v| v.kind));
        }
        let changes = shown.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(changes <= 2, "{shown:?}");
        assert_eq!(shown.last().unwrap(), &Some(Kind::GpuBound));
        // a real change (decode bottleneck) takes over after HOLD evaluations, not before
        let mut switched_at = None;
        for i in 0..30 {
            let mut s = base();
            s.util = Some(30);
            s.dec = Some(95);
            if e.push(&s).map(|v| v.kind) == Some(Kind::Decode) && switched_at.is_none() {
                switched_at = Some(i);
            }
        }
        let at = switched_at.expect("switched");
        assert!(at >= HOLD as usize - 1, "switched after {at} samples");
    }

    #[test]
    fn sentence_numbers_refresh_slowly() {
        let mut e = Explainer::default();
        let mut texts = vec![];
        for i in 0..30u8 {
            let mut s = base();
            s.util = Some(30 + i % 3);
            s.dec = Some(95);
            texts.push(e.push(&s).map(|v| v.sentence.clone()).unwrap_or_default());
        }
        let changes = texts.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((1..=4).contains(&changes), "{changes} changes: {texts:?}");
    }
}
