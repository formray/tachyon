//! Job history: render jobs (from the render logs) and GPU-busy sessions, recorded when they
//! finish as one JSON line each in `%LOCALAPPDATA%\Tachyon\history\jobs.jsonl`, and compared with
//! the previous run of the same job. Everything is accumulated on the fly (sums and peaks), so a
//! long job costs a few hundred bytes, not a stored trace.
use crate::explain::Kind;
use crate::telemetry::Sample;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

/// A GPU-busy session starts after this long at `BUSY_UTIL` or more...
pub const BUSY_START_S: f64 = 60.0;
pub const BUSY_UTIL: u8 = 50;
/// ...and a job (either kind) ends after this long below `QUIET_UTIL`.
pub const QUIET_END_S: f64 = 60.0;
pub const QUIET_UTIL: u8 = 20;
/// Shorter jobs are not recorded.
pub const MIN_JOB_S: f64 = 60.0;
/// The history file rotates to `jobs.1.jsonl` past this size.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Render,
    Gpu,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub kind: JobKind,
    pub name: String,
    /// Jobs with the same key are compared ("Shot 12 try 3" and "Shot 12 try 4" share one).
    pub key: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub duration_s: f64,
    pub avg_gpu: Option<f32>,
    pub peak_gpu: Option<u8>,
    pub avg_fps: Option<f64>,
    pub energy_wh: Option<f64>,
    /// "counter" (the driver's energy counter) or "power" (power readings added up).
    pub energy_source: Option<String>,
    pub peak_vram_mb: Option<f32>,
    pub peak_temp: Option<u8>,
    pub power_throttled_s: f64,
    pub thermal_throttled_s: f64,
    /// Main bottleneck while the job ran, e.g. "GPU-bound (82%)".
    pub bottleneck: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// The comparison with the previous run, filled in when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vs_last: Option<String>,
    /// What the pre-render check matches on and learns from (1.1; empty in older records).
    #[serde(flatten)]
    pub profile: JobProfile,
}

/// Hints about a render job, read once from its logs when it starts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobHints {
    /// Project folder name (never a path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Resolution class: "4K", "1080p", ... or "WxH".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Frames in the job, when a log says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobProfile {
    #[serde(flatten)]
    pub hints: JobHints,
    /// Peak VRAM held by the job's own processes (those that used the GPU during the job).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_vram_mb: Option<f32>,
    /// Card VRAM in use when the job started (other apps, and what the job had loaded by then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_vram_mb: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_total_mb: Option<f32>,
}

/// Strip run counters so retries compare with each other: " try 3", " (2)", " #4", " run 5".
pub fn job_key(kind: JobKind, name: &str) -> String {
    let mut n = name.trim().to_lowercase();
    loop {
        let before = n.len();
        for sep in [" try ", " run ", " #", " attempt "] {
            if let Some(i) = n.rfind(sep)
                && n[i + sep.len()..].chars().all(|c| c.is_ascii_digit())
                && i + sep.len() < n.len()
            {
                n.truncate(i);
            }
        }
        if n.ends_with(')')
            && let Some(i) = n.rfind(" (")
            && n[i + 2..n.len() - 1].chars().all(|c| c.is_ascii_digit())
        {
            n.truncate(i);
        }
        if n.len() == before {
            break;
        }
    }
    format!("{}:{}", if kind == JobKind::Render { "render" } else { "gpu" }, n.trim())
}

#[derive(Debug, Clone, Default)]
struct Acc {
    first_ms: i64,
    last_ms: i64,
    secs: f64,
    util_sum: f64,
    util_secs: f64,
    peak_util: Option<u8>,
    fps_sum: f64,
    fps_secs: f64,
    energy_first: Option<u64>,
    energy_last: Option<u64>,
    energy_ok: bool,
    power_j: f64,
    power_secs: f64,
    peak_vram: Option<f32>,
    peak_temp: Option<u8>,
    power_thr: f64,
    thermal_thr: f64,
    kinds: Vec<(Kind, f64)>,
    names: Vec<(String, f64)>,
    /// Processes that used the GPU during the job, and the peak VRAM they held together.
    pids: Vec<u32>,
    own_peak: Option<f32>,
    own_now: Option<f32>,
    start_vram: Option<f32>,
    vram_total: Option<f32>,
}

