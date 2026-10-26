//! Render job markers (1.2). A render script runs `tachyon mark start --job NAME` before a job and
//! `tachyon mark end --job NAME` after it; each call appends one line to
//! `markers/markers.jsonl` in Tachyon's data folder and exits (no window, nothing else touched).
//! The running Tachyon reads new lines every tick and builds a report for each marked job:
//! duration, average and peak GPU load, peak VRAM, power and energy, time spent clock-limited,
//! and the PCIe link. Finished reports go to `history/marked_jobs.jsonl`.
use crate::fast::FastAgg;
use crate::pcie::LinkStatus;
use crate::telemetry::Sample;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const DIR: &str = "markers";
pub const FILE: &str = "markers.jsonl";
const OLD: &str = "markers.1.jsonl";
const MAX_FILE_BYTES: u64 = 1 << 20;
pub const REPORTS: &str = "marked_jobs.jsonl";
const REPORTS_OLD: &str = "marked_jobs.1.jsonl";
const MAX_REPORTS_BYTES: u64 = 4 << 20;
/// A job with no end marker is closed after this long.
pub const MAX_JOB_MS: i64 = 48 * 3_600_000;
/// Jobs open at once (a script that never ends its jobs cannot grow this without bound).
pub const MAX_OPEN: usize = 16;
pub const MAX_NAME: usize = 120;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub t: i64,
    /// "start" or "end".
    pub op: String,
    pub job: String,
    #[serde(default)]
    pub pid: u32,
}

pub fn clean_name(s: &str) -> Result<String, String> {
    let n: String = s.trim().chars().filter(|c| !c.is_control()).collect();
    if n.is_empty() {
        return Err("the job name is empty".into());
    }
    if n.chars().count() > MAX_NAME {
        return Err(format!("the job name is longer than {MAX_NAME} characters"));
    }
    Ok(n)
}

pub fn file(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR).join(FILE)
}

/// Append one marker line.
pub fn write_mark(data_dir: &Path, op: &str, job: &str, t: i64) -> Result<Mark, String> {
    if op != "start" && op != "end" {
        return Err(format!("unknown marker \"{op}\" (start or end)"));
    }
    let m = Mark { t, op: op.into(), job: clean_name(job)?, pid: std::process::id() };
    let dir = data_dir.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let p = dir.join(FILE);
    if std::fs::metadata(&p).is_ok_and(|x| x.len() > MAX_FILE_BYTES) {
        let _ = std::fs::rename(&p, dir.join(OLD));
    }
    let mut line = serde_json::to_string(&m).map_err(|e| e.to_string())?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| format!("could not write {}: {e}", p.display()))?;
    Ok(m)
}

pub const USAGE: &str = "usage: tachyon mark start --job NAME | tachyon mark end --job NAME";

/// `tachyon mark ...` (the arguments after "mark"). Returns the exit code and a line to print.
pub fn cli(args: &[String], data_dir: Option<PathBuf>, now: i64) -> (i32, String) {
    let Some(op) = args.first().map(|s| s.to_lowercase()) else { return (2, USAGE.into()) };
    let mut job = None;
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--job=") {
            job = Some(v.to_string());
        } else if a == "--job" || a == "-j" {
            job = args.get(i + 1).cloned();
            i += 1;
        } else {
            return (2, format!("unknown argument \"{a}\"\n{USAGE}"));
        }
        i += 1;
    }
    let Some(job) = job else { return (2, USAGE.into()) };
    let Some(dir) = data_dir else { return (1, "no data folder (LOCALAPPDATA is not set)".into()) };
    match write_mark(&dir, &op, &job, now) {
        Ok(m) => (0, format!("marked {} of \"{}\" at {}", m.op, m.job, hm(m.t))),
        Err(e) => (if e.starts_with("unknown") || e.contains("name") { 2 } else { 1 }, e),
    }
}

