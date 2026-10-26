//! Per-process VRAM over time (1.2). Every few seconds the VRAM each process holds is read, and
//! a compact line goes to `history/proc_vram.jsonl` when something changed (by 64 MB or more, a
//! process came or went) or at least once a minute. On Windows the driver usually does not say
//! (NVML answers "N/A" for every process under WDDM), so Tachyon reads the same per-process
//! "Dedicated GPU memory" Task Manager shows, from the Windows performance counters
//! (`\GPU Process Memory(*)\Dedicated Usage`, pdh.dll loaded from System32), on the adapter whose
//! total matches the NVIDIA card. Read-only; names only, never paths.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;

pub const FILE: &str = "proc_vram.jsonl";
const OLD: &str = "proc_vram.1.jsonl";
/// The file rotates past this size (about 2-4 days at the defaults).
pub const MAX_FILE_BYTES: u64 = 4 << 20;
/// Processes holding less are left out.
pub const MIN_MB: f32 = 16.0;
/// At most this many processes per line (the largest).
pub const MAX_PROCS: usize = 16;
/// A change this big (MB) in any process writes a line...
pub const CHANGE_MB: f32 = 64.0;
/// ...and nothing changing still writes one this often.
pub const KEEP_ALIVE_MS: i64 = 60_000;

/// One reading: (name, pid, MB), largest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub t: i64,
    pub p: Vec<(String, u32, f32)>,
}

/// Keep the largest processes above `MIN_MB`, largest first.
pub fn top(mut v: Vec<(String, u32, f32)>) -> Vec<(String, u32, f32)> {
    v.retain(|x| x.2 >= MIN_MB && x.2.is_finite());
    v.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.1.cmp(&b.1)));
    v.truncate(MAX_PROCS);
    for x in v.iter_mut() {
        x.2 = x.2.round();
    }
    v
}

/// Decides which readings are written.
#[derive(Debug, Default)]
pub struct VramLog {
    last: Option<Point>,
}

impl VramLog {
    /// A new reading; returns the line to write, if any.
    pub fn push(&mut self, t: i64, procs: Vec<(String, u32, f32)>) -> Option<Point> {
        let p = Point { t, p: top(procs) };
        let write = match &self.last {
            None => true,
            Some(l) => {
                t - l.t >= KEEP_ALIVE_MS
                    || l.p.len() != p.p.len()
                    || p.p.iter().any(|(_, pid, mb)| l.p.iter().find(|x| x.1 == *pid).is_none_or(|x| (x.2 - mb).abs() >= CHANGE_MB))
            }
        };
        if write {
            self.last = Some(p.clone());
            Some(p)
        } else {
            None
        }
    }
}

pub fn append(dir: &Path, p: &Point) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
        std::fs::rename(&path, dir.join(OLD))?;
    }
    let mut line = serde_json::to_string(p).map_err(std::io::Error::other)?;
    line.push('\n');
    std::fs::OpenOptions::new().create(true).append(true).open(&path)?.write_all(line.as_bytes())
}

/// Readings between `from` and `to` (Unix ms), oldest first, plus the last one before `from`
/// (the state at the start of the window).
pub fn read_range(dir: &Path, from: i64, to: i64) -> Vec<Point> {
    let mut v = vec![];
    let mut before: Option<Point> = None;
    for name in [OLD, FILE] {
        let Ok(f) = std::fs::File::open(dir.join(name)) else { continue };
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            if line.len() > 16 << 10 {
                continue;
            }
            let Ok(p) = serde_json::from_str::<Point>(&line) else { continue };
            if p.t < from {
                if before.as_ref().is_none_or(|b| b.t <= p.t) {
                    before = Some(p);
                }
            } else if p.t <= to {
                v.push(p);
            }
        }
    }
    v.sort_by_key(|p| p.t);
    if let Some(b) = before {
        v.insert(0, b);
    }
    v
}

