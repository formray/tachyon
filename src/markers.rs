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

/// How a job report reads at a glance: neutral, or amber when something needs a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Note,
    Warning,
}

impl JobReport {
    /// One plain sentence: how busy the GPU was, and whether anything held it back that the
    /// user should act on (heat, a slow PCIe link, VRAM nearly full). Power-cap time under heavy
    /// load is normal and said so.
    pub fn verdict(&self) -> (String, Tone) {
        let mut tone = Tone::Note;
        let busy = match self.gpu_util_avg {
            Some(u) if u >= 90.0 => format!("the GPU almost fully busy ({u:.0}% on average)"),
            Some(u) if u >= 60.0 => format!("the GPU busy ({u:.0}% on average)"),
            Some(u) => format!("the GPU lightly used ({u:.0}% on average)"),
            None => "no GPU readings".to_string(),
        };
        let mut parts = vec![format!("Ran {} with {busy}", crate::history::fmt_duration(self.duration_s))];
        if self.thermal_limited_s >= 10.0 {
            tone = Tone::Warning;
            parts.push(format!(
                "it slowed down from heat {} of the time: check the cooling",
                share(self.thermal_limited_s, self.duration_s)
            ));
        }
        let full = self.vram_peak_mb.zip(self.vram_total_mb).is_some_and(|(p, t)| t > 0.0 && p / t >= 0.95);
        if full {
            tone = Tone::Warning;
            parts.push("VRAM was nearly full: a bigger job may not fit".into());
        }
        let degraded = self.pcie["degraded_events"].as_u64().unwrap_or(0) > 0;
        if degraded {
            tone = Tone::Warning;
            parts.push("the PCIe link was slower than the slot allows".into());
        }
        let mut s = parts.join("; ");
        s.push('.');
        if tone == Tone::Note {
            if self.power_limited_s >= 10.0 {
                s.push_str(&format!(
                    " It was at the power limit {} of the time, which is normal under full load. Nothing to fix.",
                    self.power_limit_share()
                ));
            } else {
                s.push_str(" Nothing held it back. Nothing to fix.");
            }
        }
        (s, tone)
    }

    /// Reports saved by 1.2 test builds counted idle power-saving readings (Gen2 just after an
    /// idle minute) as the lowest link. With no degraded event the link was never slow under load,
    /// so the lowest under load is the best link (or unknown when the GPU was never busy).
    pub fn revise(&mut self) {
        let p = &mut self.pcie;
        if p["degraded_events"].as_u64() != Some(0) || p["lowest_seen"].is_null() || p["lowest_seen"] == p["best_possible"] {
            return;
        }
        let busy = self.gpu_util_avg.is_some_and(|u| u >= crate::pcie::BUSY_UTIL as f32);
        p["lowest_seen"] = if busy { p["best_possible"].clone() } else { serde_json::Value::Null };
    }

    /// The PCIe link during the job in plain words.
    pub fn pcie_sentence(&self) -> String {
        let p = &self.pcie;
        let s = |k: &str| p[k].as_str().map(str::to_string);
        match (s("lowest_seen"), s("best_possible")) {
            (None, Some(best)) if p["degraded_events"].as_u64().unwrap_or(0) == 0 => {
                format!("The GPU was not busy enough to test the link (best possible: {}).", link_gloss(&best))
            }
            (Some(low), Some(best)) if low == best && p["degraded_events"].as_u64().unwrap_or(0) == 0 => {
                format!("{} for the whole job: the fastest this slot allows.", link_gloss(&best))
            }
            (Some(low), Some(best)) => format!(
                "Between {low} and {} (the best possible); {} slowdowns under load.",
                link_gloss(&best),
                p["degraded_events"].as_u64().unwrap_or(0)
            ),
            _ => "Not reported by this GPU or driver.".into(),
        }
    }

    /// Time at the power limit as a share of the job: "55%".
    pub fn power_limit_share(&self) -> String {
        share(self.power_limited_s, self.duration_s)
    }

    /// The comparison with the previous run of the same job (same name, run counters ignored) in `reps` (any order).
    pub fn compare_in(&self, reps: &[JobReport]) -> Option<Comparison> {
        previous(reps, self).map(|p| compare(p, self))
    }