fn hm(t: i64) -> String {
    Sample { t_ms: t, ..Default::default() }.local().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// All markers on disk, oldest first.
pub fn read_all(data_dir: &Path) -> Vec<Mark> {
    let mut v = vec![];
    for name in [OLD, FILE] {
        let Ok(f) = std::fs::File::open(data_dir.join(DIR).join(name)) else { continue };
        for line in std::io::BufReader::new(f.take(MAX_FILE_BYTES * 2)).lines().map_while(Result::ok) {
            if let Ok(m) = serde_json::from_str::<Mark>(&line) {
                v.push(m);
            }
        }
    }
    v.sort_by_key(|m| m.t);
    v
}

/// What a marked job looked like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobReport {
    pub job: String,
    pub start: String,
    pub end: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub duration_s: f64,
    /// "end marker", "started again" or "no end marker within 48 h".
    pub ended_by: String,
    /// Seconds Tachyon was recording during the job (less when it was started late or closed).
    pub covered_s: f64,
    pub gpu_util_avg: Option<f32>,
    pub gpu_util_peak: Option<u8>,
    pub mem_ctrl_avg: Option<f32>,
    pub vram_start_mb: Option<f32>,
    pub vram_peak_mb: Option<f32>,
    pub vram_end_mb: Option<f32>,
    pub vram_total_mb: Option<f32>,
    pub power_avg_w: Option<f32>,
    /// Highest 100 ms reading (1 s without fast sampling).
    pub power_peak_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub energy_wh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
    pub temp_avg_c: Option<f32>,
    pub temp_peak_c: Option<u8>,
    pub clk_sm_min_mhz: Option<u16>,
    pub clk_sm_max_mhz: Option<u16>,
    /// Seconds clock-limited by power (cap or brake) and by heat, and at 98 %+ of the power limit.
    pub power_limited_s: f64,
    pub thermal_limited_s: f64,
    pub at_power_limit_s: f64,
    pub clock_limit_reasons: Vec<String>,
    pub pcie: serde_json::Value,
    pub top_vram_processes: Vec<(String, f32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottleneck_mostly: Option<String>,
    pub sentence: String,
}

#[derive(Debug, Clone, Default)]
struct Acc {
    name: String,
    start_ms: i64,
    n: u32,
    util_sum: f64,
    util_n: u32,
    util_peak: Option<u8>,
    mem_sum: f64,
    mem_n: u32,
    vram_start: Option<f32>,
    vram_peak: Option<f32>,
    vram_end: Option<f32>,
    vram_total: Option<f32>,
    power_sum: f64,
    power_n: u32,
    power_peak: Option<f32>,
    power_limit: Option<f32>,
    energy_first: Option<u64>,
    energy_last: Option<u64>,
    energy_j: f64,
    last_t: Option<i64>,
    covered_ms: i64,
    temp_sum: f64,
    temp_n: u32,
    temp_peak: Option<u8>,
    clk_min: Option<u16>,
    clk_max: Option<u16>,
    fast: FastAgg,
    slow_power_s: u32,
    slow_thermal_s: u32,
    slow_at_limit_s: u32,
    throttle: u32,
    link_start: Option<String>,
    link_end: Option<String>,
    link_best: Option<String>,
    link_state_end: Option<String>,
    link_degraded_at_start: u32,
    link_degraded_now: u32,
    procs: BTreeMap<String, f32>,
    kinds: BTreeMap<&'static str, u32>,
}

fn omax<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y > x { y } else { x }),
        (x, y) => x.or(y),
    }
}
fn omin<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y < x { y } else { x }),
        (x, y) => x.or(y),
    }
}