/// Top consumers over a window: peak, time-weighted average and last value per process name,
/// when each was first and last seen, and a short timeline for the top few. `name` filters
/// (case-insensitive substring). The readings are steps: each holds until the next.
pub fn summary(points: &[Point], from: i64, to: i64, name: Option<&str>, n: usize) -> Value {
    use std::collections::BTreeMap;
    struct Acc {
        peak: f32,
        peak_t: i64,
        area: f64,
        first: i64,
        last_seen: i64,
        last_mb: f32,
        pids: Vec<u32>,
        line: Vec<(i64, f32)>,
    }
    let filt = name.map(str::to_lowercase);
    let mut m: BTreeMap<String, Acc> = BTreeMap::new();
    let mut total_peak: (f32, i64) = (0.0, 0);
    for (i, p) in points.iter().enumerate() {
        let start = p.t.max(from);
        let end = points.get(i + 1).map_or(to, |q| q.t).min(to);
        let span = (end - start).max(0) as f64;
        // the same name in several processes (Chrome, python workers) counts together
        let mut by_name: BTreeMap<&str, (f32, Vec<u32>)> = BTreeMap::new();
        for (nm, pid, mb) in &p.p {
            if filt.as_ref().is_some_and(|f| !nm.to_lowercase().contains(f)) {
                continue;
            }
            let e = by_name.entry(nm.as_str()).or_default();
            e.0 += mb;
            e.1.push(*pid);
        }
        let tot: f32 = by_name.values().map(|x| x.0).sum();
        if tot > total_peak.0 && p.t <= to {
            total_peak = (tot, p.t.max(from));
        }
        for (nm, (mb, pids)) in by_name {
            let a = m.entry(nm.to_string()).or_insert(Acc {
                peak: 0.0,
                peak_t: start,
                area: 0.0,
                first: start,
                last_seen: start,
                last_mb: 0.0,
                pids: vec![],
                line: vec![],
            });
            if mb > a.peak {
                a.peak = mb;
                a.peak_t = start;
            }
            a.area += mb as f64 * span;
            a.last_seen = end.max(start);
            a.last_mb = mb;
            for pid in pids {
                if !a.pids.contains(&pid) {
                    a.pids.push(pid);
                }
            }
            if a.line.last().is_none_or(|l| (l.1 - mb).abs() >= 1.0) {
                a.line.push((start, mb));
            }
        }
    }
    // the average counts only the time with readings (not before the first one)
    let covered_from = points.first().map_or(from, |p| p.t.max(from));
    let window = (to - covered_from).max(1) as f64;
    let hm = |t: i64| crate::telemetry::Sample { t_ms: t, ..Default::default() }.local().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut rows: Vec<(String, Acc)> = m.into_iter().collect();
    rows.sort_by(|a, b| b.1.peak.total_cmp(&a.1.peak));
    let count = rows.len();
    let top: Vec<Value> = rows
        .into_iter()
        .take(n)
        .enumerate()
        .map(|(i, (nm, a))| {
            let step = (a.line.len() / 40).max(1);
            let mut v = json!({
                "name": nm,
                "peak_mb": a.peak,
                "peak_at": hm(a.peak_t),
                "avg_mb": ((a.area / window) as f32).round(),
                "last_mb": a.last_mb,
                "first_seen": hm(a.first),
                "last_seen": hm(a.last_seen.min(to)),
                "pids": a.pids,
            });
            if i < 5 {
                v["timeline"] = json!(a.line.iter().step_by(step).map(|(t, mb)| json!({ "time": hm(*t), "mb": mb })).collect::<Vec<_>>());
            }
            v
        })
        .collect();
    json!({
        "window": { "from": hm(from), "to": hm(to) },
        "recorded_from": points.first().map(|p| hm(p.t.max(from))),
        "readings": points.len(),
        "processes": count,
        "top": top,
        "peak_total_mb": (total_peak.0 > 0.0).then_some(total_peak.0),
        "peak_total_at": (total_peak.0 > 0.0).then(|| hm(total_peak.1)),
    })
}

/// Windows performance counters for per-process dedicated GPU memory (what Task Manager shows).
#[cfg(windows)]
pub mod pdh {
    use std::ffi::c_void;

    type Status = u32;
    type FOpen = unsafe extern "system" fn(*const u16, usize, *mut isize) -> Status;
    type FAdd = unsafe extern "system" fn(isize, *const u16, usize, *mut isize) -> Status;
    type FCollect = unsafe extern "system" fn(isize) -> Status;
    type FArray = unsafe extern "system" fn(isize, u32, *mut u32, *mut u32, *mut c_void) -> Status;
    type FClose = unsafe extern "system" fn(isize) -> Status;
    const PDH_FMT_LARGE: u32 = 0x0000_0400;
    const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
    const PDH_MORE_DATA: u32 = 0x8000_07D2;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Item {
        name: *const u16,
        status: u32,
        _pad: u32,
        large: i64,
    }

