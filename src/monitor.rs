//! Sampler: polls NVML (and optionally render logs) when the UI timer fires and keeps 60 s of history.
use crate::config::Config;
use crate::nvml::{GpuSample, Nvml};
use crate::parse::Progress;
use crate::scan;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

pub const WINDOW_SECS: f64 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Util,
    Vram,
    Temp,
    Power,
    ClkGfx,
    ClkMem,
    Fan,
    Enc,
    Dec,
    PcieRx,
    PcieTx,
}
pub const METRICS: usize = 11;

#[derive(Debug, Clone, Default)]
pub struct Series {
    pub pts: VecDeque<(f64, f32)>,
}

impl Series {
    pub fn push(&mut self, t: f64, v: f32) {
        self.pts.push_back((t, v));
        while self.pts.front().is_some_and(|(t0, _)| t - t0 > WINDOW_SECS) {
            self.pts.pop_front();
        }
    }
    pub fn max(&self) -> f32 {
        self.pts.iter().map(|p| p.1).fold(0.0, f32::max)
    }
    pub fn last(&self) -> Option<f32> {
        self.pts.back().map(|p| p.1)
    }
}

#[derive(Debug, Clone, Default)]
pub struct History {
    pub s: [Series; METRICS],
    pub eased: [Option<f32>; METRICS],
}

impl History {
    pub fn get(&self, m: Metric) -> &Series {
        &self.s[m as usize]
    }
    pub fn eased(&self, m: Metric) -> Option<f32> {
        self.eased[m as usize]
    }
    /// Record a value. The eased value moves half-way per data tick (no animation frames).
    pub fn put(&mut self, m: Metric, t: f64, v: Option<f64>) {
        if let Some(v) = v {
            let v = v as f32;
            self.s[m as usize].push(t, v);
            let e = &mut self.eased[m as usize];
            *e = Some(match *e {
                Some(old) => old + (v - old) * 0.5,
                None => v,
            });
        }
    }
    pub fn record(&mut self, t: f64, g: &GpuSample) {
        let f = |x: Option<u32>| x.map(f64::from);
        self.put(Metric::Util, t, f(g.util_pct));
        self.put(Metric::Vram, t, g.vram_used_mb);
        self.put(Metric::Temp, t, f(g.temp_c));
        self.put(Metric::Power, t, g.power_w);
        self.put(Metric::ClkGfx, t, f(g.clk_gfx_mhz));
        self.put(Metric::ClkMem, t, f(g.clk_mem_mhz));
        self.put(Metric::Fan, t, f(g.fan_pct));
        self.put(Metric::Enc, t, f(g.enc_pct));
        self.put(Metric::Dec, t, f(g.dec_pct));
        self.put(Metric::PcieRx, t, g.pcie_rx_mbs);
        self.put(Metric::PcieTx, t, g.pcie_tx_mbs);
    }
}

#[derive(Debug, Clone, Default)]
pub struct RenderView {
    pub job: String,
    pub stage: Option<String>,
    /// File name only (never a full path).
    pub source_name: String,
    pub progress: Option<Progress>,
    pub fps: Option<f64>,
    pub job_elapsed: Option<f64>,
    pub job_est_total: Option<f64>,
}

/// A GPU process with its launcher chain and category.
#[derive(Debug, Clone)]
pub struct ProcView {
    pub info: crate::nvml::ProcInfo,
    /// Nearest parent first, e.g. ["cmd.exe", "explorer.exe"].
    pub chain: Vec<String>,
    pub category: String,
    pub color: (u8, u8, u8),
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub gpus: Vec<String>,
    pub driver: String,
    pub selected: u32,
    pub error: Option<String>,
    pub sample: Option<GpuSample>,
    pub procs: Option<Vec<ProcView>>,
    pub hist: History,
    pub render: Option<RenderView>,
    /// Seconds since the monitor started (for the HUD uptime).
    pub uptime: f64,
    pub ticks: u64,
    /// Milliseconds spent inside the last poll (diagnostics).
    pub poll_cost_ms: f64,
    /// Bottleneck, VRAM forecast, crash report and jobs.
    pub insights: crate::insight::Insights,
}