/// A process counts as part of the job once it uses this much of the GPU.
const OWN_SM: u8 = 5;

fn bump<K: PartialEq>(v: &mut Vec<(K, f64)>, k: K, by: f64) {
    match v.iter_mut().find(|(x, _)| *x == k) {
        Some(e) => e.1 += by,
        None => v.push((k, by)),
    }
}

impl Acc {
    fn new(s: &Sample) -> Self {
        Acc {
            first_ms: s.t_ms,
            last_ms: s.t_ms,
            energy_first: s.energy_mj,
            energy_last: s.energy_mj,
            energy_ok: true,
            start_vram: s.vram_mb,
            ..Default::default()
        }
    }

    /// VRAM of the job's processes in this sample (learning which ones they are on the way).
    fn own(&mut self, s: &Sample) {
        for p in &s.procs {
            if p.sm.unwrap_or(0) >= OWN_SM && !self.pids.contains(&p.pid) && self.pids.len() < 32 {
                self.pids.push(p.pid);
            }
        }
        let mine: Vec<f32> = s.procs.iter().filter(|p| self.pids.contains(&p.pid)).filter_map(|p| p.vram_mb).collect();
        self.own_now = (!mine.is_empty()).then(|| mine.iter().sum());
        self.own_peak = match (self.own_peak, self.own_now) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if s.vram_total_mb.is_some() {
            self.vram_total = s.vram_total_mb;
        }
    }

    fn add(&mut self, s: &Sample, dt: f64, fps: Option<f64>, kind: Option<Kind>) {
        self.last_ms = s.t_ms;
        self.secs += dt;
        if let Some(u) = s.util {
            self.util_sum += u as f64 * dt;
            self.util_secs += dt;
            self.peak_util = self.peak_util.max(Some(u));
        }
        if let Some(f) = fps {
            self.fps_sum += f * dt;
            self.fps_secs += dt;
        }
        match (s.energy_mj, self.energy_last) {
            (Some(e), Some(l)) if e < l => self.energy_ok = false, // counter reset
            (Some(e), _) => self.energy_last = Some(e),
            (None, _) => self.energy_ok = false,
        }
        if let Some(p) = s.power_w {
            self.power_j += p as f64 * dt;
            self.power_secs += dt;
        }
        self.peak_vram = match (self.peak_vram, s.vram_mb) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.peak_temp = self.peak_temp.max(s.temp);
        self.own(s);
        if s.power_throttled() {
            self.power_thr += dt;
        }
        if s.thermal_throttled() {
            self.thermal_thr += dt;
        }
        if let Some(k) = kind {
            bump(&mut self.kinds, k, dt);
        }
        if let Some(p) = s
            .procs
            .iter()
            .filter(|p| p.sm.unwrap_or(0) > 0 || p.vram_mb.is_some())
            .max_by_key(|p| (p.sm.unwrap_or(0), p.vram_mb.unwrap_or(0.0) as u32))
        {
            bump(&mut self.names, p.name.clone(), dt);
        }
    }

    /// Fold a later stretch (a quiet tail that turned out not to be the end) into this one.
    fn merge(&mut self, t: Acc) {
        self.last_ms = t.last_ms;
        self.secs += t.secs;
        self.util_sum += t.util_sum;
        self.util_secs += t.util_secs;
        self.peak_util = self.peak_util.max(t.peak_util);
        self.fps_sum += t.fps_sum;
        self.fps_secs += t.fps_secs;
        match (t.energy_first, t.energy_last, self.energy_last) {
            (Some(f), Some(l), Some(m)) if f >= m => self.energy_last = Some(l),
            _ => self.energy_ok = false,
        }
        self.energy_ok &= t.energy_ok;
        self.power_j += t.power_j;
        self.power_secs += t.power_secs;
        self.peak_vram = match (self.peak_vram, t.peak_vram) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.peak_temp = self.peak_temp.max(t.peak_temp);
        self.power_thr += t.power_thr;
        self.thermal_thr += t.thermal_thr;
        for (k, v) in t.kinds {
            bump(&mut self.kinds, k, v);
        }
        for (k, v) in t.names {
            bump(&mut self.names, k, v);
        }
        for p in t.pids {
            if !self.pids.contains(&p) && self.pids.len() < 32 {
                self.pids.push(p);
            }
        }
        self.own_peak = match (self.own_peak, t.own_peak) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.own_now = t.own_now;
        self.vram_total = t.vram_total.or(self.vram_total);
    }