    pub struct GpuMem {
        _lib: libloading::Library,
        query: isize,
        procs: isize,
        adapters: isize,
        collect: FCollect,
        array: FArray,
        close: FClose,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn wstr(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut n = 0;
        unsafe {
            while *p.add(n) != 0 && n < 512 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
        }
    }

    /// "pid_1234_luid_0x00000000_0x0000D1F2_phys_0" -> (1234, "0x00000000_0x0000D1F2").
    pub fn parse_instance(s: &str) -> Option<(Option<u32>, String)> {
        let pid = s.strip_prefix("pid_").and_then(|r| r.split('_').next()).and_then(|p| p.parse().ok());
        let i = s.find("luid_")? + 5;
        let rest = &s[i..];
        let end = rest.find("_phys").unwrap_or(rest.len());
        Some((pid, rest[..end].to_string()))
    }

    impl GpuMem {
        pub fn open() -> Option<Self> {
            unsafe {
                let lib = libloading::Library::new("pdh.dll").ok()?;
                let open: FOpen = *lib.get(b"PdhOpenQueryW\0").ok()?;
                let add: FAdd = *lib.get(b"PdhAddEnglishCounterW\0").ok()?;
                let collect: FCollect = *lib.get(b"PdhCollectQueryData\0").ok()?;
                let array: FArray = *lib.get(b"PdhGetFormattedCounterArrayW\0").ok()?;
                let close: FClose = *lib.get(b"PdhCloseQuery\0").ok()?;
                let mut q = 0isize;
                if open(std::ptr::null(), 0, &mut q) != 0 {
                    return None;
                }
                let (mut pc, mut ac) = (0isize, 0isize);
                let a = add(q, wide("\\GPU Process Memory(*)\\Dedicated Usage").as_ptr(), 0, &mut pc);
                let b = add(q, wide("\\GPU Adapter Memory(*)\\Dedicated Usage").as_ptr(), 0, &mut ac);
                if a != 0 {
                    close(q);
                    return None;
                }
                Some(GpuMem { _lib: lib, query: q, procs: pc, adapters: if b == 0 { ac } else { 0 }, collect, array, close })
            }
        }

        fn items(&self, counter: isize) -> Vec<(String, i64)> {
            let mut out = vec![];
            if counter == 0 {
                return out;
            }
            unsafe {
                let (mut size, mut count) = (0u32, 0u32);
                let r = (self.array)(counter, PDH_FMT_LARGE | PDH_FMT_NOCAP100, &mut size, &mut count, std::ptr::null_mut());
                if r != PDH_MORE_DATA || size == 0 || size > 64 << 20 {
                    return out;
                }
                let mut buf = vec![0u64; (size as usize).div_ceil(8)];
                let r = (self.array)(counter, PDH_FMT_LARGE | PDH_FMT_NOCAP100, &mut size, &mut count, buf.as_mut_ptr() as *mut c_void);
                if r != 0 {
                    return out;
                }
                let items = std::slice::from_raw_parts(buf.as_ptr() as *const Item, (count as usize).min(buf.len() * 8 / 24));
                for it in items {
                    if it.status == 0 || it.status == 0x0000_0001 {
                        out.push((wstr(it.name), it.large));
                    }
                }
            }
            out
        }

