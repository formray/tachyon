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

/// When the PC last started (Unix milliseconds), from the uptime counter.
#[cfg(windows)]
pub fn boot_time_ms() -> Option<i64> {
    let up = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() } as i64;
    Some(crate::telemetry::now_ms() - up)
}

#[cfg(not(windows))]
pub fn boot_time_ms() -> Option<i64> {
    let t = std::fs::read_to_string("/proc/uptime").ok()?;
    let up: f64 = t.split_whitespace().next()?.parse().ok()?;
    Some(crate::telemetry::now_ms() - (up * 1000.0) as i64)
}

/// System memory speed from the SMBIOS memory device tables (read-only firmware data).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RamSpeed {
    /// "DDR5", "DDR4", ...
    pub kind: String,
    /// The modules' rated speed (MT/s), usually their standard JEDEC speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rated_mts: Option<u32>,
    /// The speed the memory runs at now (MT/s): above the rated speed with EXPO/XMP on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_mts: Option<u32>,
    pub modules: u32,
    /// The first module's part number (SMBIOS), e.g. "CMK32GX5M2B6000Z30".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

/// How the memory speed compares with what the modules are made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamState {
    /// Clearly below the modules' standard (JEDEC) speed: EXPO/XMP off and the BIOS on its safe
    /// defaults, typically after a BIOS reset (or with four modules, a cautious BIOS).
    BelowStandard,
    /// At the standard JEDEC speed.
    Standard,
    /// An EXPO/XMP profile at or below the kit's rated profile speed.
    Profile,
    /// Faster than the kit's rated EXPO/XMP speed: an overclock beyond its rating.
    BeyondRating,
    /// Faster than the standard speed, kit rating unknown (EXPO/XMP or a manual overclock).
    AboveStandard,
}

fn digits_after(s: &str, prefix_end: usize, n: usize) -> Option<u32> {
    let d = s.get(prefix_end..prefix_end + n)?;
    d.chars().all(|c| c.is_ascii_digit()).then(|| d.parse().ok()).flatten()
}

/// The EXPO/XMP speed (MT/s) a kit is sold for, decoded from common part numbers:
/// Corsair `CMK32GX5M2B6000Z30` (6000), G.Skill `F5-6000J3038F16GX2` (6000),
/// Kingston Fury `KF560C36BBEK2-32` (6000), Crucial / Ballistix `CP2K16G60C40U5` (6000),
/// TeamGroup `FF3D532G6000HC38A` (6000). None when the scheme is unknown.
pub fn kit_speed(part: &str) -> Option<u32> {
    let p = part.trim().to_ascii_uppercase();
    let plausible = |v: u32| (1600..=12000).contains(&v).then_some(v);
    // Corsair: CM<series><size>GX<gen>M<n><letter><speed>...
    if p.starts_with("CM")
        && let Some(i) = p.find("GX")
        && let Some(m) = p[i..].find('M').map(|m| i + m)
    {
        return digits_after(&p, m + 3, 4).and_then(plausible);
    }
    // G.Skill: F4-3600C16D-..., F5-6000J3038F16GX2-...
    if p.starts_with("F4-") || p.starts_with("F5-") {
        return digits_after(&p, 3, 4).and_then(plausible);
    }
    // Kingston Fury: KF<gen><speed/100>C..., e.g. KF560C36BBEK2-32 (DDR5-6000), KF432C16BB (DDR4-3200)
    if p.starts_with("KF") && p.as_bytes().get(5) == Some(&b'C') {
        return digits_after(&p, 3, 2).map(|v| v * 100).and_then(plausible);
    }
    // Crucial / Ballistix: CP2K16G60C40U5, BL2K16G36C16U4B: <size>G<speed/100>C
    if (p.starts_with("CP") || p.starts_with("BL"))
        && let Some(g) = p.find('G')
        && p.as_bytes().get(g + 3) == Some(&b'C')
    {
        return digits_after(&p, g + 1, 2).map(|v| v * 100).and_then(plausible);
    }
    // TeamGroup: FF3D532G6000HC38A..., TF13D416G3600HC18J...: <size>G<speed>HC
    if (p.starts_with("FF") || p.starts_with("TF") || p.starts_with("TL"))
        && let Some(h) = p.find("HC")
        && h >= 4
    {
        return digits_after(&p, h - 4, 4).and_then(plausible);
    }
    None
}

