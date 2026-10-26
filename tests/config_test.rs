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

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("tachyon_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn first_start_writes_defaults_and_says_so() {
    let d = scratch("first");
    let p = d.join("tachyon.toml");
    let l = config::load(&p);
    assert!(l.first_run && l.warn.is_none() && l.path == p);
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(!text.contains(r"C:\Users") && !text.contains("/home/"), "no personal paths: {text}");
    assert!(l.cfg.render_roots.is_empty() && !l.cfg.agent_api, "safe defaults");
    let again = config::load(&p);
    assert!(!again.first_run && again.warn.is_none());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn damaged_settings_are_kept_and_replaced_by_defaults() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("syntax", b"poll_ms = [\n".to_vec()),
        ("types", b"poll_ms = \"fast\"\n".to_vec()),
        ("utf8", vec![b'#', 0xff, 0xfe, b'\n', b'x']),
        ("huge", vec![b'#'; (config::MAX_CONFIG_BYTES + 10) as usize]),
    ];
    for (name, body) in cases {
        let d = scratch(&format!("bad_{name}"));
        let p = d.join("tachyon.toml");
        std::fs::write(&p, &body).unwrap();
        let l = config::load(&p);
        let w = l.warn.clone().unwrap_or_default();
        assert!(w.contains("could not be read") && w.contains(".corrupt-"), "{name}: {w}");
        assert!(!l.first_run && l.cfg.poll_ms == Config::default().poll_ms);
        let kept: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1, "{name}");
        assert_eq!(std::fs::read(kept[0].path()).unwrap(), body, "{name}: the damaged file is kept byte for byte");
        let fresh = config::load(&p);
        assert!(fresh.warn.is_none(), "{name}: the defaults written in its place load");
        // a second damage keeps both copies
        std::fs::write(&p, &body).unwrap();
        let _ = config::load(&p);
        let n =
            std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".corrupt-")).count();
        assert_eq!(n, 2, "{name}: nothing is overwritten");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn package_manager_installs_keep_settings_in_the_data_folder() {
    use std::path::Path;
    use tachyon::config::managed_install;
    assert!(managed_install(Path::new(r"C:\Users\ann\AppData\Local\Microsoft\WinGet\Packages\Formray.Tachyon_Microsoft.Winget.Source_8wekyb3d8bbwe\Tachyon")));
    assert!(managed_install(Path::new(r"C:\Program Files\WinGet\Packages\Formray.Tachyon_x\Tachyon")));
    assert!(managed_install(Path::new(r"C:\Users\ann\scoop\apps\tachyon\1.0.1")));
    assert!(managed_install(Path::new(r"C:\ProgramData\scoop\apps\tachyon\current")));
    assert!(!managed_install(Path::new(r"C:\Tools\Tachyon")));
    assert!(!managed_install(Path::new(r"D:\scoop-notes\apps")));
}
