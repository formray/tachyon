//! The "what happened before" report, built once at start when the previous session did not end
//! cleanly: the last minutes from the flight recorder, the peaks, who held the VRAM, and the
//! matching Windows events. Saved next to the recorder files so the panel and the agent API can
//! open it later; exported on request as CSV and JSON.
use crate::events::SysEvent;
use crate::recorder::PrevSession;
use crate::telemetry::Sample;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How far back "just before" looks.
pub const JUST_BEFORE_MS: i64 = 60_000;
/// How much of the recording the timeline shows.
pub const TIMELINE_MS: i64 = 10 * 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The PC restarted (or lost power) while Tachyon was running.
    Restart,
    /// Tachyon stopped without closing, but the PC kept running.
    AppClosed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Peaks {
    pub gpu_util: Option<u8>,
    pub temp: Option<u8>,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub vram_mb: Option<f32>,
    pub vram_total_mb: Option<f32>,
    pub cpu: Option<f32>,
    pub cpu_core_max: Option<f32>,
    pub ram_used_mb: Option<f32>,
    pub ram_total_mb: Option<f32>,
    pub commit_mb: Option<f32>,
    pub commit_limit_mb: Option<f32>,
    /// Seconds with a power or thermal limit active (sample count, one per second).
    pub power_throttled_s: u32,
    pub thermal_throttled_s: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Holder {
    pub name: String,
    pub category: String,
    pub vram_mb: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub t_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventLine {
    #[serde(flatten)]
    pub event: SysEvent,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub cause: Cause,
    /// Time of the last record (when the PC or Tachyon stopped, to within a few seconds).
    pub stopped_ms: i64,
    pub boot_ms: Option<i64>,
    pub headline: String,
    pub just_before: String,
    /// Peaks over the last minute before the stop.
    pub peaks_last_minute: Peaks,
    /// Peaks over the whole recording.
    pub peaks: Peaks,
    pub vram_holder: Option<Holder>,
    pub timeline: Vec<Line>,
    pub events: Vec<EventLine>,
    /// Why there are no events, when the log could not be read.
    pub events_note: Option<String>,
    pub samples: usize,
    pub recorded_from_ms: Option<i64>,
    pub created_ms: i64,
    /// Hex has told the user about it.
    #[serde(default)]
    pub announced: bool,
}

fn hm(ms: i64) -> String {
    crate::telemetry::Sample { t_ms: ms, ..Default::default() }.local().format("%H:%M").to_string()
}

fn hms(ms: i64) -> String {
    crate::telemetry::Sample { t_ms: ms, ..Default::default() }.local().format("%H:%M:%S").to_string()
}

pub fn gb(mb: f32) -> String {
    format!("{:.1} GB", mb / 1024.0)
}

fn maxf(it: impl Iterator<Item = Option<f32>>) -> Option<f32> {
    it.flatten().fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))))
}

fn maxu(it: impl Iterator<Item = Option<u8>>) -> Option<u8> {
    it.flatten().max()
}

pub fn peaks(w: &[Sample]) -> Peaks {
    Peaks {
        gpu_util: maxu(w.iter().map(|s| s.util)),
        temp: maxu(w.iter().map(|s| s.temp)),
        power_w: maxf(w.iter().map(|s| s.power_w)),
        power_limit_w: w.iter().rev().find_map(|s| s.power_limit_w),
        vram_mb: maxf(w.iter().map(|s| s.vram_mb)),
        vram_total_mb: w.iter().rev().find_map(|s| s.vram_total_mb),
        cpu: maxf(w.iter().map(|s| s.cpu)),
        cpu_core_max: maxf(w.iter().map(|s| s.cpu_core_max)),
        ram_used_mb: maxf(w.iter().map(|s| s.ram_used_mb)),
        ram_total_mb: w.iter().rev().find_map(|s| s.ram_total_mb),
        commit_mb: maxf(w.iter().map(|s| s.commit_mb)),
        commit_limit_mb: w.iter().rev().find_map(|s| s.commit_limit_mb),
        power_throttled_s: w.iter().filter(|s| s.power_throttled()).count() as u32,
        thermal_throttled_s: w.iter().filter(|s| s.thermal_throttled()).count() as u32,
    }
}

