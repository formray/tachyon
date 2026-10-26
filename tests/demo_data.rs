//! Demo data for the screenshots (build box only, ignored by `cargo test`):
//! `TACHYON_DEMO_DIR=<Tachyon data folder> cargo test --test demo_data -- --ignored`
//! writes an unclean previous session that ends two minutes before this machine booted (so the
//! next start reports "Your PC restarted unexpectedly") and three earlier jobs in the history.
use tachyon::history::{self, JobKind, JobRecord};
use tachyon::recorder::Recorder;
use tachyon::telemetry::{Proc, Sample};

fn sample(t_ms: i64, i: i64, n: i64) -> Sample {
    let f = i as f32 / n as f32; // 0..1 over the session
    let hot = f > 0.85;
    Sample {
        t_ms,
        util: Some(if f < 0.05 { 30 } else { 96 + (i % 3) as u8 }),
        mem_ctrl: Some(48),
        clk_sm: Some(if hot { 2310 } else { 2655 }),
        clk_sm_max: Some(3090),
        clk_mem: Some(15001),
        temp: Some((70.0 + 16.0 * f) as u8),
        power_w: Some(330.0 + 25.0 * f),
        power_limit_w: Some(360.0),
        vram_mb: Some(9800.0 + 6400.0 * f),
        vram_total_mb: Some(16303.0),
        throttle: Some(if hot {
            0x20
        } else if f > 0.4 {
            0x4
        } else {
            0
        }),
        pcie_rx: Some(900.0),
        pcie_tx: Some(240.0),
        pcie_max: Some(63_000.0),
        enc: Some(0),
        dec: Some(0),
        fan: Some((55.0 + 30.0 * f) as u8),
        cpu: Some(28.0),
        cpu_core_max: Some(71.0),
        cores: Some(24),
        ram_used_mb: Some(21_500.0 + 6000.0 * f),
        ram_total_mb: Some(32_604.0),
        commit_mb: Some(30_000.0 + 9000.0 * f),
        commit_limit_mb: Some(49_000.0),
        energy_mj: Some(9_000_000_000 + (i as u64) * 340_000),
        procs: vec![
            Proc {
                pid: 7420,
                name: "python.exe".into(),
                category: "AI/agent".into(),
                vram_mb: Some(6200.0 + 6200.0 * f),
                sm: Some(90),
                enc: None,
                dec: None,
            },
            Proc {
                pid: 6144,
                name: "Cyberpunk2077.exe".into(),
                category: "Game".into(),
                vram_mb: Some(2100.0),
                sm: Some(0),
                enc: None,
                dec: None,
            },
            Proc {
                pid: 3120,
                name: "chrome.exe".into(),
                category: "Browser".into(),
                vram_mb: Some(740.0),
                sm: Some(1),
                enc: None,
                dec: None,
            },
        ],
    }
}

fn job(name: &str, kind: JobKind, start_ms: i64, mins: f64, gpu: f32, wh: f64, power_thr_min: f64, bottleneck: &str) -> JobRecord {
    JobRecord {
        kind,
        key: history::job_key(kind, name),
        name: name.into(),
        start_ms,
        end_ms: start_ms + (mins * 60_000.0) as i64,
        duration_s: mins * 60.0,
        avg_gpu: Some(gpu),
        peak_gpu: Some(99),
        avg_fps: None,
        energy_wh: Some(wh),
        energy_source: Some("counter".into()),
        peak_vram_mb: Some(13_900.0),
        peak_temp: Some(79),
        power_throttled_s: power_thr_min * 60.0,
        thermal_throttled_s: 0.0,
        bottleneck: Some(bottleneck.into()),
        cost: Some((wh / 1000.0 * 0.30 * 100.0).round() / 100.0),
        currency: Some("EUR".into()),
        vs_last: None,
        profile: Default::default(),
    }
}

#[test]
#[ignore]
fn write_demo_data() {
    let dir = std::path::PathBuf::from(std::env::var("TACHYON_DEMO_DIR").expect("set TACHYON_DEMO_DIR"));
    let bb = dir.join("blackbox");
    let _ = std::fs::remove_dir_all(&bb);
    let boot = tachyon::sysinfo::boot_time_ms().expect("boot time");
    let n = 600; // ten minutes at one sample a second
    let end = boot - 120_000;
    let start = end - (n - 1) * 1000;
    let (mut rec, _) = Recorder::open(&bb, 15, 1000, start).unwrap();
    for i in 0..n {
        rec.push(&sample(start + i * 1000, i, n), start + i * 1000);
    }
    rec.write_pending(end).unwrap(); // flushed, never closed: the session stays unclean
    drop(rec);

    let h = dir.join("history");
    let _ = std::fs::remove_dir_all(&h);
    let day = 86_400_000;
    let base = end - 2 * day;
    for j in [
        job("02x 169 try 1", JobKind::Render, base, 21.0, 94.0, 118.0, 1.0, "GPU-bound (91%)"),
        job("python.exe", JobKind::Gpu, base + 3 * 3_600_000, 47.0, 88.0, 236.0, 9.0, "GPU-bound at power cap (64%)"),
        job("02x 169 try 2", JobKind::Render, base + day, 24.0, 95.0, 139.0, 6.0, "GPU-bound at power cap (58%)"),
    ] {
        history::record(&h, j).unwrap();
    }
    println!("demo data in {}", dir.display());
}