/// Render-panel rule: only when render roots are configured AND a log there was written
/// within `visible_secs`. Finished batches therefore disappear completely.
pub fn render_panel_visible(roots: &[String], newest_age_secs: Option<f64>, visible_secs: u64) -> bool {
    !roots.is_empty() && newest_age_secs.is_some_and(|a| a <= visible_secs as f64)
}

struct RenderState {
    cands: Vec<scan::Candidate>,
    last_scan: Option<Instant>,
    rate: scan::RateMeter,
}

fn render_tick(cfg: &Config, st: &mut RenderState, t: f64) -> Option<RenderView> {
    if cfg.render_roots.is_empty() {
        return None;
    }
    let now = Instant::now();
    if st.last_scan.is_none_or(|l| now - l >= Duration::from_secs(cfg.rescan_secs.max(1))) {
        st.cands = scan::discover(&cfg.render_roots, cfg.scan_depth);
        st.last_scan = Some(now);
    } else {
        for c in st.cands.iter_mut().take(20) {
            if let Ok(m) = std::fs::metadata(&c.path).and_then(|m| m.modified()) {
                c.mtime = m;
            }
        }
        st.cands.sort_by_key(|c| std::cmp::Reverse(c.mtime));
    }
    let newest = st.cands.first().map(|c| SystemTime::now().duration_since(c.mtime).unwrap_or_default().as_secs_f64());
    if !render_panel_visible(&cfg.render_roots, newest, cfg.render_visible_secs) {
        return None;
    }
    let j = scan::current_job(&st.cands, cfg);
    let key = format!("{}|{}", j.source, j.stage.as_deref().unwrap_or(""));
    let fps = j.progress.as_ref().and_then(|p| st.rate.push(&key, t, p.frame)).or(j.progress.as_ref().and_then(|p| p.fps));
    let source_name = std::path::Path::new(&j.source).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Some(RenderView {
        job: j.job,
        stage: j.stage,
        source_name,
        progress: j.progress,
        fps,
        job_elapsed: j.job_elapsed,
        job_est_total: j.job_est_total,
    })
}

/// Attach launcher chains and categories to NVML's process list.
pub fn attribute(
    procs: &[crate::nvml::ProcInfo],
    table: &crate::procinfo::ProcTable,
    rules: &[crate::config::ProcessRule],
    cmdlines: &HashMap<u32, Option<String>>,
) -> Vec<ProcView> {
    procs
        .iter()
        .map(|p| {
            let exe = table.get(&p.pid).map(|(_, n)| n.clone()).unwrap_or_else(|| p.name.clone());
            let chain = crate::procinfo::chain(p.pid, table, 4);
            let (category, color) = match crate::procinfo::categorize(rules, &exe, &chain) {
                Some(r) => (r.category.clone(), crate::procinfo::parse_color(&r.color)),
                None => ("Other".to_string(), (124, 139, 161)),
            };
            let mut info = p.clone();
            if info.name.starts_with("pid ") && !exe.starts_with("pid ") {
                info.name = exe;
            }
            ProcView { info, chain, category, color, cmdline: cmdlines.get(&p.pid).cloned().flatten() }
        })
        .collect()
}

/// GPU process list filter. Hidden by default: desktop shell processes (`hide` patterns) and
/// graphics-only processes doing nothing measurable. Shown: compute ("C") processes, anything
/// using more than 1% SM/ENC/DEC, and anything with a known VRAM figure.
/// Returns the visible processes and how many were hidden.
pub fn filter_procs<'a>(v: &'a [ProcView], show_all: bool, hide: &[String]) -> (Vec<&'a ProcView>, usize) {
    if show_all {
        return (v.iter().collect(), 0);
    }
    let keep = |p: &ProcView| {
        let shell = hide.iter().any(|h| crate::procinfo::glob_match(h, &p.info.name));
        if shell {
            return false;
        }
        let busy = [p.info.sm_pct, p.info.enc_pct, p.info.dec_pct].iter().any(|x| x.is_some_and(|x| x > 1));
        p.info.kind == "C" || busy || p.info.vram_mb.is_some_and(|m| m > 0.0)
    };
    let vis: Vec<&ProcView> = v.iter().filter(|p| keep(p)).collect();
    let hidden = v.len() - vis.len();
    (vis, hidden)
}

