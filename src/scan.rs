//! Log discovery and read-only tailing.
use crate::parse::{self, Progress};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Open strictly for reading, sharing read/write/delete so the writer is never blocked.
pub fn open_shared(path: &Path) -> std::io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        o.share_mode(0x1 | 0x2 | 0x4);
    }
    o.open(path)
}

/// Last `n` bytes of a file, decoded (UTF-8 or UTF-16LE).
pub fn tail(path: &Path, n: u64) -> std::io::Result<String> {
    let mut f = open_shared(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(n);
    // keep UTF-16 alignment
    let start = start - (start % 2);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.take(n + 2).read_to_end(&mut buf)?;
    Ok(parse::decode(&buf))
}

/// First `n` bytes of a file (for ffmpeg headers).
pub fn head(path: &Path, n: u64) -> std::io::Result<String> {
    let f = open_shared(path)?;
    let mut buf = Vec::new();
    f.take(n).read_to_end(&mut buf)?;
    Ok(parse::decode(&buf))
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub mtime: SystemTime,
}

fn is_candidate(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    name == "progress.json" || name.ends_with("_progress.json") || name.ends_with(".log")
}

/// Recursive walk (depth-limited, capped) collecting candidate files, newest first.
pub fn discover(roots: &[String], depth: usize) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut budget = 50_000usize;
    let mut restat = RESTAT_MAX;
    for r in roots {
        walk(Path::new(r), depth, &mut out, &mut budget, &mut restat);
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.mtime));
    out
}

/// How many candidates per scan get their time from the file itself (see [`mtime_of`]).
const RESTAT_MAX: usize = 2_000;