/// Modules in one retail kit from the part number (Corsair `M2`, G.Skill `GX2`, Kingston `K2`, Crucial `2K`).
pub fn kit_modules(part: &str) -> Option<u32> {
    let p = part.trim().to_ascii_uppercase();
    let after = |pat: &str| p.find(pat).and_then(|i| p[i + pat.len()..].chars().next()).and_then(|c| c.to_digit(10));
    if p.starts_with("CM") {
        return p.find("GX").and_then(|i| p[i..].find('M').map(|m| i + m + 1)).and_then(|j| p[j..].chars().next()?.to_digit(10));
    }
    if p.starts_with("F4-") || p.starts_with("F5-") {
        return after("GX");
    }
    if p.starts_with("KF") {
        let b = p.as_bytes();
        let kit = (3..b.len().saturating_sub(1)).find(|&i| b[i] == b'K' && b[i + 1].is_ascii_digit()).map(|i| (b[i + 1] - b'0') as u32);
        return kit.or(Some(1));
    }
    if p.starts_with("CP") || p.starts_with("BL") {
        let b = p.as_bytes();
        return match (b.get(2), b.get(3)) {
            (Some(d), Some(b'K')) if d.is_ascii_digit() => Some((d - b'0') as u32),
            _ => Some(1),
        };
    }
    None
}

impl RamSpeed {
    /// The kit's rated EXPO/XMP speed, when its part number tells.
    pub fn kit_mts(&self) -> Option<u32> {
        self.part.as_deref().and_then(kit_speed)
    }

    pub fn state(&self) -> RamState {
        let Some(c) = self.configured_mts else { return RamState::Standard };
        if self.below_rated() {
            return RamState::BelowStandard;
        }
        match self.kit_mts() {
            Some(k) if c > k + 100 => RamState::BeyondRating,
            Some(_) if self.above_rated() || (self.rated_mts.is_none() && self.above_cpu_spec()) => RamState::Profile,
            Some(_) => RamState::Standard,
            None if self.above_rated() || self.above_cpu_spec() => RamState::AboveStandard,
            None => RamState::Standard,
        }
    }

    /// Two DIMMs per channel (four DDR5 modules) faster than most CPUs officially support that
    /// way, and whether they are two retail kits combined.
    pub fn crowded(&self) -> Option<String> {
        let c = self.configured_mts?;
        if self.kind != "DDR5" || self.modules < 4 || c <= 5200 {
            return None;
        }
        let kits = self.part.as_deref().and_then(kit_modules).filter(|k| *k > 0 && *k < self.modules).map(|k| self.modules / k);
        let mixed = kits.filter(|n| *n > 1).map(|n| format!(", from {n} separate kits")).unwrap_or_default();
        Some(format!(
            "{} DDR5 modules at {c} MT/s{mixed}: with two modules per channel most CPUs are only specified for about 3600-5200 MT/s",
            self.modules
        ))
    }

    /// The memory runs clearly slower than its rated (JEDEC) speed, e.g. 3600 MT/s on 4800 modules.
    pub fn below_rated(&self) -> bool {
        matches!((self.rated_mts, self.configured_mts), (Some(r), Some(c)) if c > 0 && c + 100 < r)
    }

    /// The memory runs faster than its rated speed (EXPO/XMP or a manual overclock).
    pub fn above_rated(&self) -> bool {
        matches!((self.rated_mts, self.configured_mts), (Some(r), Some(c)) if c > r + 100)
    }

    /// Above the speed most desktop CPUs officially support for this memory type
    /// (DDR5 5600, DDR4 3200), even when the modules report that speed as rated.
    pub fn above_cpu_spec(&self) -> bool {
        let spec = match self.kind.as_str() {
            "DDR5" => 5600,
            "DDR4" => 3200,
            _ => return false,
        };
        self.configured_mts.is_some_and(|c| c > spec + 100)
    }

    pub fn describe(&self) -> String {
        match (self.configured_mts, self.rated_mts) {
            (Some(c), Some(r)) if c != r => format!("{} at {c} MT/s (rated {r})", self.kind),
            (Some(c), _) => format!("{} at {c} MT/s", self.kind),
            (None, Some(r)) => format!("{} rated {r} MT/s", self.kind),
            _ => self.kind.clone(),
        }
    }
}

fn mem_kind(t: u8) -> &'static str {
    match t {
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "RAM",
    }
}

/// String number `idx` (1-based) of the structure whose strings start at `from`.
fn smbios_string(table: &[u8], from: usize, idx: u8) -> Option<String> {
    if idx == 0 {
        return None;
    }
    let mut k = 1;
    let mut j = from;
    while j < table.len() && table[j] != 0 {
        let end = table[j..].iter().position(|b| *b == 0).map_or(table.len(), |e| j + e);
        if k == idx {
            return Some(String::from_utf8_lossy(&table[j..end]).into_owned());
        }
        k += 1;
        j = end + 1;
        if k > 64 {
            break;
        }
    }
    None
}