/// Island (compact capsule) state: idle shows the four GPU values; while a render is active
/// (render panel visible) it grows to also show the job and a progress ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Island {
    Idle,
    Rendering,
}

impl Island {
    pub fn from_render(render_visible: bool) -> Self {
        if render_visible { Island::Rendering } else { Island::Idle }
    }
    /// Logical size (96-DPI pixels).
    pub fn size(self) -> (f32, f32) {
        match self {
            Island::Idle => (410.0, 44.0),
            Island::Rendering => (650.0, 56.0),
        }
    }
}

/// Island top-left in screen pixels. The saved anchor is the centre x and top y, so the capsule
/// grows symmetrically; by default it sits top-centre of the work area, 10 px from the top.
/// The result is clamped so the whole capsule stays on that work area.
pub fn island_pos(work: (i32, i32, i32, i32), size_px: (i32, i32), cx: Option<f32>, y: Option<f32>) -> (i32, i32) {
    let (l, t, r, b) = work;
    let cx = cx.map(|v| v.round() as i32).unwrap_or((l + r) / 2);
    let y = y.map(|v| v.round() as i32).unwrap_or(t + 10);
    let x = (cx - size_px.0 / 2).clamp(l, (r - size_px.0).max(l));
    (x, y.clamp(t, (b - size_px.1).max(t)))
}

/// Top-left that puts a window of `size_px` back inside `work` when too little of it is left
/// on any screen (a monitor was unplugged or rearranged). `grab_px` of the window, measured
/// from its top-left, has to be on a screen to count as reachable; `visible` says whether it is.
/// Returns None when the window can stay where it is.
pub fn pull_back(at: (i32, i32), size_px: (i32, i32), visible: bool, work: (i32, i32, i32, i32)) -> Option<(i32, i32)> {
    if visible {
        return None;
    }
    let (l, t, r, b) = work;
    Some((at.0.clamp(l, (r - size_px.0).max(l)), at.1.clamp(t, (b - size_px.1).max(t))))
}

/// Synchronous sampler: call `tick` from a timer. No threads.
pub struct Sampler {
    cfg: Config,
    start: Instant,
    nvml: Option<Nvml>,
    last_try: Option<Instant>,
    /// When the driver last dropped the GPU (faster reconnect for a minute).
    lost_at: Option<Instant>,
    err: Option<String>,
    cur_sel: u32,
    rs: RenderState,
    ptable: crate::procinfo::ProcTable,
    #[cfg_attr(not(windows), allow(dead_code))]
    ptable_at: Option<Instant>,
    cmdlines: HashMap<u32, Option<String>>,
    pub snap: Snapshot,
    /// Selected GPU index (the UI writes this).
    pub selected: u32,
    pub engine: crate::insight::Engine,
    agent: Option<crate::agent::Publisher>,
    /// When the overclock indicators were last read (once a minute).
    oc_at: Option<(Instant, u32)>,
}

impl Sampler {
    pub fn new(cfg: Config) -> Self {
        let sel = cfg.gpu_index;
        let mut snap = Snapshot { selected: sel, ..Default::default() };
        let agent = cfg.agent_api.then(crate::agent::Publisher::new);
        let engine = crate::insight::Engine::new(&cfg, crate::config::data_dir(), crate::telemetry::now_ms(), &mut snap.insights);
        Self {
            cfg,
            start: Instant::now(),
            nvml: None,
            last_try: None,
            lost_at: None,
            err: None,
            cur_sel: u32::MAX,
            rs: RenderState { cands: vec![], last_scan: None, rate: Default::default() },
            ptable: Default::default(),
            ptable_at: None,
            cmdlines: HashMap::new(),
            agent,
            snap,
            selected: sel,
            engine,
            oc_at: None,
        }
    }

