//! Flight recorder ("black box"): the last 10-15 minutes of samples on disk, so they survive a
//! hard crash or a sudden restart. Two preallocated ring files take turns, one per session, so
//! the previous session's data is still there when Tachyon starts again. Records are fixed-size
//! (192 bytes, checksummed), written every few seconds in one go; the data is flushed to disk
//! every 30 s, never per record. A small `session.json` marks whether the last session ended
//! cleanly. Nothing is kept in memory: exports read the ring file back.
use crate::telemetry::{Proc, Sample};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const REC: usize = 192;
const HEADER: usize = 64;
const MAGIC: &[u8; 8] = b"TACHYBB1";
const REC_MAGIC: u16 = 0x4254;
const PROC_SLOTS: usize = 3;
const NAME_LEN: usize = 24;
/// Pending records are written this often.
pub const WRITE_EVERY_MS: i64 = 5_000;
/// ... and flushed to the disk this often.
pub const SYNC_EVERY_MS: i64 = 30_000;
const CATEGORIES: [&str; 7] = ["Game", "Creative app", "Script/Python", "Browser", "AI/agent", "System", "Other"];

fn fnv(b: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for x in b {
        h ^= *x as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

struct W<'a>(&'a mut [u8], usize);
impl W<'_> {
    fn put(&mut self, b: &[u8]) {
        self.0[self.1..self.1 + b.len()].copy_from_slice(b);
        self.1 += b.len();
    }
    fn u8o(&mut self, v: Option<u8>) {
        self.put(&[v.unwrap_or(255)]);
    }
    fn f8(&mut self, v: Option<f32>) {
        self.u8o(v.map(|x| x.round().clamp(0.0, 254.0) as u8));
    }
    fn u16o(&mut self, v: Option<u16>) {
        self.put(&v.unwrap_or(u16::MAX).to_le_bytes());
    }
    fn u32o(&mut self, v: Option<u32>) {
        self.put(&v.unwrap_or(u32::MAX).to_le_bytes());
    }
    fn f32mb(&mut self, v: Option<f32>) {
        self.u32o(v.map(|x| x.round().clamp(0.0, (u32::MAX - 1) as f32) as u32));
    }
}

struct Rd<'a>(&'a [u8], usize);
impl Rd<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let s = &self.0[self.1..self.1 + n];
        self.1 += n;
        s
    }
    fn u8o(&mut self) -> Option<u8> {
        let v = self.take(1)[0];
        (v != 255).then_some(v)
    }
    fn f8(&mut self) -> Option<f32> {
        self.u8o().map(f32::from)
    }
    fn u16o(&mut self) -> Option<u16> {
        let v = u16::from_le_bytes(self.take(2).try_into().unwrap());
        (v != u16::MAX).then_some(v)
    }
    fn u32o(&mut self) -> Option<u32> {
        let v = u32::from_le_bytes(self.take(4).try_into().unwrap());
        (v != u32::MAX).then_some(v)
    }
    fn f32mb(&mut self) -> Option<f32> {
        self.u32o().map(|v| v as f32)
    }
}

