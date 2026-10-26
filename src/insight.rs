//! The analysis side of a tick: one telemetry sample feeds the bottleneck explainer, the VRAM
//! forecast, the flight recorder and the job history. All of it runs on the sampler's own tick
//! (no extra timers or threads) and only does work proportional to one sample.
use crate::config::Config;
use crate::explain::{Explainer, Verdict};
use crate::forecast::{Forecast, Forecaster};
use crate::history::{JobHints, JobRecord, JobTracker, LiveJob};
use crate::preflight::{LiveVram, Preflight, Query};

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
/// Past jobs the pre-render check looks at.
const PAST_JOBS: usize = 500;
/// The check is redone every this many ticks during the job's first `PREFLIGHT_TICKS`.
const PREFLIGHT_EVERY: u64 = 30;
const PREFLIGHT_TICKS: u64 = 900;

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
    /// Pre-render check for the running render job ("will this job fit?").
    pub preflight: Option<Preflight>,
    /// The check says out of memory with enough confidence and Hex has not said so for this job.
    pub preflight_new: bool,
    /// The PCIe link now, against the best the GPU and slot can do (1.2).
    pub pcie: Option<crate::pcie::LinkStatus>,
    /// The 100 ms sampler (1.2): None when it is off.
    pub fast: Option<FastStatus>,
    /// Render jobs marked with `tachyon mark` (1.2): open now, the last finished, a counter.
    pub marked_open: Vec<crate::markers::OpenJob>,
    pub marked_last: Option<crate::markers::JobReport>,
    pub marked_done: u64,
    pub marker_notes: Vec<String>,
    /// The latest per-process VRAM reading (1.2): (name, pid, MB), largest first.
    pub proc_vram: Vec<(String, u32, f32)>,
    /// Where those numbers come from ("windows counters" or "nvml").
    pub proc_vram_source: Option<&'static str>,
}

/// The 100 ms sampler's state, for the status.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct FastStatus {
    pub period_ms: u64,
    pub readings: u64,
    pub late: u64,
    pub minutes_kept: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<crate::fast::FastSample>,
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
    /// What the render logs say about the running job, the job history for the pre-render check
    /// (read once, then kept up to date), and which job the check is for.
    hints: Option<JobHints>,
    past: Option<Vec<JobRecord>>,
    pf_key: Option<String>,
    pf_since: u64,
    pf_told: Option<String>,
    /// 1.2: the 100 ms sampler thread, the PCIe link, render job markers, per-process VRAM.
    fast: Option<crate::fast::FastSampler>,
    fast_minutes: u32,
    link: crate::pcie::LinkTracker,
    gpu: Option<u32>,
    marks: Option<crate::markers::MarkerTracker>,
    vlog: crate::procvram::VramLog,
    // Held open while running so a second Tachyon does not share the recorder files.
    _lock: Option<std::fs::File>,
}