        /// (pid, MB) of dedicated GPU memory on the NVIDIA adapter: the adapter whose total is
        /// closest to `nvml_used_mb` (or the largest without it).
        pub fn read(&mut self, nvml_used_mb: Option<f64>) -> Option<Vec<(u32, f32)>> {
            if unsafe { (self.collect)(self.query) } != 0 {
                return None;
            }
            let mb = |b: i64| b as f64 / 1048576.0;
            let adapters: Vec<(String, f64)> =
                self.items(self.adapters).into_iter().filter_map(|(n, v)| parse_instance(&n).map(|(_, l)| (l, mb(v)))).collect();
            let luid = match nvml_used_mb {
                Some(u) => adapters.iter().min_by(|a, b| (a.1 - u).abs().total_cmp(&(b.1 - u).abs())),
                None => adapters.iter().max_by(|a, b| a.1.total_cmp(&b.1)),
            }
            .map(|a| a.0.clone());
            let mut out: Vec<(u32, f32)> = vec![];
            for (n, v) in self.items(self.procs) {
                let Some((Some(pid), l)) = parse_instance(&n) else { continue };
                if luid.as_ref().is_some_and(|x| *x != l) {
                    continue;
                }
                match out.iter_mut().find(|x| x.0 == pid) {
                    Some(x) => x.1 += mb(v) as f32,
                    None => out.push((pid, mb(v) as f32)),
                }
            }
            Some(out)
        }
    }

    impl Drop for GpuMem {
        fn drop(&mut self) {
            unsafe {
                (self.close)(self.query);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_on_change_or_once_a_minute() {
        let mut l = VramLog::default();
        let a = vec![("python.exe".to_string(), 10, 9216.0), ("dwm.exe".into(), 4, 300.0), ("tiny.exe".into(), 5, 3.0)];
        let p = l.push(0, a.clone()).unwrap();
        assert_eq!(p.p.len(), 2, "below 16 MB left out");
        assert_eq!(p.p[0].0, "python.exe");
        assert!(l.push(5_000, a.clone()).is_none());
        let mut b = a.clone();
        b[0].2 = 9300.0;
        assert!(l.push(10_000, b.clone()).is_some(), "84 MB more");
        assert!(l.push(15_000, b.clone()).is_none());
        assert!(l.push(70_000, b.clone()).is_some(), "a minute");
        b.push(("blender.exe".into(), 77, 4000.0));
        assert!(l.push(71_000, b).is_some(), "a new process");
    }

    #[test]
    fn summary_peaks_averages_and_file_round_trip() {
        let d = std::env::temp_dir().join(format!("tachyon_pv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t0 = 1_793_000_000_000i64;
        let pts = vec![
            Point { t: t0 - 30_000, p: vec![("dwm.exe".into(), 4, 200.0)] },
            Point { t: t0, p: vec![("python.exe".into(), 10, 2000.0), ("dwm.exe".into(), 4, 200.0)] },
            Point {
                t: t0 + 30_000,
                p: vec![("python.exe".into(), 10, 12000.0), ("python.exe".into(), 11, 1000.0), ("dwm.exe".into(), 4, 200.0)],
            },
            Point { t: t0 + 60_000, p: vec![("dwm.exe".into(), 4, 200.0)] },
        ];
        for p in &pts {
            append(&d, p).unwrap();
        }
        let back = read_range(&d, t0, t0 + 120_000);
        assert_eq!(back.len(), 4, "the reading before the window gives the state at its start");
        assert_eq!(back, pts);
        let s = summary(&back, t0, t0 + 120_000, None, 10);
        assert_eq!(s["top"][0]["name"], "python.exe");
        assert_eq!(s["top"][0]["peak_mb"], 13000.0, "two python workers together");
        assert_eq!(s["top"][0]["pids"], json!([10, 11]));
        // 2000 for 30 s + 13000 for 30 s over 120 s
        assert_eq!(s["top"][0]["avg_mb"], 3750.0);
        assert_eq!(s["top"][1]["avg_mb"], 200.0);
        assert_eq!(s["peak_total_mb"], 13200.0);
        // a window starting before the first reading: the average is over the recorded time
        let s2 = summary(&back[1..], t0 - 600_000, t0 + 120_000, None, 10);
        assert_eq!(s2["top"][0]["avg_mb"], 3750.0);
        let f = summary(&back, t0, t0 + 120_000, Some("DWM"), 10);
        assert_eq!(f["processes"], 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn pdh_instance_names() {
        assert_eq!(pdh::parse_instance("pid_1234_luid_0x00000000_0x0000D1F2_phys_0"), Some((Some(1234), "0x00000000_0x0000D1F2".into())));
        assert_eq!(pdh::parse_instance("luid_0x00000000_0x0000D1F2_phys_0"), Some((None, "0x00000000_0x0000D1F2".into())));
        assert_eq!(pdh::parse_instance("garbage"), None);
    }
}
