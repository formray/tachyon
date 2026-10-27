//! `tachyon mcp`: a Model Context Protocol server on stdin/stdout (JSON-RPC 2.0, one message per
//! line) so local AI agents can ask what the GPU is doing and why. Every tool is read-only. It
//! reads the status the running Tachyon publishes (with `agent_api = true`), the flight recorder
//! files and the job history; if Tachyon is not running it takes its own short reading. No
//! network: the client starts this process and talks to it through pipes.
use crate::config::Config;
use crate::events::SysEvent;
use crate::history::{self, JobRecord};
use crate::recorder;
use crate::report;
use crate::telemetry::Sample;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Where the server gets its data. The real one reads shared memory and files; tests fake it.
pub trait Source {
    /// The agent API switch from tachyon.toml.
    fn enabled(&self) -> bool;
    /// Live status: from the running Tachyon, or a short reading of its own.
    fn status(&mut self) -> Value;
    /// A fresh bottleneck verdict (standalone: a few quick readings).
    fn bottleneck(&mut self) -> Value {
        self.status()["bottleneck"].clone()
    }
    fn blackbox(&mut self) -> Vec<crate::telemetry::Sample>;
    fn report(&mut self) -> Option<report::Report>;
    fn history(&mut self, n: usize) -> Vec<JobRecord>;
    /// Windows Event Log entries in a window (System and Application logs).
    fn events(&mut self, from_ms: i64, to_ms: i64) -> Result<Vec<crate::events::SysEvent>, String> {
        crate::events::query(from_ms, to_ms, 400)
    }
    /// What the diagnosis knows about this PC now: RAM speed, GPU clock offsets, BIOS start time.
    fn context(&mut self) -> crate::diagnose::Context {
        crate::diagnose::Context {
            ram: crate::sysinfo::ram_speed(),
            firmware_boot_ms: crate::sysinfo::firmware_boot_ms(),
            ..Default::default()
        }
    }
    /// The flight recorder's sessions ("current", "previous"), samples oldest first.
    fn blackbox_sessions(&mut self) -> Vec<(String, Vec<Sample>)> {
        vec![("current".into(), self.blackbox())]
    }
    /// The newest System log entries of any kind up to `to_ms`, oldest first.
    fn last_log_before(&mut self, to_ms: i64) -> Result<Vec<SysEvent>, String> {
        crate::events::last_before(to_ms, 64)
    }
    /// When the current boot started (Unix ms, by the clock now).
    fn boot_ms(&mut self) -> Option<i64> {
        crate::sysinfo::boot_time_ms()
    }
    /// Tachyon's data folder (history, markers, the fast rings), if any.
    fn data_dir(&self) -> Option<PathBuf> {
        None
    }
    /// The 100 ms rings ("current", "previous"), oldest first.
    fn fast_sessions(&mut self) -> Vec<(String, Vec<crate::fast::FastSample>)> {
        self.data_dir().map(|d| crate::fast::read_sessions(&d.join("blackbox"))).unwrap_or_default()
    }
    /// A per-process VRAM reading now (when Tachyon is not running): (name, pid, MB) and where from.
    fn proc_vram_now(&mut self) -> Option<(Vec<(String, u32, f32)>, &'static str)> {
        None
    }
}

/// The 100 ms readings of the last 30 s before a stop: the newest fast reading within 10 min
/// before (or 2 min after, the fast ring reaches the disk sooner than the 1 s recorder) `stop_ms`.
fn fast_before(src: &mut dyn Source, stop_ms: i64) -> Option<Value> {
    let mut all: Vec<crate::fast::FastSample> = src.fast_sessions().into_iter().flat_map(|(_, v)| v).collect();
    all.sort_by_key(|s| s.t_ms);
    let last = all.iter().map(|s| s.t_ms).filter(|t| *t <= stop_ms + 120_000 && *t >= stop_ms - 600_000).max()?;
    let mut v = crate::fast::last_seconds(&all, last, 30)?;
    v["last_reading_at"] = json!(local_text(last, "%Y-%m-%d %H:%M:%S%.3f"));
    Some(v)
}

/// Adds the last seconds to a diagnosis (and their hints).
fn add_last_seconds(v: &mut Value, src: &mut dyn Source, stop_ms: Option<i64>) {
    let Some(f) = stop_ms.and_then(|t| fast_before(src, t)) else { return };
    let hints = crate::fast::crash_hints(&f);
    if !hints.is_empty() && v["diagnosis"].is_object() {
        let h = v["diagnosis"]["hints"].as_array().cloned().unwrap_or_default();
        v["diagnosis"]["hints"] = json!(h.into_iter().chain(hints.into_iter().map(Value::from)).collect::<Vec<_>>());
    }
    v["last_seconds"] = f;
}

/// Every recorded sample of both sessions, oldest first.
fn all_samples(src: &mut dyn Source) -> Vec<Sample> {
    let mut v: Vec<Sample> = src.blackbox_sessions().into_iter().flat_map(|(_, s)| s).collect();
    v.sort_by_key(|s| s.t_ms);
    v.dedup_by_key(|s| s.t_ms);
    v
}

fn local_text(ms: i64, fmt: &str) -> String {
    Sample { t_ms: ms, ..Default::default() }.local().format(fmt).to_string()
}

/// A time argument: Unix ms, RFC 3339, or local "2026-10-26 18:09[:30]" (or "18:09" for today).
fn parse_time(v: &Value, name: &str) -> Result<Option<i64>, String> {
    use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
    let bad = || format!("`{name}` must be a local date and time such as 2026-10-26 18:09, or Unix milliseconds.");
    let local = |n: NaiveDateTime| Local.from_local_datetime(&n).earliest().map(|d| d.timestamp_millis());
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n.as_i64().filter(|t| *t > 0 && *t < 8_000_000_000_000).map(Some).ok_or_else(bad),
        Value::String(s) => {
            let s = s.trim().replace('T', " ");
            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&s.replacen(' ', "T", 1)) {
                return Ok(Some(d.timestamp_millis()));
            }
            for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
                if let Ok(n) = NaiveDateTime::parse_from_str(&s, f) {
                    return local(n).map(Some).ok_or_else(bad);
                }
            }
            for f in ["%H:%M:%S", "%H:%M"] {
                if let Ok(t) = NaiveTime::parse_from_str(&s, f) {
                    let today: NaiveDate = Local::now().date_naive();
                    return local(today.and_time(t)).map(Some).ok_or_else(bad);
                }
            }
            Err(bad())
        }
        _ => Err(bad()),
    }
}