/// The fast sampler's own NVML session (opened on its thread).
pub fn fast_open() -> Option<Box<dyn crate::fast::FastSource>> {
    crate::nvml::Nvml::open().ok().map(|mut n| {
        n.pcie = false;
        Box::new(n) as Box<dyn crate::fast::FastSource>
    })
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
            hints: None,
            past: None,
            pf_key: None,
            pf_since: 0,
            pf_told: None,
            fast: None,
            fast_minutes: cfg.fast_minutes,
            link: Default::default(),
            gpu: None,
            marks: None,
            vlog: Default::default(),
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
        if cfg.fast_sampling
            && let Some(r) = e.recorder.as_ref()
        {
            match crate::fast::FastSampler::start(&bb, r.which(), cfg.fast_interval_ms(), cfg.fast_minutes, cfg.gpu_index, fast_open) {
                Ok(f) => e.fast = Some(f),
                Err(err) => out.note = Some(format!("Fast sampling is off: {err}.")),
            }
        }
        e.marks = Some(crate::markers::MarkerTracker::open(&dir, now_ms));
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

    /// The GPU the sampler shows (the fast sampler and the link history follow it).
    pub fn set_gpu(&mut self, idx: u32) {
        if self.gpu != Some(idx) {
            self.gpu = Some(idx);
            self.link = Default::default();
            if let Some(f) = self.fast.as_ref() {
                f.set_gpu(idx);
            }
        }
    }

    /// A per-process VRAM reading, (name, pid, MB): logged when it changed, kept for the
    /// marked jobs and the status.
    pub fn set_proc_vram(&mut self, now_ms: i64, list: Vec<(String, u32, f32)>, source: &'static str, out: &mut Insights) {
        if let Some(m) = self.marks.as_mut() {
            m.procs(&list);
        }
        let top = crate::procvram::top(list.clone());
        if let Some(p) = self.vlog.push(now_ms, list)
            && self.history
            && let Some(d) = self.history_dir()
        {
            let _ = crate::procvram::append(&d, &p);
        }
        out.proc_vram = top;
        out.proc_vram_source = Some(source);
    }

    /// What the render logs say about the running job (project, resolution, frames).
    pub fn set_hints(&mut self, h: Option<JobHints>) {
        self.hints = h;
    }

    /// Past jobs, oldest first (read from disk the first time).
    fn past(&mut self) -> &[JobRecord] {
        if self.past.is_none() {
            self.past = Some(self.history_dir().map(|d| crate::history::recent(&d, PAST_JOBS)).unwrap_or_default());
        }
        self.past.as_deref().unwrap_or_default()
    }

    /// The pre-render check for an ad-hoc query (the agent API), against the card now.
    pub fn preflight_for(&mut self, q: &Query, live: &LiveVram) -> Preflight {
        crate::preflight::predict(q, self.past(), live)
    }

    /// Re-check the running render job: when it starts (or its hints change), then now and then
    /// during its first minutes while its memory use settles.
    fn preflight_tick(&mut self, s: &Sample, render: Option<(&str, Option<f64>)>, forecast: &Forecast, out: &mut Insights) {
        let Some((name, _)) = render.filter(|_| self.history) else {
            self.pf_key = None;
            out.preflight = None;
            out.preflight_new = false;
            return;
        };
        let h = self.hints.clone().unwrap_or_default();
        let key = format!("{}|{}", crate::history::job_key(crate::history::JobKind::Render, name), h.resolution.as_deref().unwrap_or(""));
        let fresh = self.pf_key.as_deref() != Some(key.as_str());
        if fresh {
            self.pf_key = Some(key.clone());
            self.pf_since = self.ticks;
        }
        let age = self.ticks - self.pf_since;
        if !fresh && (!age.is_multiple_of(PREFLIGHT_EVERY) || age > PREFLIGHT_TICKS) {
            return;
        }
        let q = Query { name: name.to_string(), project: h.project, resolution: h.resolution, frames: h.frames };
        let live =
            LiveVram { total_mb: s.vram_total_mb, used_mb: s.vram_mb, job_now_mb: self.jobs.live_own_vram(), hogs: forecast.hogs.clone() };
        let p = crate::preflight::predict(&q, self.past(), &live);
        if p.alarming() && self.pf_told.as_deref() != Some(key.as_str()) {
            out.preflight_new = true;
        }
        out.preflight = Some(p);
    }

    /// Hex has told the user about the pre-render check for this job.
    pub fn mark_preflight_told(&mut self, out: &mut Insights) {
        out.preflight_new = false;
        self.pf_told = self.pf_key.clone();
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
        let (fast, latest) = self.fast.as_ref().map(|f| f.take()).unwrap_or_default();
        if let Some(f) = self.fast.as_ref() {
            let sh = f.shared.lock().map(|x| (x.total, x.late, x.note.clone())).unwrap_or_default();
            out.fast = Some(FastStatus {
                period_ms: f.period_ms,
                readings: sh.0,
                late: sh.1,
                minutes_kept: self.fast_minutes,
                note: sh.2,
                latest,
            });
        }
        if let Some(g) = g {
            let ev = self.link.push(now_ms, g.pcie_gen, g.pcie_width, g.pcie_gen_max, g.pcie_width_max, s.util);
            if !ev.is_empty()
                && self.history
                && let Some(d) = self.history_dir()
            {
                let _ = crate::pcie::append(&d, &ev);
            }
            out.pcie = Some(self.link.status.clone()).filter(|l| l.state.is_some());
        }
        if let Some(m) = self.marks.as_mut() {
            let kind = out.bottleneck.as_ref().map(|v| v.kind);
            let done = m.tick(&s, &fast, out.pcie.as_ref(), kind, self.jobs.price.as_ref());
            out.marked_open = m.open_jobs(now_ms);
            out.marker_notes = m.notes.clone();
            for r in done {
                if let Some(d) = self.history_dir() {
                    let _ = crate::markers::save_report(&d, &r);
                }
                out.marked_last = Some(r);
                out.marked_done += 1;
            }
        }
        out.forecast = if g.is_some() { self.forecaster.push(&s, 2).clone() } else { Forecast::default() };
        if let Some(r) = self.recorder.as_mut() {
            r.push(&s, now_ms);
        }
        if self.history && g.is_some() {
            let kind = out.bottleneck.as_ref().map(|v| v.kind);
            let hints = if render.is_some() { self.hints.clone() } else { None };
            if let Some(job) = self.jobs.push_with(&s, render, kind, hints.as_ref()) {
                self.finish_job(job, out);
            }
            out.live_job = self.jobs.live();
        } else {
            out.live_job = None;
        }
        if g.is_some() || render.is_none() {
            let f = out.forecast.clone();
            self.preflight_tick(&s, render, &f, out);
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
        if let Some(p) = self.past.as_mut() {
            p.push(job.clone());
            if p.len() > PAST_JOBS {
                p.remove(0);
            }
        }
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
            firmware_boot_ms: crate::sysinfo::firmware_boot_ms(),
            // the first start since the stop: the RAM speed and BIOS time read now are from right after it
            just_after: true,
            clock_shift_min: None,
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
        // the fast thread flushes its ring and stops first
        if let Some(mut f) = self.fast.take() {
            f.stop();
        }
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
    fn preflight_learns_from_finished_jobs_and_warns_once() {
        let dir = std::env::temp_dir().join(format!("tachyon_engine_pf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = Config::default();
        let t0 = crate::telemetry::now_ms() - 3_600_000;
        let mut out = Insights::default();
        let mut e = Engine::new(&cfg, Some(dir.clone()), t0, &mut out);
        let hints = JobHints { project: Some("ladial".into()), resolution: Some("4K".into()), frames: None };
        let mut t = t0;
        // three 4K runs that each fill the card from 1.5 GB to 16.2 GB
        for run in 0..4 {
            let name = format!("demo4k try {run}");
            e.set_hints(Some(hints.clone()));
            for i in 0..120 {
                t += 1000;
                let vram = 1500.0 + (i as f64 * 200.0).min(14_700.0);
                e.tick(Some(&gpu(97, vram)), None, Some((name.as_str(), None)), t, &mut out);
                let p = out.preflight.as_ref().expect("a check while a render runs");
                if i == 0 {
                    assert_eq!(p.matches, run, "{p:#?}");
                    match run {
                        0 => assert_eq!(p.fit, crate::preflight::Fit::Unknown),
                        3 => {
                            assert_eq!(p.fit, crate::preflight::Fit::WontFit, "{p:#?}");
                            assert!(out.preflight_new);
                            e.mark_preflight_told(&mut out);
                        }
                        _ => {}
                    }
                }
            }
            if run == 3 {
                assert!(!out.preflight_new, "told once per job");
            }
        }
        e.set_hints(None);
        e.tick(Some(&gpu(5, 1500.0)), None, None, t + 1000, &mut out);
        assert!(out.preflight.is_none() && !out.preflight_new);
        // an ad-hoc query (the agent API)
        let live = LiveVram { total_mb: Some(16303.0), used_mb: Some(1500.0), ..Default::default() };
        let p = e.preflight_for(&Query { name: "demo4k".into(), resolution: Some("1080p".into()), ..Default::default() }, &live);
        assert_eq!((p.fit, p.matches), (crate::preflight::Fit::Unknown, 0), "other resolution: {p:#?}");
        let _ = std::fs::remove_dir_all(&dir);
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
