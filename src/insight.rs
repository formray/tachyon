//! The analysis side of a tick: one telemetry sample feeds the bottleneck explainer, the VRAM
//! forecast, the flight recorder and the job history. All of it runs on the sampler's own tick
//! (no extra timers or threads) and only does work proportional to one sample.
use crate::config::Config;
use crate::explain::{Explainer, Verdict};
use crate::forecast::{Forecast, Forecaster};
use crate::history::{JobRecord, JobTracker, LiveJob};
use crate::monitor::ProcView;
use crate::nvml::GpuSample;
use crate::recorder::{PrevSession, Recorder};
use crate::report::Report;
use crate::sysinfo::SysMeter;
use crate::telemetry::Sample;
use std::path::PathBuf;

/// Processes kept per sample for the analysis (the recorder stores the top three).
pub const KEEP_PROCS: usize = 8;
/// The crash report is built on this tick, after the first paint.
const REPORT_TICK: u64 = 3;

/// What the UI and the agent API show.
#[derive(Debug, Clone, Default)]
pub struct Insights {
    pub latest: Option<Sample>,
    pub bottleneck: Option<Verdict>,
    pub forecast: Forecast,
    /// The latest crash report (also an older one, for the panel).
    pub report: Option<std::sync::Arc<Report>>,
    /// The report is new and Hex has not told the user yet.
    pub report_new: bool,
    pub live_job: Option<LiveJob>,
    /// The job that finished most recently in this session, with a counter to spot new ones.
    pub last_job: Option<JobRecord>,
    pub jobs_done: u64,
    /// Why the recorder or history is off, in plain words.
    pub note: Option<String>,
    pub data_dir: Option<PathBuf>,
}

pub struct Engine {
    dir: Option<PathBuf>,
    history: bool,
    sys: SysMeter,
    explainer: Explainer,
    forecaster: Forecaster,
    recorder: Option<Recorder>,
    prev: Option<PrevSession>,
    jobs: JobTracker,
    ticks: u64,
    // Held open while running so a second Tachyon does not share the recorder files.
    _lock: Option<std::fs::File>,
}

#[cfg(windows)]
fn lock(dir: &std::path::Path) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create(true).truncate(false).share_mode(0).open(dir.join("tachyon.lock")).ok()
}

#[cfg(not(windows))]
fn lock(dir: &std::path::Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(dir.join("tachyon.lock")).ok()
}

impl Engine {
    pub fn new(cfg: &Config, dir: Option<PathBuf>, now_ms: i64, out: &mut Insights) -> Self {
        let mut e = Engine {
            dir: dir.clone(),
            history: cfg.history,
            sys: SysMeter::default(),
            explainer: Explainer::default(),
            forecaster: Forecaster::default(),
            recorder: None,
            prev: None,
            jobs: JobTracker::with_price(cfg.price()),
            ticks: 0,
            _lock: None,
        };
        out.data_dir = dir.clone();
        let Some(dir) = dir else {
            out.note = Some("No data folder (LOCALAPPDATA is not set), so the flight recorder and history are off.".into());
            e.history = false;
            return e;
        };
        if std::fs::create_dir_all(&dir).is_err() {
            out.note = Some(format!("Could not create {}, so the flight recorder and history are off.", dir.display()));
            e.history = false;
            return e;
        }
        e._lock = lock(&dir);
        if e._lock.is_none() {
            out.note = Some("Another Tachyon is already recording, so this one does not.".into());
            e.history = false;
            return e;
        }
        let bb = dir.join("blackbox");
        if cfg.blackbox {
            match Recorder::open(&bb, cfg.blackbox_minutes, cfg.poll_interval().as_millis() as u64, now_ms) {
                Ok((r, prev)) => {
                    e.recorder = Some(r);
                    e.prev = prev.filter(|p| !p.session.clean);
                }
                Err(err) => out.note = Some(format!("The flight recorder is off: {err}.")),
            }
        }
        out.report = crate::report::load(&bb).map(std::sync::Arc::new);
        out.report_new = out.report.as_ref().is_some_and(|r| !r.announced);
        e
    }