impl Acc {
    fn add(&mut self, s: &Sample, fast: &FastAgg, link: Option<&LinkStatus>, kind: Option<crate::explain::Kind>) {
        use crate::telemetry::{THR_POWER, THR_THERMAL};
        self.n += 1;
        // seconds covered: gaps longer than 5 s (Tachyon closed, PC asleep) do not count
        if let Some(l) = self.last_t {
            let dt = s.t_ms - l;
            if (0..=5_000).contains(&dt) {
                self.covered_ms += dt;
                if let Some(p) = s.power_w {
                    self.energy_j += p as f64 * dt as f64 / 1000.0;
                }
            }
        }
        self.last_t = Some(s.t_ms);
        if let Some(u) = s.util {
            self.util_sum += u as f64;
            self.util_n += 1;
            self.util_peak = omax(self.util_peak, Some(u));
        }
        if let Some(m) = s.mem_ctrl {
            self.mem_sum += m as f64;
            self.mem_n += 1;
        }
        if s.vram_mb.is_some() {
            self.vram_start = self.vram_start.or(s.vram_mb);
            self.vram_end = s.vram_mb;
        }
        self.vram_peak = omax(self.vram_peak, s.vram_mb);
        self.vram_total = s.vram_total_mb.or(self.vram_total);
        if let Some(p) = s.power_w {
            self.power_sum += p as f64;
            self.power_n += 1;
        }
        self.power_peak = omax(omax(self.power_peak, s.power_w), fast.peak_power_w);
        self.power_limit = s.power_limit_w.or(self.power_limit);
        if let Some(e) = s.energy_mj {
            if self.energy_first.is_none() {
                self.energy_first = Some(e);
            }
            self.energy_last = Some(e);
        }
        if let Some(t) = s.temp {
            self.temp_sum += t as f64;
            self.temp_n += 1;
        }
        self.temp_peak = omax(omax(self.temp_peak, s.temp), fast.max_temp);
        self.clk_min = omin(omin(self.clk_min, s.clk_sm.filter(|c| *c > 0)), fast.min_clk_sm);
        self.clk_max = omax(omax(self.clk_max, s.clk_sm), fast.max_clk_sm);
        self.fast.merge(fast);
        let thr = s.throttle.unwrap_or(0);
        self.throttle |= thr | fast.throttle;
        if fast.n == 0 {
            // no fast readings this tick: count the 1 s reading
            self.slow_power_s += (thr & THR_POWER != 0) as u32;
            self.slow_thermal_s += (thr & THR_THERMAL != 0) as u32;
            self.slow_at_limit_s += s.power_w.zip(s.power_limit_w).is_some_and(|(p, l)| l > 0.0 && p / l >= 0.98) as u32;
        }
        if let Some(l) = link {
            if self.link_start.is_none() {
                self.link_start = Some(l.link.clone());
                self.link_degraded_at_start = l.degraded_events;
            }
            self.link_end = Some(l.link.clone());
            self.link_best = Some(l.best.clone());
            self.link_state_end = l.state.map(|s| s.words().to_string());
            self.link_degraded_now = l.degraded_events;
        }
        if let Some(k) = kind {
            *self.kinds.entry(k.label()).or_default() += 1;
        }
    }

    fn procs(&mut self, list: &[(String, u32, f32)]) {
        let mut by: BTreeMap<&str, f32> = BTreeMap::new();
        for (n, _, mb) in list {
            *by.entry(n).or_default() += mb;
        }
        for (n, mb) in by {
            let e = self.procs.entry(n.to_string()).or_default();
            *e = e.max(mb);
        }
    }