    /// As a history row (the history list shows marked jobs too, instead of the automatic
    /// session recorded at the same time).
    pub fn as_record(&self, vs_last: Option<String>) -> crate::history::JobRecord {
        crate::history::JobRecord {
            kind: crate::history::JobKind::Render,
            name: self.job.clone(),
            key: format!("{MARKED_KEY}{}", crate::history::job_key(crate::history::JobKind::Render, &self.job)),
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            duration_s: self.duration_s,
            avg_gpu: self.gpu_util_avg,
            peak_gpu: self.gpu_util_peak,
            avg_fps: None,
            energy_wh: self.energy_wh,
            energy_source: Some("marked".into()),
            peak_vram_mb: self.vram_peak_mb,
            peak_temp: self.temp_peak_c,
            power_throttled_s: self.power_limited_s,
            thermal_throttled_s: self.thermal_limited_s,
            bottleneck: self.bottleneck_mostly.clone(),
            cost: None,
            currency: None,
            vs_last,
            profile: Default::default(),
        }
    }
}

/// The link of one reading if it counts for "the lowest under load": the GPU busy (or a link
/// narrower than the slot, which is never power saving) and the link not idle-saving or still
/// training up. When degraded, the 100 ms minimum of this second counts too.
fn under_load_link(l: &LinkStatus, util: Option<u8>, fast: &FastAgg) -> Option<(u32, u32)> {
    use crate::pcie::State;
    let busy = util.is_some_and(|u| u >= crate::pcie::BUSY_UTIL);
    let counts = match l.state? {
        State::DegradedWidth => true,
        State::Full | State::DegradedGen => busy,
        State::IdleSaving | State::Settling | State::Unknown => false,
    };
    if !counts {
        return None;
    }
    let (mut g, mut w) = (l.gen_?, l.width?);
    if l.state.is_some_and(crate::pcie::State::degraded) {
        g = g.min(fast.min_gen.map_or(g, u32::from));
        w = w.min(fast.min_width.map_or(w, u32::from));
    }
    Some((g, w))
}

/// History keys of marked jobs start with this.
pub const MARKED_KEY: &str = "marked:";

/// `part` seconds of `whole` as a rounded share: "55%", "<1%".
pub fn share(part: f64, whole: f64) -> String {
    if whole <= 0.0 {
        return "—".into();
    }
    let p = (part / whole * 100.0).clamp(0.0, 100.0);
    if p > 0.0 && p < 1.0 { "<1%".into() } else { format!("{p:.0}%") }
}

/// A PCIe link with a short plain gloss: "Gen5 x16" -> "Gen5 x16 (PCIe 5.0, 16 lanes)".
pub fn link_gloss(link: &str) -> String {
    let mut it = link.split_whitespace();
    match (it.next().and_then(|g| g.strip_prefix("Gen")), it.next().and_then(|w| w.strip_prefix('x'))) {
        (Some(g), Some(w)) if g.parse::<u8>().is_ok() && w.parse::<u8>().is_ok() => format!("{link} (PCIe {g}.0, {w} lanes)"),
        _ => link.to_string(),
    }
}

/// A clock-limit reason in plain words, with what it means for the user.
pub fn reason_gloss(r: &str) -> String {
    let base = r.split(" (").next().unwrap_or(r);
    match base {
        "power limit" => "Power limit: the card kept its clocks inside its power budget. Normal under full load.".into(),
        "reliability policy" => "Reliability policy: the driver kept the voltage in a safe range. Normal, seen at idle too.".into(),
        "idle" => "Idle: nothing to do, so the clocks dropped. Normal.".into(),
        "temperature limit" => "Heat: the card slowed down to stay cool. Check the airflow and fans.".into(),
        "hardware slowdown" => {
            "Hardware slowdown: the board asked for lower clocks (heat or power). Check the cooling and power cables.".into()
        }
        "power brake" => "Power brake: the power supply or board asked for less power. Check the power cables.".into(),
        "sync boost" => "Sync boost: clocks matched to another GPU. Normal with several GPUs.".into(),
        _ => {
            let mut s = r.to_string();
            if let Some(c) = s.get_mut(..1) {
                c.make_ascii_uppercase();
            }
            s
        }
    }
}