    pub fn blackbox_dir(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("blackbox"))
    }

    pub fn history_dir(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("history"))
    }

    pub fn tick(
        &mut self,
        g: Option<&GpuSample>,
        procs: Option<&[ProcView]>,
        render: Option<(&str, Option<f64>)>,
        now_ms: i64,
        out: &mut Insights,
    ) {
        self.ticks += 1;
        let (cpu, mem) = self.sys.sample(2);
        let s = Sample::from_tick(now_ms, g, procs, cpu, mem, KEEP_PROCS);
        out.bottleneck = if g.is_some() { self.explainer.push(&s).cloned() } else { None };
        out.forecast = if g.is_some() { self.forecaster.push(&s, 2).clone() } else { Forecast::default() };
        if let Some(r) = self.recorder.as_mut() {
            r.push(&s, now_ms);
        }
        if self.history && g.is_some() {
            let kind = out.bottleneck.as_ref().map(|v| v.kind);
            if let Some(job) = self.jobs.push(&s, render, kind) {
                self.finish_job(job, out);
            }
            out.live_job = self.jobs.live();
        }
        if self.ticks == REPORT_TICK
            && let Some(prev) = self.prev.take()
        {
            self.build_report(&prev, now_ms, out);
        }
        out.latest = Some(s);
    }

    fn finish_job(&mut self, job: JobRecord, out: &mut Insights) {
        let job = match self.history_dir() {
            Some(d) => crate::history::record(&d, job.clone()).unwrap_or(job),
            None => job,
        };
        out.last_job = Some(job);
        out.jobs_done += 1;
    }

    fn build_report(&mut self, prev: &PrevSession, now_ms: i64, out: &mut Insights) {
        let from = prev.samples.first().map_or(prev.session.started_ms, |s| s.t_ms) - 60_000;
        let events = crate::events::query(from, now_ms, 200);
        let Some(r) = crate::report::build(prev, crate::sysinfo::boot_time_ms(), events, now_ms) else { return };
        if let Some(bb) = self.blackbox_dir() {
            let _ = crate::report::save(&bb, &r);
        }
        out.report = Some(std::sync::Arc::new(r));
        out.report_new = true;
    }

    /// Hex has told the user about the report.
    pub fn mark_announced(&mut self, out: &mut Insights) {
        out.report_new = false;
        if let Some(bb) = self.blackbox_dir() {
            crate::report::mark_announced(&bb);
        }
        if let Some(r) = out.report.as_mut() {
            std::sync::Arc::make_mut(r).announced = true;
        }
    }

    /// A fresh bottleneck verdict without hysteresis ("Why is it slow?").
    pub fn why_now(&mut self) -> Option<Verdict> {
        self.explainer.now()
    }

    /// The recording so far (for export and the agent API).
    pub fn blackbox_window(&mut self, now_ms: i64) -> Vec<Sample> {
        self.recorder.as_mut().map(|r| r.window(now_ms)).unwrap_or_default()
    }

    /// Export the recording (and the report, if any) as CSV and JSON into the exports folder.
    pub fn export(&mut self, now_ms: i64, out: &Insights) -> Result<(PathBuf, PathBuf), String> {
        let dir = self.dir.as_ref().ok_or("no data folder")?.join("exports");
        let samples = self.blackbox_window(now_ms);
        crate::report::export(&dir, out.report.as_deref(), &samples, now_ms).map_err(|e| e.to_string())
    }

    /// Clean exit: record a running job and mark the session clean.
    pub fn close(&mut self, now_ms: i64, out: &mut Insights) {
        if self.history
            && let Some(job) = self.jobs.end()
        {
            self.finish_job(job, out);
        }
        if let Some(r) = self.recorder.as_mut() {
            r.close(now_ms);
        }
        self.recorder = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(util: u32, vram: f64) -> GpuSample {
        GpuSample {
            util_pct: Some(util),
            temp_c: Some(65),
            power_w: Some(250.0),
            power_limit_w: Some(360.0),
            vram_used_mb: Some(vram),
            vram_total_mb: Some(16303.0),
            throttle: Some(0),
            ..Default::default()
        }
    }

    #[test]
    fn crash_cycle_report_then_clean_close() {
        let dir = std::env::temp_dir().join(format!("tachyon_engine_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = Config::default();
        let t0 = 1_790_000_000_000i64;
        // Session 1 runs for two minutes and is never closed (a crash).
        {
            let mut out = Insights::default();
            let mut e = Engine::new(&cfg, Some(dir.clone()), t0, &mut out);
            assert!(out.note.is_none(), "{:?}", out.note);
            for i in 0..120 {
                e.tick(Some(&gpu(97, 15000.0)), None, None, t0 + i * 1000, &mut out);
            }
            assert!(out.bottleneck.is_some());
            assert!(out.live_job.is_some(), "a busy minute starts a GPU session");
            // a crash: the lock goes with the process; drop it without close()
        }
        // Session 2: the report appears on the third tick.
        let t1 = t0 + 600_000;
        let mut out = Insights::default();
        let mut e = Engine::new(&cfg, Some(dir.clone()), t1, &mut out);
        assert!(out.report.is_none());
        for i in 0..3 {
            e.tick(Some(&gpu(5, 2000.0)), None, None, t1 + i * 1000, &mut out);
        }
        let r = out.report.clone().expect("report for the unclean session");
        assert!(out.report_new);
        assert_eq!(r.samples, 116, "written every 5 s: the last few seconds are lost, as in a real crash");
        assert!(r.just_before.contains("GPU at 97%"), "{}", r.just_before);
        e.mark_announced(&mut out);
        assert!(!out.report_new);
        let (csv, _json) = e.export(t1 + 4000, &out).unwrap();
        assert!(csv.starts_with(dir.join("exports")));
        e.close(t1 + 5000, &mut out);
        drop(e);
        // Session 3 after a clean exit: no new report, the old one is kept and already announced.
        let mut out = Insights::default();
        let mut e = Engine::new(&cfg, Some(dir.clone()), t1 + 60_000, &mut out);
        for i in 0..3 {
            e.tick(Some(&gpu(5, 2000.0)), None, None, t1 + 60_000 + i * 1000, &mut out);
        }
        assert!(out.report.as_ref().is_some_and(|r| r.announced));
        assert!(!out.report_new);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_data_dir_degrades_quietly() {
        let mut out = Insights::default();
        let mut e = Engine::new(&Config::default(), None, 0, &mut out);
        e.tick(None, None, None, 1000, &mut out);
        assert!(out.note.is_some() && out.report.is_none() && out.bottleneck.is_none());
    }
}