    fn finish(self, end_ms: i64, ended_by: &str, price: Option<&(f64, String)>) -> JobReport {
        let period_s = self.fast.period_ms.max(1) as f64 / 1000.0;
        let fast_s = |n: u32| (n as f64 * period_s * 10.0).round() / 10.0;
        let power_s = fast_s(self.fast.power_thr) + self.slow_power_s as f64;
        let thermal_s = fast_s(self.fast.thermal_thr) + self.slow_thermal_s as f64;
        let at_limit_s = fast_s(self.fast.at_limit) + self.slow_at_limit_s as f64;
        let energy_wh = match (self.energy_first, self.energy_last) {
            (Some(a), Some(b)) if b > a && self.covered_ms > 0 => Some((b - a) as f64 / 3_600_000.0),
            _ => (self.energy_j > 0.0).then_some(self.energy_j / 3600.0),
        }
        .map(|w| (w * 100.0).round() / 100.0);
        let cost = price.zip(energy_wh).map(|((p, c), wh)| format!("{:.3} {c}", wh / 1000.0 * p));
        let avg = |s: f64, n: u32| (n > 0).then(|| ((s / n as f64) * 10.0).round() as f32 / 10.0);
        let mut top: Vec<(String, f32)> = self.procs.into_iter().collect();
        top.sort_by(|a, b| b.1.total_cmp(&a.1));
        top.truncate(5);
        let mostly = self.kinds.iter().max_by_key(|(_, n)| **n).map(|(k, _)| k.to_string());
        let reasons: Vec<String> = (0..16u32)
            .filter(|b| self.throttle & (1 << b) != 0)
            .flat_map(|b| crate::telemetry::throttle_words(1 << b))
            .map(String::from)
            .collect();
        let degraded = self.link_degraded_now.saturating_sub(self.link_degraded_at_start);
        let worst = match (self.fast.min_gen, self.fast.min_width) {
            (None, None) => None,
            (g, w) => Some(crate::pcie::link_text(g.map(u32::from), w.map(u32::from))),
        };
        let pcie = serde_json::json!({
            "at_start": self.link_start,
            "at_end": self.link_end,
            "lowest_seen": worst,
            "best_possible": self.link_best,
            "degraded_events": degraded,
            "state_at_end": self.link_state_end,
        });
        let duration_s = ((end_ms - self.start_ms) as f64 / 1000.0).max(0.0);
        let covered_s = (self.covered_ms as f64 / 1000.0).round();
        let util_avg = avg(self.util_sum, self.util_n);
        let power_avg = avg(self.power_sum, self.power_n);
        let mut parts = vec![format!("\"{}\" ran {}", self.name, crate::history::fmt_duration(duration_s))];
        if covered_s + 5.0 < duration_s {
            parts.push(format!("(Tachyon recorded {} of it)", crate::history::fmt_duration(covered_s)));
        }
        if let Some(u) = util_avg {
            parts.push(format!("GPU {u:.0}% on average (peak {}%)", self.util_peak.unwrap_or(0)));
        }
        if let Some(v) = self.vram_peak {
            parts.push(format!("VRAM peak {:.1} GB", v / 1024.0));
        }
        if let Some(p) = power_avg {
            parts.push(format!("{p:.0} W average, {:.0} W peak", self.power_peak.unwrap_or(p)));
        }
        if let Some(e) = energy_wh {
            parts.push(format!("{e:.1} Wh"));
        }
        let mut sentence = parts.join(", ") + ".";
        if power_s + thermal_s >= 1.0 {
            sentence.push_str(&format!(" Clock-limited by power for {power_s:.0} s and by heat for {thermal_s:.0} s."));
        }
        if degraded > 0 {
            sentence.push_str(&format!(" The PCIe link degraded {degraded} time(s) during the job."));
        }
        JobReport {
            job: self.name,
            start: hm(self.start_ms),
            end: hm(end_ms),
            start_ms: self.start_ms,
            end_ms,
            duration_s: duration_s.round(),
            ended_by: ended_by.into(),
            covered_s,
            gpu_util_avg: util_avg,
            gpu_util_peak: self.util_peak,
            mem_ctrl_avg: avg(self.mem_sum, self.mem_n),
            vram_start_mb: self.vram_start,
            vram_peak_mb: self.vram_peak,
            vram_end_mb: self.vram_end,
            vram_total_mb: self.vram_total,
            power_avg_w: power_avg,
            power_peak_w: self.power_peak,
            power_limit_w: self.power_limit,
            energy_wh,
            cost,
            temp_avg_c: avg(self.temp_sum, self.temp_n),
            temp_peak_c: self.temp_peak,
            clk_sm_min_mhz: self.clk_min,
            clk_sm_max_mhz: self.clk_max,
            power_limited_s: power_s,
            thermal_limited_s: thermal_s,
            at_power_limit_s: at_limit_s,
            clock_limit_reasons: reasons,
            pcie,
            top_vram_processes: top,
            bottleneck_mostly: mostly,
            sentence,
        }
    }
}

/// An open marked job, for the status.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenJob {
    pub job: String,
    pub started: String,
    pub running_s: f64,
}

/// Follows the marker file and keeps the open jobs.
#[derive(Debug, Default)]
pub struct MarkerTracker {
    path: PathBuf,
    offset: u64,
    open: Vec<Acc>,
    /// Problems with the markers ("end without a start"), newest last, a few.
    pub notes: Vec<String>,
}

impl MarkerTracker {
    /// Start following `data_dir`'s marker file. Jobs started (and not ended) before Tachyon was
    /// running are picked up, with their data from now on.
    pub fn open(data_dir: &Path, now: i64) -> Self {
        let mut t = MarkerTracker { path: file(data_dir), ..Default::default() };
        let marks = read_all(data_dir);
        let mut open: Vec<(String, i64)> = vec![];
        for m in marks {
            open.retain(|(j, _)| *j != m.job);
            if m.op == "start" && now - m.t < MAX_JOB_MS {
                open.push((m.job, m.t));
            }
        }
        for (job, start) in open.into_iter().rev().take(MAX_OPEN) {
            t.open.push(Acc { name: job, start_ms: start, ..Default::default() });
        }
        t.offset = std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
        t
    }

    /// New marker lines since the last call.
    fn new_marks(&mut self) -> Vec<Mark> {
        let Ok(meta) = std::fs::metadata(&self.path) else { return vec![] };
        if meta.len() < self.offset {
            // rotated: the new file is read from its start
            self.offset = 0;
        }
        if meta.len() == self.offset {
            return vec![];
        }
        let Ok(mut f) = std::fs::File::open(&self.path) else { return vec![] };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return vec![];
        }
        let mut buf = vec![];
        if f.take(MAX_FILE_BYTES).read_to_end(&mut buf).is_err() {
            return vec![];
        }
        // only whole lines; a line being written is read next time
        let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        self.offset += end as u64;
        String::from_utf8_lossy(&buf[..end]).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    fn note(&mut self, s: String) {
        self.notes.push(s);
        if self.notes.len() > 5 {
            self.notes.remove(0);
        }
    }