    /// Settings changed while running (the Settings page). Render roots are rescanned on the next
    /// tick, the agent API starts or stops publishing, and the engine takes what it can live.
    pub fn apply(&mut self, cfg: Config) {
        if cfg.render_roots != self.cfg.render_roots || cfg.scan_depth != self.cfg.scan_depth {
            self.rs = RenderState { cands: vec![], last_scan: None, rate: Default::default() };
        }
        match (cfg.agent_api, self.agent.is_some()) {
            (true, false) => self.agent = Some(crate::agent::Publisher::new()),
            (false, true) => self.agent = None,
            _ => {}
        }
        if let Some(n) = self.nvml.as_mut() {
            n.pcie = cfg.pcie;
        }
        self.engine.apply(&cfg);
        self.cfg = cfg;
    }

    /// The agent API is publishing (`agent_api = true` and the shared memory exists).
    pub fn agent_live(&self) -> bool {
        self.agent.as_ref().is_some_and(|a| a.live())
    }

    /// Clean exit: record a running job and mark the flight recorder session clean.
    pub fn close(&mut self) {
        self.engine.close(crate::telemetry::now_ms(), &mut self.snap.insights);
    }

    pub fn tick(&mut self) {
        let t0 = Instant::now();
        let t = (t0 - self.start).as_secs_f64();
        // after a driver reset retry every 2 s for a minute, otherwise every 10 s
        let every = if self.lost_at.is_some_and(|l| t0 - l < Duration::from_secs(60)) { 2 } else { 10 };
        if self.nvml.is_none() && self.last_try.is_none_or(|l| t0 - l >= Duration::from_secs(every)) {
            self.last_try = Some(t0);
            match Nvml::open() {
                Ok(mut n) => {
                    n.pcie = self.cfg.pcie;
                    self.nvml = Some(n);
                    self.err = None;
                }
                Err(e) => {
                    self.err = Some(if every == 2 {
                        "The NVIDIA driver restarted or the GPU stopped answering. Reconnecting…".into()
                    } else {
                        crate::nvml::friendly(&e)
                    })
                }
            }
        }
        let mut sel = self.selected;
        let mut sample = None;
        if let Some(n) = self.nvml.as_mut() {
            if sel as usize >= n.names.len() {
                sel = 0;
            }
            if sel != self.cur_sel {
                n.reset_cache();
            }
            let g = n.sample(sel);
            if n.lost {
                // driver reset (TDR), driver update or GPU lost: drop this NVML session and reopen
                self.nvml = None;
                self.lost_at = Some(t0);
                self.last_try = Some(t0);
                self.err = Some("The NVIDIA driver restarted or the GPU stopped answering. Reconnecting…".into());
            } else {
                self.lost_at = self.lost_at.filter(|l| t0 - *l < Duration::from_secs(60));
                sample = Some(g);
                // overclock indicators for the crash diagnosis: a few cheap queries once a minute
                if self.oc_at.is_none_or(|(a, s)| s != sel || t0 - a >= Duration::from_secs(60)) {
                    self.oc_at = Some((t0, sel));
                    let oc = n.oc_info(sel);
                    self.engine.set_oc(oc);
                }
            }
        }
        // process attribution: Toolhelp snapshot only when there are GPU processes, every N s
        let mut procs = None;
        if let Some(list) = sample.as_ref().and_then(|g| g.procs.as_ref()) {
            #[cfg(windows)]
            {
                let due = self.ptable_at.is_none_or(|a| t0 - a >= Duration::from_secs(self.cfg.process_refresh_secs.clamp(1, 60)));
                let unknown = list.iter().any(|p| !self.ptable.contains_key(&p.pid));
                if due || unknown {
                    self.ptable = crate::procinfo::win::snapshot();
                    self.ptable_at = Some(t0);
                    self.cmdlines.retain(|pid, _| list.iter().any(|p| p.pid == *pid));
                }
                if self.cfg.process_cmdline {
                    for p in list {
                        self.cmdlines.entry(p.pid).or_insert_with(|| crate::procinfo::win::cmdline(p.pid));
                    }
                }
            }
            procs = Some(attribute(list, &self.ptable, &self.cfg.process_rules, &self.cmdlines));
        }
        let render = render_tick(&self.cfg, &mut self.rs, t);
        let s = &mut self.snap;
        if sel != self.cur_sel {
            s.hist = History::default();
            self.cur_sel = sel;
        }
        if let Some(n) = &self.nvml {
            s.gpus = n.names.clone();
            s.driver = n.driver.clone();
        }
        s.selected = sel;
        self.selected = sel;
        s.error = if self.nvml.is_none() { Some(self.err.clone().unwrap_or_else(|| "NVML unavailable".into())) } else { None };
        if let Some(g) = &sample {
            s.hist.record(t, g);
        }
        s.sample = sample;
        s.procs = procs;
        s.render = render;
        let job = s.render.as_ref().map(|r| (r.job.as_str(), r.fps));
        self.engine.tick(s.sample.as_ref(), s.procs.as_deref(), job, crate::telemetry::now_ms(), &mut s.insights);
        s.uptime = t;
        s.ticks += 1;
        if let Some(a) = self.agent.as_mut() {
            a.tick(s);
        }
        s.poll_cost_ms = t0.elapsed().as_secs_f64() * 1000.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_panel_rule() {
        let roots = vec!["X:\\renders".to_string()];
        assert!(!render_panel_visible(&[], Some(1.0), 120), "no roots = never");
        assert!(!render_panel_visible(&roots, None, 120), "no logs = hidden");
        assert!(render_panel_visible(&roots, Some(119.0), 120));
        assert!(render_panel_visible(&roots, Some(120.0), 120));
        assert!(!render_panel_visible(&roots, Some(121.0), 120), "finished batch = hidden");
        assert!(!render_panel_visible(&roots, Some(3600.0 * 5.0), 120));
    }
    #[test]
    fn attribution_names_chain_and_category() {
        use crate::nvml::ProcInfo;
        let table: crate::procinfo::ProcTable = [
            (10u32, (0u32, "explorer.exe".to_string())),
            (20, (10, "cmd.exe".into())),
            (30, (20, "python.exe".into())),
            (40, (10, "steam.exe".into())),
            (50, (40, "Game.exe".into())),
        ]
        .into_iter()
        .collect();
        let mk =
            |pid, name: &str| ProcInfo { pid, name: name.into(), vram_mb: None, sm_pct: Some(50), enc_pct: None, dec_pct: None, kind: "C" };
        let v = attribute(
            &[mk(30, "python.exe"), mk(50, "pid 50"), mk(99, "pid 99")],
            &table,
            &crate::config::default_process_rules(),
            &HashMap::new(),
        );
        assert_eq!(v[0].chain, vec!["cmd.exe", "explorer.exe"]);
        assert_eq!(v[0].category, "Script/Python");
        assert_eq!((v[1].info.name.as_str(), v[1].category.as_str()), ("Game.exe", "Game"));
        assert_eq!(v[2].category, "Other");
        assert!(v[2].chain.is_empty());
    }

    fn pv(name: &str, kind: &'static str, vram: Option<f64>, sm: Option<u32>, enc: Option<u32>) -> ProcView {
        ProcView {
            info: crate::nvml::ProcInfo { pid: 1, name: name.into(), vram_mb: vram, sm_pct: sm, enc_pct: enc, dec_pct: None, kind },
            chain: vec![],
            category: "Other".into(),
            color: (0, 0, 0),
            cmdline: None,
        }
    }

    #[test]
    fn process_filter_hides_idle_desktop_graphics() {
        let hide = crate::config::default_process_hide();
        let v = vec![
            pv("dwm.exe", "C+G", None, Some(2), None),    // shell: hidden even at 2% SM
            pv("ShellHost.exe", "C+G", None, None, None), // shell
            pv("CrossDeviceResume.exe", "C+G", None, Some(0), Some(0)),
            pv("Some App.exe", "C+G", None, Some(1), None),   // 1% is not > 1%: hidden
            pv("Grok Bot.exe", "C+G", None, Some(2), None),   // busy: shown
            pv("python.exe", "C", None, None, None),          // compute: always shown
            pv("Game.exe", "G", Some(6144.0), Some(0), None), // known VRAM: shown
            pv("obs64.exe", "G", None, Some(0), Some(30)),    // encoder: shown
        ];
        let (vis, hidden) = filter_procs(&v, false, &hide);
        let names: Vec<&str> = vis.iter().map(|p| p.info.name.as_str()).collect();
        assert_eq!(names, ["Grok Bot.exe", "python.exe", "Game.exe", "obs64.exe"]);
        assert_eq!(hidden, 4);
        let (all, h) = filter_procs(&v, true, &hide);
        assert_eq!((all.len(), h), (8, 0));
    }

    #[test]
    fn island_state_rule_and_position() {
        assert_eq!(Island::from_render(false), Island::Idle);
        assert_eq!(Island::from_render(true), Island::Rendering);
        let (iw, ih) = Island::Idle.size();
        let (rw, rh) = Island::Rendering.size();
        assert!(rw > iw && rh > ih, "rendering island is bigger");
        // default: top-centre of the work area
        assert_eq!(island_pos((0, 0, 2560, 1400), (380, 44), None, None), (1090, 10));
        // remembered centre: growing keeps the centre
        assert_eq!(island_pos((0, 0, 2560, 1400), (560, 56), Some(1280.0), Some(10.0)), (1000, 10));
        // clamped to the screen
        assert_eq!(island_pos((0, 0, 1920, 1040), (560, 56), Some(1900.0), Some(5000.0)), (1360, 984));
        // second monitor to the left (negative coordinates)
        assert_eq!(island_pos((-1920, 0, 0, 1040), (380, 44), None, None), (-1150, 10));
    }

    #[test]
    fn windows_left_on_an_unplugged_monitor_come_back() {
        // still on a screen: stays put, even partly off the edge
        assert_eq!(pull_back((1800, 100), (460, 800), true, (0, 0, 1920, 1040)), None);
        // the left monitor went away: the panel lands on the edge of the main one
        assert_eq!(pull_back((-1500, 200), (460, 800), false, (0, 0, 1920, 1040)), Some((0, 200)));
        // lower than the remaining screen: lifted until it fits
        assert_eq!(pull_back((300, 2400), (460, 800), false, (0, 0, 1920, 1040)), Some((300, 240)));
        // bigger than the screen: the top-left stays reachable
        assert_eq!(pull_back((5000, 5000), (2400, 1600), false, (0, 0, 1920, 1040)), Some((0, 0)));
    }

    #[test]
    fn series_keeps_60s_and_eases_on_ticks_only() {
        let mut h = History::default();
        for i in 0..100 {
            h.put(Metric::Util, i as f64, Some(i as f64));
        }
        let s = h.get(Metric::Util);
        assert_eq!(s.pts.front().unwrap().0, 39.0);
        assert_eq!(s.pts.len(), 61);
        let mut h = History::default();
        h.put(Metric::Temp, 0.0, Some(40.0));
        h.put(Metric::Temp, 1.0, Some(80.0));
        assert_eq!(h.eased(Metric::Temp), Some(60.0));
        h.put(Metric::Temp, 2.0, None);
        assert_eq!(h.eased(Metric::Temp), Some(60.0));
    }
}
