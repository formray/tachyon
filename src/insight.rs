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
    /// Overclock indicators: the latest reading, and the one the previous session saved.
    oc_now: Option<crate::nvml::OcInfo>,
    prev_oc: Option<crate::nvml::OcInfo>,
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
            oc_now: None,
            prev_oc: None,
            _lock: None,
        };
        e.forecaster.set_horizon_minutes(cfg.vram_forecast_minutes);
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
        e.prev_oc = load_oc(&bb);
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

    /// Settings changed while running: what can follow at once does (history on/off, energy price,
    /// VRAM forecast horizon). The flight recorder keeps its files until the next start.
    pub fn apply(&mut self, cfg: &Config) {
        self.history = cfg.history && self.dir.is_some() && self._lock.is_some();
        self.jobs.price = cfg.price();
        self.forecaster.set_horizon_minutes(cfg.vram_forecast_minutes);
    }

    /// The GPU's overclock indicators (read now and then by the sampler). Saved next to the
    /// flight recorder when they change, so a crash report after a restart knows the settings
    /// from before the crash.
    pub fn set_oc(&mut self, oc: crate::nvml::OcInfo) {
        if self.oc_now.as_ref() == Some(&oc) {
            return;
        }
        if self.recorder.is_some()
            && let Some(bb) = self.blackbox_dir()
        {
            let _ = save_oc(&bb, &oc);
        }
        self.oc_now = Some(oc);
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
        } else {
            out.live_job = None;
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
        let ctx = crate::diagnose::Context {
            oc: self.prev_oc.clone().or_else(|| self.oc_now.clone()),
            oc_after: self.prev_oc.is_none() && self.oc_now.is_some(),
            ram: crate::sysinfo::ram_speed(),
        };
        let Some(mut r) = crate::report::build_with(prev, crate::sysinfo::boot_time_ms(), events, now_ms, &ctx) else { return };
        // Hex announces unexpected restarts; a Tachyon-only stop just gets the panel banner
        r.announced = r.cause != crate::report::Cause::Restart;
        if let Some(bb) = self.blackbox_dir() {
            let _ = crate::report::save(&bb, &r);
        }
        out.report_new = !r.announced;
        out.report = Some(std::sync::Arc::new(r));
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

const OC_FILE: &str = "gpu_settings.json";

fn save_oc(dir: &std::path::Path, oc: &crate::nvml::OcInfo) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("gpu_settings.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(oc).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, dir.join(OC_FILE))
}

fn load_oc(dir: &std::path::Path) -> Option<crate::nvml::OcInfo> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(dir.join(OC_FILE)).ok()?.take(64 << 10).read_to_end(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
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
        let t0 = crate::telemetry::now_ms() - 2 * 3_600_000;
        // Session 1 runs for two minutes and is never closed (a crash).
        {
            let mut out = Insights::default();
            let mut e = Engine::new(&cfg, Some(dir.clone()), t0, &mut out);
            assert!(out.note.is_none(), "{:?}", out.note);
            let oc = crate::nvml::OcInfo { mem_offset_mhz: Some(1000), ..Default::default() };
            e.set_oc(oc.clone());
            e.set_oc(oc);
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
        assert_eq!(e.prev_oc.as_ref().and_then(|o| o.mem_offset_mhz), Some(1000), "settings from before the crash");
        e.set_oc(crate::nvml::OcInfo::default());
        for i in 0..3 {
            e.tick(Some(&gpu(5, 2000.0)), None, None, t1 + i * 1000, &mut out);
        }
        let r = out.report.clone().expect("report for the unclean session");
        // only a restart of this machine since the session is announced by Hex
        assert_eq!(out.report_new, r.cause == crate::report::Cause::Restart);
        assert_eq!(r.announced, !out.report_new);
        assert_eq!(r.samples, 116, "written every 5 s: the last few seconds are lost, as in a real crash");
        assert!(r.just_before.contains("GPU at 97%"), "{}", r.just_before);
        if let Some(d) = &r.diagnosis {
            // a restart with no event log here: the memory offset from before the crash is evidence
            assert!(d.causes.iter().any(|c| c.evidence.iter().any(|e| e.contains("memory clock offset +1000 MHz"))), "{d:#?}");
        }

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
