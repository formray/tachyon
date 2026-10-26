//! Fast sampling (1.2): clocks, power against the limit, temperature, GPU load, clock-limit
//! reasons and the PCIe link every 100 ms, on a thread of its own with its own NVML session,
//! into a second fixed-size ring next to the flight recorder (`blackbox/fast_a.bin` or
//! `fast_b.bin`, the same letter as the recorder's session, so "current" and "previous" match).
//! 40-byte checksummed records; 15 minutes at 100 ms is 9,000 records, 352 KB per file. Records
//! are written once a second and flushed to the disk every 5 s, so a power cut loses at most
//! the last few seconds. The thread also keeps a running summary (peaks since the last tick)
//! for the job reports. Nothing here sets anything on the GPU.
use crate::recorder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub const REC: usize = 40;
const HEADER: usize = 64;
const MAGIC: &[u8; 8] = b"TACHYFS1";
const REC_MAGIC: u16 = 0x5346;
/// Pending records are written this often...
pub const WRITE_EVERY_MS: i64 = 1_000;
/// ...and flushed to the disk this often.
pub const SYNC_EVERY_MS: i64 = 5_000;
/// Largest fast ring read back (60 min at 50 ms is 2.9 MB).
pub const MAX_RING_BYTES: u64 = 4 << 20;

/// One fast reading. Missing values are None.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FastSample {
    pub t_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clk_sm: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clk_mem: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_limit_w: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub util: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throttle: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcie_gen: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcie_width: Option<u8>,
}

impl FastSample {
    /// Power as a fraction of the enforced limit.
    pub fn power_frac(&self) -> Option<f32> {
        match (self.power_w, self.power_limit_w) {
            (Some(p), Some(l)) if l > 0.0 => Some(p / l),
            _ => None,
        }
    }
}

