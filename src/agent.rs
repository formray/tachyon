//! State for local AI agents. When `agent_api = true`, the running Tachyon publishes a small JSON
//! status into a named shared-memory block (`Local\TachyonAgentSnapshot`, this Windows session
//! only, no network) every two ticks; `tachyon mcp` opens it read-only. Writers and readers use a
//! sequence counter (odd while writing) so a reader never sees half a status.
use crate::monitor::Snapshot;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU32, Ordering};

pub const MAP_NAME: &str = "Local\\TachyonAgentSnapshot";
pub const MAP_SIZE: usize = 64 * 1024;
const MAGIC: u32 = 0x5441_4753; // "TAGS"
const HDR: usize = 16;

/// Round non-integer numbers to two decimals (f32 readings would otherwise show as
/// 324.8659973144531), recursively.
pub fn tidy(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(f) = n.as_f64().map(|f| (f * 100.0).round() / 100.0)
                && let Some(r) = serde_json::Number::from_f64(f)
            {
                *n = r;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(tidy),
        Value::Object(o) => o.values_mut().for_each(tidy),
        _ => {}
    }
}

/// The status an agent sees (also used by the standalone fallback).
pub fn status(snap: &Snapshot, source: &str) -> Value {
    let mut v = status_raw(snap, source);
    tidy(&mut v);
    v
}

fn status_raw(snap: &Snapshot, source: &str) -> Value {
    let i = &snap.insights;
    let now = crate::telemetry::now_ms();
    let gpu = snap.gpus.get(snap.selected as usize).cloned();
    let render = snap.render.as_ref().map(|r| {
        json!({
            "job": r.job, "stage": r.stage, "fps": r.fps,
            "frame": r.progress.as_ref().map(|p| p.frame), "total_frames": r.progress.as_ref().and_then(|p| p.total),
            "elapsed_s": r.job_elapsed, "estimated_total_s": r.job_est_total,
        })
    });
    let report = i.report.as_ref().map(|r| {
        json!({
            "cause": r.cause, "headline": r.headline, "just_before": r.just_before,
            "stopped_ms": r.stopped_ms, "new": i.report_new,
        })
    });
    json!({
        "tachyon": env!("CARGO_PKG_VERSION"),
        "source": source,
        "t_ms": now,
        "time": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        "gpu": { "name": gpu, "index": snap.selected, "driver": snap.driver },
        "error": snap.error,
        "reading": i.latest,
        "bottleneck": i.bottleneck,
        "vram_forecast": i.forecast,
        "idle_vram_note": i.forecast.hog_sentence(),
        "render": render,
        "job": i.live_job,
        "last_job": i.last_job,
        "crash_report": report,
        "data_dir": i.data_dir,
    })
}

/// Write a frame into a mapped buffer of `len` bytes at `p` (4-byte aligned).
///
/// # Safety
/// `p` must point to `len` writable bytes, aligned to 4, valid for the duration of the call.
pub unsafe fn write_frame(p: *mut u8, len: usize, body: &[u8]) -> bool {
    if len < HDR || body.len() > len - HDR {
        return false;
    }
    unsafe {
        let seq = &*(p.add(4) as *const AtomicU32);
        let s = seq.load(Ordering::Relaxed);
        let s = if s % 2 == 1 { s.wrapping_add(1) } else { s };
        seq.store(s.wrapping_add(1), Ordering::Release); // odd: writing
        std::sync::atomic::fence(Ordering::Release);
        (p as *mut u32).write_volatile(MAGIC);
        (p.add(8) as *mut u32).write_volatile(body.len() as u32);
        std::ptr::copy_nonoverlapping(body.as_ptr(), p.add(HDR), body.len());
        std::sync::atomic::fence(Ordering::Release);
        seq.store(s.wrapping_add(2), Ordering::Release); // even: done
    }
    true
}