/// Last-write time of a directory entry. On Windows, `DirEntry::metadata` is the copy NTFS keeps
/// in the directory entry (FindNextFileW data), which is updated lazily while another process
/// holds the file open for writing: a render log being appended can look minutes old there.
/// `fs::metadata` opens the file without blocking the writer and reads the current time.
fn mtime_of(e: &std::fs::DirEntry, p: &Path, restat: &mut usize) -> Option<SystemTime> {
    if cfg!(windows) && *restat > 0 {
        *restat -= 1;
        if let Ok(m) = std::fs::metadata(p).and_then(|m| m.modified()) {
            return Some(m);
        }
    }
    e.metadata().and_then(|m| m.modified()).ok()
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Candidate>, budget: &mut usize, restat: &mut usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            // never descend into the bin or frame dumps
            if depth > 0 && !n.starts_with('.') && n != "frames" && n != "__pycache__" {
                walk(&p, depth - 1, out, budget, restat);
            }
        } else if ft.is_file() && is_candidate(&p) {
            if let Some(m) = mtime_of(&e, &p, restat) {
                out.push(Candidate { path: p, mtime: m });
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct JobView {
    /// Display name, e.g. "demo4k · 02x 169 try 1".
    pub job: String,
    /// Current stage, e.g. "stage A render · 2/4 chunks" or "ffedit.exe".
    pub stage: Option<String>,
    /// File the progress came from (shown on hover).
    pub source: String,
    /// Progress of the current stage (or the whole job for single-stage renders).
    pub progress: Option<Progress>,
    /// Seconds since the newest write under the roots.
    pub age_secs: f64,
    /// Nothing written for `idle_secs` and no batch job open.
    pub idle: bool,
    /// A batch job is open but its logs have been quiet for `idle_secs`.
    pub quiet: bool,
    /// Seconds since the open batch job started.
    pub job_elapsed: Option<f64>,
    /// Estimated total seconds for the open batch job (from the last successful job's s/frame).
    pub job_est_total: Option<f64>,
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Project folder for a path: the folder that holds `logs`, or the parent folder.
fn project_of(p: &Path) -> String {
    let mut d = p.parent();
    while let Some(x) = d {
        let n = name_of(x);
        if n == "logs" || n == "work4k" || n == "work" {
            return x.parent().map(|y| y.file_name().unwrap_or_default().to_string_lossy().into_owned()).unwrap_or_default();
        }
        if x.parent().is_some_and(|y| name_of(y) == "projects" || name_of(y) == "renders") {
            return x.file_name().unwrap_or_default().to_string_lossy().into_owned();
        }
        d = x.parent();
    }
    p.parent().and_then(|d| d.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn job_name_from_path(p: &Path) -> String {
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let proj = project_of(p);
    if proj.is_empty() {
        stem
    } else if stem.eq_ignore_ascii_case("progress") {
        proj
    } else {
        format!("{proj} · {stem}")
    }
}

fn age(m: SystemTime) -> f64 {
    SystemTime::now().duration_since(m).unwrap_or(Duration::ZERO).as_secs_f64()
}

/// Look for a total-frames hint in sibling batch/queue logs or the job's own header.
fn find_total(log: &Path, tail_bytes: u64) -> Option<u64> {
    let dir = log.parent()?;
    for name in ["batch.log", "queue.log"] {
        for d in [Some(dir), dir.parent()].into_iter().flatten() {
            let p = d.join(name);
            if p != log && p.exists() {
                if let Ok(t) = tail(&p, tail_bytes.max(256 * 1024)) {
                    if let Some(n) = parse::batch_state(&t).and_then(|b| b.nf).or_else(|| parse::total_frames(&t)) {
                        return Some(n);
                    }
                }
            }
        }
    }
    let h = head(log, 64 * 1024).ok()?;
    parse::total_frames(&h).or_else(|| parse::ffmpeg_header_total(&h))
}

struct OpenBatch {
    project: String,
    name: String,
    nf: Option<u64>,
    elapsed: Option<f64>,
    est_total: Option<f64>,
}

fn open_batch(cands: &[Candidate], cfg: &crate::config::Config) -> Option<OpenBatch> {
    let c = cands.iter().find(|c| {
        let n = name_of(&c.path);
        (n == "batch.log" || n == "queue.log") && age(c.mtime) < cfg.max_job_secs as f64
    })?;
    let t = tail(&c.path, 256 * 1024).ok()?;
    let b = parse::batch_state(&t)?;
    let name = b.running.clone()?;
    let mdate = chrono::DateTime::<chrono::Local>::from(c.mtime).date_naive();
    let elapsed = b.started.as_ref().and_then(|s| parse::elapsed_since(s, chrono::Local::now().naive_local(), mdate));
    if elapsed.is_some_and(|e| e > cfg.max_job_secs as f64) {
        return None;
    }
    Some(OpenBatch {
        project: project_of(&c.path),
        name,
        nf: b.nf,
        elapsed,
        est_total: match (b.nf, b.last_ok_spf) {
            (Some(n), Some(spf)) => Some(n as f64 * spf),
            _ => None,
        },
    })
}

/// Progress of a parallel chunk set in `dir` with file prefix `pre` (e.g. r_*.log).
fn chunk_set(dir: &Path, pre: &str, total: Option<u64>, cfg: &crate::config::Config) -> Option<(Progress, String)> {
    let mut chunks = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if let Some((p, start)) = parse::chunk_name(&n) {
            if p == pre {
                let t = head(&e.path(), 256 * 1024).unwrap_or_default();
                chunks.push(parse::parse_chunk(start, &t));
            }
        }
    }
    let (done, tot, fin, n) = parse::chunk_progress(&mut chunks, total)?;
    let label = cfg.stage_names.get(pre).cloned().unwrap_or_else(|| format!("{pre}_* chunks"));
    Some((Progress { frame: done, total: Some(tot), fps: None, speed: None }, format!("{label} · {fin}/{n} chunks")))
}

/// Decide what to show. Order: fresh progress.json, then the newest fresh source with
/// parseable progress (chunk set, ffmpeg `frame=`, or `N/M` lines), then stage-only info.
pub fn current_job(cands: &[Candidate], cfg: &crate::config::Config) -> JobView {
    let Some(newest) = cands.first() else {
        return JobView { job: "no logs found".into(), idle: true, ..Default::default() };
    };
    let newest_age = age(newest.mtime);
    let batch = open_batch(cands, cfg);
    let fresh = |c: &Candidate| age(c.mtime) <= cfg.idle_secs as f64;

    let mut v = JobView { age_secs: newest_age, ..Default::default() };
    if let Some(b) = &batch {
        v.job = if b.project.is_empty() { b.name.clone() } else { format!("{} · {}", b.project, b.name) };
        v.job_elapsed = b.elapsed;
        v.job_est_total = b.est_total;
    }

    // 1. fresh progress.json
    for c in cands.iter().take_while(|c| age(c.mtime) <= cfg.progress_json_fresh_secs as f64) {
        if c.path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Some(j) = head(&c.path, 64 * 1024).ok().and_then(|t| parse::progress_json(&t)) {
                v.job = j.job.clone().unwrap_or_else(|| job_name_from_path(&c.path));
                v.source = c.path.display().to_string();
                v.progress = Some(Progress { frame: j.frame, total: j.total, fps: j.fps, speed: None });
                return v;
            }
        }
    }

    // 2. fresh progress sources, newest first
    let mut stage_only: Option<(String, String)> = None;
    let mut seen_sets: Vec<(PathBuf, String)> = Vec::new();
    for c in cands.iter().take_while(|c| fresh(c)).take(40) {
        let n = name_of(&c.path);
        if !n.ends_with(".log") || n == "batch.log" || n == "queue.log" {
            continue;
        }
        let bn = batch.as_ref().filter(|b| b.project == project_of(&c.path)).and_then(|b| b.nf);
        if let Some((pre, _)) = parse::chunk_name(&n) {
            let dir = c.path.parent().unwrap_or(Path::new(".")).to_path_buf();
            if seen_sets.iter().any(|(d, p)| *d == dir && *p == pre) {
                continue;
            }
            seen_sets.push((dir.clone(), pre.clone()));
            if let Some((p, stage)) = chunk_set(&dir, &pre, bn, cfg) {
                if v.job.is_empty() {
                    v.job = job_name_from_path(&dir.join("x"));
                }
                v.source = dir.display().to_string();
                v.stage = Some(stage);
                v.progress = Some(p);
                return v;
            }
            continue;
        }
        let Ok(t) = tail(&c.path, cfg.tail_bytes) else { continue };
        let sec = parse::current_section(&t);
        let stage = parse::current_stage(&t);
        if let Some(mut p) = parse::ffmpeg_progress(sec).or_else(|| parse::slash_progress(sec)) {
            if p.total.is_none() {
                p.total = bn.or_else(|| find_total(&c.path, cfg.tail_bytes));
            }
            if v.job.is_empty() {
                v.job = job_name_from_path(&c.path);
            }
            v.source = c.path.display().to_string();
            v.stage = stage;
            v.progress = Some(p);
            return v;
        }
        if stage_only.is_none() {
            stage_only = Some((stage.unwrap_or_default(), c.path.display().to_string()));
        }
    }

    // 3. nothing with progress
    if let Some((stage, src)) = stage_only {
        if !stage.is_empty() {
            v.stage = Some(stage);
        }
        v.source = src;
    } else {
        v.source = newest.path.display().to_string();
    }
    if v.job.is_empty() {
        v.job = job_name_from_path(&newest.path);
    }
    v.idle = batch.is_none() && newest_age > cfg.idle_secs as f64;
    v.quiet = batch.is_some() && newest_age > cfg.idle_secs as f64;
    v
}

/// Smoothed frames/second from successive observations of the frame counter.
#[derive(Debug, Default)]
pub struct RateMeter {
    key: String,
    samples: std::collections::VecDeque<(f64, u64)>,
}

impl RateMeter {
    /// `t` in seconds (monotonic). Returns fps over the last ~30 s window.
    pub fn push(&mut self, key: &str, t: f64, frame: u64) -> Option<f64> {
        if key != self.key || self.samples.back().is_some_and(|(_, f)| frame < *f) {
            self.key = key.to_string();
            self.samples.clear();
        }
        if self.samples.back().map(|(_, f)| *f) != Some(frame) || self.samples.is_empty() {
            self.samples.push_back((t, frame));
        }
        while self.samples.len() > 2 && t - self.samples.front().unwrap().0 > 30.0 {
            self.samples.pop_front();
        }
        let (t0, f0) = *self.samples.front()?;
        let (t1, f1) = *self.samples.back()?;
        if t1 - t0 >= 3.0 && f1 > f0 { Some((f1 - f0) as f64 / (t1 - t0)) } else { None }
    }
}