/// Encode one sample (the top three processes only).
pub fn encode(s: &Sample, seq: u32) -> [u8; REC] {
    let mut b = [0u8; REC];
    let mut w = W(&mut b, 0);
    w.put(&REC_MAGIC.to_le_bytes());
    w.put(&[1, 0]);
    w.put(&seq.to_le_bytes());
    w.put(&s.t_ms.to_le_bytes());
    w.u8o(s.util);
    w.u8o(s.mem_ctrl);
    w.u8o(s.temp);
    w.u8o(s.fan);
    w.u8o(s.enc);
    w.u8o(s.dec);
    w.f8(s.cpu);
    w.f8(s.cpu_core_max);
    w.u16o(s.clk_sm);
    w.u16o(s.clk_mem);
    w.u16o(s.power_w.map(|p| (p * 10.0).round().clamp(0.0, 65534.0) as u16));
    w.u16o(s.power_limit_w.map(|p| p.round().clamp(0.0, 65534.0) as u16));
    w.f32mb(s.vram_mb);
    w.f32mb(s.vram_total_mb);
    w.u32o(s.throttle);
    w.f32mb(s.pcie_rx);
    w.f32mb(s.pcie_tx);
    w.f32mb(s.ram_used_mb);
    w.f32mb(s.ram_total_mb);
    w.f32mb(s.commit_mb);
    w.f32mb(s.commit_limit_mb);
    w.f32mb(s.pcie_max);
    w.put(&s.energy_mj.unwrap_or(u64::MAX).to_le_bytes());
    w.u16o(s.clk_sm_max);
    w.u16o(s.cores);
    debug_assert_eq!(w.1, 84);
    for i in 0..PROC_SLOTS {
        match s.procs.get(i) {
            Some(p) => {
                w.put(&p.pid.to_le_bytes());
                w.f32mb(p.vram_mb);
                w.u8o(p.sm);
                w.u8o(p.enc);
                w.u8o(p.dec);
                let cat = CATEGORIES.iter().position(|c| c.eq_ignore_ascii_case(&p.category)).map(|i| i as u8 + 1).unwrap_or(0);
                w.put(&[cat]);
                let mut name = [0u8; NAME_LEN];
                let mut n = p.name.len().min(NAME_LEN);
                while !p.name.is_char_boundary(n) {
                    n -= 1;
                }
                name[..n].copy_from_slice(&p.name.as_bytes()[..n]);
                w.put(&name);
            }
            None => w.put(&[0u8; 12 + NAME_LEN]),
        }
    }
    let crc = fnv(&b[..REC - 4]);
    b[REC - 4..].copy_from_slice(&crc.to_le_bytes());
    b
}

/// Decode one record; None if empty or damaged.
pub fn decode(b: &[u8]) -> Option<(u32, Sample)> {
    if b.len() < REC || u16::from_le_bytes([b[0], b[1]]) != REC_MAGIC {
        return None;
    }
    if fnv(&b[..REC - 4]) != u32::from_le_bytes(b[REC - 4..REC].try_into().ok()?) {
        return None;
    }
    let mut r = Rd(b, 4);
    let seq = u32::from_le_bytes(r.take(4).try_into().ok()?);
    let t_ms = i64::from_le_bytes(r.take(8).try_into().ok()?);
    let mut s = Sample {
        t_ms,
        util: r.u8o(),
        mem_ctrl: r.u8o(),
        temp: r.u8o(),
        fan: r.u8o(),
        enc: r.u8o(),
        dec: r.u8o(),
        cpu: r.f8(),
        cpu_core_max: r.f8(),
        clk_sm: r.u16o(),
        clk_mem: r.u16o(),
        power_w: r.u16o().map(|p| p as f32 / 10.0),
        power_limit_w: r.u16o().map(f32::from),
        vram_mb: r.f32mb(),
        vram_total_mb: r.f32mb(),
        throttle: r.u32o(),
        pcie_rx: r.f32mb(),
        pcie_tx: r.f32mb(),
        ram_used_mb: r.f32mb(),
        ram_total_mb: r.f32mb(),
        commit_mb: r.f32mb(),
        commit_limit_mb: r.f32mb(),
        pcie_max: r.f32mb(),
        ..Default::default()
    };
    let e = u64::from_le_bytes(r.take(8).try_into().ok()?);
    s.energy_mj = (e != u64::MAX).then_some(e);
    s.clk_sm_max = r.u16o();
    s.cores = r.u16o();
    for _ in 0..PROC_SLOTS {
        let pid = u32::from_le_bytes(r.take(4).try_into().ok()?);
        let vram = r.f32mb();
        let (sm, enc, dec) = (r.u8o(), r.u8o(), r.u8o());
        let cat = r.take(1)[0];
        let raw = r.take(NAME_LEN);
        let name = String::from_utf8_lossy(&raw[..raw.iter().position(|c| *c == 0).unwrap_or(NAME_LEN)]).into_owned();
        if pid == 0 && name.is_empty() {
            continue;
        }
        let category = if cat == 0 { "Other" } else { CATEGORIES.get(cat as usize - 1).copied().unwrap_or("Other") };
        s.procs.push(Proc { pid, name, category: category.into(), vram_mb: vram, sm, enc, dec });
    }
    Some((seq, s))
}