    fn top_name(&self) -> Option<String> {
        self.names.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(n, _)| n.clone())
    }

    fn bottleneck(&self) -> Option<String> {
        let total: f64 = self.kinds.iter().map(|k| k.1).sum();
        if total <= 0.0 {
            return None;
        }
        let best = |f: &dyn Fn(Kind) -> bool| self.kinds.iter().filter(|(k, _)| f(*k)).max_by(|a, b| a.1.total_cmp(&b.1)).copied();
        let pick = best(&|k: Kind| k.is_problem()).filter(|(_, v)| v / total >= 0.2).or_else(|| best(&|_| true))?;
        Some(format!("{} ({:.0}%)", pick.0.label(), pick.1 / total * 100.0))
    }

    fn finish(&self, kind: JobKind, name: String, price: Option<(f64, &str)>) -> JobRecord {
        let duration_s = (self.last_ms - self.first_ms) as f64 / 1000.0 + 1.0;
        let (energy_wh, src) = match (self.energy_ok, self.energy_first, self.energy_last) {
            (true, Some(f), Some(l)) if l > f => (Some((l - f) as f64 / 3_600_000.0), Some("counter")),
            _ if self.power_secs >= self.secs * 0.8 && self.power_secs > 0.0 => (Some(self.power_j / 3600.0), Some("power")),
            _ => (None, None),
        };
        let cost = price.zip(energy_wh).map(|((p, _), wh)| (wh / 1000.0 * p * 100.0).round() / 100.0);
        JobRecord {
            key: job_key(kind, &name),
            kind,
            name,
            start_ms: self.first_ms,
            end_ms: self.last_ms,
            duration_s,
            avg_gpu: (self.util_secs > 0.0).then(|| (self.util_sum / self.util_secs) as f32),
            peak_gpu: self.peak_util,
            avg_fps: (self.fps_secs > 0.0).then(|| self.fps_sum / self.fps_secs),
            energy_wh,
            energy_source: src.map(str::to_string),
            peak_vram_mb: self.peak_vram,
            peak_temp: self.peak_temp,
            power_throttled_s: self.power_thr,
            thermal_throttled_s: self.thermal_thr,
            bottleneck: self.bottleneck(),
            cost,
            currency: cost.and(price.map(|(_, c)| c.to_string())),
            vs_last: None,
            profile: JobProfile {
                hints: JobHints::default(),
                own_vram_mb: self.own_peak,
                start_vram_mb: self.start_vram,
                vram_total_mb: self.vram_total,
            },
        }
    }
}

struct Live {
    kind: JobKind,
    name: Option<String>,
    hints: JobHints,
    acc: Acc,
    tail: Option<Acc>,
    quiet_s: f64,
}

/// Follows the samples and returns a finished job when one ends.
#[derive(Default)]
pub struct JobTracker {
    live: Option<Live>,
    busy: Option<Acc>,
    busy_s: f64,
    last_ms: Option<i64>,
    /// (price per kWh, currency)
    pub price: Option<(f64, String)>,
}

/// What is being recorded right now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveJob {
    pub kind: JobKind,
    pub name: String,
    pub start_ms: i64,
    pub elapsed_s: f64,
}

impl JobTracker {
    pub fn with_price(price: Option<(f64, String)>) -> Self {
        JobTracker { price, ..Default::default() }
    }

    /// One sample. `render` is the current render job (name, fps), `kind` the bottleneck verdict.
    pub fn push(&mut self, s: &Sample, render: Option<(&str, Option<f64>)>, kind: Option<Kind>) -> Option<JobRecord> {
        self.push_with(s, render, kind, None)
    }