fn fnv(b: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for x in b {
        h ^= *x as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

fn u16o(v: Option<u16>) -> [u8; 2] {
    v.unwrap_or(u16::MAX).to_le_bytes()
}
fn deci(v: Option<f32>) -> [u8; 2] {
    u16o(v.map(|p| (p * 10.0).round().clamp(0.0, 65_534.0) as u16))
}

pub fn encode(s: &FastSample, seq: u32) -> [u8; REC] {
    let mut b = [0u8; REC];
    b[0..2].copy_from_slice(&REC_MAGIC.to_le_bytes());
    b[2] = 1;
    b[4..8].copy_from_slice(&seq.to_le_bytes());
    b[8..16].copy_from_slice(&s.t_ms.to_le_bytes());
    b[16..18].copy_from_slice(&u16o(s.clk_sm));
    b[18..20].copy_from_slice(&u16o(s.clk_mem));
    b[20..22].copy_from_slice(&deci(s.power_w));
    b[22..24].copy_from_slice(&deci(s.power_limit_w));
    b[24] = s.temp.unwrap_or(255);
    b[25] = s.util.unwrap_or(255);
    b[26] = s.pcie_gen.unwrap_or(255);
    b[27] = s.pcie_width.unwrap_or(255);
    b[28..32].copy_from_slice(&s.throttle.unwrap_or(u32::MAX).to_le_bytes());
    let crc = fnv(&b[..REC - 4]);
    b[REC - 4..].copy_from_slice(&crc.to_le_bytes());
    b
}

pub fn decode(b: &[u8]) -> Option<(u32, FastSample)> {
    if b.len() < REC || u16::from_le_bytes([b[0], b[1]]) != REC_MAGIC || b[2] != 1 {
        return None;
    }
    if fnv(&b[..REC - 4]) != u32::from_le_bytes(b[REC - 4..REC].try_into().ok()?) {
        return None;
    }
    let u16_ = |i: usize| Some(u16::from_le_bytes([b[i], b[i + 1]])).filter(|v| *v != u16::MAX);
    let u8_ = |i: usize| Some(b[i]).filter(|v| *v != 255);
    let thr = u32::from_le_bytes(b[28..32].try_into().ok()?);
    Some((
        u32::from_le_bytes(b[4..8].try_into().ok()?),
        FastSample {
            t_ms: i64::from_le_bytes(b[8..16].try_into().ok()?),
            clk_sm: u16_(16),
            clk_mem: u16_(18),
            power_w: u16_(20).map(|v| v as f32 / 10.0),
            power_limit_w: u16_(22).map(|v| v as f32 / 10.0),
            temp: u8_(24),
            util: u8_(25),
            pcie_gen: u8_(26),
            pcie_width: u8_(27),
            throttle: (thr != u32::MAX).then_some(thr),
        },
    ))
}

pub fn ring_path(dir: &Path, which: &str) -> PathBuf {
    dir.join(format!("fast_{which}.bin"))
}

/// Every valid record of a fast ring's bytes, oldest first.
pub fn read_ring_bytes(buf: &[u8]) -> Vec<FastSample> {
    if buf.len() < HEADER || &buf[..8] != MAGIC {
        return vec![];
    }
    let mut v: Vec<(u32, FastSample)> = buf[HEADER..].as_chunks::<REC>().0.iter().filter_map(|c| decode(c)).collect();
    v.sort_by_key(|(seq, _)| *seq);
    v.into_iter().map(|(_, s)| s).collect()
}

pub fn read_ring(path: &Path) -> Vec<FastSample> {
    let Ok(f) = File::open(path) else { return vec![] };
    let mut buf = Vec::new();
    if f.take(MAX_RING_BYTES).read_to_end(&mut buf).is_err() {
        return vec![];
    }
    read_ring_bytes(&buf)
}

/// Both fast rings: ("current", the letter in the recorder's `session.json`) and ("previous").
pub fn read_sessions(dir: &Path) -> Vec<(String, Vec<FastSample>)> {
    let cur = recorder::read_session(dir).map(|s| s.file);
    ["a", "b"]
        .iter()
        .map(|w| {
            let name = if cur.as_deref() == Some(*w) { "current" } else { "previous" };
            (name.to_string(), read_ring(&ring_path(dir, w)))
        })
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

/// The fast ring file of one session.
pub struct FastRing {
    file: File,
    path: PathBuf,
    slots: u32,
    seq: u32,
    pending: Vec<(u32, [u8; REC])>,
    last_write: i64,
    last_sync: i64,
    pub writes: u64,
}

impl FastRing {
    /// Create (or reset) `fast_<which>.bin` in `dir` for `minutes` at `period_ms`.
    pub fn open(dir: &Path, which: &str, minutes: u32, period_ms: u64, now_ms: i64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let slots = ((minutes.clamp(1, 60) as u64 * 60_000) / period_ms.clamp(50, 1000)).clamp(16, 72_000) as u32;
        let path = ring_path(dir, which);
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        file.set_len(0)?;
        file.set_len((HEADER + slots as usize * REC) as u64)?;
        let mut h = [0u8; HEADER];
        h[..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&slots.to_le_bytes());
        h[12..16].copy_from_slice(&(REC as u32).to_le_bytes());
        h[16..20].copy_from_slice(&(period_ms as u32).to_le_bytes());
        h[20..28].copy_from_slice(&now_ms.to_le_bytes());
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&h)?;
        Ok(Self { file, path, slots, seq: 0, pending: vec![], last_write: now_ms, last_sync: now_ms, writes: 0 })
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn push(&mut self, s: &FastSample, now_ms: i64) {
        self.seq = self.seq.wrapping_add(1);
        self.pending.push((self.seq, encode(s, self.seq)));
        if now_ms - self.last_write >= WRITE_EVERY_MS {
            let _ = self.write_pending(now_ms);
        }
    }

    /// Write queued records (one write per contiguous run); flush every `SYNC_EVERY_MS`.
    pub fn write_pending(&mut self, now_ms: i64) -> std::io::Result<()> {
        self.last_write = now_ms;
        let mut i = 0;
        while i < self.pending.len() {
            let start = (self.pending[i].0.wrapping_sub(1) % self.slots) as usize;
            let mut run = Vec::with_capacity(REC * 16);
            let mut slot = start;
            while i < self.pending.len() && (self.pending[i].0.wrapping_sub(1) % self.slots) as usize == slot {
                run.extend_from_slice(&self.pending[i].1);
                i += 1;
                slot += 1;
                if slot == self.slots as usize {
                    break;
                }
            }
            self.file.seek(SeekFrom::Start((HEADER + start * REC) as u64))?;
            self.file.write_all(&run)?;
            self.writes += 1;
        }
        self.pending.clear();
        if now_ms - self.last_sync >= SYNC_EVERY_MS {
            self.last_sync = now_ms;
            self.file.sync_data()?;
        }
        Ok(())
    }

    pub fn close(&mut self, now_ms: i64) {
        let _ = self.write_pending(now_ms);
        let _ = self.file.sync_data();
    }
}

/// What the fast thread saw since the last `take` (one sampler tick, about a second): enough
/// for the job reports to keep the 100 ms peaks without storing the samples.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FastAgg {
    pub n: u32,
    pub peak_power_w: Option<f32>,
    pub peak_power_frac: Option<f32>,
    /// Readings at 98 % of the power limit or more.
    pub at_limit: u32,
    /// Largest rise in power from one reading to the next, W.
    pub max_step_w: Option<f32>,
    pub min_clk_sm: Option<u16>,
    pub max_clk_sm: Option<u16>,
    pub max_temp: Option<u8>,
    /// Union of the clock-limit reason bits seen.
    pub throttle: u32,
    /// Readings with power-cap/brake (`THR_POWER`) or thermal/slowdown (`THR_THERMAL`) bits.
    pub power_thr: u32,
    pub thermal_thr: u32,
    pub min_gen: Option<u8>,
    pub min_width: Option<u8>,
    pub period_ms: u32,
}

fn omax<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y > x { y } else { x }),
        (x, y) => x.or(y),
    }
}
fn omin<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y < x { y } else { x }),
        (x, y) => x.or(y),
    }
}