/// Plain summary of what the flight recorder saw: the window and its last minute.
fn recorder_summary(window: &[Sample]) -> Value {
    let Some(last) = window.last() else { return Value::Null };
    let end = last.t_ms;
    let lm: Vec<Sample> = window.iter().filter(|s| s.t_ms >= end - report::JUST_BEFORE_MS).cloned().collect();
    let p = report::peaks(&lm);
    let vals = |f: &dyn Fn(&Sample) -> Option<f32>| lm.iter().filter_map(f).collect::<Vec<f32>>();
    let stat = |v: &[f32]| {
        (!v.is_empty()).then(|| {
            let min = v.iter().copied().fold(f32::MAX, f32::min);
            let max = v.iter().copied().fold(f32::MIN, f32::max);
            (min, (v.iter().sum::<f32>() / v.len() as f32 * 10.0).round() / 10.0, max)
        })
    };
    let util = stat(&vals(&|s| s.util.map(f32::from)));
    let power = stat(&vals(&|s| s.power_w));
    let temp = stat(&vals(&|s| s.temp.map(f32::from)));
    let cpu = stat(&vals(&|s| s.cpu));
    let limit = p.power_limit_w;
    let mut procs: Vec<(String, Option<f32>, Option<u8>)> = vec![];
    for s in &lm {
        for pr in &s.procs {
            match procs.iter_mut().find(|q| q.0 == pr.name) {
                Some(q) => {
                    q.1 = match (q.1, pr.vram_mb) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
                    q.2 = q.2.max(pr.sm);
                }
                None => procs.push((pr.name.clone(), pr.vram_mb, pr.sm)),
            }
        }
    }
    let mut mask = 0u32;
    for s in &lm {
        mask |= s.throttle.unwrap_or(0);
    }
    let idle = crate::diagnose::idle_before(&p, &lm).is_some();
    let mut parts = vec![];
    if let Some((lo, avg, hi)) = util {
        parts.push(format!("GPU {lo:.0}-{hi:.0}% busy (average {avg:.0}%)"));
    }
    if let Some((lo, _, hi)) = power {
        let of = limit.filter(|l| *l > 0.0).map(|l| format!(" of its {l:.0} W limit (at most {:.0}%)", hi / l * 100.0)).unwrap_or_default();
        parts.push(format!("{lo:.0}-{hi:.0} W{of}"));
    }
    if let Some((lo, _, hi)) = temp {
        parts.push(format!("{lo:.0}-{hi:.0} °C"));
    }
    if let Some((_, avg, _)) = cpu {
        parts.push(format!("CPU {avg:.0}%"));
    }
    let state = if idle {
        "the PC was idle"
    } else if p.gpu_util.is_some_and(|u| u >= 90) {
        "the GPU was fully busy"
    } else {
        "the GPU was in use"
    };
    let mut sentence = format!("In the last minute before {} {state}: {}.", local_text(end, "%H:%M:%S"), parts.join(", "));
    if !procs.is_empty() {
        sentence.push_str(&format!(" On the GPU: {}.", procs.iter().map(|p| p.0.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let words = crate::telemetry::throttle_words(mask);
    if !words.is_empty() {
        sentence.push_str(&format!(" Clock limits reported: {}.", words.join(", ")));
    }
    json!({
        "recorded_from": window.first().map(|s| local_text(s.t_ms, "%Y-%m-%d %H:%M:%S")),
        "recorded_to": local_text(end, "%Y-%m-%d %H:%M:%S"),
        "samples": window.len(),
        "idle": idle,
        "last_minute": {
            "gpu_pct": util.map(|(lo, avg, hi)| json!({ "min": lo, "avg": avg, "max": hi })),
            "power_w": power.map(|(lo, avg, hi)| json!({ "min": lo, "avg": avg, "max": hi })),
            "power_limit_w": limit,
            "power_max_pct_of_limit": power.zip(limit).filter(|(_, l)| *l > 0.0).map(|((_, _, hi), l)| (hi / l * 100.0).round()),
            "temp_c": temp.map(|(lo, _, hi)| json!({ "min": lo, "max": hi })),
            "vram_max_mb": p.vram_mb,
            "cpu_avg_pct": cpu.map(|c| c.1),
            "power_throttled_s": p.power_throttled_s,
            "thermal_throttled_s": p.thermal_throttled_s,
        },
        "processes": procs.iter().map(|(n, v, sm)| json!({ "name": n, "vram_mb": v, "sm_pct": sm })).collect::<Vec<_>>(),
        "clock_limits": words,
        "sentence": sentence,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Around {
    /// A whole local day: [start, end).
    Day(i64, i64),
    /// A moment (local date and time).
    At(i64),
}

/// A local date ("2026-10-22", the whole day) or date and time ("2026-10-22 18:40[:30]").
fn parse_around(s: &str) -> Option<Around> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let s = s.trim().replace('T', " ");
    let local = |n: NaiveDateTime| Local.from_local_datetime(&n).earliest().map(|d| d.timestamp_millis());
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(&s, f) {
            return local(n).map(Around::At);
        }
    }
    let d = NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()?;
    let t = local(d.and_hms_opt(0, 0, 0)?)?;
    Some(Around::Day(t, t + 86_400_000))
}

/// Restart reports can be logged a while after the stop, and after a BIOS reset the clock at the
/// next start can run hours ahead: the search looks this far past the time asked about.
const LATE_MS: i64 = 3 * 3_600_000;
/// The recorder writes every 5 s and flushes every 30 s: after a power cut the last ~35 s can be lost.
const RECORDER_LOSS_MS: i64 = crate::recorder::SYNC_EVERY_MS + crate::recorder::WRITE_EVERY_MS;

/// One unexpected restart in the Event Log: its reports, and when the PC most likely stopped.
#[derive(Debug, Clone)]
struct Stop {
    /// First and last restart report (Kernel-Power 41, EventLog 6008, BugCheck 1001), in the
    /// corrected clock (see `shift_ms`).
    first: i64,
    last: i64,
    /// The same, as logged.
    raw_first: i64,
    raw_last: i64,
    /// What to add to the restart reports' stamps: when Windows corrected its clock after this
    /// restart (Kernel-General 1, 30 min or more), the reports were logged with the wrong clock.
    shift_ms: i64,
    correction: Option<SysEvent>,
    /// Event Log record numbers of the restart reports (0 = unknown).
    min_rec: u64,
    /// The last flight recorder sample before the restart (Tachyon's clock before the stop).
    recorder_ms: Option<i64>,
    /// Windows' last entry in the System log before the boot, and EventLog 6008's last sign of life.
    log_ms: Option<i64>,
    alive_ms: Option<i64>,
}

impl Stop {
    /// Earliest moment the PC can have stopped (the latest sign of life).
    fn earliest_ms(&self) -> Option<i64> {
        [self.recorder_ms, self.log_ms, self.alive_ms].into_iter().flatten().max()
    }
    /// Latest moment, when the recorder was running: its last sample plus what may not have reached the disk.
    fn until_ms(&self) -> Option<i64> {
        let f = self.earliest_ms()?;
        self.recorder_ms.map(|r| (r + RECORDER_LOSS_MS).max(f)).filter(|u| *u <= self.first.max(f))
    }
    fn source(&self) -> &'static str {
        match self.earliest_ms() {
            Some(f) if Some(f) == self.recorder_ms => "the flight recorder's last sample",
            Some(f) if Some(f) == self.alive_ms => "Windows' last sign of life (EventLog 6008)",
            Some(_) => "the last Windows log entry before the restart",
            None => "the restart reports (the stop time itself is unknown)",
        }
    }
}

/// Restart reports grouped into restarts (reports less than 15 minutes apart).
fn restarts(found: &[SysEvent]) -> Vec<(i64, i64)> {
    let mut t: Vec<i64> = found.iter().filter(|e| e.is_restart()).map(|e| e.t_ms).collect();
    t.sort_unstable();
    let mut out: Vec<(i64, i64)> = vec![];
    for x in t {
        match out.last_mut() {
            Some(c) if x - c.1 <= 15 * 60_000 => c.1 = x,
            _ => out.push((x, x)),
        }
    }
    out
}

/// The clock correction that tells a restart's reports were logged with a wrong clock: Windows
/// moving its clock by 30 min or more after the restart and before the next one (by the Event
/// Log's record order). Its own stamp is in the corrected clock, so it can look older than the
/// reports it corrects: an hour-fast clock fixed half an hour after the restart is stamped half an
/// hour before them. Without record numbers, by the stamps alone.
fn clock_correction(group: &[&SysEvent], next: Option<u64>, raw_first: i64, changes: &[SysEvent]) -> Option<SysEvent> {
    let max_rec = group.iter().map(|e| e.record).max().unwrap_or(0);
    let min_rec = group.iter().map(|e| e.record).filter(|r| *r > 0).min().unwrap_or(0);
    changes
        .iter()
        .filter(|e| e.clock_change_s().is_some_and(|d| d.abs() >= 1800))
        .filter(|e| e.t_ms <= raw_first + 86_400_000)
        .filter(|e| {
            if e.record > 0 && min_rec > 0 {
                e.record > max_rec && next.is_none_or(|n| e.record < n)
            } else {
                e.t_ms >= raw_first + e.clock_change_s().unwrap_or(0) * 1000 - 15 * 60_000
            }
        })
        .min_by_key(|e| if e.record > 0 { (e.record, 0) } else { (0, e.t_ms) })
        .cloned()
}

/// `diagnose_crash` without a crash report (or for a date or time): the unexpected restart in the
/// Event Log that matches, with the stop time worked out from the flight recorder's last sample
/// and Windows' last log entries, the recorder's readings from just before, and what Tachyon can
/// read about the PC now.
fn diagnose_from_event_log(args: &Value, src: &mut dyn Source, now: i64) -> Result<Value, String> {
    let tol = args["tolerance_minutes"].as_u64().unwrap_or(90).clamp(5, 720) as i64 * 60_000;
    let around = match args["around"].as_str() {
        Some(a) => Some(
            parse_around(a)
                .ok_or("`around` must be a local date (2026-10-22) or date and time (2026-10-22 18:40).")
                .map(|p| (p, a.trim().to_string()))?,
        ),
        None => None,
    };
    let (from, to, what) = match &around {
        // not capped at now: after a BIOS reset the restart reports can carry a clock that ran ahead
        Some((Around::At(t), a)) => (t - tol, t + tol + LATE_MS, format!("around {a} (within {} min)", tol / 60_000)),
        Some((Around::Day(f, t), a)) => (*f, t + LATE_MS, format!("on {a}")),
        None => {
            let days = args["days"].as_u64().unwrap_or(14).clamp(1, 60) as i64;
            let span = if days == 1 { "day".to_string() } else { format!("{days} days") };
            (now - days * 86_400_000, now + LATE_MS, format!("in the last {span}"))
        }
    };
    let found = src.events(from, to)?;
    let gpu_resets = found.iter().filter(|e| e.is_gpu()).count();
    let samples = all_samples(src);
    let groups = restarts(&found);
    let members = |&(f, l): &(i64, i64)| found.iter().filter(move |e| e.is_restart() && e.t_ms >= f && e.t_ms <= l).collect::<Vec<_>>();
    // clock corrections from before the first restart to a day after the last
    let changes: Vec<SysEvent> = match (groups.first(), groups.last()) {
        (Some(a), Some(b)) => src
            .events(a.0 - 13 * 3_600_000, b.1 + 86_400_000)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.clock_change_s().is_some())
            .collect(),
        _ => vec![],
    };
    // work out when the PC stopped, for each restart
    let mut stops: Vec<Stop> = vec![];
    for (i, g) in groups.iter().enumerate() {
        let (raw_first, raw_last) = *g;
        let group = members(g);
        let min_rec = group.iter().map(|e| e.record).filter(|r| *r > 0).min().unwrap_or(0);
        let next = groups.get(i + 1).and_then(|n| members(n).iter().map(|e| e.record).filter(|r| *r > 0).min());
        let correction = clock_correction(&group, next, raw_first, &changes);
        let shift_ms = correction.as_ref().and_then(|c| c.clock_change_s()).unwrap_or(0) * 1000;
        let (first, last) = (raw_first + shift_ms, raw_last + shift_ms);
        let floor = stops.last().map_or(first - 12 * 3_600_000, |p| p.last);
        let recorder_ms = samples.iter().rev().find(|s| s.t_ms < first && s.t_ms > floor).map(|s| s.t_ms);
        let alive_ms = group.iter().filter_map(|e| e.last_alive_ms()).filter(|t| *t < first).max();
        // Windows' last entry before the boot: by record order when known, else by the stamps
        // (in the corrected clock, so that entries after the correction do not count)
        let log = src.last_log_before(first.min(raw_first)).unwrap_or_default();
        let boot = log
            .iter()
            .filter(|e| e.is_boot_marker() && e.t_ms >= raw_first - 10 * 60_000)
            .map(|e| e.t_ms + shift_ms)
            .min()
            .unwrap_or(first);
        let log_ms = log
            .iter()
            .filter(|e| !e.is_boot_marker() && e.t_ms < boot && e.t_ms > floor)
            .filter(|e| min_rec == 0 || e.record == 0 || e.record < min_rec)
            .map(|e| e.t_ms)
            .max();
        stops.push(Stop { first, last, raw_first, raw_last, shift_ms, correction, min_rec, recorder_ms, log_ms, alive_ms });
    }
    let list = |stops: &[Stop]| {
        stops
            .iter()
            .map(|s| {
                format!(
                    "restart logged at {}{}",
                    local_text(s.first, "%Y-%m-%d %H:%M"),
                    s.earliest_ms().map(|f| format!(", stopped at about {}", local_text(f, "%Y-%m-%d %H:%M:%S"))).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
    };
    // which restart: the one whose stop is nearest the time asked about (within the tolerance)
    let pick = match &around {
        Some((Around::At(t), _)) => stops
            .iter()
            .filter_map(|s| {
                let d = match s.earliest_ms() {
                    Some(f) => (f - t).abs(),
                    // no stop time: the restart's own reports, which can come late or with a shifted clock
                    None => (s.first - t).abs().min((s.first - t - LATE_MS).abs().max(0)),
                };
                let ok = if s.earliest_ms().is_some() { d <= tol } else { s.first >= t - tol && s.first <= t + tol + LATE_MS };
                ok.then_some((d, s))
            })
            .min_by_key(|(d, _)| *d)
            .map(|(_, s)| s.clone()),
        Some((Around::Day(f, t), _)) => {
            stops.iter().rev().find(|s| s.earliest_ms().unwrap_or(s.first) >= *f && s.earliest_ms().unwrap_or(s.first) < *t).cloned()
        }
        None => stops.last().cloned(),
    };
    let Some(stop) = pick else {
        let mut note = format!("No unexpected restart in the Windows Event Log {what}");
        if gpu_resets > 0 {
            note.push_str(&format!(", but {gpu_resets} GPU driver event(s)"));
        }
        note.push('.');
        if !stops.is_empty() {
            note.push_str(&format!(" Found nearby: {}.", list(&stops).join("; ")));
        }
        return Ok(json!({
            "diagnosis": null,
            "source": "event_log",
            "gpu_driver_events": gpu_resets,
            "restarts_found": list(&stops),
            "note": note,
        }));
    };
    let (first, last) = (stop.first, stop.last);
    // the crash itself comes before the restart's reports; GPU driver resets in the 12 hours
    // before count as signs of the same instability; Windows correcting its clock afterwards
    // tells how far off it was. Reports logged with the wrong clock are moved to the right time
    // (and keep the logged time for the list).
    let shift = stop.shift_ms;
    let fix_rec = stop.correction.as_ref().map_or(0, |c| c.record);
    let shifted = |e: &SysEvent| {
        shift != 0
            && e.clock_change_s().is_none()
            && if e.record > 0 && stop.min_rec > 0 && fix_rec > 0 {
                e.record >= stop.min_rec && e.record < fix_rec
            } else {
                e.t_ms >= stop.raw_first - 10 * 60_000 && e.t_ms <= stop.raw_last + 10 * 60_000
            }
    };
    let mut logged_at: Vec<(i64, i64)> = vec![];
    let mut ev: Vec<SysEvent> = src
        .events(first.min(stop.raw_first) - 12 * 3_600_000, last.max(stop.raw_last) + 10 * 60_000)?
        .into_iter()
        .filter_map(|mut e| {
            if shifted(&e) {
                logged_at.push((e.t_ms + shift, e.t_ms));
                e.t_ms += shift;
            } else if e.clock_change_s().is_some() || e.t_ms > last + 10 * 60_000 {
                // corrections are listed below; anything else logged after the restart is not before it
                return None;
            }
            Some(e)
        })
        .collect();
    let later = src.events(first - 12 * 3_600_000, first + 86_400_000).unwrap_or_default();
    let corrections: Vec<SysEvent> = later
        .into_iter()
        .filter(|e| e.clock_change_s().is_some())
        .filter(|e| match &stop.correction {
            Some(c) if c.record > 0 && e.record > 0 => e.record >= c.record,
            Some(c) => e.t_ms >= c.t_ms,
            None => e.t_ms > last + 10 * 60_000,
        })
        .collect();
    ev.extend(corrections.iter().cloned());
    ev.sort_by_key(|e| e.t_ms);
    ev.dedup();

    // the flight recorder: only when it was running until (about) the stop
    let rec_ok = stop.recorder_ms.is_some_and(|r| {
        let others = [stop.log_ms, stop.alive_ms].into_iter().flatten().max();
        match (others, &around) {
            (Some(o), _) => r >= o - 5 * 60_000,
            (None, Some((Around::At(t), _))) => (r - t).abs() <= tol,
            (None, _) => first - r <= 20 * 60_000,
        }
    });
    let window: Vec<Sample> = match stop.recorder_ms.filter(|_| rec_ok) {
        Some(r) => samples.iter().filter(|s| s.t_ms <= r && s.t_ms >= r - report::TIMELINE_MS).cloned().collect(),
        None => vec![],
    };
    let end = window.last().map_or(0, |s| s.t_ms);
    let last_minute: Vec<Sample> = window.iter().filter(|s| s.t_ms >= end - report::JUST_BEFORE_MS).cloned().collect();
    let peaks = report::peaks(&last_minute);
    let holder = report::vram_holder(&window);

    // is this restart the current boot? (the restart stamp in today's clock vs the boot time now)
    let corr_ms: i64 = if shift != 0 { 0 } else { corrections.iter().filter_map(|e| e.clock_change_s()).sum::<i64>() * 1000 };
    let mut ctx = src.context();
    ctx.just_after =
        src.boot_ms().is_some_and(|b| (first + corr_ms - b).abs() <= 15 * 60_000) && stops.last().is_some_and(|s| s.first == first);
    if !ctx.just_after {
        ctx.firmware_boot_ms = None;
    }
    // the clock after the restart vs the time of the stop
    let mut clock_note = None;
    let fixed = corrections.iter().filter_map(|e| e.clock_change_s()).filter(|d| d.abs() >= 1800).max_by_key(|d| d.abs());
    if let (Some(c), Some(d)) = (&stop.correction, stop.correction.as_ref().and_then(|c| c.clock_change_s())) {
        clock_note = Some(format!(
            "Windows {} its clock by {} at {}, after this restart: the restart reports were logged with a clock {} by about that much ({}). The times here are corrected.",
            if d < 0 { "put back" } else { "put forward" },
            crate::events::signed_duration(d).trim_start_matches(['-', '+']),
            local_text(c.t_ms, "%H:%M:%S"),
            if d < 0 { "running ahead" } else { "running behind" },
            match logged_at.iter().min() {
                Some((_, raw)) =>
                    format!("logged at {} for a restart at about {}", local_text(*raw, "%H:%M:%S"), local_text(first, "%H:%M:%S")),
                None =>
                    format!("logged at {} for a restart at about {}", local_text(stop.raw_first, "%H:%M:%S"), local_text(first, "%H:%M:%S")),
            }
        ));
    } else if let Some(d) = fixed {
        clock_note = Some(format!(
            "After the restart Windows corrected its clock by {}: the restart times above were logged with a clock {} by about that much.",
            crate::events::signed_duration(d),
            if d < 0 { "running ahead" } else { "running behind" }
        ));
    } else if let Some(f) = stop.earliest_ms() {
        let gap = first - f;
        if gap >= 50 * 60_000 {
            let hours = ((gap as f64 / 3_600_000.0).round() as i64).max(1);
            let reset = ctx.ram.as_ref().is_some_and(|r| r.below_rated()) || ctx.firmware_boot_ms.is_some_and(|f| f >= 45_000);
            clock_note = Some(format!(
                "Windows logged the restart at {}, {} min after the last sign of life at {}. If the PC was off for only a few minutes, the clock after the restart ran about {hours} h ahead{}.",
                local_text(first, "%H:%M:%S"),
                gap / 60_000,
                local_text(f, "%H:%M:%S"),
                if reset {
                    " (a BIOS reset often sets the hardware clock wrong, and the RAM speed says the BIOS was reset)"
                } else {
                    " (a BIOS reset or a wrong hardware clock)"
                }
            ));
            if reset {
                ctx.clock_shift_min = Some(hours * 60);
            }
        } else if gap < -60_000 {
            clock_note = Some(format!(
                "Windows logged the restart at {}, before the last sign of life at {}: the clock after the restart ran behind.",
                local_text(first, "%H:%M:%S"),
                local_text(f, "%H:%M:%S")
            ));
            ctx.clock_shift_min = Some(gap / 60_000);
        }
    }
    let d = crate::diagnose::diagnose(report::Cause::Restart, &ev, true, &peaks, &last_minute, holder.as_ref(), &ctx);
    let stopped = stop.earliest_ms().map(|f| match (stop.until_ms().filter(|u| *u > f), stop.recorder_ms) {
        (Some(u), Some(r)) if r == f => format!(
            "between {} and about {} (the flight recorder's last sample, plus up to 35 s that may not have reached the disk)",
            local_text(f, "%Y-%m-%d %H:%M:%S"),
            local_text(u, "%H:%M:%S")
        ),
        (Some(u), Some(r)) => format!(
            "between {} and about {} (from {}; the flight recorder's last sample is from {}, and up to 35 s after it may not have reached the disk)",
            local_text(f, "%Y-%m-%d %H:%M:%S"),
            local_text(u, "%H:%M:%S"),
            stop.source(),
            local_text(r, "%H:%M:%S")
        ),
        _ => format!("at about {} ({})", local_text(f, "%Y-%m-%d %H:%M:%S"), stop.source()),
    });
    let matched = match &around {
        Some((Around::At(t), a)) => Some(match stop.earliest_ms() {
            Some(f) => format!(
                "{a}: the stop at {} is {} min from it (tolerance {} min).",
                local_text(f, "%H:%M:%S"),
                (f - t).abs() / 60_000,
                tol / 60_000
            ),
            None => format!("{a}: matched by the restart reports at {} (no stop time found).", local_text(first, "%H:%M:%S")),
        }),
        _ => None,
    };
    let rec_last = stop.recorder_ms.filter(|_| rec_ok);
    let fmt_ev = |e: &SysEvent| match logged_at.iter().find(|(t, _)| *t == e.t_ms) {
        Some((_, raw)) => format!(
            "{}  {} (logged as {}, with the clock that was wrong)",
            local_text(e.t_ms, "%m-%d %H:%M:%S"),
            e.describe_with_recorder(rec_last),
            local_text(*raw, "%H:%M:%S")
        ),
        None => format!("{}  {}", local_text(e.t_ms, "%m-%d %H:%M:%S"), e.describe_with_recorder(rec_last)),
    };
    // Windows' last log entry before the restart, when the recorder ran well past it, is just the
    // last thing Windows happened to write, not the stop
    let log_note = match (stop.log_ms, rec_last) {
        (Some(l), Some(r)) if r - l > crate::events::STALE_ALIVE_MS => Some(format!(
            "Earlier than the flight recorder's last sample at {}: the last thing Windows wrote before the stop, not when the PC stopped.",
            local_text(r, "%H:%M:%S")
        )),
        _ => None,
    };
    let note = if window.is_empty() {
        let why = match stop.recorder_ms {
            Some(r) => format!(
                "its last sample before this restart is from {}, too long before the stop to show the moments before it",
                local_text(r, "%Y-%m-%d %H:%M")
            ),
            None => "it has no samples from before this restart (Tachyon was not running, or the recording has since been replaced: it keeps the current and the previous session)".into(),
        };
        format!(
            "From the Windows Event Log only (the 12 hours before the restart): Tachyon's flight recorder could not help, {why}. The GPU clock offsets and RAM speed are as they are now, not necessarily as they were then."
        )
    } else {
        format!(
            "From the Windows Event Log (the 12 hours before the restart) and Tachyon's flight recorder ({} samples, the {} min before the stop). The GPU clock offsets and RAM speed are as they are now, not necessarily as they were then.",
            window.len(),
            (end - window[0].t_ms + 59_999) / 60_000
        )
    };
    let mut out = json!({
        "source": if window.is_empty() { "event_log" } else { "event_log+flight_recorder" },
        "restart_at": local_text(first, "%Y-%m-%d %H:%M"),
        "stopped": stopped,
        "stopped_ms": stop.earliest_ms(),
        "matched": matched,
        "clock_note": clock_note,
        "before_the_stop": recorder_summary(&window),
        "most_vram": holder,
        "diagnosis": d,
        "events": ev.iter().map(fmt_ev).collect::<Vec<_>>(),
        "last_log_entry": stop.log_ms.map(|t| local_text(t, "%Y-%m-%d %H:%M:%S")),
        "last_log_entry_note": log_note,
        "other_restarts": list(&stops.iter().filter(|s| s.first != first).cloned().collect::<Vec<_>>()),
        "note": note,
    });
    add_last_seconds(&mut out, src, stop.earliest_ms());
    Ok(out)
}

fn tool(name: &str, title: &str, description: &str, props: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
    })
}

pub fn tools() -> Value {
    json!([
        tool(
            "get_status",
            "GPU status",
            "What the NVIDIA GPU is doing right now: load, temperature, power, VRAM, clocks, CPU and RAM, the main bottleneck, the VRAM forecast, the running render job and the last crash report headline.",
            json!({})
        ),
        tool(
            "get_bottleneck",
            "Why is it slow?",
            "The current bottleneck in one plain sentence with a confidence (sure, likely, possibly), the process involved and the readings behind it.",
            json!({})
        ),
        tool(
            "get_vram_forecast",
            "VRAM forecast",
            "VRAM use and trend: when it will be full at the current rate (only when the trend is steady), the fastest-growing process, and background apps holding VRAM without using the GPU.",
            json!({})
        ),
        tool("get_processes", "GPU processes", "Processes using the GPU with their category, VRAM and engine use.", json!({})),
        tool(
            "get_blackbox",
            "Flight recorder",
            "Tachyon's flight recorder: peaks, the process holding the most VRAM, a per-minute timeline, a plain summary of the last minute and, if asked, the samples. By default the last minutes up to now; with `since`/`until` any window still on disk, or a whole `session` (the recorder keeps the current and the previous session, e.g. the one before an unexpected restart). Also the last crash report, if any.",
            json!({
                "minutes": { "type": "number", "description": "Window length in minutes (1-60, default 5; with `session` the whole session).", "minimum": 1, "maximum": 60 },
                "since": { "type": "string", "description": "Optional start: local date and time (2026-10-26 18:00), or Unix ms." },
                "until": { "type": "string", "description": "Optional end: local date and time (2026-10-26 18:10), or Unix ms. With `since` only: since + minutes (default 60)." },
                "session": { "type": "string", "enum": ["current", "previous", "all"], "description": "Optional: read one recorder session. `previous` = the session before Tachyon last started (e.g. the minutes before an unexpected restart)." },
                "include_samples": { "type": "boolean", "description": "Also return the samples (at most 300, evenly spread). Default false." },
            })
        ),
        tool(
            "get_history",
            "Job history",
            "The most recent finished render jobs and GPU-busy sessions: duration, average and peak GPU load, fps, energy in Wh, peak VRAM and temperature, time spent throttled, main bottleneck and the comparison with the previous run.",
            json!({ "n": { "type": "integer", "description": "How many jobs (1-100, default 10), newest last.", "minimum": 1, "maximum": 100 } })
        ),
        tool(
            "diagnose_crash",
            "Why did it crash?",
            "The probable causes of the last unexpected restart or GPU driver failure, ranked with a rough percentage: unstable RAM or EXPO/XMP, GPU overclock, power or PSU, overheating, driver timeout (TDR) or an application bug. Each with the evidence (bug check codes, NVIDIA driver resets, GPU timeout reports, WHEA errors, temperatures, power, clock offsets, memory speed) and one piece of advice. Uses Tachyon's crash report; without one (or with `around`) it diagnoses the latest unexpected restart in the Windows Event Log. Estimates, never certain; Tachyon changes no settings.",
            json!({
                "around": { "type": "string", "description": "Optional local date (2026-10-22) or date and time (2026-10-22 18:40) of a past crash to diagnose from the Event Log and the flight recorder. Matched against the time the PC actually stopped (the recorder's last sample or Windows' last entry), not only the restart time." },
                "tolerance_minutes": { "type": "integer", "description": "With a time in `around`: how far the stop may be from it (5-720, default 90).", "minimum": 5, "maximum": 720 },
                "days": { "type": "integer", "description": "Without a crash report: how many days back to look (1-60, default 14).", "minimum": 1, "maximum": 60 },
            })
        ),
        tool(
            "preflight",
            "Will this job fit?",
            "Pre-render check: predicts a render job's peak VRAM and duration from similar past runs (same job, same project, or a similar name, at the same resolution) and compares it with the VRAM free right now, including memory held by idle background apps. Returns fits / tight / wont_fit / unknown with a confidence, one plain sentence and the evidence. Without arguments it checks the render job that is running. Thin history gives `unknown`, never a guess.",
            json!({
                "name": { "type": "string", "description": "Job name, e.g. \"demo4k try 3\" (default: the running render job)." },
                "project": { "type": "string", "description": "Optional project folder name." },
                "resolution": { "type": "string", "description": "Optional resolution: 4K, 1440p, 1080p, 720p, 8K or WxH such as 3840x2160." },
                "frames": { "type": "integer", "description": "Optional frame count, for the duration.", "minimum": 1 },
            })
        ),
        tool(
            "get_pcie_link",
            "PCIe link",
            "The GPU's PCIe link now (generation and width) against the best this GPU and slot can do, and the logged changes. Dropping to a lower generation when idle is normal power saving and is only counted; a link narrower than the slot (x8 instead of x16) or below its generation under load is flagged as degraded (riser cable, slot shared with an M.2 drive, seating, BIOS setting).",
            json!({
                "days": { "type": "integer", "description": "How many days of logged changes (1-60, default 7).", "minimum": 1, "maximum": 60 },
                "n": { "type": "integer", "description": "At most this many changes, newest last (1-200, default 20).", "minimum": 1, "maximum": 200 },
            })
        ),
        tool(
            "get_process_vram",
            "VRAM per process over time",
            "Which processes held the most video memory over a time range: peak, time-weighted average over the time the process was running (running_s) and last value per process (several processes with the same name count together), when each was first and last seen, and a short timeline for the top ones. Also the reading now. Recorded every few seconds while Tachyon runs (on Windows from the same per-process counters Task Manager uses).",
            json!({
                "minutes": { "type": "number", "description": "Window length in minutes up to now or `until` (1-10080, default 60).", "minimum": 1, "maximum": 10080 },
                "since": { "type": "string", "description": "Optional start: local date and time (2026-10-26 18:00), or Unix ms." },
                "until": { "type": "string", "description": "Optional end: local date and time, or Unix ms (default now)." },
                "name": { "type": "string", "description": "Optional part of a process name, e.g. python." },
                "top": { "type": "integer", "description": "How many processes (1-50, default 10).", "minimum": 1, "maximum": 50 },
            })
        ),
        tool(
            "get_job_reports",
            "Marked render jobs",
            "Reports for render jobs marked with `tachyon mark start --job NAME` / `tachyon mark end --job NAME` (a render script runs them): duration, average and peak GPU load, peak VRAM, average and peak power, energy (and cost when a price is set), time clock-limited by power or heat, the PCIe link during the job and the processes holding the most VRAM. Also the jobs still open.",
            json!({
                "n": { "type": "integer", "description": "How many reports (1-100, default 10), newest last.", "minimum": 1, "maximum": 100 },
                "name": { "type": "string", "description": "Optional part of the job name." },
            })
        ),
        tool(
            "compare_last_run",
            "Compare with last time",
            "Compares the latest job (or the latest job whose name contains `name`) with the previous run of the same job.",
            json!({ "name": { "type": "string", "description": "Optional part of the job name." } })
        ),
    ])
}

const OFF: &str = "The agent API is off. Turn on Agent API in Tachyon's Settings (right-click menu, AI agents), or set agent_api = true in tachyon.toml next to tachyon.exe.";

fn minute_lines(w: &[crate::telemetry::Sample]) -> Vec<Value> {
    let mut out = vec![];
    for chunk in w.chunk_by(|a, b| a.t_ms.div_euclid(60_000) == b.t_ms.div_euclid(60_000)) {
        let p = report::peaks(chunk);
        let avg = |f: &dyn Fn(&crate::telemetry::Sample) -> Option<f32>| {
            let v: Vec<f32> = chunk.iter().filter_map(f).collect();
            (!v.is_empty()).then(|| (v.iter().sum::<f32>() / v.len() as f32 * 10.0).round() / 10.0)
        };
        out.push(json!({
            "t_ms": chunk[0].t_ms,
            "time": chunk[0].local().format("%H:%M").to_string(),
            "avg_gpu": avg(&|s| s.util.map(f32::from)),
            "max_temp": p.temp,
            "avg_power_w": avg(&|s| s.power_w),
            "max_vram_mb": p.vram_mb,
            "avg_cpu": avg(&|s| s.cpu),
            "power_throttled_s": p.power_throttled_s,
            "thermal_throttled_s": p.thermal_throttled_s,
        }));
    }
    out
}

/// The `get_blackbox` tool: a window of the flight recorder (by default the last minutes up to now).
fn blackbox(args: &Value, src: &mut dyn Source, now: i64) -> Result<Value, String> {
    let minutes = args["minutes"].as_f64().map(|m| m.clamp(1.0, 60.0));
    let since = parse_time(&args["since"], "since")?;
    let until = parse_time(&args["until"], "until")?;
    let sessions = src.blackbox_sessions();
    let which = args["session"].as_str().unwrap_or("all");
    let mut pool: Vec<Sample> = match which {
        "all" => sessions.iter().flat_map(|(_, v)| v.iter().cloned()).collect(),
        "current" | "previous" => sessions.iter().filter(|(n, _)| n == which).flat_map(|(_, v)| v.iter().cloned()).collect(),
        _ => return Err("`session` must be current, previous or all.".into()),
    };
    pool.sort_by_key(|s| s.t_ms);
    pool.dedup_by_key(|s| s.t_ms);
    let ms = |m: f64| (m * 60_000.0) as i64;
    let (from, to) = match (since, until) {
        (Some(f), Some(t)) => (f, t),
        (Some(f), None) => (f, f + ms(minutes.unwrap_or(60.0))),
        (None, Some(t)) => (t - ms(minutes.unwrap_or(5.0)), t),
        // a whole session (its last hour at most)
        (None, None) if args["session"].is_string() => {
            let end = pool.last().map_or(now, |s| s.t_ms);
            (end - ms(minutes.unwrap_or(60.0)), end)
        }
        (None, None) => (now - ms(minutes.unwrap_or(5.0)), now),
    };
    if to < from {
        return Err("`until` is before `since`.".into());
    }
    let w: Vec<Sample> = pool.into_iter().filter(|s| s.t_ms >= from && s.t_ms <= to).collect();
    let mut mask = 0u32;
    for s in &w {
        mask |= s.throttle.unwrap_or(0);
    }
    let mut v = json!({
        "minutes": ((to - from) as f64 / 60_000.0 * 10.0).round() / 10.0,
        "window": { "from": local_text(from, "%Y-%m-%d %H:%M:%S"), "to": local_text(to, "%Y-%m-%d %H:%M:%S") },
        "samples_in_window": w.len(),
        "from_ms": w.first().map(|s| s.t_ms),
        "to_ms": w.last().map(|s| s.t_ms),
        "peaks": report::peaks(&w),
        "most_vram": report::vram_holder(&w),
        "timeline": minute_lines(&w),
        "summary": recorder_summary(&w),
        "clock_limits": crate::telemetry::throttle_words(mask),
        "sessions": sessions.iter().map(|(n, v)| json!({
            "session": n,
            "samples": v.len(),
            "from": v.first().map(|s| local_text(s.t_ms, "%Y-%m-%d %H:%M:%S")),
            "to": v.last().map(|s| local_text(s.t_ms, "%Y-%m-%d %H:%M:%S")),
        })).collect::<Vec<_>>(),
        "last_crash_report": src.report(),
    });
    if w.is_empty() {
        v["note"] = json!(
            "Nothing recorded in this window. The flight recorder runs while Tachyon is open (blackbox = true) and keeps the current and the previous session: see `sessions` for what is on disk."
        );
    }
    if args["include_samples"].as_bool().unwrap_or(false) {
        let step = w.len().div_ceil(300).max(1);
        v["samples"] = json!(w.iter().step_by(step).collect::<Vec<_>>());
    }
    Ok(v)
}

fn call(name: &str, args: &Value, src: &mut dyn Source) -> Result<Value, String> {
    if !src.enabled() {
        return Err(OFF.into());
    }
    let now = crate::telemetry::now_ms();
    match name {
        "get_status" => Ok(src.status()),
        "get_bottleneck" => {
            let b = src.bottleneck();
            Ok(if b.is_null() { json!({ "bottleneck": null, "note": "No GPU reading yet." }) } else { json!({ "bottleneck": b }) })
        }
        "get_vram_forecast" => {
            let s = src.status();
            Ok(json!({ "forecast": s["vram_forecast"], "idle_vram_note": s["idle_vram_note"], "source": s["source"] }))
        }
        "get_processes" => {
            let s = src.status();
            Ok(json!({ "processes": s["reading"]["procs"], "source": s["source"] }))
        }
        "get_blackbox" => blackbox(args, src, now),
        "get_history" => {
            let n = args["n"].as_u64().unwrap_or(10).clamp(1, 100) as usize;
            let jobs = src.history(n);
            Ok(json!({ "jobs": jobs, "count": jobs.len() }))
        }
        "diagnose_crash" => {
            let report = if args["around"].is_string() { None } else { src.report() };
            let Some(r) = report else {
                return diagnose_from_event_log(args, src, now);
            };
            let (d, recomputed) = match r.diagnosis.clone() {
                Some(d) => (Some(d), false),
                None => {
                    // a report from 1.0: work it out from what the report kept
                    let mut ctx = src.context();
                    ctx.oc = None;
                    ctx.just_after = r.cause == report::Cause::Restart
                        && r.boot_ms.zip(src.boot_ms()).is_some_and(|(a, b)| (a - b).abs() <= 15 * 60_000);
                    if !ctx.just_after {
                        ctx.firmware_boot_ms = None;
                    }
                    let ev: Vec<_> = r.events.iter().map(|e| e.event.clone()).collect();
                    (
                        crate::diagnose::diagnose(
                            r.cause,
                            &ev,
                            r.events_note.is_none(),
                            &r.peaks_last_minute,
                            &[],
                            r.vram_holder.as_ref(),
                            &ctx,
                        ),
                        true,
                    )
                }
            };
            let mut v = json!({
                "headline": r.headline,
                "stopped_ms": r.stopped_ms,
                "cause": r.cause,
                "diagnosis": d,
                "events": r.events.iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
                "just_before": r.just_before,
            });
            let window: Vec<Sample> =
                all_samples(src).into_iter().filter(|s| s.t_ms <= r.stopped_ms && s.t_ms >= r.stopped_ms - report::TIMELINE_MS).collect();
            if !window.is_empty() {
                v["before_the_stop"] = recorder_summary(&window);
            }
            if d.is_none() {
                v["note"] = json!("Tachyon stopped without closing, but Windows logged nothing that points to a system problem.");
            } else if recomputed {
                v["note"] = json!("Worked out now from an older report, without the GPU overclock reading from before the crash.");
            }
            add_last_seconds(&mut v, src, Some(r.stopped_ms));
            Ok(v)
        }
        "preflight" => preflight(args, src),
        "get_pcie_link" => pcie_link(args, src, now),
        "get_process_vram" => process_vram(args, src, now),
        "get_job_reports" => job_reports(args, src, now),
        "compare_last_run" => {
            let filter = args["name"].as_str().map(str::to_lowercase);
            let jobs = src.history(2000);
            let Some(cur) = jobs.iter().rev().find(|j| filter.as_ref().is_none_or(|f| j.name.to_lowercase().contains(f))) else {
                return Ok(json!({ "comparison": null, "note": "No finished job matches yet." }));
            };
            let prev = jobs.iter().rev().find(|j| j.key == cur.key && j.end_ms <= cur.start_ms);
            Ok(match prev {
                Some(p) => json!({ "comparison": history::compare(p, cur), "latest": cur, "previous": p }),
                None => json!({ "comparison": null, "latest": cur, "note": "This is the first recorded run of this job." }),
            })
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

const NO_DIR: &str = "No Tachyon data folder (LOCALAPPDATA is not set), so nothing has been recorded.";

/// The `get_pcie_link` tool.
fn pcie_link(args: &Value, src: &mut dyn Source, now: i64) -> Result<Value, String> {
    let days = args["days"].as_u64().unwrap_or(7).clamp(1, 60) as i64;
    let n = args["n"].as_u64().unwrap_or(20).clamp(1, 200) as usize;
    let s = src.status();
    let mut v = json!({ "now": s["pcie_link"], "source": s["source"] });
    let Some(dir) = src.data_dir() else {
        v["note"] = json!(NO_DIR);
        return Ok(v);
    };
    let from = now - days * 86_400_000;
    let all: Vec<crate::pcie::LinkEvent> = crate::pcie::recent(&dir.join("history"), 5000).into_iter().filter(|e| e.t_ms >= from).collect();
    let degraded = all.iter().filter(|e| e.state.degraded()).count();
    let idle: u32 = all.iter().filter_map(|e| e.idle_changes).sum();
    let skip = all.len().saturating_sub(n);
    v["days"] = json!(days);
    v["degraded_events"] = json!(degraded);
    v["idle_changes_logged"] = json!(idle);
    v["changes"] = json!(
        all[skip..]
            .iter()
            .map(|e| json!({ "time": local_text(e.t_ms, "%Y-%m-%d %H:%M:%S"), "from": e.from, "to": e.to, "state": e.state, "gpu_pct": e.gpu_pct, "idle_changes": e.idle_changes, "sentence": e.sentence }))
            .collect::<Vec<_>>()
    );
    v["summary"] = json!(match (degraded, &s["pcie_link"]["sentence"]) {
        (0, Value::String(now_s)) => format!("{now_s} No degraded link in the last {days} day(s)."),
        (0, _) => format!("No degraded link logged in the last {days} day(s)."),
        (d, Value::String(now_s)) => format!("{now_s} The link was degraded {d} time(s) in the last {days} day(s): see `changes`."),
        (d, _) => format!("The link was degraded {d} time(s) in the last {days} day(s): see `changes`."),
    });
    if s["source"] != "tachyon" {
        v["note"] = json!("Tachyon is not running: the link now is a single reading, and changes are only logged while Tachyon runs.");
    }
    Ok(v)
}

/// The `get_process_vram` tool.
fn process_vram(args: &Value, src: &mut dyn Source, now: i64) -> Result<Value, String> {
    let minutes = args["minutes"].as_f64().map(|m| m.clamp(1.0, 10_080.0));
    let since = parse_time(&args["since"], "since")?;
    let until = parse_time(&args["until"], "until")?;
    let ms = |m: f64| (m * 60_000.0) as i64;
    let (from, to) = match (since, until) {
        (Some(f), Some(t)) => (f, t),
        (Some(f), None) => (f, minutes.map_or(now, |m| f + ms(m))),
        (None, Some(t)) => (t - ms(minutes.unwrap_or(60.0)), t),
        (None, None) => (now - ms(minutes.unwrap_or(60.0)), now),
    };
    if to < from {
        return Err("`until` is before `since`.".into());
    }
    let top = args["top"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
    let name = args["name"].as_str().map(str::trim).filter(|n| !n.is_empty());
    let s = src.status();
    let mut v = json!({});
    if s["process_vram"].is_object() {
        v["now"] = s["process_vram"].clone();
    } else if let Some((list, source)) = src.proc_vram_now() {
        let list = crate::procvram::top(list);
        v["now"] = json!({ "source": source, "top": list.iter().take(top).map(|(n, pid, mb)| json!({ "name": n, "pid": pid, "vram_mb": mb })).collect::<Vec<_>>() });
    }
    let Some(dir) = src.data_dir() else {
        v["note"] = json!(NO_DIR);
        return Ok(v);
    };
    let pts = crate::procvram::read_range(&dir.join("history"), from, to);
    v["history"] = crate::procvram::summary(&pts, from, to, name, top);
    if pts.is_empty() {
        v["note"] = json!(
            "Nothing recorded in this window: per-process VRAM is logged every few seconds while Tachyon runs (history = true, proc_vram_secs > 0)."
        );
    }
    Ok(v)
}

/// The `get_job_reports` tool.
fn job_reports(args: &Value, src: &mut dyn Source, now: i64) -> Result<Value, String> {
    let n = args["n"].as_u64().unwrap_or(10).clamp(1, 100) as usize;
    let filter = args["name"].as_str().map(str::to_lowercase).filter(|f| !f.trim().is_empty());
    let Some(dir) = src.data_dir() else { return Ok(json!({ "reports": [], "note": NO_DIR })) };
    let all = crate::markers::reports(&dir.join("history"), 5000);
    let mut r: Vec<&crate::markers::JobReport> =
        all.iter().filter(|r| filter.as_ref().is_none_or(|f| r.job.to_lowercase().contains(f))).collect();
    let skip = r.len().saturating_sub(n);
    r.drain(..skip);
    let s = src.status();
    let running = s["source"] == "tachyon";
    let open = if running {
        s["marked_jobs"]["open"].clone()
    } else {
        // what the marker file says is open (nothing measures it while Tachyon is closed)
        let mut open: Vec<(String, i64)> = vec![];
        for m in crate::markers::read_all(&dir) {
            open.retain(|(j, _)| *j != m.job);
            if m.op == "start" && now - m.t < crate::markers::MAX_JOB_MS {
                open.push((m.job, m.t));
            }
        }
        json!(
            open.iter()
                .map(|(j, t)| json!({ "job": j, "started": local_text(*t, "%Y-%m-%d %H:%M:%S"), "running_s": (now - t) / 1000 }))
                .collect::<Vec<_>>()
        )
    };
    // each with the comparison against the previous comparable marked job
    let reps: Vec<Value> = r
        .iter()
        .map(|j| {
            let mut x = json!(j);
            match j.compare_in(&all) {
                Some(c) => {
                    x["compared_with"] = json!({
                        "job": c.prev_job,
                        "started": local_text(c.prev_start_ms, "%Y-%m-%d %H:%M:%S"),
                        "same_job": c.same_job,
                    });
                    x["vs_previous"] = json!(c.sentence());
                }
                None => x["vs_previous"] = json!(crate::markers::FIRST_RUN),
            }
            x
        })
        .collect();
    let mut v = json!({ "reports": reps, "count": r.len(), "open": open, "source": s["source"] });
    if !running {
        v["note"] = json!("Tachyon is not running: markers are written but only a running Tachyon measures the jobs.");
    } else if r.is_empty() {
        v["note"] = json!(
            "No marked job has finished yet. A render script marks a job with `tachyon mark start --job NAME` and `tachyon mark end --job NAME`."
        );
    }
    if let Some(notes) = s["marked_jobs"]["notes"].as_array().filter(|a| !a.is_empty()) {
        v["marker_notes"] = json!(notes);
    }
    Ok(v)
}

/// The `preflight` tool: the running job's published check, or a check for the job asked about.
fn preflight(args: &Value, src: &mut dyn Source) -> Result<Value, String> {
    use crate::preflight::{LiveVram, Query, predict};
    let s = src.status();
    let arg = |k: &str| args[k].as_str().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let resolution = arg("resolution").map(|r| crate::parse::resolution_hint(&r).map(crate::parse::resolution_label).unwrap_or(r));
    let running = s["render"]["job"].as_str().map(str::to_string);
    let Some(name) = arg("name").or_else(|| running.clone()) else {
        return Ok(
            json!({ "preflight": null, "note": "No render job is running. Pass `name` (and `resolution`) to check a job before starting it." }),
        );
    };
    let is_running = running
        .as_ref()
        .is_some_and(|r| history::job_key(history::JobKind::Render, r) == history::job_key(history::JobKind::Render, &name));
    // the running Tachyon's own check knows what the job already holds
    if is_running
        && !s["preflight"].is_null()
        && arg("project").is_none()
        && args["frames"].is_null()
        && resolution.as_ref().is_none_or(|r| s["preflight"]["resolution"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(r)))
    {
        return Ok(json!({ "preflight": s["preflight"], "source": s["source"] }));
    }
    let from_render = |k: &str| if is_running { s["render"][k].as_str().map(str::to_string) } else { None };
    let q = Query {
        name,
        project: arg("project").or_else(|| from_render("project")),
        resolution: resolution.or_else(|| from_render("resolution")),
        frames: args["frames"].as_u64().or_else(|| if is_running { s["render"]["total_frames"].as_u64() } else { None }),
    };
    let r = &s["reading"];
    let live = LiveVram {
        total_mb: r["vram_total_mb"].as_f64().map(|v| v as f32),
        used_mb: r["vram_mb"].as_f64().map(|v| v as f32),
        job_now_mb: 0.0,
        hogs: serde_json::from_value(s["vram_forecast"]["hogs"].clone()).unwrap_or_default(),
    };
    let p = predict(&q, &src.history(500), &live);
    let mut v = json!({ "preflight": p, "source": s["source"] });
    if is_running {
        v["note"] = json!("This job is already running: what it holds now counts as used here.");
    } else if live.used_mb.is_none() {
        v["note"] = json!("No VRAM reading right now, so only the history is shown.");
    }
    Ok(v)
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Handle one JSON-RPC message. Returns the reply, or None for notifications.
pub fn handle(msg: &Value, src: &mut dyn Source) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let id = id?; // none: a notification (initialized, cancelled, ...)
    if msg["jsonrpc"] != "2.0" || method.is_empty() {
        return Some(err(&id, -32600, "Invalid request"));
    }
    Some(match method {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOLS[0]);
            let v = if PROTOCOLS.contains(&asked) { asked } else { PROTOCOLS[0] };
            ok(
                &id,
                json!({
                    "protocolVersion": v,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "tachyon", "title": "Tachyon GPU monitor", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Read-only view of this PC's NVIDIA GPU from Tachyon. Use get_bottleneck to explain slowness, get_vram_forecast before loading large models, get_blackbox and diagnose_crash after a crash, preflight before starting a render (will it fit?), get_history and compare_last_run for render jobs, get_job_reports for jobs a script marked with `tachyon mark`, get_process_vram for who held VRAM when, and get_pcie_link for the PCIe link.",

                }),
            )
        }
        "ping" => ok(&id, json!({})),
        "tools/list" => ok(&id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args, src).map(|mut v| {
                crate::agent::tidy(&mut v);
                v
            }) {
                Ok(v) => ok(
                    &id,
                    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }], "structuredContent": v, "isError": false }),
                ),
                Err(e) => ok(&id, json!({ "content": [{ "type": "text", "text": e }], "isError": true })),
            }
        }
        _ => err(&id, -32601, &format!("Method not found: {method}")),
    })
}

/// The real source.
pub struct Live {
    cfg: Config,
    dir: Option<PathBuf>,
    standalone: Option<Standalone>,
}

struct Standalone {
    nvml: Option<crate::nvml::Nvml>,
    sys: crate::sysinfo::SysMeter,
    explainer: crate::explain::Explainer,
}

impl Live {
    pub fn new(cfg: Config) -> Self {
        Live { cfg, dir: crate::config::data_dir(), standalone: None }
    }

    /// One reading of our own (Tachyon not running or agent API off in the running copy).
    fn own_reading(&mut self) -> crate::monitor::Snapshot {
        let st = self.standalone.get_or_insert_with(|| Standalone {
            nvml: crate::nvml::Nvml::open().ok(),
            sys: Default::default(),
            explainer: Default::default(),
        });
        let mut snap = crate::monitor::Snapshot::default();
        let (cpu, mem) = st.sys.sample(1);
        let mut g = None;
        if let Some(n) = st.nvml.as_mut() {
            let sel = self.cfg.gpu_index.min(n.names.len().saturating_sub(1) as u32);
            snap.gpus = n.names.clone();
            snap.driver = n.driver.clone();
            snap.selected = sel;
            g = Some(n.sample(sel));
        } else {
            snap.error = Some("NVML unavailable".into());
        }
        let procs = g.as_ref().and_then(|g| g.procs.as_ref()).map(|list| {
            #[cfg(windows)]
            let table = crate::procinfo::win::snapshot();
            #[cfg(not(windows))]
            let table = crate::procinfo::ProcTable::default();
            crate::monitor::attribute(list, &table, &self.cfg.process_rules, &Default::default())
        });
        let s = crate::telemetry::Sample::from_tick(
            crate::telemetry::now_ms(),
            g.as_ref(),
            procs.as_deref(),
            cpu,
            mem,
            crate::insight::KEEP_PROCS,
        );
        snap.insights.bottleneck = st.explainer.push(&s).cloned();
        if let Some(g) = g.as_ref() {
            let mut lt = crate::pcie::LinkTracker::default();
            lt.push(s.t_ms, g.pcie_gen, g.pcie_width, g.pcie_gen_max, g.pcie_width_max, s.util);
            snap.insights.pcie = Some(lt.status).filter(|l| l.state.is_some());
        }
        snap.insights.latest = Some(s);
        snap.insights.data_dir = self.dir.clone();
        snap.insights.report = self.dir.as_ref().and_then(|d| report::load(&d.join("blackbox"))).map(std::sync::Arc::new);
        snap.sample = g;
        snap.procs = procs;
        snap
    }
}

impl Source for Live {
    fn enabled(&self) -> bool {
        self.cfg.agent_api
    }

    fn status(&mut self) -> Value {
        if let Some(v) =
            crate::agent::read_published().filter(|v| v["t_ms"].as_i64().is_some_and(|t| crate::telemetry::now_ms() - t < 10_000))
        {
            return v;
        }
        // CPU load needs two readings; take them a moment apart.
        if self.standalone.is_none() {
            self.own_reading();
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let snap = self.own_reading();
        let mut v = crate::agent::status(&snap, "standalone");
        v["note"] =
            json!("Tachyon is not running (or runs without agent_api = true), so this is a single reading: no VRAM trend or job yet.");
        v
    }

    fn bottleneck(&mut self) -> Value {
        let v = self.status();
        if v["source"] == "tachyon" {
            return v["bottleneck"].clone();
        }
        // standalone: a few quick readings for the rules to work on
        let mut last = None;
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            last = self.own_reading().insights.bottleneck;
        }
        serde_json::to_value(last).unwrap_or(Value::Null)
    }

    fn blackbox(&mut self) -> Vec<crate::telemetry::Sample> {
        self.dir.as_ref().map(|d| recorder::read_current(&d.join("blackbox"))).unwrap_or_default()
    }

    fn blackbox_sessions(&mut self) -> Vec<(String, Vec<Sample>)> {
        self.dir.as_ref().map(|d| recorder::read_sessions(&d.join("blackbox"))).unwrap_or_default()
    }

    fn report(&mut self) -> Option<report::Report> {
        report::load(&self.dir.as_ref()?.join("blackbox"))
    }

    fn history(&mut self, n: usize) -> Vec<JobRecord> {
        let Some(d) = self.dir.as_ref().map(|d| d.join("history")) else { return vec![] };
        // an automatic job that is the same work as a marked job is not listed twice
        let spans: Vec<(i64, i64)> = crate::markers::reports(&d, 200).iter().map(|r| (r.start_ms, r.end_ms)).collect();
        let mut v = history::recent(&d, n + spans.len());
        v.retain(|j| !crate::markers::duplicates_marked(j.start_ms, j.end_ms, &spans));
        let skip = v.len().saturating_sub(n);
        v.split_off(skip)
    }

    fn data_dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    fn proc_vram_now(&mut self) -> Option<(Vec<(String, u32, f32)>, &'static str)> {
        let snap = self.own_reading();
        let nv: Vec<(String, u32, f32)> =
            snap.procs.iter().flatten().filter_map(|p| p.info.vram_mb.map(|m| (p.info.name.clone(), p.info.pid, m as f32))).collect();
        if !nv.is_empty() {
            return Some((nv, "nvml"));
        }
        #[cfg(windows)]
        {
            let used = snap.sample.as_ref().and_then(|g| g.vram_used_mb);
            let list = crate::procvram::pdh::GpuMem::open()?.read(used)?;
            let table = crate::procinfo::win::snapshot();
            Some((
                list.into_iter()
                    .map(|(pid, mb)| (table.get(&pid).map(|(_, n)| n.clone()).unwrap_or_else(|| format!("pid {pid}")), pid, mb))
                    .collect(),
                "windows counters",
            ))
        }
        #[cfg(not(windows))]
        None
    }

    fn context(&mut self) -> crate::diagnose::Context {
        // the clock offsets now (read-only NVML queries); they may differ from the day of the crash
        let oc = crate::nvml::Nvml::open().ok().map(|mut n| {
            let sel = self.cfg.gpu_index.min(n.names.len().saturating_sub(1) as u32);
            n.oc_info(sel)
        });
        crate::diagnose::Context {
            oc: oc.filter(|o| !o.is_empty()),
            oc_after: true,
            ram: crate::sysinfo::ram_speed(),
            firmware_boot_ms: crate::sysinfo::firmware_boot_ms(),
            ..Default::default()
        }
    }
}

/// Run the server until stdin closes.
pub fn serve(cfg: Config) {
    let mut src = Live::new(cfg);
    let stdin = std::io::stdin();
    serve_io(stdin.lock(), std::io::stdout().lock(), &mut src);
}

/// Longest request line read; longer lines are answered with an error and skipped.
pub const MAX_LINE: usize = 1 << 20;
/// Most requests answered from one JSON-RPC batch.
pub const MAX_BATCH: usize = 64;

/// Read one line of at most `MAX_LINE` bytes. `Ok(None)` = end of input; `Err(())` = too long
/// (the rest of the line has been skipped).
fn read_line_capped(input: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Option<Result<(), ()>>> {
    buf.clear();
    let mut too_long = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && !too_long { None } else { Some(if too_long { Err(()) } else { Ok(()) }) });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            if buf.len() + take > MAX_LINE + 1 {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        input.consume(take);
        if done {
            return Ok(Some(if too_long { Err(()) } else { Ok(()) }));
        }
    }
}

/// The JSON-RPC loop over any reader and writer: one request (or batch) per line.
/// Bad UTF-8, garbage and oversized lines get an error reply; the server keeps going.
pub fn serve_io(mut input: impl BufRead, mut out: impl Write, src: &mut dyn Source) {
    let mut buf = Vec::with_capacity(4096);
    loop {
        let reply = match read_line_capped(&mut input, &mut buf) {
            Err(_) | Ok(None) => break,
            Ok(Some(Err(()))) => Some(err(&Value::Null, -32600, "Request too large")),
            Ok(Some(Ok(()))) => {
                let Ok(line) = std::str::from_utf8(&buf) else {
                    let r = err(&Value::Null, -32700, "Parse error: not UTF-8");
                    let _ = writeln!(out, "{r}");
                    let _ = out.flush();
                    continue;
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(Value::Array(batch)) if batch.is_empty() => Some(err(&Value::Null, -32600, "Empty batch")),
                    Ok(Value::Array(batch)) if batch.len() > MAX_BATCH => Some(err(&Value::Null, -32600, "Batch too large")),
                    Ok(Value::Array(batch)) => {
                        let r: Vec<Value> = batch.iter().filter_map(|m| handle(m, src)).collect();
                        (!r.is_empty()).then_some(Value::Array(r))
                    }
                    Ok(m) => handle(&m, src),
                    Err(_) => Some(err(&Value::Null, -32700, "Parse error")),
                }
            }
        };
        if let Some(r) = reply {
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::Sample;

    #[derive(Default)]
    struct Fake {
        on: bool,
        jobs: Vec<JobRecord>,
        report: Option<report::Report>,
        status: Option<Value>,
        events: Vec<crate::events::SysEvent>,
        ctx: crate::diagnose::Context,
        sessions: Vec<(String, Vec<Sample>)>,
        log: Vec<crate::events::SysEvent>,
        boot: Option<i64>,
        dir: Option<PathBuf>,
        fast: Vec<(String, Vec<crate::fast::FastSample>)>,
    }

    impl Source for Fake {
        fn enabled(&self) -> bool {
            self.on
        }
        fn status(&mut self) -> Value {
            if let Some(s) = &self.status {
                return s.clone();
            }
            json!({ "source": "tachyon", "bottleneck": { "kind": "gpu_bound", "sentence": "The GPU is the limit." }, "vram_forecast": { "warning": "At this rate VRAM will be full in ~3 min." }, "reading": { "procs": [{ "name": "python.exe" }] } })
        }
        fn blackbox(&mut self) -> Vec<Sample> {
            let now = crate::telemetry::now_ms();
            (0..600).map(|i| Sample { t_ms: now - (600 - i) * 1000, util: Some(90), ..Default::default() }).collect()
        }
        fn report(&mut self) -> Option<report::Report> {
            self.report.clone()
        }
        fn history(&mut self, n: usize) -> Vec<JobRecord> {
            let k = self.jobs.len().saturating_sub(n);
            self.jobs[k..].to_vec()
        }
        fn events(&mut self, from: i64, to: i64) -> Result<Vec<crate::events::SysEvent>, String> {
            Ok(self.events.iter().filter(|e| e.t_ms >= from && e.t_ms <= to).cloned().collect())
        }
        fn context(&mut self) -> crate::diagnose::Context {
            self.ctx.clone()
        }
        fn blackbox_sessions(&mut self) -> Vec<(String, Vec<Sample>)> {
            if self.sessions.is_empty() { vec![("current".into(), self.blackbox())] } else { self.sessions.clone() }
        }
        fn last_log_before(&mut self, to: i64) -> Result<Vec<crate::events::SysEvent>, String> {
            Ok(self.log.iter().filter(|e| e.t_ms <= to).cloned().collect())
        }
        fn boot_ms(&mut self) -> Option<i64> {
            self.boot
        }
        fn data_dir(&self) -> Option<PathBuf> {
            self.dir.clone()
        }
        fn fast_sessions(&mut self) -> Vec<(String, Vec<crate::fast::FastSample>)> {
            self.fast.clone()
        }
    }

    /// The real recording of 26 Oct 2026 (the last 5 minutes, 1.0 format, as found on the PC).
    fn oct26() -> Vec<Sample> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/blackbox_2026-10-26_idle_power_loss.bin");
        crate::recorder::read_ring(&p)
    }

    /// 26 Oct 2026: idle at the desktop, the PC went off at 18:09 (a USB stick in the BIOS
    /// FlashBack port). Kernel-Power 41 code 0, no dump; the BIOS came back on its defaults, so
    /// the RAM ran at 3600 and the clock at the next start was 1 h fast: Windows logged the
    /// restart at 19:12 for a boot at 18:12. `around 18:09` must find it and use the recorder.
    fn oct26_fake() -> (Fake, i64) {
        use crate::events::{KERNEL_GENERAL, KERNEL_POWER};
        let rec = oct26();
        let last = rec.last().unwrap().t_ms; // 18:09:04 Rome
        let shifted_boot = last + 3 * 60_000 + 18_000 + 3_600_000; // 18:12:22 real, logged as 19:12:22
        let e = |t: i64, p: &str, id: u32, code: Option<String>| SysEvent { t_ms: t, provider: p.into(), id, level: 2, code, record: 0 };
        let alive = chrono::DateTime::from_timestamp_millis(last + 29_000).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let events = vec![
            e(last - 3 * 86_400_000, "nvlddmkm", 153, None), // days before: outside the 12 h
            e(shifted_boot + 18_000, KERNEL_POWER, 41, None),
            e(shifted_boot + 19_000, "EventLog", 6008, Some(alive)),
        ];
        let log = vec![
            e(last - 7 * 60_000, "Service Control Manager", 7036, None),
            e(shifted_boot + 2_000, KERNEL_GENERAL, 12, None),
            e(shifted_boot + 9_000, "EventLog", 6005, None),
            e(shifted_boot + 18_000, KERNEL_POWER, 41, None),
        ];
        let kit = crate::sysinfo::RamSpeed {
            kind: "DDR5".into(),
            rated_mts: Some(4800),
            configured_mts: Some(3600),
            modules: 4,
            part: Some("CMK32GX5M2B6000Z30".into()),
        };
        let now_rec: Vec<Sample> =
            (0..120).map(|i| Sample { t_ms: shifted_boot + 120_000 + i * 1000, util: Some(2), ..Default::default() }).collect();
        let f = Fake {
            on: true,
            events,
            log,
            boot: Some(shifted_boot),
            ctx: crate::diagnose::Context { ram: Some(kit), firmware_boot_ms: Some(31_000), ..Default::default() },
            sessions: vec![("current".into(), now_rec), ("previous".into(), rec)],
            ..Default::default()
        };
        (f, last)
    }

    #[test]
    fn diagnose_crash_uses_the_recorder_and_matches_the_stop_time() {
        let call = |f: &mut Fake, a: Value| rpc(f, 5, "tools/call", json!({ "name": "diagnose_crash", "arguments": a }))["result"].clone();
        let (mut f, last) = oct26_fake();
        let at = local_text(last, "%Y-%m-%d %H:%M");
        let r = call(&mut f, json!({ "around": at, "days": 1 }))["structuredContent"].clone();
        let txt = serde_json::to_string_pretty(&r).unwrap();
        assert_eq!(r["source"], "event_log+flight_recorder", "{txt}");
        assert_eq!(r["stopped_ms"], last + 29_000, "the 6008 sign of life is the latest");
        assert!(r["stopped"].as_str().unwrap().contains(&format!("and about {}", local_text(last + 35_000, "%H:%M:%S"))), "{txt}");
        assert!(r["matched"].as_str().unwrap().contains("is 0 min from it"), "{txt}");
        assert!(r["clock_note"].as_str().unwrap().contains("ran about 1 h ahead"), "{txt}");
        let b = &r["before_the_stop"];
        assert_eq!(b["idle"], true, "{txt}");
        assert_eq!(b["last_minute"]["power_limit_w"], 360.0);
        assert!(b["last_minute"]["power_w"]["max"].as_f64().unwrap() < 30.0, "{txt}");
        let sentence = b["sentence"].as_str().unwrap();
        assert!(
            sentence.contains("the PC was idle") && sentence.contains("CrossDeviceResume.exe") && sentence.contains("reliability policy"),
            "{sentence}"
        );
        assert!(!txt.contains('\u{fffd}'), "no garbage characters");
        let d = &r["diagnosis"];
        assert_eq!(d["causes"][0]["suspect"], "power_cut", "{txt}");
        let power = d["causes"].as_array().unwrap().iter().find(|c| c["suspect"] == "power").map_or(0, |c| c["percent"].as_u64().unwrap());
        assert!(power <= 20, "{txt}");
        assert!(d["hints"][0].as_str().unwrap().starts_with("BIOS likely reset to safe defaults (EXPO/XMP off) at this restart"), "{txt}");
        assert!(d["hints"][0].as_str().unwrap().contains("about 1 h ahead"), "{txt}");
        assert!(
            !r["note"].as_str().unwrap().contains("no readings") && r["note"].as_str().unwrap().contains("flight recorder (300 samples"),
            "{txt}"
        );
        assert!(!r["events"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("nvlddmkm")), "{txt}");
        // the stop is matched, not the boot: 16:00 with 90 min tolerance finds nothing, 17:00 with 120 does
        let early = local_text(last - 129 * 60_000, "%Y-%m-%d %H:%M");
        let r = call(&mut f, json!({ "around": early }))["structuredContent"].clone();
        assert!(r["diagnosis"].is_null() && r["note"].as_str().unwrap().contains("Found nearby"), "{r:#}");
        let r = call(&mut f, json!({ "around": early, "tolerance_minutes": 150 }))["structuredContent"].clone();
        assert_eq!(r["diagnosis"]["causes"][0]["suspect"], "power_cut", "{r:#}");
        // the whole day, and no `around` (latest restart)
        let day = local_text(last, "%Y-%m-%d");
        for a in [json!({ "around": day }), json!({ "days": 1 })] {
            let r = call(&mut f, a)["structuredContent"].clone();
            assert_eq!(r["source"], "event_log+flight_recorder", "{r:#}");
        }
        // without the recorder: the Event Log only, and it says why
        f.sessions = vec![("current".into(), vec![Sample { t_ms: last + 7_200_000, ..Default::default() }])];
        let r = call(&mut f, json!({ "around": at }))["structuredContent"].clone();
        assert_eq!(r["source"], "event_log");
        assert!(r["note"].as_str().unwrap().contains("no samples from before this restart"), "{r:#}");
        assert!(r["before_the_stop"].is_null());
    }

    /// The Event Log exactly as on the PC (System log, record numbers included): the restart at
    /// 18:12:22 logged at 19:12 (BIOS clock 1 h fast), Windows putting the clock back at 18:45:41
    /// (stamped before the reports it corrects), a real entry at 19:12:11 after that, and a clean
    /// restart at 19:06:51 with EXPO back on (5600 MT/s).
    #[test]
    fn diagnose_crash_on_the_real_26_oct_event_log() {
        use crate::events::{KERNEL_GENERAL, KERNEL_POWER};
        let rec = oct26();
        let last = rec.last().unwrap().t_ms; // 18:09:04
        let real = |hms: (i64, i64, i64)| last + ((hms.0 - 18) * 3600 + (hms.1 - 9) * 60 + hms.2 - 4) * 1000;
        let ahead = 3_600_000;
        let e = |t: i64, p: &str, id: u32, code: Option<String>, record: u64| SysEvent {
            t_ms: t,
            provider: p.into(),
            id,
            level: 2,
            code,
            record,
        };
        let alive = chrono::DateTime::from_timestamp_millis(real((17, 31, 14))).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let kp41 = e(real((18, 12, 25)) + ahead, KERNEL_POWER, 41, None, 40145);
        let e6008 = e(real((18, 12, 38)) + ahead, "EventLog", 6008, Some(alive), 40132);
        let fix = e(real((18, 45, 41)), KERNEL_GENERAL, 1, Some("-3600".into()), 40552);
        let log = vec![
            e(real((17, 12, 21)), "Service Control Manager", 7040, None, 40112),
            e(real((17, 15, 0)), "Service Control Manager", 7040, None, 40113),
            e(real((18, 12, 22)) + ahead, KERNEL_GENERAL, 12, None, 40114),
            e(real((18, 12, 22)) + ahead, "Microsoft-Windows-Kernel-Boot", 153, None, 40116),
            e(real((18, 12, 22)) + ahead, "Microsoft-Windows-HAL", 16, None, 40131),
            e6008.clone(),
            e(real((18, 12, 24)) + ahead, "Microsoft-Windows-FilterManager", 6, None, 40136),
            kp41.clone(),
            fix.clone(),
            e(real((19, 6, 51)), KERNEL_GENERAL, 12, None, 40700),
            e(real((19, 6, 55)), "EventLog", 6005, None, 40710),
            e(real((19, 12, 11)), "Microsoft-Windows-TPM-WMI", 1808, None, 40796),
        ];
        let kit = crate::sysinfo::RamSpeed {
            kind: "DDR5".into(),
            rated_mts: Some(4800),
            configured_mts: Some(5600),
            modules: 4,
            part: Some("CMK32GX5M2B6000Z30".into()),
        };
        let older: Vec<Sample> =
            (0..900).map(|i| Sample { t_ms: last - 6 * 3_600_000 + i * 1000, util: Some(1), ..Default::default() }).collect();
        let mut f = Fake {
            on: true,
            events: vec![e6008, kp41, fix],
            log,
            boot: Some(real((19, 6, 51))),
            ctx: crate::diagnose::Context { ram: Some(kit), firmware_boot_ms: Some(9_000), ..Default::default() },
            sessions: vec![("previous".into(), older), ("current".into(), rec)],
            ..Default::default()
        };
        let at = local_text(last, "%Y-%m-%d %H:%M");
        let r = rpc(&mut f, 7, "tools/call", json!({ "name": "diagnose_crash", "arguments": { "around": at, "days": 1 } }))["result"]["structuredContent"].clone();
        let txt = serde_json::to_string_pretty(&r).unwrap();
        assert_eq!(r["source"], "event_log+flight_recorder", "{txt}");
        assert_eq!(r["stopped_ms"], last, "the recorder's last sample, not the 19:12:11 entry: {txt}");
        assert_eq!(r["restart_at"], local_text(real((18, 12, 25)), "%Y-%m-%d %H:%M"), "{txt}");
        assert_eq!(r["last_log_entry"], local_text(real((17, 15, 0)), "%Y-%m-%d %H:%M:%S"), "{txt}");
        assert!(r["matched"].as_str().unwrap().contains("is 0 min from it"), "{txt}");
        let note = r["clock_note"].as_str().unwrap();
        assert!(note.contains("put back its clock by 1 h 0 min at") && note.contains("running ahead"), "{txt}");
        let events: Vec<&str> = r["events"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(events.iter().any(|e| e.contains("Kernel-Power 41") && e.contains("(logged as")), "{txt}");
        assert!(events.iter().any(|e| e.contains("Kernel-General 1")), "{txt}");
        // EventLog 6008's 17:31:14 and the last log entry at 17:15:00 are older than the recorder's
        // 18:09:04: neither is presented as the stop
        let stop_hms = local_text(last, "%H:%M:%S");
        let e6008_line = events.iter().find(|e| e.contains("EventLog 6008")).expect(&txt);
        assert!(e6008_line.contains(&local_text(real((17, 31, 14)), "%H:%M:%S")), "{txt}");
        assert!(
            e6008_line.contains(&format!(
                "(stale: Tachyon's flight recorder shows activity until {stop_hms}, so it is not when the PC stopped.)"
            )),
            "{txt}"
        );
        let log_note = r["last_log_entry_note"].as_str().expect(&txt);
        assert!(log_note.contains(&stop_hms) && log_note.contains("not when the PC stopped"), "{txt}");
        assert!(r["stopped"].as_str().unwrap().contains(&stop_hms) && r["stopped"].as_str().unwrap().contains("flight recorder"), "{txt}");
        for k in ["stopped", "matched", "clock_note"] {
            let v = r[k].as_str().unwrap_or_default();
            assert!(
                !v.contains(&local_text(real((17, 31, 14)), "%H:%M:%S")) && !v.contains(&local_text(real((17, 15, 0)), "%H:%M:%S")),
                "{k}: {txt}"
            );
        }
        assert_eq!(r["before_the_stop"]["idle"], true, "{txt}");
        let d = &r["diagnosis"];
        assert_eq!(d["causes"][0]["suspect"], "power_cut", "{txt}");
        let power = d["causes"].as_array().unwrap().iter().find(|c| c["suspect"] == "power").map_or(0, |c| c["percent"].as_u64().unwrap());
        assert!(power <= 20, "{txt}");
        assert!(r["other_restarts"].as_array().unwrap().is_empty(), "the clean restart at 19:06 is not unexpected: {txt}");
        // without record numbers (older data): the stamps alone give the same stop
        for ev in f.events.iter_mut().chain(f.log.iter_mut()) {
            ev.record = 0;
        }
        let r =
            rpc(&mut f, 8, "tools/call", json!({ "name": "diagnose_crash", "arguments": { "around": at } }))["result"]["structuredContent"]
                .clone();
        assert_eq!(r["stopped_ms"], last, "{r:#}");
        assert_eq!(r["diagnosis"]["causes"][0]["suspect"], "power_cut", "{r:#}");
    }

    #[test]
    fn blackbox_reads_older_windows_and_the_previous_session() {
        let call = |f: &mut Fake, a: Value| rpc(f, 6, "tools/call", json!({ "name": "get_blackbox", "arguments": a }))["result"].clone();
        let (mut f, last) = oct26_fake();
        let r = call(&mut f, json!({ "session": "previous", "include_samples": true }))["structuredContent"].clone();
        assert_eq!(r["samples_in_window"], 300, "{}", r["window"]);
        assert_eq!(r["to_ms"], last);
        assert_eq!(r["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(r["summary"]["idle"], true);
        assert!(r["clock_limits"][0].as_str().unwrap().starts_with("reliability policy"));
        let since = local_text(last - 60_000, "%Y-%m-%d %H:%M:%S");
        let until = local_text(last, "%Y-%m-%d %H:%M:%S");
        let r = call(&mut f, json!({ "since": since, "until": until }))["structuredContent"].clone();
        assert!((59..=61).contains(&r["samples_in_window"].as_u64().unwrap()), "{r:#}");
        let r = call(&mut f, json!({ "until": until, "minutes": 2 }))["structuredContent"].clone();
        assert!((119..=121).contains(&r["samples_in_window"].as_u64().unwrap()));
        let r = call(&mut f, json!({ "since": last - 30_000 }))["structuredContent"].clone();
        assert!((30..=32).contains(&r["samples_in_window"].as_u64().unwrap()), "Unix ms, until = since + 60 min: {r:#}");
        // the default is still "the last minutes up to now"
        // (the fake's samples are dated 26 Oct 2026, so do not count them: on that evening they are "now")
        let r = call(&mut f, json!({}))["structuredContent"].clone();
        assert_eq!(r["minutes"], 5.0);
        let to = chrono::NaiveDateTime::parse_from_str(r["window"]["to"].as_str().unwrap(), "%Y-%m-%d %H:%M:%S").unwrap();
        let to = to.and_local_timezone(chrono::Local).earliest().unwrap().timestamp_millis();
        assert!((to - crate::telemetry::now_ms()).abs() < 60_000, "{r:#}");
        if r["samples_in_window"] == 0 {
            assert!(r["note"].as_str().unwrap().contains("sessions"));
        }
        for bad in [json!({ "since": "yesterday" }), json!({ "session": "old" }), json!({ "since": until, "until": since })] {
            assert_eq!(call(&mut f, bad.clone())["isError"], true, "{bad}");
        }
    }

    fn job(name: &str, start: i64, dur: f64) -> JobRecord {
        serde_json::from_value(json!({
            "kind": "render", "name": name, "key": history::job_key(history::JobKind::Render, name), "start_ms": start, "end_ms": start + (dur * 1000.0) as i64,
            "duration_s": dur, "avg_gpu": 90.0, "peak_gpu": 99, "avg_fps": null, "energy_wh": null, "energy_source": null, "peak_vram_mb": null,
            "peak_temp": null, "power_throttled_s": 0.0, "thermal_throttled_s": 0.0, "bottleneck": null
        }))
        .unwrap()
    }

    fn rpc(src: &mut Fake, id: i64, method: &str, params: Value) -> Value {
        handle(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }), src).unwrap()
    }

    #[test]
    fn handshake_and_tool_list() {
        let mut f = Fake { on: true, ..Default::default() };
        let r = rpc(
            &mut f,
            1,
            "initialize",
            json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "tachyon");
        let r = rpc(&mut f, 1, "initialize", json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(r["result"]["protocolVersion"], PROTOCOLS[0]);
        assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), &mut f).is_none());
        let r = rpc(&mut f, 2, "tools/list", json!({}));
        let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "get_status",
                "get_bottleneck",
                "get_vram_forecast",
                "get_processes",
                "get_blackbox",
                "get_history",
                "diagnose_crash",
                "preflight",
                "get_pcie_link",
                "get_process_vram",
                "get_job_reports",
                "compare_last_run"
            ]
        );
        assert!(r["result"]["tools"].as_array().unwrap().iter().all(|t| t["annotations"]["readOnlyHint"] == true));
        assert_eq!(rpc(&mut f, 3, "ping", json!({}))["result"], json!({}));
        assert_eq!(rpc(&mut f, 4, "resources/list", json!({}))["error"]["code"], -32601);
    }

    #[test]
    fn preflight_checks_a_named_job_or_the_running_one() {
        let mut jobs = vec![];
        for i in 0..3 {
            let mut j = job(&format!("demo4k try {i}"), i * 4_000_000, 1800.0);
            j.peak_vram_mb = Some(15000.0);
            j.profile.own_vram_mb = Some(15900.0 + i as f32 * 10.0);
            j.profile.hints.resolution = Some("4K".into());
            j.profile.hints.frames = Some(240);
            jobs.push(j);
        }
        let status = json!({
            "source": "tachyon",
            "reading": { "vram_mb": 1800.0, "vram_total_mb": 16376.0 },
            "vram_forecast": { "hogs": [{ "name": "chrome.exe", "category": "Browser", "vram_mb": 1230.0, "idle_secs": 300.0 }] },
            "render": null, "preflight": null,
        });
        let mut f = Fake { on: true, jobs, status: Some(status.clone()), ..Default::default() };
        let call = |f: &mut Fake, args: Value| rpc(f, 9, "tools/call", json!({ "name": "preflight", "arguments": args }))["result"].clone();
        // nothing running, no name
        let r = call(&mut f, json!({}));
        assert!(r["structuredContent"]["note"].as_str().unwrap().starts_with("No render job is running"));
        // before starting: named, resolution written any way
        let r = call(&mut f, json!({ "name": "demo4k try 3", "resolution": "3840x2160", "frames": 480 }))["structuredContent"].clone();
        let p = &r["preflight"];
        assert_eq!(p["fit"], "wont_fit", "{r:#}");
        assert_eq!(p["resolution"], "4K");
        assert_eq!(p["duration_s"], 3600.0);
        assert!(
            p["sentence"].as_str().unwrap().starts_with("At 4K this job needs ~15.5 GB of VRAM; 14.2 GB is free."),
            "{}",
            p["sentence"]
        );
        assert!(p["hog_note"].as_str().unwrap().starts_with("chrome.exe holds 1.2 GB"));
        // at 1080p there is no history
        let r = call(&mut f, json!({ "name": "demo4k", "resolution": "1080p" }))["structuredContent"].clone();
        assert_eq!(r["preflight"]["fit"], "unknown");
        // running: the published check is returned as is
        let mut s = status;
        s["render"] = json!({ "job": "demo4k try 3", "resolution": "4K" });
        s["preflight"] = json!({ "job": "demo4k try 3", "resolution": "4K", "fit": "tight", "sentence": "published" });
        f.status = Some(s);
        assert_eq!(call(&mut f, json!({}))["structuredContent"]["preflight"]["sentence"], "published");
        assert_eq!(call(&mut f, json!({ "name": "Demo4K try 3" }))["structuredContent"]["preflight"]["sentence"], "published");
        // asked about something else while it runs: computed, with the running job's hints when it is the same job
        let r = call(&mut f, json!({ "name": "demo4k", "frames": 240 }))["structuredContent"].clone();
        assert_eq!(r["preflight"]["resolution"], "4K");
        assert!(r["note"].as_str().unwrap().contains("already running"));
        // the switch
        f.on = false;
        assert_eq!(call(&mut f, json!({}))["isError"], true);
    }

    #[test]
    fn tools_answer_and_respect_the_switch() {
        let mut f = Fake {
            on: true,
            jobs: vec![job("Shot 12 try 1", 0, 1000.0), job("Other", 2_000_000, 50.0), job("Shot 12 try 2", 3_000_000, 1140.0)],
            ..Default::default()
        };
        let call =
            |f: &mut Fake, name: &str, args: Value| rpc(f, 9, "tools/call", json!({ "name": name, "arguments": args }))["result"].clone();
        let r = call(&mut f, "get_bottleneck", json!({}));
        assert_eq!(r["isError"], false);
        assert_eq!(r["structuredContent"]["bottleneck"]["kind"], "gpu_bound");
        assert!(r["content"][0]["text"].as_str().unwrap().contains("The GPU is the limit."));
        let r = call(&mut f, "get_vram_forecast", json!({}));
        assert_eq!(r["structuredContent"]["forecast"]["warning"], "At this rate VRAM will be full in ~3 min.");
        assert_eq!(call(&mut f, "get_processes", json!({}))["structuredContent"]["processes"][0]["name"], "python.exe");
        let r = call(&mut f, "get_blackbox", json!({ "minutes": 2, "include_samples": true }))["structuredContent"].clone();
        assert!((119..=121).contains(&r["samples_in_window"].as_u64().unwrap()), "{}", r["samples_in_window"]);
        assert_eq!(r["peaks"]["gpu_util"], 90);
        assert!(r["timeline"].as_array().unwrap().len() >= 2);
        assert_eq!(r["samples"].as_array().unwrap().len(), r["samples_in_window"].as_u64().unwrap() as usize);
        assert_eq!(call(&mut f, "get_history", json!({ "n": 2 }))["structuredContent"]["count"], 2);
        let r = call(&mut f, "compare_last_run", json!({ "name": "shot 12" }))["structuredContent"].clone();
        assert_eq!(r["comparison"], "14% slower than last time.");
        let r = call(&mut f, "compare_last_run", json!({}))["structuredContent"].clone();
        assert_eq!(r["comparison"], "14% slower than last time.", "latest job is Shot 12 try 2");
        assert_eq!(call(&mut f, "nope", json!({}))["isError"], true);
        f.on = false;
        let r = call(&mut f, "get_status", json!({}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("agent_api = true"));
    }

    #[test]
    fn diagnose_crash_from_the_event_log() {
        use crate::events::{KERNEL_POWER, SysEvent, WER_SYSTEM};
        let call = |f: &mut Fake, a: Value| rpc(f, 5, "tools/call", json!({ "name": "diagnose_crash", "arguments": a }))["result"].clone();
        let Some(Around::Day(d0, d1)) = parse_around("2026-10-22") else { panic!() };
        assert_eq!(d1 - d0, 86_400_000);
        let Some(Around::At(t)) = parse_around("2026-10-22 18:40") else { panic!() };
        assert_eq!(parse_around("2026-10-22T18:40:30"), Some(Around::At(t + 30_000)));
        let ev = |dt: i64, p: &str, id: u32, code: Option<&str>| SysEvent {
            t_ms: t + dt * 1000,
            provider: p.into(),
            id,
            level: 2,
            code: code.map(Into::into),
            record: 0,
        };
        let mut f = Fake {
            on: true,
            events: vec![
                ev(-86_400, "nvlddmkm", 153, None),   // a day earlier: outside the crash window
                ev(-9 * 3600, "nvlddmkm", 153, None), // hours before: inside it
                ev(-900, "nvlddmkm", 153, None),
                ev(-60, "nvlddmkm", 153, None),
                ev(90, KERNEL_POWER, 41, Some("0x0000001A")),
                ev(150, WER_SYSTEM, 1001, Some("0x0000001a")),
            ],
            ctx: crate::diagnose::Context {
                ram: Some(crate::sysinfo::RamSpeed {
                    kind: "DDR5".into(),
                    rated_mts: Some(4800),
                    configured_mts: Some(6000),
                    modules: 2,
                    part: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        for args in [json!({ "around": "2026-10-22" }), json!({ "around": "2026-10-22 18:40" })] {
            let r = call(&mut f, args)["structuredContent"].clone();
            assert_eq!(r["source"], "event_log");
            assert_eq!(r["diagnosis"]["causes"][0]["suspect"], "ram", "{r:#}");
            assert_eq!(r["events"].as_array().unwrap().len(), 5, "{r:#}");
            assert!(r["note"].as_str().unwrap().contains("Event Log only"));
        }
        assert!(call(&mut f, json!({ "around": "22/10" }))["isError"] == true);
        assert!(
            call(&mut f, json!({ "around": "2026-10-21" }))["structuredContent"]["note"].as_str().unwrap().contains("1 GPU driver event")
        );
    }

    #[test]
    fn diagnose_crash_from_the_report() {
        use crate::events::SysEvent;
        use crate::recorder::{PrevSession, Session};
        let call = |f: &mut Fake| rpc(f, 5, "tools/call", json!({ "name": "diagnose_crash", "arguments": {} }))["result"].clone();
        let mut f = Fake { on: true, ..Default::default() };
        assert!(call(&mut f)["structuredContent"]["note"].as_str().unwrap().starts_with("No unexpected restart in the Windows Event Log"));
        let t0 = 1_790_000_040_000i64;
        let prev = PrevSession {
            session: Session { version: "1.0.1".into(), pid: 1, file: "a".into(), started_ms: t0, clean: false, ended_ms: None },
            samples: (0..120)
                .map(|i| Sample {
                    t_ms: t0 + i * 1000,
                    util: Some(97),
                    temp: Some(66),
                    power_w: Some(264.0),
                    power_limit_w: Some(360.0),
                    ..Default::default()
                })
                .collect(),
        };
        let stop = t0 + 119_000;
        let ev = |dt: i64, p: &str, id: u32, code: Option<&str>| SysEvent {
            t_ms: stop + dt,
            provider: p.into(),
            id,
            level: 2,
            code: code.map(str::to_string),
            record: 0,
        };
        let events = vec![ev(-5000, "nvlddmkm", 153, None), ev(60_000, crate::events::KERNEL_POWER, 41, Some("0x0000001A"))];
        let mut r = report::build(&prev, Some(stop + 30_000), Ok(events), stop + 90_000).unwrap();
        assert!(r.diagnosis.is_some());
        f.report = Some(r.clone());
        let v = call(&mut f)["structuredContent"].clone();
        assert_eq!(v["diagnosis"]["causes"][0]["suspect"], "ram", "{v:#}");
        assert!(v["note"].is_null());
        assert!(v["last_seconds"].is_null(), "no fast readings");
        // 1.2: the 100 ms readings of the last seconds, with a +180 W step 300 ms before the stop
        let fast: Vec<crate::fast::FastSample> = (0..400)
            .map(|i| crate::fast::FastSample {
                t_ms: stop - 39_900 + i * 100,
                power_w: Some(if i >= 396 { 355.0 } else { 175.0 }),
                power_limit_w: Some(360.0),
                clk_sm: Some(2800),
                temp: Some(66),
                util: Some(97),
                throttle: Some(if i >= 397 { 0x80 } else { 0 }),
                pcie_gen: Some(5),
                pcie_width: Some(16),
                ..Default::default()
            })
            .collect();
        f.fast = vec![("previous".into(), fast)];
        let v = call(&mut f)["structuredContent"].clone();
        assert_eq!(v["last_seconds"]["readings"], 300, "{:#}", v["last_seconds"]);
        assert_eq!(v["last_seconds"]["biggest_power_jump"]["w"], 180.0);
        let hints = v["diagnosis"]["hints"].as_array().unwrap();
        assert!(hints.iter().any(|h| h.as_str().unwrap().contains("power spike of +180 W")), "{hints:?}");
        assert!(hints.iter().any(|h| h.as_str().unwrap().contains("power brake")), "{hints:?}");
        // a 1.0 report without a diagnosis: worked out from the report
        r.diagnosis = None;
        f.report = Some(r);
        let v = call(&mut f)["structuredContent"].clone();
        assert_eq!(v["diagnosis"]["causes"][0]["suspect"], "ram");
        assert!(v["note"].as_str().unwrap().contains("older report"));
    }

    #[test]
    fn pcie_process_vram_and_job_reports_read_the_history() {
        let d = std::env::temp_dir().join(format!("tachyon_mcp12_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let h = d.join("history");
        let now = crate::telemetry::now_ms();
        let call = |f: &mut Fake, name: &str, a: Value| {
            rpc(f, 11, "tools/call", json!({ "name": name, "arguments": a }))["result"]["structuredContent"].clone()
        };
        let mut f = Fake { on: true, ..Default::default() };
        assert!(call(&mut f, "get_job_reports", json!({}))["note"].as_str().unwrap().starts_with("No Tachyon data folder"));
        f.dir = Some(d.clone());
        // PCIe: an idle downshift and a degraded link under load
        let mut lt = crate::pcie::LinkTracker::default();
        let mut ev = vec![];
        ev.extend(lt.push(now - 60_000, Some(5), Some(16), Some(5), Some(16), Some(90)));
        ev.extend(lt.push(now - 50_000, Some(1), Some(16), Some(5), Some(16), Some(1)));
        for i in 0..10 {
            ev.extend(lt.push(now - 20_000 + i * 1000, Some(3), Some(16), Some(5), Some(16), Some(98)));
        }
        crate::pcie::append(&h, &ev).unwrap();
        f.status = Some(
            json!({ "source": "tachyon", "pcie_link": lt.status, "marked_jobs": { "open": [{ "job": "shot 13", "started": "x", "running_s": 5 }] } }),
        );
        let v = call(&mut f, "get_pcie_link", json!({}));
        assert_eq!(v["degraded_events"], 1, "{v:#}");
        assert_eq!(v["now"]["state"], "degraded_gen");
        assert!(v["summary"].as_str().unwrap().contains("degraded 1 time(s)"), "{v:#}");
        assert!(v["changes"].as_array().unwrap().iter().any(|c| c["state"] == "idle_saving"));
        // per-process VRAM
        let mut log = crate::procvram::VramLog::default();
        for (i, mb) in [(0, 2000.0), (1, 9000.0), (2, 9000.0), (3, 500.0)] {
            if let Some(p) = log.push(now - 600_000 + i * 120_000, vec![("python.exe".into(), 10, mb), ("dwm.exe".into(), 4, 300.0)]) {
                crate::procvram::append(&h, &p).unwrap();
            }
        }
        let v = call(&mut f, "get_process_vram", json!({ "minutes": 30 }));
        assert_eq!(v["history"]["top"][0]["name"], "python.exe", "{v:#}");
        assert_eq!(v["history"]["top"][0]["peak_mb"], 9000.0);
        let v = call(&mut f, "get_process_vram", json!({ "minutes": 30, "name": "dwm" }));
        assert_eq!(v["history"]["processes"], 1);
        assert!(call(&mut f, "get_process_vram", json!({ "since": "2026-10-26 18:00", "until": "2026-10-26 17:00" })).is_null());
        // marked jobs
        let r = crate::markers::JobReport {
            job: "shot 12".into(),
            start: "s".into(),
            end: "e".into(),
            start_ms: now - 120_000,
            end_ms: now - 60_000,
            duration_s: 60.0,
            ended_by: "end marker".into(),
            covered_s: 60.0,
            gpu_util_avg: Some(97.5),
            gpu_util_peak: Some(99),
            mem_ctrl_avg: None,
            vram_start_mb: None,
            vram_peak_mb: Some(12000.0),
            vram_end_mb: None,
            vram_total_mb: None,
            power_avg_w: Some(340.0),
            power_peak_w: Some(372.0),
            power_limit_w: Some(360.0),
            energy_wh: Some(5.8),
            cost: None,
            temp_avg_c: None,
            temp_peak_c: Some(70),
            clk_sm_min_mhz: None,
            clk_sm_max_mhz: None,
            power_limited_s: 12.0,
            thermal_limited_s: 0.0,
            at_power_limit_s: 3.0,
            clock_limit_reasons: vec![],
            pcie: json!({ "at_start": "Gen5 x16" }),
            top_vram_processes: vec![("python.exe".into(), 8900.0)],
            bottleneck_mostly: Some("GPU-bound".into()),
            sentence: "\"shot 12\" ran 1 min.".into(),
        };
        crate::markers::save_report(&h, &r).unwrap();
        let v = call(&mut f, "get_job_reports", json!({ "name": "SHOT" }));
        assert_eq!(v["count"], 1, "{v:#}");
        assert_eq!(v["reports"][0]["energy_wh"], 5.8);
        assert_eq!(v["open"][0]["job"], "shot 13");
        assert_eq!(call(&mut f, "get_job_reports", json!({ "name": "other" }))["count"], 0);
        // Tachyon closed: open jobs come from the marker file
        crate::markers::write_mark(&d, "start", "overnight", now - 10_000).unwrap();
        f.status = Some(json!({ "source": "standalone" }));
        let v = call(&mut f, "get_job_reports", json!({}));
        assert_eq!(v["open"][0]["job"], "overnight");
        assert!(v["note"].as_str().unwrap().contains("not running"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn serve_bytes(input: &[u8]) -> Vec<Value> {
        let mut f = Fake { on: true, jobs: vec![job("a", 1, 60.0)], ..Default::default() };
        let mut out = Vec::new();
        serve_io(std::io::Cursor::new(input.to_vec()), &mut out, &mut f);
        String::from_utf8(out).expect("replies are UTF-8").lines().map(|l| serde_json::from_str(l).expect("each reply is JSON")).collect()
    }

    #[test]
    fn hostile_input_gets_errors_and_the_server_keeps_going() {
        let ping = br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
        let mut input = Vec::new();
        input.extend_from_slice(b"\xff\xfe not utf-8\n");
        input.extend_from_slice(b"garbage {{{\n");
        input.extend_from_slice(&vec![b'x'; MAX_LINE + 100]);
        input.push(b'\n');
        input.extend_from_slice(format!("[{}]\n", vec![std::str::from_utf8(ping).unwrap(); MAX_BATCH + 1].join(",")).as_bytes());
        input.extend_from_slice(b"[]\n");
        input.extend_from_slice(&[b'['; 10_000]);
        input.push(b'\n');
        input.extend_from_slice(b"42\n\"str\"\nnull\n{}\n");
        input.extend_from_slice(ping);
        input.push(b'\n');
        let r = serve_bytes(&input);
        let codes: Vec<i64> = r.iter().map(|v| v["error"]["code"].as_i64().unwrap_or(0)).collect();
        assert_eq!(&codes[..6], &[-32700, -32700, -32600, -32600, -32600, -32700], "{r:?}");
        assert_eq!(r.last().unwrap()["id"], 7, "still answering after all that");
        assert_eq!(r.last().unwrap()["result"], json!({}));
    }

    #[test]
    fn random_bytes_never_panic_or_hang() {
        let mut rng = crate::fuzz::Rng::new(0x7ac4);
        let seeds: [&[u8]; 4] = [
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_history","arguments":{"n":5}}}"#,
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_blackbox","arguments":{"minutes":1e308}}}"#,
            br#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":null}}"#,
            br#"[{"jsonrpc":"2.0","id":4,"method":"tools/list"},{"id":null}]"#,
        ];
        for round in 0..400 {
            let mut input = Vec::new();
            for _ in 0..4 {
                let k = rng.below(seeds.len());
                input.extend(rng.mutate(seeds[k]));
                input.push(b'\n');
            }
            let t = std::time::Instant::now();
            let _ = serve_bytes(&input);
            assert!(t.elapsed().as_secs() < 5, "round {round} took too long");
        }
    }
}