/// The previous run of the same job: the latest earlier marked job with the same name (run
/// counters ignored). A different job is never compared; none means "first run".
pub fn previous<'a>(reps: &'a [JobReport], cur: &JobReport) -> Option<&'a JobReport> {
    let key = |j: &JobReport| crate::history::job_key(crate::history::JobKind::Render, &j.job);
    reps.iter().filter(|r| r.end_ms <= cur.start_ms + 1000 && r.start_ms < cur.start_ms && key(r) == key(cur)).max_by_key(|r| r.end_ms)
}

/// A job against the previous run of the same job.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub prev_job: String,
    pub prev_start_ms: i64,
    /// The same job run again (speed is comparable); otherwise a different job.
    pub same_job: bool,
    /// "4% faster than last time", "VRAM peak +1.2 GB", ...
    pub parts: Vec<String>,
}

impl Comparison {
    /// One sentence with its capital and full stop.
    pub fn sentence(&self) -> String {
        let mut s = self.parts.join(", ");
        if let Some(c) = s.get_mut(..1) {
            c.make_ascii_uppercase();
        }
        s.push('.');
        s
    }
}

/// When there is nothing earlier to compare with.
pub const FIRST_RUN: &str = "First run, nothing to compare.";

/// Duration, average GPU, VRAM peak, average power and energy against `prev`.
pub fn compare(prev: &JobReport, cur: &JobReport) -> Comparison {
    let same_job = crate::history::job_key(crate::history::JobKind::Render, &prev.job)
        == crate::history::job_key(crate::history::JobKind::Render, &cur.job);
    let mut parts = vec![];
    let (a, b) = (prev.duration_s.max(1.0), cur.duration_s.max(1.0));
    let ch = (b / a - 1.0) * 100.0;
    parts.push(if ch.abs() < 3.0 {
        if same_job { "about as fast as last time".to_string() } else { "about as long".to_string() }
    } else if same_job {
        if ch > 0.0 { format!("{ch:.0}% slower than last time") } else { format!("{:.0}% faster than last time", -ch) }
    } else if b / a >= 2.0 {
        format!("ran {:.1}× as long", b / a)
    } else if a / b >= 2.0 {
        format!("ran {:.1}× shorter", a / b)
    } else if ch > 0.0 {
        format!("ran {ch:.0}% longer")
    } else {
        format!("ran {:.0}% shorter", -ch)
    });
    if let (Some(p), Some(c)) = (prev.gpu_util_avg, cur.gpu_util_avg) {
        let d = c - p;
        parts.push(if d.abs() < 2.0 { format!("same GPU load ({c:.0}%)") } else { format!("GPU {d:+.0} points ({c:.0}% vs {p:.0}%)") });
    }
    if let (Some(p), Some(c)) = (prev.vram_peak_mb, cur.vram_peak_mb) {
        let d = c - p;
        parts.push(if d.abs() < 64.0 {
            "same VRAM peak".to_string()
        } else if d.abs() >= 1024.0 {
            format!("VRAM {:+.1} GB", d / 1024.0)
        } else {
            format!("VRAM {d:+.0} MB")
        });
    }
    if let (Some(p), Some(c)) = (prev.power_avg_w, cur.power_avg_w) {
        let d = c - p;
        parts.push(if d.abs() < 5.0 { "same average power".to_string() } else { format!("power {d:+.0} W on average") });
    }
    if let (Some(p), Some(c)) = (prev.energy_wh, cur.energy_wh) {
        let d = c - p;
        parts.push(if d.abs() < 0.1 { "same energy".to_string() } else { format!("energy {d:+.1} Wh") });
    }
    Comparison { prev_job: prev.job.clone(), prev_start_ms: prev.start_ms, same_job, parts }
}

/// Two time spans overlap by at least half of the shorter one.
pub fn overlaps(a: (i64, i64), b: (i64, i64)) -> bool {
    let ov = a.1.min(b.1) - a.0.max(b.0);
    let shorter = (a.1 - a.0).min(b.1 - b.0).max(1);
    ov > 0 && ov * 2 >= shorter
}

/// An automatic job (render log or GPU-busy session) that is the same work as a marked job: the
/// marked job wins and the automatic one is not listed again.
pub fn duplicates_marked(start_ms: i64, end_ms: i64, marked: &[(i64, i64)]) -> bool {
    marked.iter().any(|m| overlaps((start_ms, end_ms), *m))
}