impl FastAgg {
    pub fn add(&mut self, s: &FastSample, prev: Option<&FastSample>) {
        use crate::telemetry::{THR_POWER, THR_THERMAL};
        self.n += 1;
        self.peak_power_w = omax(self.peak_power_w, s.power_w);
        let frac = s.power_frac();
        self.peak_power_frac = omax(self.peak_power_frac, frac);
        if frac.is_some_and(|f| f >= 0.98) {
            self.at_limit += 1;
        }
        if let (Some(a), Some(b)) = (prev.and_then(|p| p.power_w), s.power_w) {
            self.max_step_w = omax(self.max_step_w, Some(b - a));
        }
        self.min_clk_sm = omin(self.min_clk_sm, s.clk_sm.filter(|c| *c > 0));
        self.max_clk_sm = omax(self.max_clk_sm, s.clk_sm);
        self.max_temp = omax(self.max_temp, s.temp);
        let t = s.throttle.unwrap_or(0);
        self.throttle |= t;
        if t & THR_POWER != 0 {
            self.power_thr += 1;
        }
        if t & THR_THERMAL != 0 {
            self.thermal_thr += 1;
        }
        self.min_gen = omin(self.min_gen, s.pcie_gen);
        self.min_width = omin(self.min_width, s.pcie_width);
    }

    /// Merge another summary into this one (a job's running totals).
    pub fn merge(&mut self, o: &FastAgg) {
        self.n += o.n;
        self.peak_power_w = omax(self.peak_power_w, o.peak_power_w);
        self.peak_power_frac = omax(self.peak_power_frac, o.peak_power_frac);
        self.at_limit += o.at_limit;
        self.max_step_w = omax(self.max_step_w, o.max_step_w);
        self.min_clk_sm = omin(self.min_clk_sm, o.min_clk_sm);
        self.max_clk_sm = omax(self.max_clk_sm, o.max_clk_sm);
        self.max_temp = omax(self.max_temp, o.max_temp);
        self.throttle |= o.throttle;
        self.power_thr += o.power_thr;
        self.thermal_thr += o.thermal_thr;
        self.min_gen = omin(self.min_gen, o.min_gen);
        self.min_width = omin(self.min_width, o.min_width);
        if o.period_ms > 0 {
            self.period_ms = o.period_ms;
        }
    }
}

/// Shared between the fast thread and the sampler.
#[derive(Debug, Default)]
pub struct Shared {
    pub agg: FastAgg,
    pub latest: Option<FastSample>,
    pub total: u64,
    /// Readings that came later than one period after the previous one.
    pub late: u64,
    pub note: Option<String>,
}

/// Where the fast thread reads from: the real NVML, or a fake in tests.
pub trait FastSource {
    fn read(&mut self, idx: u32, t_ms: i64) -> Option<FastSample>;
    /// The GPU session is gone (driver reset): drop it and reopen.
    fn lost(&self) -> bool {
        false
    }
}