    /// One tick: read new markers, add the sample to every open job, and return the reports of
    /// the jobs that ended.
    pub fn tick(
        &mut self,
        s: &Sample,
        fast: &FastAgg,
        link: Option<&LinkStatus>,
        kind: Option<crate::explain::Kind>,
        price: Option<&(f64, String)>,
    ) -> Vec<JobReport> {
        let mut done = vec![];
        for m in self.new_marks() {
            let i = self.open.iter().position(|a| a.name == m.job);
            match (m.op.as_str(), i) {
                ("start", Some(i)) => {
                    let a = self.open.remove(i);
                    done.push(a.finish(m.t, "started again", price));
                    self.open.push(Acc { name: m.job, start_ms: m.t, ..Default::default() });
                }
                ("start", None) => {
                    if self.open.len() >= MAX_OPEN {
                        let a = self.open.remove(0);
                        self.note(format!("Too many open marked jobs; \"{}\" was closed.", a.name));
                        done.push(a.finish(m.t, "too many open jobs", price));
                    }
                    self.open.push(Acc { name: m.job, start_ms: m.t, ..Default::default() });
                }
                ("end", Some(i)) => {
                    let a = self.open.remove(i);
                    let end = m.t.max(a.start_ms);
                    done.push(a.finish(end, "end marker", price));
                }
                ("end", None) => self.note(format!("An end marker for \"{}\" at {} had no start.", m.job, hm(m.t))),
                _ => {}
            }
        }
        let now = s.t_ms;
        let mut i = 0;
        while i < self.open.len() {
            if now - self.open[i].start_ms >= MAX_JOB_MS {
                let a = self.open.remove(i);
                done.push(a.finish(now, "no end marker within 48 h", price));
            } else {
                if s.t_ms >= self.open[i].start_ms {
                    self.open[i].add(s, fast, link, kind);
                }
                i += 1;
            }
        }
        done
    }

    /// Per-process VRAM readings for the open jobs.
    pub fn procs(&mut self, list: &[(String, u32, f32)]) {
        for a in self.open.iter_mut() {
            a.procs(list);
        }
    }

    pub fn open_jobs(&self, now: i64) -> Vec<OpenJob> {
        self.open
            .iter()
            .map(|a| OpenJob { job: a.name.clone(), started: hm(a.start_ms), running_s: ((now - a.start_ms) as f64 / 1000.0).round() })
            .collect()
    }
}

pub fn save_report(history_dir: &Path, r: &JobReport) -> std::io::Result<()> {
    std::fs::create_dir_all(history_dir)?;
    let p = history_dir.join(REPORTS);
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > MAX_REPORTS_BYTES) {
        std::fs::rename(&p, history_dir.join(REPORTS_OLD))?;
    }
    let mut line = serde_json::to_string(r).map_err(std::io::Error::other)?;
    line.push('\n');
    std::fs::OpenOptions::new().create(true).append(true).open(&p)?.write_all(line.as_bytes())
}