/// Parse the SMBIOS structure table (type 17 memory devices). Damaged tables give None or a
/// partial answer, never a panic.
pub fn parse_smbios(table: &[u8]) -> Option<RamSpeed> {
    let rd16 = |b: &[u8], o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let rd32 = |b: &[u8], o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    let mut r = RamSpeed::default();
    let mut i = 0usize;
    let mut guard = 0;
    while i + 4 <= table.len() && guard < 4096 {
        guard += 1;
        let (ty, len) = (table[i], table[i + 1] as usize);
        if len < 4 || i + len > table.len() {
            break;
        }
        let s = &table[i..i + len];
        if ty == 17 && len >= 0x17 {
            let size = rd16(s, 0x0C).unwrap_or(0);
            if size != 0 && size != 0xFFFF {
                r.modules += 1;
                if let Some(k) = s.get(0x12) {
                    r.kind = mem_kind(*k).into();
                }
                let speed = match rd16(s, 0x15) {
                    Some(0xFFFF) => rd32(s, 0x54),
                    v => v.map(u32::from),
                }
                .filter(|v| *v > 0 && *v < 100_000);
                let conf = match rd16(s, 0x20).filter(|_| len >= 0x22) {
                    Some(0xFFFF) => rd32(s, 0x58),
                    v => v.map(u32::from),
                }
                .filter(|v| *v > 0 && *v < 100_000);
                r.rated_mts = r.rated_mts.max(speed);
                r.configured_mts = r.configured_mts.max(conf);
                if r.part.is_none()
                    && let Some(&idx) = s.get(0x1A)
                {
                    r.part = smbios_string(table, i + len, idx).map(|p| p.trim().to_string()).filter(|p| !p.is_empty() && p.is_ascii());
                }
            }
        }
        if ty == 127 {
            break;
        }
        // skip the formatted part, then the strings up to the double NUL
        let mut j = i + len;
        while j + 1 < table.len() && !(table[j] == 0 && table[j + 1] == 0) {
            j += 1;
        }
        i = j + 2;
    }
    (r.modules > 0).then_some(r)
}

/// The memory speed, read once when needed (Windows: GetSystemFirmwareTable, no admin rights).
#[cfg(windows)]
pub fn ram_speed() -> Option<RamSpeed> {
    use windows_sys::Win32::System::SystemInformation::GetSystemFirmwareTable;
    const RSMB: u32 = u32::from_be_bytes(*b"RSMB");
    unsafe {
        let n = GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0);
        if !(8..=(1 << 20)).contains(&n) {
            return None;
        }
        let mut buf = vec![0u8; n as usize];
        if GetSystemFirmwareTable(RSMB, 0, buf.as_mut_ptr().cast(), n) != n {
            return None;
        }
        // RawSMBIOSData: 4 version bytes, u32 length, then the table
        let len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        parse_smbios(&buf[8..(8 + len).min(buf.len())])
    }
}

#[cfg(not(windows))]
pub fn ram_speed() -> Option<RamSpeed> {
    None
}

/// How long the firmware (BIOS/UEFI) took at the last start, in ms: Windows keeps it as
/// `FwPOSTTime` (Task Manager's "Last BIOS time"). Read-only; None when missing.
#[cfg(windows)]
pub fn firmware_boot_ms() -> Option<u32> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
    let wz = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wz(r"SYSTEM\CurrentControlSet\Control\Session Manager\Power");
    let val = wz("FwPOSTTime");
    let mut v: u32 = 0;
    let mut sz: u32 = 4;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut v as *mut _ as *mut _,
            &mut sz,
        )
    };
    (r == 0 && v > 0 && v < 3_600_000).then_some(v)
}