impl FastSource for crate::nvml::Nvml {
    fn read(&mut self, idx: u32, t_ms: i64) -> Option<FastSample> {
        let i = idx.min(self.names.len().saturating_sub(1) as u32);
        self.fast_sample(i, t_ms)
    }
    fn lost(&self) -> bool {
        self.lost
    }
}

/// The running fast sampler: stop it with `stop` (or by dropping it).
pub struct FastSampler {
    stop: Arc<AtomicBool>,
    pub gpu: Arc<AtomicU32>,
    pub shared: Arc<Mutex<Shared>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub period_ms: u64,
    pub path: PathBuf,
}

impl FastSampler {
    /// Start the thread. `open` runs on the new thread (each NVML session stays on its thread).
    pub fn start<F>(dir: &Path, which: &str, period_ms: u64, minutes: u32, gpu: u32, mut open: F) -> std::io::Result<Self>
    where
        F: FnMut() -> Option<Box<dyn FastSource>> + Send + 'static,
    {
        let period_ms = period_ms.clamp(50, 1000);
        let now = crate::telemetry::now_ms();
        let mut ring = FastRing::open(dir, which, minutes, period_ms, now)?;
        let path = ring.path().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let gpu = Arc::new(AtomicU32::new(gpu));
        let shared =
            Arc::new(Mutex::new(Shared { agg: FastAgg { period_ms: period_ms as u32, ..Default::default() }, ..Default::default() }));
        let (st, g, sh) = (stop.clone(), gpu.clone(), shared.clone());
        let thread = std::thread::Builder::new().name("tachyon-fast".into()).spawn(move || {
            let period = std::time::Duration::from_millis(period_ms);
            let mut src: Option<Box<dyn FastSource>> = None;
            let mut retry_at = std::time::Instant::now();
            let mut prev: Option<FastSample> = None;
            let mut next = std::time::Instant::now();
            while !st.load(Ordering::Relaxed) {
                let now_i = std::time::Instant::now();
                if src.is_none() && now_i >= retry_at {
                    src = open();
                    if src.is_none() {
                        retry_at = now_i + std::time::Duration::from_secs(5);
                        if let Ok(mut s) = sh.lock() {
                            s.note = Some("No NVIDIA driver answer for fast sampling; retrying every 5 s.".into());
                        }
                    }
                }
                let t = crate::telemetry::now_ms();
                if let Some(s) = src.as_mut() {
                    let r = s.read(g.load(Ordering::Relaxed), t);
                    if s.lost() {
                        src = None;
                        prev = None;
                        retry_at = now_i + std::time::Duration::from_secs(2);
                    } else if let Some(f) = r {
                        ring.push(&f, t);
                        if let Ok(mut s) = sh.lock() {
                            s.agg.add(&f, prev.as_ref());
                            if prev.is_some_and(|p| f.t_ms - p.t_ms > period_ms as i64 * 2) {
                                s.late += 1;
                            }
                            s.latest = Some(f);
                            s.total += 1;
                            s.note = None;
                        }
                        prev = Some(f);
                    }
                }
                // the next reading on a fixed grid; a late one skips ahead instead of bunching up
                next += period;
                let after = std::time::Instant::now();
                if next <= after {
                    next = after + period;
                }
                std::thread::sleep(next - after);
            }
            ring.close(crate::telemetry::now_ms());
        })?;
        Ok(Self { stop, gpu, shared, thread: Some(thread), period_ms, path })
    }

    /// The summary since the last call (and resets it), plus the latest reading.
    pub fn take(&self) -> (FastAgg, Option<FastSample>) {
        match self.shared.lock() {
            Ok(mut s) => {
                let p = s.agg.period_ms;
                let a = std::mem::replace(&mut s.agg, FastAgg { period_ms: p, ..Default::default() });
                (a, s.latest)
            }
            Err(_) => (FastAgg::default(), None),
        }
    }

    pub fn total(&self) -> u64 {
        self.shared.lock().map(|s| s.total).unwrap_or(0)
    }

    pub fn set_gpu(&self, idx: u32) {
        self.gpu.store(idx, Ordering::Relaxed);
    }