/// What `session.json` says about a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: String,
    pub pid: u32,
    /// "a" or "b": which ring file the session writes.
    pub file: String,
    pub started_ms: i64,
    pub clean: bool,
    #[serde(default)]
    pub ended_ms: Option<i64>,
}

/// The previous session, read at start.
#[derive(Debug, Clone)]
pub struct PrevSession {
    pub session: Session,
    /// Oldest first.
    pub samples: Vec<Sample>,
}

impl PrevSession {
    pub fn last_ms(&self) -> Option<i64> {
        self.samples.last().map(|s| s.t_ms)
    }
}

fn ring_path(dir: &Path, which: &str) -> PathBuf {
    dir.join(format!("blackbox_{which}.bin"))
}

/// Read every valid record of a ring file, oldest first.
pub fn read_ring(path: &Path) -> Vec<Sample> {
    let Ok(mut f) = File::open(path) else { return vec![] };
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() || buf.len() < HEADER || &buf[..8] != MAGIC {
        return vec![];
    }
    let mut v: Vec<(u32, Sample)> = buf[HEADER..].as_chunks::<REC>().0.iter().filter_map(|c| decode(c)).collect();
    v.sort_by_key(|(seq, _)| *seq);
    v.into_iter().map(|(_, s)| s).collect()
}

/// Samples of the session currently written (or last written) in `dir`.
pub fn read_current(dir: &Path) -> Vec<Sample> {
    match read_session(dir) {
        Some(s) => read_ring(&ring_path(dir, &s.file)),
        None => vec![],
    }
}

pub fn read_session(dir: &Path) -> Option<Session> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("session.json")).ok()?).ok()
}