#[cfg(not(windows))]
pub fn firmware_boot_ms() -> Option<u32> {
    None
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

    /// One type 17 record: size 16 GB, DDR5, rated `speed`, configured `conf`.
    pub fn smbios_dimm(speed: u16, conf: u16) -> Vec<u8> {
        smbios_dimm_part(speed, conf, "")
    }

    pub fn smbios_dimm_part(speed: u16, conf: u16, part: &str) -> Vec<u8> {
        let mut s = vec![0u8; 0x28];
        s[0] = 17;
        s[1] = 0x28;
        s[0x0C..0x0E].copy_from_slice(&16384u16.to_le_bytes());
        s[0x12] = 0x22;
        s[0x15..0x17].copy_from_slice(&speed.to_le_bytes());
        s[0x20..0x22].copy_from_slice(&conf.to_le_bytes());
        // strings: 1 locator, 2 manufacturer, 3 part number (padded with spaces, as Corsair does)
        if part.is_empty() {
            s.extend_from_slice(b"DIMM A1\0Kingston\0\0");
        } else {
            s[0x1A] = 3;
            s.extend_from_slice(format!("DIMM A1\0Corsair\0{part}   \0\0").as_bytes());
        }
        s
    }

    #[test]
    fn kit_part_numbers() {
        for (p, v, n) in [
            ("CMK32GX5M2B6000Z30", Some(6000), Some(2)),
            ("CMH64GX5M4B5600C40", Some(5600), Some(4)),
            ("CMK16GX4M2B3200C16", Some(3200), Some(2)),
            ("F5-6000J3038F16GX2-TZ5RK", Some(6000), Some(2)),
            ("F4-3600C16D-32GTZN", Some(3600), None),
            ("KF560C36BBEK2-32", Some(6000), Some(2)),
            ("KF432C16BB/16", Some(3200), Some(1)),
            ("CP2K16G60C40U5", Some(6000), Some(2)),
            ("BL2K16G36C16U4B", Some(3600), Some(2)),
            ("FF3D532G6000HC38ADC01", Some(6000), None),
            ("M425R2GA3BB0-CWMOD", None, None),
            ("", None, None),
            ("CMK", None, None),
        ] {
            assert_eq!((kit_speed(p), kit_modules(p)), (v, n), "{p}");
        }
    }

    #[test]
    fn expo_profile_vs_beyond_rating() {
        let st = |rated, conf, part: &str| {
            let mut t = vec![];
            for _ in 0..4 {
                t.extend(smbios_dimm_part(rated, conf, part));
            }
            parse_smbios(&t).unwrap()
        };
        // Giuseppe's PC: 4 x CMK32GX5M2B6000Z30 (two 2-module kits), now 5600; at 6000 on the crash day
        let now = st(4800, 5600, "CMK32GX5M2B6000Z30");
        assert_eq!(now.part.as_deref(), Some("CMK32GX5M2B6000Z30"));
        assert_eq!((now.kit_mts(), now.state()), (Some(6000), RamState::Profile));
        assert_eq!(st(4800, 6000, "CMK32GX5M2B6000Z30").state(), RamState::Profile, "at the rated EXPO speed");
        assert_eq!(st(4800, 6400, "CMK32GX5M2B6000Z30").state(), RamState::BeyondRating);
        assert_eq!(st(4800, 4800, "CMK32GX5M2B6000Z30").state(), RamState::Standard);
        assert_eq!(st(4800, 6000, "").state(), RamState::AboveStandard, "no part number: rating unknown");
        // 26 Oct: a BIOS reset left the same kit at 3600, below its 4800 standard
        let reset = st(4800, 3600, "CMK32GX5M2B6000Z30");
        assert!(reset.below_rated() && !reset.above_rated());
        assert_eq!(reset.state(), RamState::BelowStandard);
        assert_eq!(st(4800, 4800, "").state(), RamState::Standard);
        assert_eq!(st(4800, 4750, "").state(), RamState::Standard, "a few MT/s under is still the standard speed");
        assert_eq!(reset.crowded(), None);
        assert_eq!(
            now.crowded().as_deref(),
            Some(
                "4 DDR5 modules at 5600 MT/s, from 2 separate kits: with two modules per channel most CPUs are only specified for about 3600-5200 MT/s"
            )
        );
        assert_eq!(st(4800, 5200, "CMK32GX5M2B6000Z30").crowded(), None);
    }

    #[test]
    fn smbios_memory_speed() {
        let mut t = vec![0u8, 4, 0, 0, 0, 0]; // a type 0 stub with no strings
        t.extend(smbios_dimm(4800, 6000));
        t.extend(smbios_dimm(4800, 6000));
        let mut empty = smbios_dimm(0, 0);
        empty[0x0C] = 0;
        empty[0x0D] = 0;
        t.extend(empty);
        t.extend([127, 4, 0, 0, 0, 0]);
        let r = parse_smbios(&t).unwrap();
        assert_eq!((r.kind.as_str(), r.rated_mts, r.configured_mts, r.modules), ("DDR5", Some(4800), Some(6000), 2));
        assert!(r.above_rated() && r.above_cpu_spec());
        assert_eq!(r.describe(), "DDR5 at 6000 MT/s (rated 4800)");
        let at_5600 = parse_smbios(&smbios_dimm(4800, 5600)).unwrap();
        assert!(at_5600.above_rated() && !at_5600.above_cpu_spec());
        let expo_as_rated = parse_smbios(&smbios_dimm(6000, 6000)).unwrap();
        assert!(!expo_as_rated.above_rated() && expo_as_rated.above_cpu_spec());
        assert!(parse_smbios(&[]).is_none());
        assert!(parse_smbios(&[17, 200, 0]).is_none(), "length past the end");
        let mut rng = crate::fuzz::Rng::new(17);
        for _ in 0..2000 {
            let _ = parse_smbios(&rng.mutate(&t));
        }
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
