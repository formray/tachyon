//! Config defaults, the example render toml, threshold colours and window persistence.
use std::path::PathBuf;
use tachyon::config::{self, Config, Level, WindowCfg};

#[test]
fn friend_defaults_are_a_pure_gpu_monitor() {
    let c = Config::default();
    assert!(c.render_roots.is_empty(), "no render panel by default");
    assert_eq!(c.render_visible_secs, 120);
    assert_eq!(c.poll_ms, 1000);
    // v3: no utilisation band colouring unless util_band_* is set
    assert_eq!((c.gpu.util_band_low, c.gpu.util_band_high), (None, None));
    assert!(!c.gpu.util_below_band_amber);
    assert_eq!((c.gpu.temp_amber_c, c.gpu.temp_red_c), (80.0, 83.0));
    assert_eq!((c.gpu.vram_amber_mb, c.gpu.vram_red_mb), (14500.0, 15600.0));
    assert!(c.window.always_on_top);
    assert!(c.stage_names.is_empty() && !c.process_cmdline && c.pcie);
    assert_eq!(c.process_refresh_secs, 4);
    assert_eq!(c.process_rules.len(), 6);
    // no personal paths anywhere in the defaults
    let text = toml::to_string_pretty(&c).unwrap();
    assert!(!text.contains(r"C:\Users"), "{text}");
}

#[test]
fn poll_interval_is_clamped() {
    let mut c = Config::default();
    c.poll_ms = 10;
    assert_eq!(c.poll_interval().as_millis(), 250);
    c.poll_ms = 99999;
    assert_eq!(c.poll_interval().as_millis(), 2000);
}

#[test]
fn example_toml_and_v1_roots_alias() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/tachyon_render.toml");
    let c: Config = toml::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    assert_eq!(c.render_roots, vec![r"C:\work\projects".to_string(), r"C:\work\renders".to_string()]);
    assert_eq!((c.gpu.util_band_low, c.gpu.util_band_high), (Some(77.0), Some(82.0)));
    assert!(c.gpu.util_below_band_amber && c.glass == config::GlassMode::Auto);
    assert_eq!(c.start_mode(), config::UiMode::Island);
    // below-band amber only while a render is active
    assert_eq!(c.gpu.util_level_ctx(20.0, true), Level::Amber);
    assert_ne!(c.gpu.util_level_ctx(20.0, false), Level::Amber);
    assert_eq!(c.gpu.util_level(80.0), Level::Ok);
    assert_eq!((c.gpu.temp_amber_c, c.gpu.temp_red_c), (80.0, 83.0));
    assert_eq!(c.stage_names.get("r").map(String::as_str), Some("stage A render"));
    assert_eq!(c.process_rules, tachyon::config::default_process_rules());
    assert_eq!(c.process_hide, tachyon::config::default_process_hide());
    // a v1 file used `roots`
    let v1: Config = toml::from_str(r"roots = ['D:\x']").unwrap();
    assert_eq!(v1.render_roots, vec![r"D:\x".to_string()]);
}

#[test]
fn threshold_levels() {
    let t0 = Config::default().gpu;
    for u in [0.0, 20.0, 80.0, 100.0] {
        assert_eq!(t0.util_level(u), Level::Ok, "no band by default");
    }
    let mut t = t0.clone();
    t.util_band_low = Some(77.0);
    t.util_band_high = Some(82.0);
    assert_eq!(t.util_level(80.0), Level::Ok);
    assert_eq!(t.util_level(90.0), Level::Amber);
    assert_eq!(t.util_level(20.0), Level::Neutral);
    let mut t2 = t.clone();
    t2.util_below_band_amber = true;
    assert_eq!(t2.util_level(20.0), Level::Amber);
    assert_eq!(t.temp_level(79.0), Level::Ok);
    assert_eq!(t.temp_level(80.0), Level::Amber);
    assert_eq!(t.temp_level(83.0), Level::Red);
    // 16 GB card: absolute MB thresholds win
    assert_eq!(t.vram_level(14400.0, 16303.0), Level::Ok);
    assert_eq!(t.vram_level(14500.0, 16303.0), Level::Amber);
    assert_eq!(t.vram_level(15600.0, 16303.0), Level::Red);
    // 8 GB card: % thresholds win (90% / 97%)
    assert_eq!(t.vram_level(7000.0, 8192.0), Level::Ok);
    assert_eq!(t.vram_level(7400.0, 8192.0), Level::Amber);
    assert_eq!(t.vram_level(8000.0, 8192.0), Level::Red);
    assert_eq!(t.power_level(350.0, 360.0), Level::Ok);
    assert_eq!(t.power_level(355.0, 360.0), Level::Amber);
}

#[test]
fn window_state_saved_without_touching_comments() {
    let text = "# my comment\nrender_roots = []\n\n[gpu]\ntemp_red_c = 85 # keep\n";
    let w = WindowCfg { x: Some(120.4), y: Some(80.0), compact: true, ..Default::default() };
    let out = config::with_window(text, &w).unwrap();
    assert!(out.contains("# my comment") && out.contains("temp_red_c = 85 # keep"));
    let c: Config = toml::from_str(&out).unwrap();
    assert_eq!(c.window.x, Some(120.0));
    assert!(c.window.compact);
    assert_eq!(c.gpu.temp_red_c, 85.0);
    // writes go only to the given path
    let dir = std::env::temp_dir().join(format!("rigcfg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("rig_monitor.toml");
    let (c, warn) = config::load_or_create(&p);
    assert!(warn.is_none() && p.exists() && c.render_roots.is_empty());
    config::save_window(&p, &w);
    let (c2, _) = config::load_or_create(&p);
    assert_eq!(c2.window.y, Some(80.0));
    let _ = std::fs::remove_dir_all(&dir);
}