fn write_session(dir: &Path, s: &Session) -> std::io::Result<()> {
    let tmp = dir.join("session.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(s).unwrap_or_default())?;
    std::fs::rename(tmp, dir.join("session.json"))
}

pub struct Recorder {
    dir: PathBuf,
    file: File,
    session: Session,
    slots: u32,
    seq: u32,
    pending: Vec<(u32, [u8; REC])>,
    last_write: i64,
    last_sync: i64,
    pub writes: u64,
}

impl Recorder {
    /// Open the recorder in `dir` (created if needed) for `minutes` of samples at `poll_ms`.
    /// Returns the previous session (if any) before its file can be overwritten.
    pub fn open(dir: &Path, minutes: u32, poll_ms: u64, now_ms: i64) -> std::io::Result<(Self, Option<PrevSession>)> {
        std::fs::create_dir_all(dir)?;
        let prev = read_session(dir).map(|s| {
            let samples = read_ring(&ring_path(dir, &s.file));
            PrevSession { session: s, samples }
        });
        let which = if prev.as_ref().is_some_and(|p| p.session.file == "a") { "b" } else { "a" };
        let slots = ((minutes.clamp(1, 60) as u64 * 60_000) / poll_ms.max(250)).clamp(16, 14_400) as u32;
        let path = ring_path(dir, which);
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        let size = (HEADER + slots as usize * REC) as u64;
        // a fresh ring every session: old records must not mix with new ones
        file.set_len(0)?;
        file.set_len(size)?;
        let mut h = [0u8; HEADER];
        h[..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&slots.to_le_bytes());
        h[12..16].copy_from_slice(&(REC as u32).to_le_bytes());
        h[16..20].copy_from_slice(&(poll_ms as u32).to_le_bytes());
        h[20..28].copy_from_slice(&now_ms.to_le_bytes());
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&h)?;
        let session = Session {
            version: env!("CARGO_PKG_VERSION").into(),
            pid: std::process::id(),
            file: which.into(),
            started_ms: now_ms,
            clean: false,
            ended_ms: None,
        };
        write_session(dir, &session)?;
        let me = Self { dir: dir.into(), file, session, slots, seq: 0, pending: vec![], last_write: now_ms, last_sync: now_ms, writes: 0 };
        Ok((me, prev))
    }

    pub fn path(&self) -> PathBuf {
        ring_path(&self.dir, &self.session.file)
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }

    /// Queue a sample; pending samples are written every `WRITE_EVERY_MS`.
    pub fn push(&mut self, s: &Sample, now_ms: i64) {
        self.seq = self.seq.wrapping_add(1);
        self.pending.push((self.seq, encode(s, self.seq)));
        if now_ms - self.last_write >= WRITE_EVERY_MS {
            let _ = self.write_pending(now_ms);
        }
    }

    /// Write queued records (contiguous runs in one write each). Flushes to disk every 30 s.
    pub fn write_pending(&mut self, now_ms: i64) -> std::io::Result<()> {
        self.last_write = now_ms;
        let mut i = 0;
        while i < self.pending.len() {
            let start_slot = (self.pending[i].0.wrapping_sub(1) % self.slots) as usize;
            let mut run = Vec::with_capacity(REC * 8);
            let mut slot = start_slot;
            while i < self.pending.len() && (self.pending[i].0.wrapping_sub(1) % self.slots) as usize == slot {
                run.extend_from_slice(&self.pending[i].1);
                i += 1;
                slot += 1;
                if slot == self.slots as usize {
                    break;
                }
            }
            self.file.seek(SeekFrom::Start((HEADER + start_slot * REC) as u64))?;
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

    /// Clean exit: write everything, flush, mark the session clean.
    pub fn close(&mut self, now_ms: i64) {
        let _ = self.write_pending(now_ms);
        let _ = self.file.sync_data();
        self.session.clean = true;
        self.session.ended_ms = Some(now_ms);
        let _ = write_session(&self.dir, &self.session);
    }

    /// Everything recorded this session so far (writes pending records first).
    pub fn window(&mut self, now_ms: i64) -> Vec<Sample> {
        let _ = self.write_pending(now_ms);
        read_ring(&self.path())
    }
}

/// CSV of samples, one row each (process columns: the top three).
pub fn to_csv(samples: &[Sample]) -> String {
    let mut o = String::from(
        "time,gpu_pct,mem_ctrl_pct,sm_mhz,mem_mhz,temp_c,power_w,power_limit_w,vram_mb,vram_total_mb,throttle_mask,pcie_rx_mbs,pcie_tx_mbs,enc_pct,dec_pct,fan_pct,cpu_pct,cpu_busiest_core_pct,ram_used_mb,ram_total_mb,commit_mb,commit_limit_mb,energy_mj,proc1,proc1_vram_mb,proc1_sm_pct,proc2,proc2_vram_mb,proc2_sm_pct,proc3,proc3_vram_mb,proc3_sm_pct\n",
    );
    fn o_<T: std::fmt::Display>(v: Option<T>) -> String {
        v.map(|x| x.to_string()).unwrap_or_default()
    }
    for s in samples {
        let mut row = vec![
            s.local().format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            o_(s.util),
            o_(s.mem_ctrl),
            o_(s.clk_sm),
            o_(s.clk_mem),
            o_(s.temp),
            o_(s.power_w.map(|p| format!("{p:.1}"))),
            o_(s.power_limit_w),
            o_(s.vram_mb),
            o_(s.vram_total_mb),
            o_(s.throttle.map(|t| format!("0x{t:x}"))),
            o_(s.pcie_rx),
            o_(s.pcie_tx),
            o_(s.enc),
            o_(s.dec),
            o_(s.fan),
            o_(s.cpu),
            o_(s.cpu_core_max),
            o_(s.ram_used_mb),
            o_(s.ram_total_mb),
            o_(s.commit_mb),
            o_(s.commit_limit_mb),
            o_(s.energy_mj),
        ];
        for i in 0..PROC_SLOTS {
            match s.procs.get(i) {
                Some(p) => {
                    row.push(p.name.replace([',', '"'], "_"));
                    row.push(o_(p.vram_mb));
                    row.push(o_(p.sm));
                }
                None => row.extend([String::new(), String::new(), String::new()]),
            }
        }
        o += &row.join(",");
        o.push('\n');
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tachyon_bb_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    pub fn sample(i: i64) -> Sample {
        Sample {
            t_ms: 1_700_000_000_000 + i * 1000,
            util: Some((i % 100) as u8),
            temp: Some(70),
            power_w: Some(312.4),
            power_limit_w: Some(360.0),
            vram_mb: Some(11000.0 + i as f32),
            vram_total_mb: Some(16303.0),
            throttle: Some(0x4),
            cpu: Some(23.6),
            energy_mj: Some(123_456_789),
            procs: vec![Proc {
                pid: 4242,
                name: "python.exe".into(),
                category: "AI/agent".into(),
                vram_mb: Some(9216.0),
                sm: Some(71),
                enc: None,
                dec: Some(0),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn record_round_trip_and_damage() {
        let s = sample(5);
        let b = encode(&s, 77);
        let (seq, d) = decode(&b).unwrap();
        assert_eq!(seq, 77);
        assert_eq!(d.t_ms, s.t_ms);
        assert_eq!((d.util, d.temp, d.throttle, d.energy_mj), (s.util, s.temp, s.throttle, s.energy_mj));
        assert_eq!(d.power_w, Some(312.4));
        assert_eq!(d.cpu, Some(24.0), "CPU stored as whole percent");
        assert_eq!(d.procs, s.procs);
        assert_eq!(d.clk_sm, None, "missing stays missing");
        let mut bad = b;
        bad[20] ^= 1;
        assert!(decode(&bad).is_none(), "checksum");
        assert!(decode(&[0u8; REC]).is_none(), "empty slot");
        // long names are cut at a character boundary
        let mut s2 = sample(1);
        s2.procs[0].name = "ééééééééééééééé.exe".into();
        let (_, d2) = decode(&encode(&s2, 1)).unwrap();
        assert!(d2.procs[0].name.len() <= NAME_LEN && d2.procs[0].name.starts_with("éé"));
    }

    #[test]
    fn ring_wraps_and_keeps_the_latest() {
        let d = tmp("wrap");
        let (mut r, prev) = Recorder::open(&d, 1, 1000, 0).unwrap();
        assert!(prev.is_none());
        assert_eq!(r.slots(), 60);
        for i in 0..150 {
            r.push(&sample(i), i * 1000);
        }
        let w = r.window(150_000);
        assert_eq!(w.len(), 60);
        assert_eq!(w.first().unwrap().t_ms, sample(90).t_ms);
        assert_eq!(w.last().unwrap().t_ms, sample(149).t_ms);
        assert!(r.writes < 150 / 4, "batched writes, got {}", r.writes);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unclean_session_survives_the_next_start() {
        let d = tmp("crash");
        {
            let (mut r, _) = Recorder::open(&d, 15, 1000, 1000).unwrap();
            for i in 0..40 {
                r.push(&sample(i), 1000 + i * 1000);
            }
            // simulated hard crash: no close(); only what was written every 5 s is on disk
        }
        let (mut r2, prev) = Recorder::open(&d, 15, 1000, 99_000).unwrap();
        let p = prev.unwrap();
        assert!(!p.session.clean);
        assert_eq!(p.session.file, "a");
        assert!(p.samples.len() >= 35 && p.samples.len() <= 40, "{}", p.samples.len());
        assert_eq!(read_session(&d).unwrap().file, "b", "new session uses the other file");
        r2.push(&sample(1), 100_000);
        r2.close(101_000);
        let (_r3, prev3) = Recorder::open(&d, 15, 1000, 200_000).unwrap();
        let p3 = prev3.unwrap();
        assert!(p3.session.clean);
        assert_eq!(p3.samples.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn csv_has_a_row_per_sample() {
        let c = to_csv(&[sample(1), sample(2)]);
        let lines: Vec<&str> = c.lines().collect();
        assert_eq!(lines.len(), 3);
        let cols = lines[0].split(',').count();
        assert!(lines[1..].iter().all(|l| l.split(',').count() == cols));
        assert!(lines[1].contains(",0x4,") && lines[1].contains(",python.exe,9216,71"));
    }
}