    /// `push` with what the render logs say about the job (project, resolution, frames).
    pub fn push_with(
        &mut self,
        s: &Sample,
        render: Option<(&str, Option<f64>)>,
        kind: Option<Kind>,
        hints: Option<&JobHints>,
    ) -> Option<JobRecord> {
        let dt = self.last_ms.map_or(1.0, |l| ((s.t_ms - l) as f64 / 1000.0).clamp(0.0, 5.0));
        self.last_ms = Some(s.t_ms);
        let util = s.util.unwrap_or(0);
        let quiet = util < QUIET_UTIL;
        let mut done = None;

        // A render job that changed or a GPU session that a render job takes over ends now.
        if let Some(l) = &self.live {
            let switch = match (l.kind, render) {
                (JobKind::Render, Some((n, _))) => l.name.as_deref() != Some(n),
                (JobKind::Render, None) => true,
                (JobKind::Gpu, Some(_)) => true,
                (JobKind::Gpu, None) => false,
            };
            if switch {
                done = self.end();
            }
        }
        if self.live.is_none()
            && let Some((n, _)) = render
        {
            self.busy = None;
            self.live = Some(Live {
                kind: JobKind::Render,
                name: Some(n.to_string()),
                hints: JobHints::default(),
                acc: Acc::new(s),
                tail: None,
                quiet_s: 0.0,
            });
        }
        if let (Some(l), Some(h)) = (self.live.as_mut(), hints)
            && l.kind == JobKind::Render
        {
            // hints can arrive later (the total frame count appears once the log says it)
            l.hints.project = h.project.clone().or(l.hints.project.take());
            l.hints.resolution = h.resolution.clone().or(l.hints.resolution.take());
            l.hints.frames = h.frames.or(l.hints.frames);
        }

        let fps = render.and_then(|r| r.1);
        if let Some(l) = self.live.as_mut() {
            if quiet {
                l.quiet_s += dt;
                l.tail.get_or_insert_with(|| Acc::new(s)).add(s, dt, fps, kind);
            } else {
                if let Some(t) = l.tail.take() {
                    l.acc.merge(t);
                }
                l.quiet_s = 0.0;
                l.acc.add(s, dt, fps, kind);
            }
            if l.quiet_s >= QUIET_END_S {
                return done.or_else(|| self.end());
            }
            return done;
        }

        // No job: watch for a GPU-busy stretch.
        if util >= BUSY_UTIL {
            let b = self.busy.get_or_insert_with(|| Acc::new(s));
            b.add(s, dt, None, kind);
            self.busy_s += dt;
            if self.busy_s >= BUSY_START_S {
                let acc = self.busy.take().unwrap_or_default();
                self.busy_s = 0.0;
                self.live = Some(Live { kind: JobKind::Gpu, name: None, hints: JobHints::default(), acc, tail: None, quiet_s: 0.0 });
            }
        } else {
            self.busy = None;
            self.busy_s = 0.0;
        }
        done
    }

    /// End the current job (also on exit). Returns it if it was long enough.
    pub fn end(&mut self) -> Option<JobRecord> {
        let l = self.live.take()?;
        let name = l.name.or_else(|| l.acc.top_name()).unwrap_or_else(|| "GPU work".into());
        let price = self.price.as_ref().map(|(p, c)| (*p, c.as_str()));
        let mut r = l.acc.finish(l.kind, name, price);
        r.profile.hints = l.hints;
        (r.duration_s >= MIN_JOB_S).then_some(r)
    }

    /// VRAM the running job's own processes hold right now (0 when none yet).
    pub fn live_own_vram(&self) -> f32 {
        self.live.as_ref().and_then(|l| l.tail.as_ref().and_then(|t| t.own_now).or(l.acc.own_now)).unwrap_or(0.0)
    }

    pub fn live(&self) -> Option<LiveJob> {
        let l = self.live.as_ref()?;
        Some(LiveJob {
            kind: l.kind,
            name: l.name.clone().or_else(|| l.acc.top_name()).unwrap_or_else(|| "GPU work".into()),
            start_ms: l.acc.first_ms,
            elapsed_s: (l.acc.last_ms - l.acc.first_ms) as f64 / 1000.0,
        })
    }
}

pub fn fmt_duration(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", (s + 30) / 60),
        _ => format!("{} h {:02} min", s / 3600, (s % 3600) / 60),
    }
}

