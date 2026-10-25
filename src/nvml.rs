//! Minimal NVML binding, loaded dynamically at run time (no link-time dependency).
//! Only query functions are used: nothing is ever set on the GPU or on any process.
use libloading::Library;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};

type Ret = c_int;
type Dev = *mut c_void;

const OK: Ret = 0;
const NOT_SUPPORTED: Ret = 3;
const INSUFFICIENT_SIZE: Ret = 7;
const FUNCTION_NOT_FOUND: Ret = 13;
const NOT_AVAILABLE: c_ulonglong = u64::MAX;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Util {
    gpu: c_uint,
    memory: c_uint,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Mem {
    total: c_ulonglong,
    free: c_ulonglong,
    used: c_ulonglong,
}
/// nvmlProcessInfo_t (v2/v3 layout).
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ProcV2 {
    pid: c_uint,
    used: c_ulonglong,
    gi: c_uint,
    ci: c_uint,
}
/// nvmlProcessInfo_v1_t.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ProcV1 {
    pid: c_uint,
    used: c_ulonglong,
}
/// nvmlProcessUtilizationSample_t.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ProcUtil {
    pid: c_uint,
    ts: c_ulonglong,
    sm: c_uint,
    mem: c_uint,
    enc: c_uint,
    dec: c_uint,
}

type FnV = unsafe extern "C" fn() -> Ret;
type FnCount = unsafe extern "C" fn(*mut c_uint) -> Ret;
type FnHandle = unsafe extern "C" fn(c_uint, *mut Dev) -> Ret;
type FnStr = unsafe extern "C" fn(*mut c_char, c_uint) -> Ret;
type FnDevStr = unsafe extern "C" fn(Dev, *mut c_char, c_uint) -> Ret;
type FnUtil = unsafe extern "C" fn(Dev, *mut Util) -> Ret;
type FnMem = unsafe extern "C" fn(Dev, *mut Mem) -> Ret;
type FnU32 = unsafe extern "C" fn(Dev, *mut c_uint) -> Ret;
type FnArgU32 = unsafe extern "C" fn(Dev, c_uint, *mut c_uint) -> Ret;
type FnU64 = unsafe extern "C" fn(Dev, *mut c_ulonglong) -> Ret;
type FnCodec = unsafe extern "C" fn(Dev, *mut c_uint, *mut c_uint) -> Ret;
type FnProcV2 = unsafe extern "C" fn(Dev, *mut c_uint, *mut ProcV2) -> Ret;
type FnProcV1 = unsafe extern "C" fn(Dev, *mut c_uint, *mut ProcV1) -> Ret;
type FnProcUtil = unsafe extern "C" fn(Dev, *mut ProcUtil, *mut c_uint, c_ulonglong) -> Ret;
type FnPidName = unsafe extern "C" fn(c_uint, *mut c_char, c_uint) -> Ret;

