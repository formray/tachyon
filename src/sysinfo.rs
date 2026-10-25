//! System load next to the GPU: total CPU, the busiest core, RAM and commit charge.
//! Read-only and cheap: one NtQuerySystemInformation call per tick for the cores and
//! GetPerformanceInfo every few seconds for memory (it walks the process list).
//! Elsewhere (Linux build box) the same values come from /proc, so the logic is testable.

/// CPU load over the last interval.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CpuLoad {
    /// All logical cores, 0-100.
    pub total: f32,
    /// The busiest logical core, 0-100.
    pub max_core: f32,
    pub cores: u16,
}

/// Memory in MB.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MemInfo {
    pub ram_used_mb: f32,
    pub ram_total_mb: f32,
    pub commit_mb: f32,
    pub commit_limit_mb: f32,
}

/// Per-core (busy, total) time counters, any unit.
pub type CoreTimes = Vec<(u64, u64)>;

/// Load between two readings of the per-core counters. None if the core count changed
/// or no time passed.
pub fn cpu_load(prev: &[(u64, u64)], cur: &[(u64, u64)]) -> Option<CpuLoad> {
    if prev.len() != cur.len() || cur.is_empty() {
        return None;
    }
    let (mut busy, mut total, mut max_core) = (0u64, 0u64, 0f32);
    for ((pb, pt), (cb, ct)) in prev.iter().zip(cur) {
        let db = cb.saturating_sub(*pb);
        let dt = ct.saturating_sub(*pt);
        busy += db;
        total += dt;
        if dt > 0 {
            max_core = max_core.max((db as f32 / dt as f32 * 100.0).min(100.0));
        }
    }
    if total == 0 {
        return None;
    }
    Some(CpuLoad { total: (busy as f32 / total as f32 * 100.0).min(100.0), max_core, cores: cur.len() as u16 })
}

/// Parse the `cpuN` lines of /proc/stat into (busy, total) jiffies per core.
pub fn parse_proc_stat(text: &str) -> CoreTimes {
    text.lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(|c| c.is_ascii_digit()))
        .filter_map(|l| {
            let v: Vec<u64> = l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
            if v.len() < 4 {
                return None;
            }
            let total: u64 = v.iter().take(8).sum();
            let idle = v[3] + v.get(4).copied().unwrap_or(0);
            Some((total - idle, total))
        })
        .collect()
}

/// Parse /proc/meminfo (kB values).
pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let get =
        |k: &str| text.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok());
    let total = get("MemTotal:")?;
    let avail = get("MemAvailable:")?;
    Some(MemInfo {
        ram_used_mb: ((total - avail) / 1024.0) as f32,
        ram_total_mb: (total / 1024.0) as f32,
        commit_mb: get("Committed_AS:").map(|v| (v / 1024.0) as f32).unwrap_or(0.0),
        commit_limit_mb: get("CommitLimit:").map(|v| (v / 1024.0) as f32).unwrap_or(0.0),
    })
}

/// Keeps the previous core counters and the last memory reading.
#[derive(Default)]
pub struct SysMeter {
    prev: Option<CoreTimes>,
    mem: Option<MemInfo>,
    mem_tick: u32,
    #[cfg(windows)]
    ntq: Option<win::NtQuery>,
    #[cfg(windows)]
    ntq_tried: bool,
}

impl SysMeter {
    /// One reading: CPU every call, memory every `mem_every` calls. Missing APIs give None.
    pub fn sample(&mut self, mem_every: u32) -> (Option<CpuLoad>, Option<MemInfo>) {
        let cur = self.core_times();
        let load = match (&self.prev, &cur) {
            (Some(p), Some(c)) => cpu_load(p, c),
            _ => None,
        };
        if cur.is_some() {
            self.prev = cur;
        }
        if self.mem.is_none() || self.mem_tick == 0 {
            self.mem = read_mem();
        }
        self.mem_tick = (self.mem_tick + 1) % mem_every.max(1);
        (load, self.mem)
    }

    #[cfg(windows)]
    fn core_times(&mut self) -> Option<CoreTimes> {
        if !self.ntq_tried {
            self.ntq_tried = true;
            self.ntq = win::NtQuery::load();
        }
        self.ntq.as_mut()?.core_times()
    }

    #[cfg(not(windows))]
    fn core_times(&mut self) -> Option<CoreTimes> {
        std::fs::read_to_string("/proc/stat").ok().map(|t| parse_proc_stat(&t)).filter(|v| !v.is_empty())
    }
}