/// The process holding the most VRAM over a window.
pub fn vram_holder(w: &[Sample]) -> Option<Holder> {
    w.iter()
        .flat_map(|s| s.procs.iter())
        .filter_map(|p| p.vram_mb.map(|v| (p, v)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(p, v)| Holder { name: p.name.clone(), category: p.category.clone(), vram_mb: v })
}

fn avg(it: impl Iterator<Item = Option<f32>>) -> Option<f32> {
    let v: Vec<f32> = it.flatten().collect();
    (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32)
}

/// One line of numbers for a stretch of samples.
fn summary(w: &[Sample]) -> String {
    let mut parts = vec![];
    if let Some(u) = avg(w.iter().map(|s| s.util.map(f32::from))) {
        parts.push(format!("GPU {u:.0}%"));
    }
    if let Some(t) = maxu(w.iter().map(|s| s.temp)) {
        parts.push(format!("{t} °C"));
    }
    if let Some(p) = avg(w.iter().map(|s| s.power_w)) {
        parts.push(format!("{p:.0} W"));
    }
    if let Some(v) = maxf(w.iter().map(|s| s.vram_mb)) {
        parts.push(format!("VRAM {}", gb(v)));
    }
    if let Some(c) = avg(w.iter().map(|s| s.cpu)) {
        parts.push(format!("CPU {c:.0}%"));
    }
    if let Some(r) = maxf(w.iter().map(|s| s.ram_used_mb)) {
        parts.push(format!("RAM {}", gb(r)));
    }
    if w.iter().any(|s| s.thermal_throttled()) {
        parts.push("heat limit".into());
    } else if w.iter().any(|s| s.power_throttled()) {
        parts.push("power limit".into());
    }
    if parts.is_empty() { "no readings".into() } else { parts.join(" · ") }
}

/// "Just before: ..." in a sentence short enough for the Hex bubble.
fn just_before(p: &Peaks, holder: Option<&Holder>, gpu_events: &[&SysEvent]) -> String {
    let mut parts = vec![];
    if let Some(u) = p.gpu_util {
        parts.push(format!("GPU at {u}%"));
    }
    if let Some(t) = p.temp {
        parts.push(format!("{t} °C"));
    }
    match (p.vram_mb, p.vram_total_mb) {
        (Some(v), Some(t)) => {
            let who = holder.map(|h| format!(", mostly {}", h.name)).unwrap_or_default();
            parts.push(format!("VRAM {:.1} of {}{who}", v / 1024.0, gb(t)));
        }
        (Some(v), None) => parts.push(format!("VRAM {}", gb(v))),
        _ => {}
    }
    if let (Some(c), Some(l)) = (p.commit_mb, p.commit_limit_mb)
        && l > 0.0
        && c / l >= 0.95
    {
        parts.push("memory commit nearly full".into());
    }
    if p.thermal_throttled_s > 0 {
        parts.push("the GPU was slowing down from heat".into());
    }
    let mut s = if parts.is_empty() {
        "Just before: no readings were recorded.".to_string()
    } else {
        format!("Just before: {}.", parts.join(", "))
    };
    if let Some(e) = gpu_events.last() {
        s.push_str(if e.provider == "Display" { " Windows logged a GPU timeout." } else { " The NVIDIA driver logged an error." });
    }
    s
}

/// Build the report for an unclean previous session. `events` is the Event Log result for the
/// recording window up to now. Returns None when the previous session ended cleanly.
pub fn build(prev: &PrevSession, boot_ms: Option<i64>, events: Result<Vec<SysEvent>, String>, now_ms: i64) -> Option<Report> {
    if prev.session.clean {
        return None;
    }
    let stopped_ms = prev.last_ms().unwrap_or(prev.session.started_ms);
    let (events, events_note) = match events {
        Ok(v) => (v, None),
        Err(e) => (vec![], Some(format!("No Windows events: {e}."))),
    };
    let rebooted = boot_ms.is_some_and(|b| b > stopped_ms);
    let restart_event = events.iter().any(|e| e.is_restart() && e.t_ms >= stopped_ms - JUST_BEFORE_MS);
    let cause = if rebooted || restart_event { Cause::Restart } else { Cause::AppClosed };
    let last: Vec<Sample> = prev.samples.iter().filter(|s| s.t_ms >= stopped_ms - JUST_BEFORE_MS).cloned().collect();
    let p_last = peaks(&last);
    let holder = vram_holder(&last).or_else(|| vram_holder(&prev.samples));
    let gpu_events: Vec<&SysEvent> =
        events.iter().filter(|e| e.is_gpu() && e.t_ms >= stopped_ms - JUST_BEFORE_MS && e.t_ms <= stopped_ms + 5_000).collect();
    let headline = match cause {
        Cause::Restart => format!("Your PC restarted unexpectedly at {}.", hm(stopped_ms)),
        Cause::AppClosed => format!("Tachyon stopped unexpectedly at {} (the PC kept running).", hm(stopped_ms)),
    };
    let just = just_before(&p_last, holder.as_ref(), &gpu_events);

    // Timeline: one line per minute over the last ten minutes, then the events, by time.
    let from = stopped_ms - TIMELINE_MS;
    let mut timeline = vec![];
    let tl: Vec<&Sample> = prev.samples.iter().filter(|s| s.t_ms > from).collect();
    let mut i = 0;
    while i < tl.len() {
        let minute = tl[i].t_ms.div_euclid(60_000);
        let mut j = i;
        while j < tl.len() && tl[j].t_ms.div_euclid(60_000) == minute {
            j += 1;
        }
        let chunk: Vec<Sample> = tl[i..j].iter().map(|s| (*s).clone()).collect();
        timeline.push(Line { t_ms: chunk[0].t_ms, text: summary(&chunk) });
        i = j;
    }
    if let Some(s) = prev.samples.last() {
        timeline.push(Line { t_ms: s.t_ms, text: format!("Last record: {}", summary(std::slice::from_ref(s))) });
    }
    for e in events.iter().filter(|e| e.t_ms > from) {
        timeline.push(Line { t_ms: e.t_ms, text: e.describe() });
    }
    if let Some(b) = boot_ms.filter(|_| rebooted) {
        timeline.push(Line { t_ms: b, text: "Windows started again.".into() });
    }
    timeline.sort_by_key(|l| l.t_ms);

    Some(Report {
        cause,
        stopped_ms,
        boot_ms,
        headline,
        just_before: just,
        peaks_last_minute: p_last,
        peaks: peaks(&prev.samples),
        vram_holder: holder,
        timeline,
        events: events.into_iter().map(|e| EventLine { text: e.describe(), event: e }).collect(),
        events_note,
        samples: prev.samples.len(),
        recorded_from_ms: prev.samples.first().map(|s| s.t_ms),
        created_ms: now_ms,
        announced: false,
    })
}

impl Report {
    /// What Hex says, once.
    pub fn announcement(&self) -> String {
        format!("{} {}", self.headline, self.just_before)
    }

    /// The report as plain text (panel and export).
    pub fn to_text(&self) -> String {
        let mut out = format!("{}\n{}\n\n", self.headline, self.just_before);
        let p = &self.peaks_last_minute;
        out.push_str("Peaks in the last minute\n");
        let mut row = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                out.push_str(&format!("  {k:<12}{v}\n"));
            }
        };
        row("GPU", p.gpu_util.map(|u| format!("{u}%")));
        row("Temperature", p.temp.map(|t| format!("{t} °C")));
        row(
            "Power",
            p.power_w.map(|w| match p.power_limit_w {
                Some(l) => format!("{w:.0} W of {l:.0} W"),
                None => format!("{w:.0} W"),
            }),
        );
        row(
            "VRAM",
            p.vram_mb.map(|v| match p.vram_total_mb {
                Some(t) => format!("{:.1} of {}", v / 1024.0, gb(t)),
                None => gb(v),
            }),
        );
        row(
            "CPU",
            p.cpu.map(|c| match p.cpu_core_max {
                Some(m) => format!("{c:.0}% (busiest core {m:.0}%)"),
                None => format!("{c:.0}%"),
            }),
        );
        row(
            "RAM",
            p.ram_used_mb.map(|r| match p.ram_total_mb {
                Some(t) => format!("{:.1} of {}", r / 1024.0, gb(t)),
                None => gb(r),
            }),
        );
        row("Commit", p.commit_mb.zip(p.commit_limit_mb).map(|(c, l)| format!("{:.1} of {}", c / 1024.0, gb(l))));
        if p.power_throttled_s + p.thermal_throttled_s > 0 {
            row("Limits", Some(format!("power {} s, heat {} s", p.power_throttled_s, p.thermal_throttled_s)));
        }
        if let Some(h) = &self.vram_holder {
            out.push_str(&format!("\nMost VRAM: {} ({}), {}\n", h.name, h.category, gb(h.vram_mb)));
        }
        out.push_str("\nTimeline\n");
        for l in &self.timeline {
            out.push_str(&format!("  {}  {}\n", hms(l.t_ms), l.text));
        }
        out.push_str("\nWindows events\n");
        if self.events.is_empty() {
            out.push_str(&format!("  {}\n", self.events_note.as_deref().unwrap_or("None found for this time.")));
        }
        for e in &self.events {
            out.push_str(&format!("  {}  {}\n", hms(e.event.t_ms), e.text));
        }
        out
    }
}