/// "14% slower than last time; GPU spent 6 min power-throttled."
pub fn compare(prev: &JobRecord, cur: &JobRecord) -> String {
    if cur.kind == JobKind::Gpu && cur.avg_fps.is_none() {
        return compare_session(prev, cur);
    }
    // Speed: frames per second when both have it, otherwise duration.
    let change = match (prev.avg_fps, cur.avg_fps) {
        (Some(p), Some(c)) if p > 0.0 && c > 0.0 => (p / c - 1.0) * 100.0,
        _ => (cur.duration_s / prev.duration_s.max(1.0) - 1.0) * 100.0,
    };
    let mut parts = vec![if change.abs() < 3.0 {
        "About as fast as last time".to_string()
    } else if change > 0.0 {
        format!("{change:.0}% slower than last time")
    } else {
        format!("{:.0}% faster than last time", -change)
    }];
    if cur.thermal_throttled_s >= 60.0 {
        parts.push(format!("GPU spent {} slowed by heat", fmt_duration(cur.thermal_throttled_s)));
    } else if cur.power_throttled_s >= 60.0 && cur.power_throttled_s > prev.power_throttled_s * 1.2 {
        parts.push(format!("GPU spent {} power-throttled", fmt_duration(cur.power_throttled_s)));
    }
    if let (Some(p), Some(c)) = (prev.energy_wh, cur.energy_wh)
        && p > 0.0
        && ((c / p - 1.0) * 100.0).abs() >= 10.0
    {
        parts.push(format!("energy {:+.0}%", (c / p - 1.0) * 100.0));
    }
    format!("{}.", parts.join("; "))
}

/// A GPU-busy session has no fixed amount of work, so its length says nothing about speed:
/// compare how long it ran, the load and the average power instead.
fn compare_session(prev: &JobRecord, cur: &JobRecord) -> String {
    let mut parts = vec![format!("Ran {} ({} last time)", fmt_duration(cur.duration_s), fmt_duration(prev.duration_s))];
    if let (Some(p), Some(c)) = (prev.avg_gpu, cur.avg_gpu)
        && (c - p).abs() >= 5.0
    {
        parts.push(format!("average GPU {c:.0}% (was {p:.0}%)"));
    }
    if cur.thermal_throttled_s >= 60.0 {
        parts.push(format!("GPU spent {} slowed by heat", fmt_duration(cur.thermal_throttled_s)));
    }
    let watts = |r: &JobRecord| r.energy_wh.filter(|_| r.duration_s >= 1.0).map(|e| e * 3600.0 / r.duration_s);
    if let (Some(p), Some(c)) = (watts(prev), watts(cur))
        && p > 0.0
        && ((c / p - 1.0) * 100.0).abs() >= 10.0
    {
        parts.push(format!("average power {:+.0}%", (c / p - 1.0) * 100.0));
    }
    format!("{}.", parts.join("; "))
}

const FILE: &str = "jobs.jsonl";
const OLD: &str = "jobs.1.jsonl";

/// Append one record, rotating the file when it grows past the cap.
pub fn append(dir: &Path, r: &JobRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join(FILE);
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
        std::fs::rename(&p, dir.join(OLD))?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    let mut line = serde_json::to_vec(r).map_err(std::io::Error::other)?;
    line.push(b'\n');
    f.write_all(&line)
}

/// Longest job line kept; a damaged longer line is skipped.
const MAX_LINE: usize = 64 << 10;

/// Every readable job of one file: a damaged line (bad JSON, bad UTF-8, huge) is skipped,
/// the lines after it still count. Reads at most twice the rotation size.
fn read_file(p: &Path, out: &mut Vec<JobRecord>) {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(p) else { return };
    let mut r = std::io::BufReader::new(f.take(2 * MAX_FILE_BYTES));
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.len() > MAX_LINE && line.last() != Some(&b'\n') {
            // skip the rest of an over-long line
            let mut sink = Vec::new();
            if r.read_until(b'\n', &mut sink).is_err() {
                return;
            }
            continue;
        }
        if let Ok(rec) = serde_json::from_slice(&line) {
            out.push(rec);
        }
    }
}