    /// Stop the thread and flush its ring.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FastSampler {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The fast readings of the last `secs` seconds up to `stop_ms`, in plain numbers and a sentence:
/// power against the limit (peak, biggest 100 ms jump, time at the limit), temperature, the SM
/// clock (range and its biggest drop), the clock-limit reasons with how long each was set, and
/// the PCIe link. None when there are no readings in that window.
pub fn last_seconds(all: &[FastSample], stop_ms: i64, secs: i64) -> Option<Value> {
    let w: Vec<&FastSample> = all.iter().filter(|s| s.t_ms <= stop_ms && s.t_ms > stop_ms - secs * 1000).collect();
    let last = *w.last()?;
    let first = w[0];
    let period = if w.len() > 1 { ((last.t_ms - first.t_ms) as f64 / (w.len() - 1) as f64).max(1.0) } else { 100.0 };
    let dur = |n: usize| (n as f64 * period / 100.0).round() / 10.0;
    let vals = |f: &dyn Fn(&FastSample) -> Option<f32>| w.iter().filter_map(|s| f(s)).collect::<Vec<f32>>();
    let stat = |v: &[f32]| {
        (!v.is_empty()).then(|| {
            let lo = v.iter().copied().fold(f32::MAX, f32::min);
            let hi = v.iter().copied().fold(f32::MIN, f32::max);
            (lo, v.iter().sum::<f32>() / v.len() as f32, hi)
        })
    };
    let power = stat(&vals(&|s| s.power_w));
    let limit = w.iter().rev().find_map(|s| s.power_limit_w);
    let temp = stat(&vals(&|s| s.temp.map(f32::from)));
    let clk = stat(&vals(&|s| s.clk_sm.map(f32::from)));
    let util = stat(&vals(&|s| s.util.map(f32::from)));
    let mut jump: Option<(f32, i64)> = None;
    let mut drop_: Option<(f32, i64)> = None;
    for p in w.windows(2) {
        if let (Some(a), Some(b)) = (p[0].power_w, p[1].power_w)
            && jump.is_none_or(|j| b - a > j.0)
        {
            jump = Some((b - a, p[1].t_ms));
        }
        if let (Some(a), Some(b)) = (p[0].clk_sm, p[1].clk_sm) {
            let d = a as f32 - b as f32;
            if drop_.is_none_or(|x| d > x.0) {
                drop_ = Some((d, p[1].t_ms));
            }
        }
    }
    let at_limit = w.iter().filter(|s| s.power_frac().is_some_and(|f| f >= 0.98)).count();
    let mut reasons: Vec<(String, f64)> = vec![];
    for bit in 0..16u32 {
        let m = 1u32 << bit;
        let n = w.iter().filter(|s| s.throttle.unwrap_or(0) & m != 0).count();
        if n > 0 {
            for word in crate::telemetry::throttle_words(m) {
                reasons.push((word.to_string(), dur(n)));
            }
        }
    }
    let gens: Vec<u8> = w.iter().filter_map(|s| s.pcie_gen).collect();
    let widths: Vec<u8> = w.iter().filter_map(|s| s.pcie_width).collect();
    let hm = |t: i64| crate::telemetry::Sample { t_ms: t, ..Default::default() }.local().format("%H:%M:%S%.3f").to_string();
    let mut parts = vec![];
    if let Some((lo, _, hi)) = power {
        let of = limit.filter(|l| *l > 0.0).map(|l| format!(" (at most {:.0}% of the {l:.0} W limit)", hi / l * 100.0)).unwrap_or_default();
        parts.push(format!("power {lo:.0}-{hi:.0} W{of}"));
    }
    if let Some((j, t)) = jump.filter(|j| j.0 >= 30.0) {
        parts.push(format!("the biggest jump +{j:.0} W in one reading at {}", hm(t)));
    }
    if let Some((lo, _, hi)) = temp {
        parts.push(format!("{lo:.0}-{hi:.0} °C"));
    }
    if let Some((lo, _, hi)) = clk {
        parts.push(format!("SM clock {lo:.0}-{hi:.0} MHz"));
    }
    let mut sentence = format!(
        "In the last {} s before {} ({} readings every ~{:.0} ms): {}.",
        ((last.t_ms - first.t_ms) as f64 / 1000.0).round(),
        hm(last.t_ms),
        w.len(),
        period,
        if parts.is_empty() { "no GPU values".to_string() } else { parts.join(", ") }
    );
    let notable: Vec<String> = reasons
        .iter()
        .filter(|(r, _)| !r.starts_with("idle") && !r.starts_with("reliability"))
        .map(|(r, s)| format!("{r} for {s} s"))
        .collect();
    if !notable.is_empty() {
        sentence.push_str(&format!(" Clock limits: {}.", notable.join(", ")));
    }
    Some(json!({
        "from": hm(first.t_ms),
        "to": hm(last.t_ms),
        "readings": w.len(),
        "period_ms": period.round(),
        "power_w": power.map(|(lo, avg, hi)| json!({ "min": lo, "avg": avg, "max": hi })),
        "power_limit_w": limit,
        "power_max_pct_of_limit": power.zip(limit).filter(|(_, l)| *l > 0.0).map(|((_, _, hi), l)| (hi / l * 100.0).round()),
        "at_power_limit_s": dur(at_limit),
        "biggest_power_jump": jump.map(|(j, t)| json!({ "w": j, "at": hm(t) })),
        "temp_c": temp.map(|(lo, _, hi)| json!({ "min": lo, "max": hi })),
        "sm_clock_mhz": clk.map(|(lo, avg, hi)| json!({ "min": lo, "avg": avg, "max": hi })),
        "biggest_clock_drop": drop_.filter(|d| d.0 > 0.0).map(|(d, t)| json!({ "mhz": d, "at": hm(t) })),
        "gpu_pct": util.map(|(lo, avg, hi)| json!({ "min": lo, "avg": avg, "max": hi })),
        "clock_limits": reasons.iter().map(|(r, s)| json!({ "reason": r, "seconds": s })).collect::<Vec<_>>(),
        "pcie_link": (!gens.is_empty()).then(|| json!({
            "gen_min": gens.iter().min(), "gen_max": gens.iter().max(),
            "width_min": widths.iter().min(), "width_max": widths.iter().max(),
        })),
        "last": last,
        "sentence": sentence,
    }))
}

/// Short hints for the crash diagnosis from the last seconds (empty when nothing stands out).
pub fn crash_hints(v: &Value) -> Vec<String> {
    let mut h = vec![];
    if let Some(p) = v["power_max_pct_of_limit"].as_f64().filter(|p| *p >= 98.0) {
        h.push(format!("In the last seconds the GPU drew up to {p:.0}% of its power limit (100 ms readings)."));
    }
    if let Some(j) = v["biggest_power_jump"]["w"].as_f64().filter(|j| *j >= 100.0) {
        h.push(format!(
            "A power spike of +{j:.0} W within one 100 ms reading at {}: sudden load steps like this are what trips a weak or overloaded power supply.",
            v["biggest_power_jump"]["at"].as_str().unwrap_or("?")
        ));
    }
    for r in v["clock_limits"].as_array().into_iter().flatten() {
        let name = r["reason"].as_str().unwrap_or("");
        if name.starts_with("power brake") || name.starts_with("hardware slowdown") || name.starts_with("temperature limit (hardware)") {
            h.push(format!("The GPU reported \"{name}\" for {} s just before the stop.", r["seconds"]));
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(i: i64, p: f32) -> FastSample {
        FastSample {
            t_ms: 1_793_000_000_000 + i * 100,
            clk_sm: Some(2800),
            clk_mem: Some(15_000),
            power_w: Some(p),
            power_limit_w: Some(360.0),
            temp: Some(70),
            util: Some(99),
            throttle: Some(0x4),
            pcie_gen: Some(5),
            pcie_width: Some(16),
        }
    }

    #[test]
    fn record_round_trip_and_damage() {
        let s = fs(3, 351.3);
        let b = encode(&s, 9);
        let (seq, d) = decode(&b).unwrap();
        assert_eq!(seq, 9);
        assert_eq!(d.power_w, Some(351.3));
        assert_eq!((d.clk_sm, d.temp, d.throttle, d.pcie_gen, d.pcie_width), (Some(2800), Some(70), Some(4), Some(5), Some(16)));
        let empty = encode(&FastSample { t_ms: 5, ..Default::default() }, 1);
        assert_eq!(decode(&empty).unwrap().1, FastSample { t_ms: 5, ..Default::default() });
        for i in 0..REC {
            let mut bad = b;
            bad[i] ^= 0x10;
            assert!(decode(&bad).is_none(), "byte {i}");
        }
    }

    #[test]
    fn ring_wraps_keeps_the_newest_and_sizes_for_15_min() {
        let d = std::env::temp_dir().join(format!("tachyon_fast_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let r = FastRing::open(&d, "a", 15, 100, 0).unwrap();
        assert_eq!(r.slots(), 9000);
        assert_eq!(std::fs::metadata(r.path()).unwrap().len(), (HEADER + 9000 * REC) as u64, "352 KB");
        let mut r = FastRing::open(&d, "a", 1, 1000, 0).unwrap();
        assert_eq!(r.slots(), 60);
        for i in 0..150 {
            r.push(&fs(i, 100.0 + i as f32), i * 100);
        }
        r.close(20_000);
        let v = read_ring(&d.join("fast_a.bin"));
        assert_eq!(v.len(), 60);
        assert_eq!(v.first().unwrap().t_ms, fs(90, 0.0).t_ms);
        assert_eq!(v.last().unwrap().power_w, Some(249.0));
        assert!(read_sessions(&d).iter().all(|(n, _)| n == "previous"), "no session.json: both are previous");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn last_seconds_finds_the_spike_and_the_reasons() {
        let mut v: Vec<FastSample> = (0..300).map(|i| fs(i, 180.0)).collect();
        v[250].power_w = Some(395.0);
        v[250].throttle = Some(0x4 | 0x80);
        v[251].clk_sm = Some(1900);
        let stop = v.last().unwrap().t_ms;
        let r = last_seconds(&v, stop, 10).unwrap();
        assert_eq!(r["readings"], 100);
        assert_eq!(r["biggest_power_jump"]["w"].as_f64().unwrap().round(), 215.0);
        assert_eq!(r["power_max_pct_of_limit"], 110.0);
        assert_eq!(r["biggest_clock_drop"]["mhz"], 900.0);
        let reasons: Vec<&str> = r["clock_limits"].as_array().unwrap().iter().map(|x| x["reason"].as_str().unwrap()).collect();
        assert!(reasons.iter().any(|x| x.starts_with("power brake")), "{reasons:?}");
        assert!(r["sentence"].as_str().unwrap().contains("power brake"), "{r:#}");
        let h = crash_hints(&r);
        assert!(h.iter().any(|x| x.contains("+215 W")) && h.iter().any(|x| x.contains("power brake")), "{h:?}");
        assert!(last_seconds(&v, v[0].t_ms - 1, 10).is_none());
    }

    #[test]
    fn agg_peaks_and_merge() {
        let mut a = FastAgg::default();
        let s1 = fs(0, 100.0);
        let mut s2 = fs(1, 360.0);
        s2.throttle = Some(0x20);
        s2.clk_sm = Some(2100);
        a.add(&s1, None);
        a.add(&s2, Some(&s1));
        assert_eq!((a.n, a.peak_power_w, a.at_limit, a.max_step_w), (2, Some(360.0), 1, Some(260.0)));
        assert_eq!((a.power_thr, a.thermal_thr, a.min_clk_sm), (1, 1, Some(2100)));
        let mut b = FastAgg::default();
        b.merge(&a);
        b.merge(&a);
        assert_eq!((b.n, b.at_limit, b.throttle), (4, 2, 0x24));
    }

    struct Fake(u32);
    impl FastSource for Fake {
        fn read(&mut self, _: u32, t: i64) -> Option<FastSample> {
            self.0 += 1;
            Some(FastSample { t_ms: t, power_w: Some(self.0 as f32), power_limit_w: Some(360.0), ..Default::default() })
        }
    }

    #[test]
    fn thread_samples_writes_and_stops() {
        let d = std::env::temp_dir().join(format!("tachyon_fast_thr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut f = FastSampler::start(&d, "b", 50, 1, 0, || Some(Box::new(Fake(0)) as Box<dyn FastSource>)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let (agg, latest) = f.take();
        assert!(agg.n >= 5, "{agg:?}");
        assert!(latest.is_some());
        f.stop();
        let v = read_ring(&d.join("fast_b.bin"));
        assert!(v.len() as u64 >= agg.n as u64, "{} {}", v.len(), agg.n);
        assert!(v.windows(2).all(|p| p[1].t_ms >= p[0].t_ms));
        let _ = std::fs::remove_dir_all(&d);
    }
}