/// Saved reports, oldest first, at most `n` (the newest).
pub fn reports(history_dir: &Path, n: usize) -> Vec<JobReport> {
    let mut v = vec![];
    for name in [REPORTS_OLD, REPORTS] {
        let Ok(f) = std::fs::File::open(history_dir.join(name)) else { continue };
        for line in std::io::BufReader::new(f.take(MAX_REPORTS_BYTES * 2)).lines().map_while(Result::ok) {
            if let Ok(r) = serde_json::from_str::<JobReport>(&line) {
                v.push(r);
            }
        }
    }
    v.sort_by_key(|r| r.end_ms);
    let skip = v.len().saturating_sub(n);
    v.split_off(skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tachyon_mk_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn s(t: i64, util: u8, power: f32, vram: f32) -> Sample {
        Sample {
            t_ms: t,
            util: Some(util),
            power_w: Some(power),
            power_limit_w: Some(360.0),
            vram_mb: Some(vram),
            vram_total_mb: Some(16303.0),
            temp: Some(60),
            clk_sm: Some(2800),
            throttle: Some(if power > 350.0 { 0x4 } else { 0 }),
            ..Default::default()
        }
    }

    #[test]
    fn cli_parses_and_validates() {
        let d = tmp("cli");
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cli(&a(&["start"]), Some(d.clone()), 1).0, 2);
        assert_eq!(cli(&a(&["pause", "--job", "x"]), Some(d.clone()), 1).0, 2);
        assert_eq!(cli(&a(&["start", "--job", "  "]), Some(d.clone()), 1).0, 2);
        assert_eq!(cli(&a(&["start", "--frames", "3"]), Some(d.clone()), 1).0, 2);
        assert_eq!(cli(&a(&["start", "--job", "x"]), None, 1).0, 1);
        assert_eq!(cli(&a(&["START", "--job", "shot 12"]), Some(d.clone()), 1000).0, 0);
        assert_eq!(cli(&a(&["end", "--job=shot 12"]), Some(d.clone()), 2000).0, 0);
        let m = read_all(&d);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].op.as_str(), m[0].job.as_str(), m[1].op.as_str()), ("start", "shot 12", "end"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_marked_job_gets_a_report() {
        let d = tmp("job");
        let t0 = 1_793_000_000_000i64;
        write_mark(&d, "end", "ghost", t0 - 5000).unwrap();
        let mut tr = MarkerTracker::open(&d, t0);
        write_mark(&d, "start", "shot 12", t0).unwrap();
        let fast = FastAgg { n: 10, peak_power_w: Some(372.0), power_thr: 4, period_ms: 100, ..Default::default() };
        let link =
            LinkStatus { link: "Gen5 x16".into(), best: "Gen5 x16".into(), state: Some(crate::pcie::State::Full), ..Default::default() };
        let mut done = vec![];
        for i in 0..61 {
            let (u, p, v) = if i < 30 { (99, 340.0, 9000.0 + i as f32 * 100.0) } else { (97, 355.0, 12000.0) };
            done.extend(tr.tick(
                &s(t0 + i * 1000, u, p, v),
                &fast,
                Some(&link),
                Some(crate::explain::Kind::GpuBound),
                Some(&(0.30, "EUR".into())),
            ));
            if i == 10 {
                tr.procs(&[("python.exe".into(), 10, 8000.0), ("python.exe".into(), 11, 900.0), ("dwm.exe".into(), 4, 200.0)]);
                assert_eq!(tr.open_jobs(t0 + 10_000)[0].running_s, 10.0);
            }
        }
        assert!(done.is_empty());
        write_mark(&d, "end", "shot 12", t0 + 60_500).unwrap();
        let r = tr.tick(&s(t0 + 61_000, 5, 40.0, 3000.0), &fast, Some(&link), None, Some(&(0.30, "EUR".into())));
        assert_eq!(r.len(), 1);
        let r = &r[0];
        assert_eq!((r.job.as_str(), r.ended_by.as_str(), r.duration_s), ("shot 12", "end marker", 61.0));
        assert_eq!(r.gpu_util_peak, Some(99));
        assert_eq!(r.vram_peak_mb, Some(12000.0));
        assert_eq!(r.power_peak_w, Some(372.0), "the 100 ms peak");
        assert_eq!(r.covered_s, 60.0);
        // each second counts at the power read at its end: 29 s x 340 W + 31 s x 355 W = 20865 J = 5.80 Wh
        assert_eq!(r.energy_wh, Some(5.8));
        assert_eq!(r.power_limited_s, 24.4, "61 ticks x 4 readings x 100 ms, plus the 1 s readings: {r:?}");
        assert_eq!(r.top_vram_processes[0], ("python.exe".to_string(), 8900.0));
        assert_eq!(r.bottleneck_mostly.as_deref(), Some("GPU-bound"));
        assert_eq!(r.pcie["at_start"], "Gen5 x16");
        assert!(r.sentence.contains("\"shot 12\" ran 1 min"), "{}", r.sentence);
        assert!(r.cost.is_some());
        assert!(tr.notes.is_empty(), "the old end marker was before Tachyon started");
        save_report(&d.join("history"), r).unwrap();
        assert_eq!(reports(&d.join("history"), 10), vec![r.clone()]);
        // an end without a start, and a job started before Tachyon was running
        write_mark(&d, "end", "nobody", t0 + 62_000).unwrap();
        write_mark(&d, "start", "overnight", t0 + 63_000).unwrap();
        tr.tick(&s(t0 + 64_000, 0, 30.0, 1000.0), &fast, None, None, None);
        assert_eq!(tr.notes.len(), 1);
        let tr2 = MarkerTracker::open(&d, t0 + 70_000);
        assert_eq!(tr2.open_jobs(t0 + 70_000)[0].job, "overnight");
        let _ = std::fs::remove_dir_all(&d);
    }
}
