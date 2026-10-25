//! End-to-end selection over a fake tree laid out like a typical projects folder.
use std::path::{Path, PathBuf};
use tachyon::{config::Config, scan};

fn samples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples")
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

#[test]
fn stage_a_chunks_win_over_job_log() {
    let r = tree("a");
    let m = r.join("projects/demo4k");
    std::fs::copy(samples().join("batch.log"), m.join("logs/batch.log")).unwrap();
    std::fs::write(m.join("logs/demo4k_02x_169_x.log"), "> C:\\p\\python.exe C:\\p\\run/stage2.py\nkeyframes [0]\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    for f in ["r_0", "r_350", "r_733", "r_1070"] {
        std::fs::copy(samples().join(format!("chunks_r/{f}.log")), m.join(format!("work4k/02x_169/{f}.log"))).unwrap();
    }
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
    std::fs::copy(samples().join("batch.log"), m.join("logs/batch.log")).unwrap();
    std::fs::copy(samples().join("job.log"), m.join("logs/demo4k_02x_169_x.log")).unwrap();
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
    std::fs::copy(samples().join("job.log"), m.join("logs/demo4k_02x_169_x.log")).unwrap();
    std::fs::write(m.join("logs/progress.json"), r#"{"job":"dm","frame":5,"total":10,"fps":1.0,"started":null}"#).unwrap();
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
