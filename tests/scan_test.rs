//! End-to-end selection over a fake tree laid out like a typical projects folder.
//!
//! Every fixture gets an explicit last-write time. `std::fs::copy` keeps the source's mtime on
//! Windows (CopyFileExW) but not on Linux, and the samples' mtime is the checkout time, so
//! copied fixtures used to look minutes old on Windows CI and fall outside `idle_secs`.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tachyon::{config::Config, scan};

const JOB_LOG_STAGE2: &[u8] = b"> C:\\p\\python.exe C:\\p\\run/stage2.py\nkeyframes [0]\n";
const CHUNKS: [&str; 4] = ["r_0", "r_350", "r_733", "r_1070"];

fn samples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples")
}

/// Write `bytes` to `dst` with last-write time `at` (the same on every OS).
fn put_bytes(dst: &Path, bytes: &[u8], at: SystemTime) {
    std::fs::write(dst, bytes).unwrap();
    let f = std::fs::File::options().write(true).open(dst).unwrap();
    f.set_modified(at).unwrap();
}

/// Copy a sample to `dst` with last-write time `at`.
fn put(sample: &str, dst: &Path, at: SystemTime) {
    put_bytes(dst, &std::fs::read(samples().join(sample)).unwrap(), at);
}

/// `s` seconds ago.
fn ago(s: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(s)
}

fn tree(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("tachyon_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("projects/demo4k/logs")).unwrap();
    std::fs::create_dir_all(root.join("projects/demo4k/work4k/02x_169")).unwrap();
    root
}

fn cfg(root: &Path) -> Config {
    let mut c = Config::default();
    c.render_roots = vec![root.join("projects").display().to_string()];
    c.max_job_secs = 2 * 86400;
    c.stage_names.insert("r".into(), "stage A render".into());
    c.stage_names.insert("p".into(), "stage C post".into());
    c
}

/// The batch log, the job log and the four stage-A chunk logs, with these last-write times.
fn stage_a_tree(m: &Path, batch: SystemTime, job: SystemTime, chunks: SystemTime) {
    put("batch.log", &m.join("logs/batch.log"), batch);
    put_bytes(&m.join("logs/demo4k_02x_169_x.log"), JOB_LOG_STAGE2, job);
    for f in CHUNKS {
        put(&format!("chunks_r/{f}.log"), &m.join(format!("work4k/02x_169/{f}.log")), chunks);
    }
}

#[test]
fn stage_a_chunks_win_over_job_log() {
    let r = tree("a");
    let m = r.join("projects/demo4k");
    stage_a_tree(&m, ago(20), ago(10), ago(1));
    let c = cfg(&r);
    let v = scan::current_job(&scan::discover(&c.render_roots, c.scan_depth), &c);
    assert_eq!(v.job, "demo4k · 02x 169 try 1");
    assert_eq!(v.stage.as_deref(), Some("stage A render · 2/4 chunks"));
    let p = v.progress.unwrap();
    assert_eq!((p.frame, p.total), (876, Some(1420)));
    assert!(!v.idle);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn ffedit_stage_uses_batch_nf() {
    let r = tree("b");
    let m = r.join("projects/demo4k");
    put("batch.log", &m.join("logs/batch.log"), ago(5));
    put("job.log", &m.join("logs/demo4k_02x_169_x.log"), ago(1));
    let c = cfg(&r);
    let v = scan::current_job(&scan::discover(&c.render_roots, c.scan_depth), &c);
    assert_eq!(v.stage.as_deref(), Some("ffedit.exe"));
    let p = v.progress.unwrap();
    assert_eq!((p.frame, p.total, p.fps), (942, Some(1420), Some(84.0)));
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn progress_json_preferred_and_empty_tree_idle() {
    let r = tree("c");
    let m = r.join("projects/demo4k");
    put("job.log", &m.join("logs/demo4k_02x_169_x.log"), ago(2));
    put_bytes(&m.join("logs/progress.json"), br#"{"job":"dm","frame":5,"total":10,"fps":1.0,"started":null}"#, ago(1));
    let c = cfg(&r);
    let v = scan::current_job(&scan::discover(&c.render_roots, c.scan_depth), &c);
    assert_eq!(v.job, "dm");
    assert_eq!(v.progress.unwrap().frame, 5);

    let e = std::env::temp_dir().join(format!("tachyon_empty_{}", std::process::id()));
    std::fs::create_dir_all(&e).unwrap();
    let mut c2 = Config::default();
    c2.render_roots = vec![e.display().to_string()];
    let v = scan::current_job(&scan::discover(&c2.render_roots, 4), &c2);
    assert!(v.idle);
    let _ = std::fs::remove_dir_all(&r);
    let _ = std::fs::remove_dir_all(&e);
}

/// What the windows-msvc CI run saw: copies that kept the checkout mtime (150 s old, beyond
/// `idle_secs`) are not a live render, whatever their content; the open batch shows as quiet.
#[test]
fn old_logs_are_not_live() {
    let r = tree("d");
    let m = r.join("projects/demo4k");
    stage_a_tree(&m, ago(150), ago(150), ago(150));
    let c = cfg(&r);
    let v = scan::current_job(&scan::discover(&c.render_roots, c.scan_depth), &c);
    assert!(v.progress.is_none() && v.stage.is_none(), "{v:?}");
    assert!(v.quiet && !v.idle, "{v:?}");
    let _ = std::fs::remove_dir_all(&r);
}

/// Coarse timestamps (FAT32 keeps 2 s, some shares 1 s): the job log and the chunk logs get the
/// very same mtime, and the stage must not depend on which one the directory lists first.
#[test]
fn equal_mtimes_still_pick_the_chunk_set() {
    let r = tree("e");
    let m = r.join("projects/demo4k");
    let whole = ago(4).duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() / 2 * 2;
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(whole);
    stage_a_tree(&m, t, t, t);
    let c = cfg(&r);
    let mut cands = scan::discover(&c.render_roots, c.scan_depth);
    assert!(cands.windows(2).all(|w| w[0].mtime == w[1].mtime), "fixture: all mtimes equal");
    for rev in [false, true] {
        if rev {
            cands.reverse();
        }
        let v = scan::current_job(&cands, &c);
        assert_eq!(v.stage.as_deref(), Some("stage A render · 2/4 chunks"), "reversed listing: {rev}");
    }
    let _ = std::fs::remove_dir_all(&r);
}