#[cfg(windows)]
fn read_mem() -> Option<MemInfo> {
    win::mem()
}

#[cfg(not(windows))]
fn read_mem() -> Option<MemInfo> {
    std::fs::read_to_string("/proc/meminfo").ok().and_then(|t| parse_meminfo(&t))
}

#[cfg(windows)]
mod win {
    use super::{CoreTimes, MemInfo};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};

    /// SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION (100 ns units; kernel time includes idle).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Spi {
        idle: i64,
        kernel: i64,
        user: i64,
        dpc: i64,
        interrupt: i64,
        count: u32,
    }
    type NtQsi = unsafe extern "system" fn(u32, *mut core::ffi::c_void, u32, *mut u32) -> i32;
    const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;

    pub struct NtQuery {
        f: NtQsi,
        buf: Vec<Spi>,
    }

    impl NtQuery {
        pub fn load() -> Option<Self> {
            unsafe {
                let name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
                let h = GetModuleHandleW(name.as_ptr());
                if h.is_null() {
                    return None;
                }
                let p = GetProcAddress(h, c"NtQuerySystemInformation".as_ptr() as *const u8)?;
                Some(Self { f: std::mem::transmute::<unsafe extern "system" fn() -> isize, NtQsi>(p), buf: vec![Spi::default(); 64] })
            }
        }

        /// The first processor group (up to 64 logical cores).
        pub fn core_times(&mut self) -> Option<CoreTimes> {
            let mut ret = 0u32;
            let size = (self.buf.len() * size_of::<Spi>()) as u32;
            let st = unsafe { (self.f)(SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION, self.buf.as_mut_ptr().cast(), size, &mut ret) };
            if st != 0 {
                return None;
            }
            let n = (ret as usize / size_of::<Spi>()).min(self.buf.len());
            Some(
                self.buf[..n]
                    .iter()
                    .map(|s| {
                        let total = (s.kernel + s.user).max(0) as u64;
                        (total.saturating_sub(s.idle.max(0) as u64), total)
                    })
                    .collect(),
            )
        }
    }

    pub fn mem() -> Option<MemInfo> {
        unsafe {
            let mut pi: PERFORMANCE_INFORMATION = std::mem::zeroed();
            pi.cb = size_of::<PERFORMANCE_INFORMATION>() as u32;
            if GetPerformanceInfo(&mut pi, pi.cb) == 0 {
                return None;
            }
            let pg = pi.PageSize as f64 / 1048576.0;
            Some(MemInfo {
                ram_used_mb: ((pi.PhysicalTotal - pi.PhysicalAvailable) as f64 * pg) as f32,
                ram_total_mb: (pi.PhysicalTotal as f64 * pg) as f32,
                commit_mb: (pi.CommitTotal as f64 * pg) as f32,
                commit_limit_mb: (pi.CommitLimit as f64 * pg) as f32,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_load_total_and_busiest_core() {
        let prev = vec![(100, 1000), (100, 1000), (0, 0)];
        // core 0: 900 of 1000 busy, core 1: 100 of 1000, core 2: 0 of 1000
        let cur = vec![(1000, 2000), (200, 2000), (0, 1000)];
        let l = cpu_load(&prev, &cur).unwrap();
        assert_eq!(l.cores, 3);
        assert!((l.total - 1000.0 / 3000.0 * 100.0).abs() < 0.01, "{l:?}");
        assert!((l.max_core - 90.0).abs() < 0.01);
        assert_eq!(cpu_load(&prev, &cur[..2]), None, "core count changed");
        assert_eq!(cpu_load(&cur, &cur), None, "no time passed");
    }

    #[test]
    fn proc_parsers() {
        let st = "cpu  10 0 10 80 0 0 0 0 0 0\ncpu0 5 0 5 40 0 0 0 0 0 0\ncpu1 5 0 5 30 10 0 0 0 0 0\nintr 1\n";
        assert_eq!(parse_proc_stat(st), vec![(10, 50), (10, 50)]);
        let mi = "MemTotal:       16384000 kB\nMemAvailable:    8192000 kB\nCommitLimit:    20000000 kB\nCommitted_AS:   10240000 kB\n";
        let m = parse_meminfo(mi).unwrap();
        assert_eq!((m.ram_total_mb.round(), m.ram_used_mb.round(), m.commit_mb.round()), (16000.0, 8000.0, 10000.0));
    }
}