#[derive(Default)]
struct Api {
    shutdown: Option<FnV>,
    count: Option<FnCount>,
    handle: Option<FnHandle>,
    driver: Option<FnStr>,
    name: Option<FnDevStr>,
    util: Option<FnUtil>,
    mem: Option<FnMem>,
    temp: Option<FnArgU32>,
    power: Option<FnU32>,
    power_limit: Option<FnU32>,
    clock: Option<FnArgU32>,
    max_clock: Option<FnArgU32>,
    fan: Option<FnU32>,
    enc: Option<FnCodec>,
    dec: Option<FnCodec>,
    pcie: Option<FnArgU32>,
    reasons: Option<FnU64>,
    gfx_procs: Option<FnProcV2>,
    cmp_procs: Option<FnProcV2>,
    gfx_procs_v1: Option<FnProcV1>,
    cmp_procs_v1: Option<FnProcV1>,
    proc_util: Option<FnProcUtil>,
    pid_name: Option<FnPidName>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    /// None when the driver does not report per-process memory (common under WDDM).
    pub vram_mb: Option<f64>,
    pub sm_pct: Option<u32>,
    pub enc_pct: Option<u32>,
    pub dec_pct: Option<u32>,
    /// "G" graphics, "C" compute, "C+G" both.
    pub kind: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct GpuSample {
    pub util_pct: Option<u32>,
    pub mem_ctrl_pct: Option<u32>,
    pub temp_c: Option<u32>,
    pub vram_used_mb: Option<f64>,
    pub vram_total_mb: Option<f64>,
    pub power_w: Option<f64>,
    pub power_limit_w: Option<f64>,
    pub clk_gfx_mhz: Option<u32>,
    pub clk_mem_mhz: Option<u32>,
    pub clk_gfx_max_mhz: Option<u32>,
    pub clk_mem_max_mhz: Option<u32>,
    pub fan_pct: Option<u32>,
    pub enc_pct: Option<u32>,
    pub dec_pct: Option<u32>,
    pub pcie_rx_mbs: Option<f64>,
    pub pcie_tx_mbs: Option<f64>,
    pub throttle: Option<u64>,
    /// None = process listing not supported.
    pub procs: Option<Vec<ProcInfo>>,
}

pub struct Nvml {
    _lib: Library,
    api: Api,
    pub driver: String,
    pub names: Vec<String>,
    handles: Vec<Dev>,
    /// Calls that returned NOT_SUPPORTED, per device index, so they are not retried every tick.
    unsupported: HashMap<u32, HashSet<&'static str>>,
    pid_names: HashMap<u32, String>,
    last_proc_ts: u64,
    /// Query PCIe throughput (costs ~2 x 20 ms of NVML sampling per tick).
    pub pcie: bool,
    pcie_phase: bool,
    pcie_last: (Option<f64>, Option<f64>),
}

const NVML_TEMPERATURE_GPU: c_uint = 0;
const CLOCK_GRAPHICS: c_uint = 0;
const CLOCK_MEM: c_uint = 2;
const PCIE_TX: c_uint = 0;
const PCIE_RX: c_uint = 1;

fn candidates() -> Vec<String> {
    if cfg!(windows) {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        vec![format!(r"{sys}\System32\nvml.dll"), r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll".into()]
    } else {
        vec!["libnvidia-ml.so.1".into(), "libnvidia-ml.so".into()]
    }
}

fn cstr(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Throttle / clock-event reason bits in plain words.
pub fn throttle_words(mask: u64) -> Vec<&'static str> {
    const R: [(u64, &str); 9] = [
        (0x1, "idle"),
        (0x2, "app clock limit"),
        (0x4, "power cap"),
        (0x8, "hardware slowdown"),
        (0x10, "sync boost"),
        (0x20, "thermal (driver)"),
        (0x40, "thermal (hardware)"),
        (0x80, "power brake"),
        (0x100, "display clock"),
    ];
    R.iter().filter(|(b, _)| mask & b != 0).map(|(_, w)| *w).collect()
}

/// Strip a path from a process name: only the executable's file name is ever shown.
pub fn exe_name(raw: &str) -> String {
    raw.rsplit(['\\', '/']).next().unwrap_or(raw).trim().to_string()
}

impl Nvml {
    pub fn open() -> Result<Self, String> {
        let mut last = String::from("nvml not found");
        for path in candidates() {
            // SAFETY: loading the vendor's NVML system library.
            match unsafe { Library::new(&path) } {
                Ok(lib) => return Self::init(lib),
                Err(e) => last = format!("{e}"),
            }
        }
        Err(last)
    }

    fn init(lib: Library) -> Result<Self, String> {
        macro_rules! sym {
            ($($n:literal),+) => {{
                let mut f = None;
                $( if f.is_none() { f = unsafe { lib.get::<_>(concat!($n, "\0").as_bytes()).ok().map(|s: libloading::Symbol<_>| *s) }; } )+
                f
            }};
        }
        let api = Api {
            shutdown: sym!("nvmlShutdown"),
            count: sym!("nvmlDeviceGetCount_v2", "nvmlDeviceGetCount"),
            handle: sym!("nvmlDeviceGetHandleByIndex_v2", "nvmlDeviceGetHandleByIndex"),
            driver: sym!("nvmlSystemGetDriverVersion"),
            name: sym!("nvmlDeviceGetName"),
            util: sym!("nvmlDeviceGetUtilizationRates"),
            mem: sym!("nvmlDeviceGetMemoryInfo"),
            temp: sym!("nvmlDeviceGetTemperature"),
            power: sym!("nvmlDeviceGetPowerUsage"),
            power_limit: sym!("nvmlDeviceGetEnforcedPowerLimit"),
            clock: sym!("nvmlDeviceGetClockInfo"),
            max_clock: sym!("nvmlDeviceGetMaxClockInfo"),
            fan: sym!("nvmlDeviceGetFanSpeed"),
            enc: sym!("nvmlDeviceGetEncoderUtilization"),
            dec: sym!("nvmlDeviceGetDecoderUtilization"),
            pcie: sym!("nvmlDeviceGetPcieThroughput"),
            reasons: sym!("nvmlDeviceGetCurrentClocksEventReasons", "nvmlDeviceGetCurrentClocksThrottleReasons"),
            gfx_procs: sym!("nvmlDeviceGetGraphicsRunningProcesses_v3", "nvmlDeviceGetGraphicsRunningProcesses_v2"),
            cmp_procs: sym!("nvmlDeviceGetComputeRunningProcesses_v3", "nvmlDeviceGetComputeRunningProcesses_v2"),
            gfx_procs_v1: sym!("nvmlDeviceGetGraphicsRunningProcesses"),
            cmp_procs_v1: sym!("nvmlDeviceGetComputeRunningProcesses"),
            proc_util: sym!("nvmlDeviceGetProcessUtilization"),
            pid_name: sym!("nvmlSystemGetProcessName"),
        };
        let init: FnV = sym!("nvmlInit_v2", "nvmlInit").ok_or("nvmlInit missing")?;
        let r = unsafe { init() };
        if r != OK {
            return Err(format!("nvmlInit = {r}"));
        }
        let mut me = Self {
            _lib: lib,
            api,
            driver: String::new(),
            names: vec![],
            handles: vec![],
            unsupported: HashMap::new(),
            pid_names: HashMap::new(),
            last_proc_ts: 0,
            pcie: true,
            pcie_phase: false,
            pcie_last: (None, None),
        };
        unsafe {
            if let Some(f) = me.api.driver {
                let mut b = [0 as c_char; 96];
                if f(b.as_mut_ptr(), b.len() as c_uint) == OK {
                    me.driver = cstr(&b);
                }
            }
            let mut n: c_uint = 0;
            if let Some(f) = me.api.count {
                if f(&mut n) != OK {
                    n = 0;
                }
            }
            let h = me.api.handle.ok_or("no device handle fn")?;
            for i in 0..n.max(1) {
                let mut d: Dev = std::ptr::null_mut();
                if h(i, &mut d) != OK || d.is_null() {
                    continue;
                }
                let mut name = format!("GPU {i}");
                if let Some(f) = me.api.name {
                    let mut b = [0 as c_char; 96];
                    if f(d, b.as_mut_ptr(), b.len() as c_uint) == OK {
                        name = cstr(&b);
                    }
                }
                me.handles.push(d);
                me.names.push(name);
            }
        }
        if me.handles.is_empty() {
            return Err("no NVIDIA GPU found".into());
        }
        Ok(me)
    }

    /// Forget cached per-device values (call when switching GPU).
    pub fn reset_cache(&mut self) {
        self.pcie_last = (None, None);
        self.last_proc_ts = 0;
    }

    fn try_call(&mut self, idx: u32, key: &'static str, f: impl FnOnce() -> Ret) -> bool {
        if self.unsupported.get(&idx).is_some_and(|s| s.contains(key)) {
            return false;
        }
        let r = f();
        if r == NOT_SUPPORTED || r == FUNCTION_NOT_FOUND {
            self.unsupported.entry(idx).or_default().insert(key);
        }
        r == OK
    }

    /// One sample of every metric for device `idx`; unsupported metrics are None.
    pub fn sample(&mut self, idx: u32) -> GpuSample {
        let mut s = GpuSample::default();
        let Some(&d) = self.handles.get(idx as usize) else { return s };
        let a = std::mem::take(&mut self.api);
        unsafe {
            if let Some(f) = a.util {
                let mut u = Util::default();
                if self.try_call(idx, "util", || f(d, &mut u)) {
                    s.util_pct = Some(u.gpu);
                    s.mem_ctrl_pct = Some(u.memory);
                }
            }
            if let Some(f) = a.mem {
                let mut m = Mem::default();
                if self.try_call(idx, "mem", || f(d, &mut m)) {
                    s.vram_used_mb = Some(m.used as f64 / 1048576.0);
                    s.vram_total_mb = Some(m.total as f64 / 1048576.0);
                }
            }
            if let Some(f) = a.temp {
                let mut t = 0;
                if self.try_call(idx, "temp", || f(d, NVML_TEMPERATURE_GPU, &mut t)) {
                    s.temp_c = Some(t);
                }
            }
            if let Some(f) = a.power {
                let mut p = 0;
                if self.try_call(idx, "power", || f(d, &mut p)) {
                    s.power_w = Some(p as f64 / 1000.0);
                }
            }
            if let Some(f) = a.power_limit {
                let mut p = 0;
                if self.try_call(idx, "plim", || f(d, &mut p)) {
                    s.power_limit_w = Some(p as f64 / 1000.0);
                }
            }
            if let Some(f) = a.clock {
                let mut c = 0;
                if self.try_call(idx, "clkg", || f(d, CLOCK_GRAPHICS, &mut c)) {
                    s.clk_gfx_mhz = Some(c);
                }
                let mut c = 0;
                if self.try_call(idx, "clkm", || f(d, CLOCK_MEM, &mut c)) {
                    s.clk_mem_mhz = Some(c);
                }
            }
            if let Some(f) = a.max_clock {
                let mut c = 0;
                if self.try_call(idx, "maxg", || f(d, CLOCK_GRAPHICS, &mut c)) {
                    s.clk_gfx_max_mhz = Some(c);
                }
                let mut c = 0;
                if self.try_call(idx, "maxm", || f(d, CLOCK_MEM, &mut c)) {
                    s.clk_mem_max_mhz = Some(c);
                }
            }
            if let Some(f) = a.fan {
                let mut p = 0;
                if self.try_call(idx, "fan", || f(d, &mut p)) {
                    s.fan_pct = Some(p);
                }
            }
            if let Some(f) = a.enc {
                let (mut u, mut p) = (0, 0);
                if self.try_call(idx, "enc", || f(d, &mut u, &mut p)) {
                    s.enc_pct = Some(u);
                }
            }
            if let Some(f) = a.dec {
                let (mut u, mut p) = (0, 0);
                if self.try_call(idx, "dec", || f(d, &mut u, &mut p)) {
                    s.dec_pct = Some(u);
                }
            }
            if let Some(f) = a.pcie.filter(|_| self.pcie) {
                // NVML samples PCIe over ~20 ms per call (wall time, not CPU); can be disabled with pcie = false.
                // One direction per tick (alternating) so a tick never blocks for more than ~20 ms.
                self.pcie_phase = !self.pcie_phase;
                let (key, ctr) = if self.pcie_phase { ("pcrx", PCIE_RX) } else { ("pctx", PCIE_TX) };
                let mut v = 0;
                if self.try_call(idx, key, || f(d, ctr, &mut v)) {
                    let mbs = v as f64 / 1024.0;
                    if self.pcie_phase { self.pcie_last.0 = Some(mbs) } else { self.pcie_last.1 = Some(mbs) }
                }
                s.pcie_rx_mbs = self.pcie_last.0;
                s.pcie_tx_mbs = self.pcie_last.1;
            }
            if let Some(f) = a.reasons {
                let mut m = 0;
                if self.try_call(idx, "thr", || f(d, &mut m)) {
                    s.throttle = Some(m);
                }
            }
        }
        s.procs = self.processes(&a, idx, d);
        self.api = a;
        s
    }

    fn list_v2(&mut self, idx: u32, key: &'static str, f: FnProcV2, d: Dev) -> Option<Vec<(u32, Option<f64>)>> {
        let mut buf = vec![ProcV2::default(); 64];
        let mut n = buf.len() as c_uint;
        let mut r = unsafe { f(d, &mut n, buf.as_mut_ptr()) };
        if r == INSUFFICIENT_SIZE {
            buf = vec![ProcV2::default(); n as usize + 16];
            n = buf.len() as c_uint;
            r = unsafe { f(d, &mut n, buf.as_mut_ptr()) };
        }
        if r == NOT_SUPPORTED {
            self.unsupported.entry(idx).or_default().insert(key);
        }
        if r != OK {
            return None;
        }
        Some(buf[..n as usize].iter().map(|p| (p.pid, (p.used != NOT_AVAILABLE).then(|| p.used as f64 / 1048576.0))).collect())
    }

    fn list_v1(&mut self, idx: u32, key: &'static str, f: FnProcV1, d: Dev) -> Option<Vec<(u32, Option<f64>)>> {
        let mut buf = vec![ProcV1::default(); 96];
        let mut n = buf.len() as c_uint;
        let r = unsafe { f(d, &mut n, buf.as_mut_ptr()) };
        if r == NOT_SUPPORTED {
            self.unsupported.entry(idx).or_default().insert(key);
        }
        if r != OK {
            return None;
        }
        Some(buf[..n as usize].iter().map(|p| (p.pid, (p.used != NOT_AVAILABLE).then(|| p.used as f64 / 1048576.0))).collect())
    }

    fn processes(&mut self, a: &Api, idx: u32, d: Dev) -> Option<Vec<ProcInfo>> {
        let skip = |me: &Self, k| me.unsupported.get(&idx).is_some_and(|s| s.contains(k));
        let g = match (a.gfx_procs, a.gfx_procs_v1) {
            (Some(f), _) if !skip(self, "gp") => self.list_v2(idx, "gp", f, d),
            (_, Some(f)) if !skip(self, "gp1") => self.list_v1(idx, "gp1", f, d),
            _ => None,
        };
        let c = match (a.cmp_procs, a.cmp_procs_v1) {
            (Some(f), _) if !skip(self, "cp") => self.list_v2(idx, "cp", f, d),
            (_, Some(f)) if !skip(self, "cp1") => self.list_v1(idx, "cp1", f, d),
            _ => None,
        };
        if g.is_none() && c.is_none() {
            return None;
        }
        let mut map: Vec<ProcInfo> = Vec::new();
        for (list, kind) in [(g.unwrap_or_default(), "G"), (c.unwrap_or_default(), "C")] {
            for (pid, mb) in list {
                if let Some(p) = map.iter_mut().find(|p| p.pid == pid) {
                    p.kind = "C+G";
                    p.vram_mb = match (p.vram_mb, mb) {
                        (Some(x), Some(y)) => Some(x.max(y)),
                        (x, y) => x.or(y),
                    };
                } else {
                    map.push(ProcInfo { pid, name: String::new(), vram_mb: mb, sm_pct: None, enc_pct: None, dec_pct: None, kind });
                }
            }
        }
        // per-process SM utilisation since the previous tick
        if let Some(f) = a.proc_util.filter(|_| !skip(self, "pu")) {
            let mut n: c_uint = 0;
            let r = unsafe { f(d, std::ptr::null_mut(), &mut n, self.last_proc_ts) };
            if r == NOT_SUPPORTED {
                self.unsupported.entry(idx).or_default().insert("pu");
            }
            if (r == OK || r == INSUFFICIENT_SIZE) && n > 0 {
                let mut buf = vec![ProcUtil::default(); n as usize + 8];
                let mut n2 = buf.len() as c_uint;
                if unsafe { f(d, buf.as_mut_ptr(), &mut n2, self.last_proc_ts) } == OK {
                    for u in &buf[..n2 as usize] {
                        self.last_proc_ts = self.last_proc_ts.max(u.ts);
                        if let Some(p) = map.iter_mut().find(|p| p.pid == u.pid) {
                            p.sm_pct = Some(p.sm_pct.unwrap_or(0).max(u.sm));
                            p.enc_pct = Some(p.enc_pct.unwrap_or(0).max(u.enc));
                            p.dec_pct = Some(p.dec_pct.unwrap_or(0).max(u.dec));
                        }
                    }
                }
            }
        }
        self.pid_names.retain(|pid, _| map.iter().any(|p| p.pid == *pid));
        for p in map.iter_mut() {
            if let Some(n) = self.pid_names.get(&p.pid) {
                p.name = n.clone();
                continue;
            }
            let mut name = String::new();
            if let Some(f) = a.pid_name {
                let mut b = [0 as c_char; 260];
                if unsafe { f(p.pid, b.as_mut_ptr(), b.len() as c_uint) } == OK {
                    name = exe_name(&cstr(&b));
                }
            }
            if name.is_empty() {
                name = format!("pid {}", p.pid);
            }
            self.pid_names.insert(p.pid, name.clone());
            p.name = name;
        }
        map.sort_by(|x, y| {
            y.vram_mb.unwrap_or(-1.0).total_cmp(&x.vram_mb.unwrap_or(-1.0)).then(y.sm_pct.unwrap_or(0).cmp(&x.sm_pct.unwrap_or(0)))
        });
        Some(map)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        if let Some(f) = self.api.shutdown {
            unsafe {
                f();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn throttle_reason_words() {
        assert!(throttle_words(0).is_empty());
        assert_eq!(throttle_words(0x1), vec!["idle"]);
        assert_eq!(throttle_words(0x4 | 0x20), vec!["power cap", "thermal (driver)"]);
        assert_eq!(throttle_words(0x40 | 0x80), vec!["thermal (hardware)", "power brake"]);
    }
    #[test]
    fn process_names_never_show_paths() {
        assert_eq!(exe_name(r"C:\Users\someone\Games\game.exe"), "game.exe");
        assert_eq!(exe_name("/usr/bin/python3"), "python3");
        assert_eq!(exe_name("dwm.exe"), "dwm.exe");
    }
    #[test]
    fn ffi_layouts_match_nvml_headers() {
        assert_eq!(std::mem::size_of::<ProcV2>(), 24);
        assert_eq!(std::mem::size_of::<ProcV1>(), 16);
        assert_eq!(std::mem::size_of::<ProcUtil>(), 32);
        assert_eq!(std::mem::size_of::<Mem>(), 24);
    }
}