const REPORT_FILE: &str = "last_report.json";

pub fn save(dir: &Path, r: &Report) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("last_report.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, dir.join(REPORT_FILE))
}

pub fn load(dir: &Path) -> Option<Report> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(dir.join(REPORT_FILE)).ok()?.take(4 << 20).read_to_end(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Remember that Hex has told the user.
pub fn mark_announced(dir: &Path) {
    if let Some(mut r) = load(dir) {
        r.announced = true;
        let _ = save(dir, &r);
    }
}

/// Write the samples as CSV and the report plus samples as JSON into `dir`.
pub fn export(dir: &Path, report: Option<&Report>, samples: &[Sample], now_ms: i64) -> std::io::Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dir)?;
    let stamp = Sample { t_ms: now_ms, ..Default::default() }.local().format("%Y%m%d_%H%M%S").to_string();
    let csv = dir.join(format!("blackbox_{stamp}.csv"));
    let json = dir.join(format!("blackbox_{stamp}.json"));
    std::fs::write(&csv, crate::recorder::to_csv(samples))?;
    let body = serde_json::json!({ "tachyon": env!("CARGO_PKG_VERSION"), "report": report, "samples": samples });
    std::fs::write(&json, serde_json::to_vec_pretty(&body).map_err(std::io::Error::other)?)?;
    Ok((csv, json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::Session;
    use crate::telemetry::Proc;

    const T0: i64 = 1_790_000_040_000; // on a minute boundary

    fn s(i: i64, util: u8, vram: f32) -> Sample {
        Sample {
            t_ms: T0 + i * 1000,
            util: Some(util),
            temp: Some(70 + (i % 10) as u8),
            power_w: Some(300.0),
            power_limit_w: Some(360.0),
            vram_mb: Some(vram),
            vram_total_mb: Some(16303.0),
            throttle: Some(if i > 590 { 0x20 } else { 0 }),
            cpu: Some(30.0),
            cpu_core_max: Some(90.0),
            procs: vec![Proc {
                pid: 9,
                name: "python.exe".into(),
                category: "AI/agent".into(),
                vram_mb: Some(vram - 2000.0),
                sm: Some(90),
                enc: None,
                dec: None,
            }],
            ..Default::default()
        }
    }

    fn prev(clean: bool, n: i64) -> PrevSession {
        PrevSession {
            session: Session { version: "1.0.0".into(), pid: 1, file: "a".into(), started_ms: T0, clean, ended_ms: None },
            samples: (0..n).map(|i| s(i, 97, 9000.0 + i as f32 * 10.0)).collect(),
        }
    }

    fn ev(t_ms: i64, provider: &str, id: u32) -> SysEvent {
        SysEvent { t_ms, provider: provider.into(), id, level: 2, code: None }
    }

    #[test]
    fn clean_session_has_no_report() {
        assert!(build(&prev(true, 60), None, Ok(vec![]), T0 + 999_000).is_none());
    }

    #[test]
    fn restart_with_events_and_numbers() {
        let p = prev(false, 600);
        let stop = T0 + 599_000;
        let events = vec![ev(stop - 3000, "nvlddmkm", 153), ev(stop + 70_000, "Microsoft-Windows-Kernel-Power", 41)];
        let r = build(&p, Some(stop + 40_000), Ok(events), stop + 90_000).unwrap();
        assert_eq!(r.cause, Cause::Restart);
        assert_eq!(r.stopped_ms, stop);
        assert_eq!(r.headline, format!("Your PC restarted unexpectedly at {}.", hm(stop)));
        assert!(r.just_before.starts_with("Just before: GPU at 97%, 79 °C, VRAM 14.6 of 15.9 GB, mostly python.exe"), "{}", r.just_before);
        assert!(r.just_before.contains("slowing down from heat"));
        assert!(r.just_before.ends_with("The NVIDIA driver logged an error."));
        assert_eq!(r.peaks_last_minute.thermal_throttled_s, 9);
        assert_eq!(r.vram_holder.as_ref().unwrap().name, "python.exe");
        // 10 minute lines + last record + 2 events + boot.
        assert_eq!(r.timeline.len(), 14, "{:#?}", r.timeline);
        assert!(r.timeline.windows(2).all(|w| w[0].t_ms <= w[1].t_ms));
        assert!(r.timeline.iter().any(|l| l.text == "Windows started again."));
        let text = r.to_text();
        assert!(text.contains("reset the GPU after it stopped responding"));
        assert!(text.contains("Most VRAM: python.exe (AI/agent)"));
        assert!(r.announcement().starts_with("Your PC restarted unexpectedly at "));
    }

    #[test]
    fn app_closed_without_reboot_and_no_event_log() {
        let p = prev(false, 30);
        let stop = T0 + 29_000;
        let r = build(&p, Some(T0 - 3_600_000), Err("Windows did not allow reading the System event log".into()), stop + 5000).unwrap();
        assert_eq!(r.cause, Cause::AppClosed);
        assert!(r.headline.contains("the PC kept running"));
        assert!(r.to_text().contains("No Windows events: Windows did not allow reading the System event log."));
    }

    #[test]
    fn save_load_announce_and_export() {
        let dir = std::env::temp_dir().join(format!("tachyon_report_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = prev(false, 120);
        let r = build(&p, Some(T0 + 200_000), Ok(vec![]), T0 + 300_000).unwrap();
        save(&dir, &r).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back, r);
        mark_announced(&dir);
        assert!(load(&dir).unwrap().announced);
        let (csv, json) = export(&dir.join("exports"), Some(&r), &p.samples, T0 + 300_000).unwrap();
        assert_eq!(std::fs::read_to_string(csv).unwrap().lines().count(), 121);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
        assert_eq!(v["report"]["cause"], "restart");
        assert_eq!(v["samples"].as_array().unwrap().len(), 120);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