/// The history list with the marked jobs in it: automatic jobs that duplicate a marked job are
/// dropped, the marked ones added (with their comparison), oldest first.
pub fn merge_history(auto: Vec<crate::history::JobRecord>, marked: &[JobReport]) -> Vec<crate::history::JobRecord> {
    let spans: Vec<(i64, i64)> = marked.iter().map(|m| (m.start_ms, m.end_ms)).collect();
    let mut v: Vec<crate::history::JobRecord> = auto.into_iter().filter(|j| !duplicates_marked(j.start_ms, j.end_ms, &spans)).collect();
    for m in marked {
        let vs = match m.compare_in(marked) {
            Some(c) => c.sentence(),
            None => FIRST_RUN.to_string(),
        };
        v.push(m.as_record(Some(vs)));
    }
    v.sort_by_key(|j| j.end_ms);
    v
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
    /// The slowest link seen under load (idle power saving and training up are not counted).
    load_low: Option<(u32, u32)>,
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
            if let Some(low) = under_load_link(l, s.util, fast) {
                self.load_low = Some(self.load_low.map_or(low, |cur| cur.min(low)));
            }
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
        // the slowest link under load only: an idle downshift (Gen1/Gen2 to save power, e.g. the
        // first readings after an idle minute) is normal and not "the lowest the job saw"
        let worst = self.load_low.map(|(g, w)| crate::pcie::link_text(Some(g), Some(w)));
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
            sentence.push_str(&format!(
                " At the power limit {} of the time, slowed by heat {} of the time.",
                share(power_s, duration_s),
                share(thermal_s, duration_s)
            ));
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

    /// The open jobs as (start, now) spans.
    pub fn open_spans(&self, now: i64) -> Vec<(i64, i64)> {
        self.open.iter().map(|a| (a.start_ms, now.max(a.start_ms))).collect()
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
            if let Ok(mut r) = serde_json::from_str::<JobReport>(&line) {
                r.revise();
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

    /// Live test "test 1.2 A" run 2 (RTX 5080, 29 Oct 2026): a minute idle before the job, so the
    /// link was at Gen2 to save power and the first 100 ms readings of the job still saw Gen2.
    /// The report said "Between Gen2 x16 and Gen5 x16" with no slowdown. Only readings under
    /// load count: the lowest is Gen5 x16. A real slowdown under load still shows.
    #[test]
    fn idle_power_saving_before_a_job_is_not_its_lowest_link() {
        let d = tmp("pcie_idle");
        let t0 = 1_793_000_000_000i64;
        let mut tr = MarkerTracker::open(&d, t0);
        let mut link = crate::pcie::LinkTracker::default();
        let fast_at = |g: u8| FastAgg { n: 10, min_gen: Some(g), min_width: Some(16), period_ms: 100, ..Default::default() };
        // 60 s idle at Gen2 x16
        for i in 0..60 {
            link.push(t0 + i * 1000, Some(2), Some(16), Some(5), Some(16), Some(1));
            tr.tick(&s(t0 + i * 1000, 1, 30.0, 2000.0), &fast_at(2), Some(&link.status), None, None);
        }
        write_mark(&d, "start", "test 1.2 A", t0 + 60_000).unwrap();
        let mut done = vec![];
        for i in 60..120 {
            let t = t0 + i * 1000;
            // the first second of load: the link reads Gen2 for a moment, then Gen5 x16
            let (g_now, fast) = if i == 60 { (2, fast_at(2)) } else { (5, fast_at(if i == 61 { 2 } else { 5 })) };
            link.push(t, Some(g_now), Some(16), Some(5), Some(16), Some(99));
            done.extend(tr.tick(&s(t, 99, 310.0, 11600.0), &fast, Some(&link.status), None, None));
        }
        write_mark(&d, "end", "test 1.2 A", t0 + 120_000).unwrap();
        link.push(t0 + 120_000, Some(5), Some(16), Some(5), Some(16), Some(99));
        done.extend(tr.tick(&s(t0 + 120_000, 99, 310.0, 11600.0), &fast_at(5), Some(&link.status), None, None));
        assert_eq!(done.len(), 1);
        let r = &done[0];
        assert_eq!(r.pcie["lowest_seen"], "Gen5 x16", "{}", r.pcie);
        assert_eq!(r.pcie["degraded_events"], 0);
        assert!(r.pcie_sentence().starts_with("Gen5 x16 (PCIe 5.0, 16 lanes) for the whole job"), "{}", r.pcie_sentence());
        // the same job with the link stuck at Gen3 under load: that is still reported
        link.push(t0 + 125_000, Some(2), Some(16), Some(5), Some(16), Some(1));
        write_mark(&d, "start", "slow link", t0 + 130_000).unwrap();
        let mut done = vec![];
        for i in 130..160 {
            let t = t0 + i * 1000;
            link.push(t, Some(3), Some(16), Some(5), Some(16), Some(99));
            done.extend(tr.tick(&s(t, 99, 310.0, 11600.0), &fast_at(3), Some(&link.status), None, None));
        }
        write_mark(&d, "end", "slow link", t0 + 160_000).unwrap();
        done.extend(tr.tick(&s(t0 + 160_000, 99, 310.0, 11600.0), &fast_at(3), Some(&link.status), None, None));
        assert_eq!(done[0].pcie["lowest_seen"], "Gen3 x16", "{}", done[0].pcie);
        assert_eq!(done[0].pcie["degraded_events"], 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The A2 report as a 1.2 test build saved it is read back with the right lowest link.
    #[test]
    fn saved_reports_with_an_idle_lowest_link_are_revised() {
        let d = tmp("pcie_revise");
        let h = d.join("history");
        std::fs::create_dir_all(&h).unwrap();
        // the A2 line as the test4 build saved it ("lowest_seen": "Gen2 x16", no slowdown)
        let a2 = include_str!("../tests/samples/marked_job_A2_2026-10-29.json").trim();
        let light = a2
            .replace("\"test 1.2 A\"", "\"light\"")
            .replace("\"gpu_util_avg\":99.3", "\"gpu_util_avg\":4.0")
            .replace("\"lowest_seen\":\"Gen2 x16\"", "\"lowest_seen\":\"Gen1 x16\"");
        assert!(light.contains("Gen1 x16") && light.contains(":4.0"));
        let text = format!("{a2}\n{light}\n");
        std::fs::write(h.join("marked_jobs.jsonl"), text).unwrap();
        let r = reports(&h, 10);
        assert_eq!(r.len(), 2, "{r:?}");
        let a2 = r.iter().find(|r| r.job == "test 1.2 A").unwrap();
        assert_eq!(a2.pcie["lowest_seen"], "Gen5 x16");
        assert!(a2.pcie_sentence().contains("for the whole job"), "{}", a2.pcie_sentence());
        let light = r.iter().find(|r| r.job == "light").unwrap();
        assert!(light.pcie["lowest_seen"].is_null());
        assert!(light.pcie_sentence().starts_with("The GPU was not busy enough"), "{}", light.pcie_sentence());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Giuseppe's two demo jobs on the RTX 5080 (26 Oct 2026): plain verdicts, no false alarms.
    #[test]
    fn verdicts_of_the_real_demo_jobs() {
        let reps: Vec<JobReport> =
            include_str!("../tests/samples/marked_jobs_2026-10-26.jsonl").lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let heavy = reps.iter().find(|r| r.job == "demo 1.2 heavy").expect("heavy job");
        let (v, tone) = heavy.verdict();
        assert_eq!(tone, Tone::Note, "{v}");
        assert!(v.starts_with("Ran 5 min") && v.contains("almost fully busy (99% on average)"), "{v}");
        assert!(v.contains("at the power limit 55% of the time, which is normal under full load"), "{v}");
        assert_eq!(heavy.pcie_sentence(), "Gen5 x16 (PCIe 5.0, 16 lanes) for the whole job: the fastest this slot allows.");
        let light = reps.iter().find(|r| r.job == "demo 1.2").expect("light job");
        let (v, tone) = light.verdict();
        assert_eq!(tone, Tone::Note);
        assert!(v.contains("Nothing held it back"), "{v}");
        // heat makes it a warning, and the sentence says what to do
        let mut hot = heavy.clone();
        hot.thermal_limited_s = 42.0;
        let (v, tone) = hot.verdict();
        assert_eq!(tone, Tone::Warning);
        assert!(v.contains("slowed down from heat 14% of the time: check the cooling") && !v.contains("Nothing to fix"), "{v}");
    }

    fn demo_reports() -> Vec<JobReport> {
        include_str!("../tests/samples/marked_jobs_2026-10-26.jsonl").lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Only a previous run of the same job is compared: "demo 1.2 heavy" is not compared with
    /// "demo 1.2" (a different job), so both are first runs.
    #[test]
    fn job_compared_only_with_a_previous_run_of_the_same_job() {
        let reps = demo_reports();
        let heavy = reps.iter().find(|r| r.job == "demo 1.2 heavy").unwrap();
        let light = reps.iter().find(|r| r.job == "demo 1.2").unwrap();
        assert!(light.compare_in(&reps).is_none(), "first run");
        assert!(heavy.compare_in(&reps).is_none(), "a different job is not compared");
        // the same job again matches by name, before any newer other job; speed is comparable
        let mut again = heavy.clone();
        again.job = "demo 1.2 heavy try 2".into();
        again.start_ms = heavy.end_ms + 600_000;
        again.end_ms = again.start_ms + 295_000;
        again.duration_s = 295.0;
        again.vram_peak_mb = heavy.vram_peak_mb.map(|v| v + 1228.8);
        let mut other = light.clone();
        other.start_ms = heavy.end_ms + 60_000;
        other.end_ms = other.start_ms + 119_000;
        let all = vec![light.clone(), heavy.clone(), other, again.clone()];
        let c = again.compare_in(&all).unwrap();
        assert!(c.same_job);
        assert_eq!((c.prev_job.as_str(), c.prev_start_ms), ("demo 1.2 heavy", heavy.start_ms));
        assert!(c.sentence().starts_with("4% faster than last time, same GPU load (99%), VRAM +1.2 GB"), "{}", c.sentence());
    }

    #[test]
    fn plain_words_for_links_reasons_and_shares() {
        assert_eq!(link_gloss("Gen5 x16"), "Gen5 x16 (PCIe 5.0, 16 lanes)");
        assert_eq!(link_gloss("unknown"), "unknown");
        assert!(reason_gloss("power limit").contains("Normal under full load"));
        assert!(
            reason_gloss("reliability policy (the driver holding the clocks in a safe range, seen at idle too)")
                .starts_with("Reliability policy:")
        );
        assert!(reason_gloss("temperature limit (driver)").contains("airflow"));
        assert_eq!(share(168.7, 307.0), "55%");
        assert_eq!(share(0.2, 307.0), "<1%");
        assert_eq!(share(0.0, 307.0), "0%");
    }

    /// The automatic "python.exe" sessions recorded during both demo jobs are the same work: the
    /// marked jobs win; the render-log jobs from earlier stay.
    #[test]
    fn marked_jobs_replace_their_automatic_duplicates() {
        let reps = demo_reports();
        let auto: Vec<crate::history::JobRecord> =
            include_str!("../tests/samples/history_jobs_2026-10-26.jsonl").lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        assert_eq!(auto.iter().filter(|j| j.name == "python.exe").count(), 2);
        let spans: Vec<(i64, i64)> = reps.iter().map(|m| (m.start_ms, m.end_ms)).collect();
        for j in auto.iter().filter(|j| j.name == "python.exe") {
            assert!(duplicates_marked(j.start_ms, j.end_ms, &spans), "{}", j.start_ms);
        }
        let merged = merge_history(auto.clone(), &reps);
        let names: Vec<&str> = merged.iter().map(|j| j.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "tachyon_v1_test_2026-10-26 · gpu_test_render",
                "tachyon_v1_test_2026-10-26 · gpu_test_render try 2",
                "demo 1.2",
                "demo 1.2 heavy"
            ]
        );
        assert!(merged[3].key.starts_with(MARKED_KEY));
        assert_eq!(merged[2].vs_last.as_deref(), Some(FIRST_RUN));
        assert_eq!(merged[3].vs_last.as_deref(), Some(FIRST_RUN), "not compared with the other job");
        // a short overlap at the edge is not a duplicate
        assert!(!overlaps((0, 100_000), (90_000, 400_000)));
        assert!(overlaps((0, 100_000), (40_000, 400_000)));
    }
}