/// The last `n` jobs, newest last.
pub fn recent(dir: &Path, n: usize) -> Vec<JobRecord> {
    let mut v = vec![];
    read_file(&dir.join(FILE), &mut v);
    if v.len() < n {
        let mut old = vec![];
        read_file(&dir.join(OLD), &mut old);
        old.extend(v);
        v = old;
    }
    let skip = v.len().saturating_sub(n);
    v.split_off(skip)
}

/// The latest earlier run with the same key.
pub fn previous(dir: &Path, key: &str, before_ms: i64) -> Option<JobRecord> {
    recent(dir, 2000).into_iter().rev().find(|r| r.key == key && r.end_ms <= before_ms)
}

/// Fill in `vs_last` from the history and append.
pub fn record(dir: &Path, mut r: JobRecord) -> std::io::Result<JobRecord> {
    if let Some(p) = previous(dir, &r.key, r.start_ms) {
        r.vs_last = Some(compare(&p, &r));
    }
    append(dir, &r)?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::Proc;

    fn s(i: i64, util: u8, power: f32, energy: Option<u64>) -> Sample {
        Sample {
            t_ms: 1_790_000_000_000 + i * 1000,
            util: Some(util),
            temp: Some(60 + (util / 4)),
            power_w: Some(power),
            vram_mb: Some(4000.0 + i as f32),
            throttle: Some(if util > 90 { 0x4 } else { 0 }),
            energy_mj: energy,
            procs: vec![Proc {
                pid: 7,
                name: "blender.exe".into(),
                category: "3D".into(),
                vram_mb: Some(3000.0),
                sm: Some(util),
                enc: None,
                dec: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn keys_ignore_run_counters() {
        assert_eq!(job_key(JobKind::Render, "Shot 12 try 3"), "render:shot 12");
        assert_eq!(job_key(JobKind::Render, "shot 12 (2)"), "render:shot 12");
        assert_eq!(job_key(JobKind::Render, "Shot 12 #4"), "render:shot 12");
        assert_eq!(job_key(JobKind::Gpu, "python.exe"), "gpu:python.exe");
        assert_eq!(job_key(JobKind::Render, "Shot 12"), "render:shot 12");
        assert_eq!(job_key(JobKind::Render, "intro 2024"), "render:intro 2024");
    }

    #[test]
    fn gpu_session_starts_after_a_busy_minute_and_ends_after_a_quiet_one() {
        let mut t = JobTracker::default();
        let mut out = vec![];
        let mut i = 0;
        // 30 s busy then a dip: no session.
        for _ in 0..30 {
            assert!(t.push(&s(i, 80, 300.0, None), None, Some(Kind::GpuBound)).is_none());
            i += 1;
        }
        t.push(&s(i, 10, 50.0, None), None, None);
        i += 1;
        assert!(t.live().is_none());
        // 10 min busy (95% with a power limit), then quiet.
        for _ in 0..600 {
            out.extend(t.push(&s(i, 95, 350.0, None), None, Some(Kind::PowerLimit)));
            i += 1;
        }
        assert_eq!(t.live().unwrap().kind, JobKind::Gpu);
        for _ in 0..70 {
            out.extend(t.push(&s(i, 5, 40.0, None), None, Some(Kind::Idle)));
            i += 1;
        }
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!((r.kind, r.name.as_str(), r.key.as_str()), (JobKind::Gpu, "blender.exe", "gpu:blender.exe"));
        assert!((r.duration_s - 600.0).abs() <= 1.0, "quiet tail trimmed: {}", r.duration_s);
        assert_eq!(r.peak_gpu, Some(95));
        assert!((r.avg_gpu.unwrap() - 95.0).abs() < 0.01);
        // 350 W for 600 s = 58.3 Wh, from power readings.
        assert_eq!(r.energy_source.as_deref(), Some("power"));
        assert!((r.energy_wh.unwrap() - 58.33).abs() < 0.5, "{:?}", r.energy_wh);
        assert!((r.power_throttled_s - 600.0).abs() <= 1.0);
        assert_eq!(r.bottleneck.as_deref(), Some(format!("{} (100%)", Kind::PowerLimit.label()).as_str()));
        assert!(t.live().is_none());
    }

    #[test]
    fn render_job_uses_energy_counter_fps_and_short_dips() {
        let mut t = JobTracker { price: Some((0.30, "EUR".into())), ..Default::default() };
        let mut out = vec![];
        let mut e = 1_000_000u64;
        for i in 0..300 {
            // a 20 s dip in the middle (loading) stays inside the job
            let util = if (100..120).contains(&i) { 5 } else { 70 };
            e += 250_000; // 250 J per second = 250 W
            out.extend(t.push(&s(i, util, 250.0, Some(e)), Some(("Shot 12 try 2", Some(24.0))), Some(Kind::GpuBound)));
        }
        // job name changes: the first one is recorded
        out.extend(t.push(&s(300, 70, 250.0, Some(e + 250_000)), Some(("Shot 13", Some(24.0))), None));
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!((r.kind, r.key.as_str()), (JobKind::Render, "render:shot 12"));
        assert!((r.duration_s - 300.0).abs() <= 1.0);
        assert!((r.avg_fps.unwrap() - 24.0).abs() < 1e-9);
        assert_eq!(r.energy_source.as_deref(), Some("counter"));
        // 299 deltas of 250 J = 20.76 Wh
        assert!((r.energy_wh.unwrap() - 20.76).abs() < 0.05, "{:?}", r.energy_wh);
        assert_eq!(r.cost, Some(0.01));
        assert_eq!(r.currency.as_deref(), Some("EUR"));
        assert_eq!(t.live().unwrap().name, "Shot 13");
    }

    #[test]
    fn render_job_keeps_hints_and_its_own_vram() {
        let mut t = JobTracker::default();
        let mut out = vec![];
        let h = JobHints { project: Some("ladial".into()), resolution: Some("4K".into()), frames: None };
        for i in 0..200 {
            let mut x = s(i, 90, 300.0, None);
            x.vram_total_mb = Some(16376.0);
            x.procs[0].vram_mb = Some(3000.0 + i as f32 * 10.0);
            // an idle app holding memory is not part of the job
            x.procs.push(Proc { pid: 9, name: "chrome.exe".into(), vram_mb: Some(1200.0), sm: Some(0), ..x.procs[0].clone() });
            // the frame count shows up in the log a little later
            let h = if i < 20 { h.clone() } else { JobHints { frames: Some(240), ..Default::default() } };
            out.extend(t.push_with(&x, Some(("demo4k try 1", None)), Some(Kind::GpuBound), Some(&h)));
            if i == 50 {
                assert_eq!(t.live_own_vram(), 3500.0);
            }
        }
        out.extend(t.push(&s(200, 90, 300.0, None), Some(("other", None)), None));
        let r = &out[0];
        assert_eq!(r.profile.hints, JobHints { project: Some("ladial".into()), resolution: Some("4K".into()), frames: Some(240) });
        assert_eq!(r.profile.own_vram_mb, Some(4990.0));
        assert_eq!(r.profile.start_vram_mb, Some(4000.0));
        assert_eq!(r.profile.vram_total_mb, Some(16376.0));
        // flat in the JSONL, and older records without it still read
        let line = serde_json::to_string(r).unwrap();
        assert!(line.contains("\"resolution\":\"4K\"") && line.contains("\"own_vram_mb\":4990"), "{line}");
        assert_eq!(serde_json::from_str::<JobRecord>(&line).unwrap(), *r);
        let mut old: serde_json::Value = serde_json::from_str(&line).unwrap();
        for k in ["project", "resolution", "frames", "own_vram_mb", "start_vram_mb", "vram_total_mb"] {
            old.as_object_mut().unwrap().remove(k);
        }
        assert_eq!(serde_json::from_value::<JobRecord>(old).unwrap().profile, JobProfile::default());
    }

    #[test]
    fn comparison_sentences() {
        let base = JobRecord {
            kind: JobKind::Render,
            name: "Shot 12".into(),
            key: "render:shot 12".into(),
            start_ms: 0,
            end_ms: 1_000_000,
            duration_s: 1000.0,
            avg_gpu: Some(90.0),
            peak_gpu: Some(99),
            avg_fps: None,
            energy_wh: Some(100.0),
            energy_source: Some("counter".into()),
            peak_vram_mb: None,
            peak_temp: None,
            power_throttled_s: 0.0,
            thermal_throttled_s: 0.0,
            bottleneck: None,
            cost: None,
            currency: None,
            vs_last: None,
            profile: Default::default(),
        };
        let slower = JobRecord { duration_s: 1140.0, power_throttled_s: 360.0, energy_wh: Some(104.0), ..base.clone() };
        assert_eq!(compare(&base, &slower), "14% slower than last time; GPU spent 6 min power-throttled.");
        let faster = JobRecord { duration_s: 900.0, energy_wh: Some(80.0), ..base.clone() };
        assert_eq!(compare(&base, &faster), "10% faster than last time; energy -20%.");
        let same = JobRecord { duration_s: 1010.0, ..base.clone() };
        assert_eq!(compare(&base, &same), "About as fast as last time.");
        let fps_a = JobRecord { avg_fps: Some(30.0), ..base.clone() };
        let fps_b = JobRecord { avg_fps: Some(25.0), duration_s: 500.0, thermal_throttled_s: 120.0, ..base.clone() };
        assert_eq!(compare(&fps_a, &fps_b), "20% slower than last time; GPU spent 2 min slowed by heat.");
        // GPU-busy sessions: no "slower", just length, load and average power
        let s1 = JobRecord { kind: JobKind::Gpu, duration_s: 60.0, avg_gpu: Some(79.0), energy_wh: Some(5.0), ..base.clone() };
        let s2 = JobRecord { kind: JobKind::Gpu, duration_s: 120.0, avg_gpu: Some(95.0), energy_wh: Some(11.0), ..base.clone() };
        assert_eq!(compare(&s1, &s2), "Ran 2 min (1 min last time); average GPU 95% (was 79%); average power +10%.");
        let s3 = JobRecord { energy_wh: Some(10.0), avg_gpu: Some(81.0), ..s2.clone() };
        assert_eq!(compare(&s2, &s3), "Ran 2 min (2 min last time); average GPU 81% (was 95%).");
    }

    #[test]
    fn file_append_recent_previous_and_rotation() {
        let dir = std::env::temp_dir().join(format!("tachyon_hist_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut t = JobTracker::default();
        let mut mk = |start: i64, secs: i64| {
            let mut r = None;
            for i in 0..secs {
                t.push(&s(start + i, 80, 200.0, None), Some(("Shot 1", None)), None);
            }
            r = r.or(t.end());
            r.unwrap()
        };
        let a = mk(0, 100);
        let b = mk(100, 120);
        let a = record(&dir, a).unwrap();
        assert!(a.vs_last.is_none());
        let b = record(&dir, b).unwrap();
        assert_eq!(b.vs_last.as_deref(), Some("20% slower than last time; energy +20%."), "{b:?}");
        assert_eq!(recent(&dir, 10).len(), 2);
        assert_eq!(previous(&dir, "render:shot 1", b.start_ms).unwrap().start_ms, a.start_ms);
        // rotation keeps reading across both files
        std::fs::write(dir.join(FILE), vec![b'\n'; MAX_FILE_BYTES as usize + 1]).unwrap();
        append(&dir, &a).unwrap();
        assert!(dir.join(OLD).exists());
        assert_eq!(recent(&dir, 10).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_job_starting_quiet_keeps_the_energy_counter() {
        // Real renders load for a few seconds first: the job's first sample is quiet, so the
        // quiet tail is merged into an accumulator that has only seen that one sample.
        let mut t = JobTracker::default();
        let mut e = 1_000_000u64;
        let mut out = vec![];
        for i in 0..120 {
            let util = if i < 5 { 2 } else { 80 };
            e += 250_000;
            out.extend(t.push(&s(i, util, 250.0, Some(e)), Some(("Shot 12", None)), None));
        }
        out.extend(t.end());
        let r = &out[0];
        assert_eq!(r.energy_source.as_deref(), Some("counter"), "{r:?}");
        // 119 deltas of 250 J
        assert!((r.energy_wh.unwrap() - 119.0 * 250.0 / 3600.0).abs() < 0.01, "{:?}", r.energy_wh);
    }
}