/// Read a consistent frame, retrying briefly while the writer is busy.
///
/// # Safety
/// `p` must point to `len` readable bytes, aligned to 4.
pub unsafe fn read_frame(p: *const u8, len: usize) -> Option<Vec<u8>> {
    if len < HDR {
        return None;
    }
    unsafe {
        let seq = &*(p.add(4) as *const AtomicU32);
        for _ in 0..50 {
            let a = seq.load(Ordering::Acquire);
            if a % 2 == 1 {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            if (p as *const u32).read_volatile() != MAGIC {
                return None;
            }
            let n = (p.add(8) as *const u32).read_volatile() as usize;
            if n > len - HDR {
                return None;
            }
            let mut out = vec![0u8; n];
            std::ptr::copy_nonoverlapping(p.add(HDR), out.as_mut_ptr(), n);
            std::sync::atomic::fence(Ordering::Acquire);
            if seq.load(Ordering::Acquire) == a {
                return Some(out);
            }
        }
    }
    None
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Memory::*;

    pub struct Map {
        h: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
    }

    fn name() -> Vec<u16> {
        super::MAP_NAME.encode_utf16().chain([0]).collect()
    }

    impl Map {
        pub fn create() -> Option<Map> {
            unsafe {
                let h =
                    CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, super::MAP_SIZE as u32, name().as_ptr());
                if h.is_null() {
                    return None;
                }
                let view = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, super::MAP_SIZE);
                if view.Value.is_null() {
                    CloseHandle(h);
                    return None;
                }
                Some(Map { h, view })
            }
        }
        pub fn open_read() -> Option<Map> {
            unsafe {
                let h = OpenFileMappingW(FILE_MAP_READ, 0, name().as_ptr());
                if h.is_null() {
                    return None;
                }
                let view = MapViewOfFile(h, FILE_MAP_READ, 0, 0, super::MAP_SIZE);
                if view.Value.is_null() {
                    CloseHandle(h);
                    return None;
                }
                Some(Map { h, view })
            }
        }
        pub fn ptr(&self) -> *mut u8 {
            self.view.Value as *mut u8
        }
    }

    impl Drop for Map {
        fn drop(&mut self) {
            unsafe {
                UnmapViewOfFile(self.view);
                CloseHandle(self.h);
            }
        }
    }
}

/// The GUI side: publishes the status.
pub struct Publisher {
    #[cfg(windows)]
    map: Option<win::Map>,
    tick: u32,
}

impl Publisher {
    pub fn new() -> Self {
        Publisher {
            #[cfg(windows)]
            map: win::Map::create(),
            tick: 0,
        }
    }

    /// The shared memory block exists (always false off Windows).
    pub fn live(&self) -> bool {
        #[cfg(windows)]
        {
            self.map.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Publish every second call (the sampler ticks about once a second).
    pub fn tick(&mut self, snap: &Snapshot) {
        self.tick = self.tick.wrapping_add(1);
        if !self.tick.is_multiple_of(2) {
            return;
        }
        #[cfg(windows)]
        if let Some(m) = &self.map {
            let body = serde_json::to_vec(&status(snap, "tachyon")).unwrap_or_default();
            unsafe { write_frame(m.ptr(), MAP_SIZE, &body) };
        }
        #[cfg(not(windows))]
        let _ = snap;
    }
}

impl Default for Publisher {
    fn default() -> Self {
        Self::new()
    }
}

/// The agent side: the latest published status, if Tachyon is running with the agent API on.
pub fn read_published() -> Option<Value> {
    #[cfg(windows)]
    {
        let m = win::Map::open_read()?;
        let body = unsafe { read_frame(m.ptr(), MAP_SIZE) }?;
        serde_json::from_slice(&body).ok()
    }
    #[cfg(not(windows))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_reject_garbage() {
        let mut buf = vec![0u64; 512];
        let p = buf.as_mut_ptr() as *mut u8;
        let len = buf.len() * 8;
        assert!(unsafe { read_frame(p, len) }.is_none(), "empty block");
        assert!(unsafe { write_frame(p, len, b"{\"a\":1}") });
        assert_eq!(unsafe { read_frame(p, len) }.unwrap(), b"{\"a\":1}");
        assert!(unsafe { write_frame(p, len, b"{\"a\":22}") });
        assert_eq!(unsafe { read_frame(p, len) }.unwrap(), b"{\"a\":22}");
        let seq = u32::from_le_bytes(unsafe { std::slice::from_raw_parts(p.add(4), 4) }.try_into().unwrap());
        assert_eq!(seq, 4, "even after two writes");
        assert!(!unsafe { write_frame(p, len, &vec![b'x'; len]) }, "too big is refused");
    }

    #[test]
    fn status_has_the_documented_fields() {
        let mut snap = Snapshot { gpus: vec!["NVIDIA GeForce RTX 5080".into()], driver: "581.57".into(), ..Default::default() };
        snap.insights.latest = Some(crate::telemetry::Sample { util: Some(42), ..Default::default() });
        let v = status(&snap, "tachyon");
        for k in ["tachyon", "source", "t_ms", "gpu", "reading", "bottleneck", "vram_forecast", "job", "last_job", "crash_report"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["gpu"]["name"], "NVIDIA GeForce RTX 5080");
        assert_eq!(v["reading"]["util"], 42);
        let mut x = json!({ "a": 324.8659973144531, "b": [0.3499999940395355, 7], "c": { "d": 1.0 } });
        tidy(&mut x);
        assert_eq!(x, json!({ "a": 324.87, "b": [0.35, 7], "c": { "d": 1.0 } }));
    }
}
