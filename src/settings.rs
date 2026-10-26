//! The Settings page's model: which `tachyon.toml` keys it edits, their limits, what applies live
//! and what needs a restart, and how the file is saved. Saving edits only the keys that changed
//! (comments, unknown keys and everything else stay as they were), writes a temporary file and
//! renames it over `tachyon.toml`, and keeps the file as it was before as `tachyon.toml.bak`.
//! Nothing here touches the GPU: it only edits Tachyon's own settings file.
use crate::config::{Config, HEADER, UiMode};
use std::path::{Path, PathBuf};

/// Refresh rates offered (ms). `poll_ms` is clamped to 250..=2000 anyway.
pub const POLL_STEPS: [u64; 5] = [250, 500, 1000, 1500, 2000];
/// Bubble auto-close choices (s); 0 = stays open until clicked.
pub const BUBBLE_STEPS: [u64; 5] = [5, 8, 15, 30, 0];
/// VRAM forecast horizons (min); 0 = no forecast warning.
pub const FORECAST_STEPS: [u32; 6] = [5, 10, 15, 30, 60, 0];
/// Flight recorder lengths (min).
pub const BLACKBOX_STEPS: [u32; 5] = [5, 10, 15, 30, 60];
pub const CURRENCIES: [&str; 6] = ["EUR", "USD", "GBP", "CHF", "JPY", "SEK"];
/// At most this many project folders.
pub const MAX_ROOTS: usize = 12;

/// Numeric limits (inclusive) and step for the steppers.
pub struct Limit {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}
pub const TEMP: Limit = Limit { min: 50.0, max: 100.0, step: 1.0 };
pub const VRAM_MB: Limit = Limit { min: 512.0, max: 98304.0, step: 256.0 };
pub const PRICE: Limit = Limit { min: 0.0, max: 5.0, step: 0.01 };

impl Limit {
    /// Clamp to the range and round to the step.
    pub fn fit(&self, v: f64) -> f64 {
        let v = if v.is_finite() { v } else { self.min };
        let r = ((v - self.min) / self.step).round() * self.step + self.min;
        (r.clamp(self.min, self.max) * 1000.0).round() / 1000.0
    }
    /// Clamp only (a value typed into the file keeps its exact number).
    pub fn clamp(&self, v: f64) -> f64 {
        if v.is_finite() { v.clamp(self.min, self.max) } else { self.min }
    }
    pub fn bump(&self, v: f64, by: i32) -> f64 {
        self.fit(v + self.step * by as f64)
    }
}

/// Next/previous entry of a list of choices (wraps; an unlisted value jumps to the nearest).
pub fn cycle<T: Copy + PartialEq + PartialOrd>(steps: &[T], cur: T, by: i32) -> T {
    let i = steps.iter().position(|s| *s == cur).unwrap_or_else(|| steps.iter().position(|s| *s >= cur).unwrap_or(0));
    let n = steps.len() as i32;
    steps[((i as i32 + by).rem_euclid(n)) as usize]
}

/// Next/previous entry without wrapping (steppers stop at the ends).
pub fn cycle_clamped<T: Copy + PartialEq + PartialOrd>(steps: &[T], cur: T, by: i32) -> T {
    let i = steps.iter().position(|s| *s == cur).unwrap_or_else(|| steps.iter().position(|s| *s >= cur).unwrap_or(0));
    steps[(i as i32 + by).clamp(0, steps.len() as i32 - 1) as usize]
}

/// (can step down, can step up) for a list stepper.
pub fn ends<T: Copy + PartialEq + PartialOrd>(steps: &[T], cur: T) -> (bool, bool) {
    (cycle_clamped(steps, cur, -1) != cur, cycle_clamped(steps, cur, 1) != cur)
}

/// Energy price stepper: from off, + starts at 0.20; stepping down to 0 turns it off.
pub fn bump_price(p: Option<f64>, by: i32) -> Option<f64> {
    match p {
        None if by > 0 => Some(0.20),
        None => None,
        Some(p) => Some(PRICE.bump(p, by)).filter(|v| *v > 0.0),
    }
}

/// Make a config consistent before it is used or saved: limits, amber below red, no duplicate roots.
pub fn sanitise(c: &mut Config) {
    c.poll_ms = c.poll_ms.clamp(250, 2000);
    c.blackbox_minutes = c.blackbox_minutes.clamp(1, 120);
    c.vram_forecast_minutes = c.vram_forecast_minutes.min(240);
    c.bubble_close_secs = c.bubble_close_secs.min(600);
    let g = &mut c.gpu;
    g.temp_red_c = TEMP.clamp(g.temp_red_c as f64) as f32;
    g.temp_amber_c = (TEMP.clamp(g.temp_amber_c as f64) as f32).min(g.temp_red_c - 1.0);
    g.vram_red_mb = VRAM_MB.clamp(g.vram_red_mb as f64) as f32;
    g.vram_amber_mb = (VRAM_MB.clamp(g.vram_amber_mb as f64) as f32).min(g.vram_red_mb - VRAM_MB.step as f32);
    c.price_per_kwh = c.price_per_kwh.map(|p| PRICE.clamp(p)).filter(|p| *p > 0.0);
    let cur = c.currency.trim();
    c.currency = if cur.is_empty() || cur.chars().count() > 8 { "EUR".into() } else { cur.to_string() };
    let mut seen: Vec<String> = vec![];
    c.render_roots.retain(|r| {
        let k = r.trim().trim_end_matches(['\\', '/']).to_lowercase();
        if k.is_empty() || seen.contains(&k) {
            return false;
        }
        seen.push(k);
        true
    });
    c.render_roots.truncate(MAX_ROOTS);
}

/// A folder chosen in the picker, checked before it becomes a project folder.
pub fn check_root(roots: &[String], dir: &Path) -> Result<String, String> {
    if !dir.is_absolute() {
        return Err("Pick a full folder path.".into());
    }
    if !dir.is_dir() {
        return Err("That folder does not exist.".into());
    }
    let s = dir.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
    let s = if s.ends_with(':') { format!("{s}\\") } else { s };
    if roots.iter().any(|r| r.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(s.trim_end_matches(['\\', '/']))) {
        return Err("That folder is already in the list.".into());
    }
    if roots.len() >= MAX_ROOTS {
        return Err(format!("Up to {MAX_ROOTS} folders."));
    }
    Ok(s)
}

/// Settings that only take effect after a restart: `running` is the flight recorder's
/// (on, minutes) as Tachyon started.
pub fn restart_needed(running: (bool, u32), edited: &Config) -> Vec<&'static str> {
    let mut v = vec![];
    if running.0 != edited.blackbox || (edited.blackbox && running.1 != edited.blackbox_minutes) {
        v.push("flight recorder");
    }
    v
}

/// The `tachyon.toml` text with the keys the Settings page owns set to `c`. Only keys whose value
/// differs from what `text` already says are written, so an unchanged file stays byte-identical.
pub fn with_settings(text: &str, c: &Config) -> Result<String, String> {
    use toml_edit::{Array, Item, value};
    let old: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    macro_rules! top {
        ($k:literal, $f:ident, $v:expr) => {
            if old.$f != c.$f {
                doc[$k] = value($v);
            }
        };
    }
    if old.start_mode() != c.start_mode() || (old.mode.is_none() && c.mode.is_some()) {
        if let Some(m) = c.mode {
            doc["mode"] = value(m.as_str());
        }
    }
    top!("poll_ms", poll_ms, c.poll_ms as i64);
    top!("gpu_index", gpu_index, c.gpu_index as i64);
    top!("blackbox", blackbox, c.blackbox);
    top!("blackbox_minutes", blackbox_minutes, c.blackbox_minutes as i64);
    top!("history", history, c.history);
    top!("currency", currency, c.currency.as_str());
    top!("agent_api", agent_api, c.agent_api);
    top!("hex_announce", hex_announce, c.hex_announce);
    top!("bubble_close_secs", bubble_close_secs, c.bubble_close_secs as i64);
    top!("vram_forecast_minutes", vram_forecast_minutes, c.vram_forecast_minutes as i64);
    if old.price_per_kwh != c.price_per_kwh {
        match c.price_per_kwh {
            Some(p) => doc["price_per_kwh"] = value(p),
            None => {
                doc.remove("price_per_kwh");
            }
        }
    }
    if old.render_roots != c.render_roots {
        let mut a = Array::new();
        for r in &c.render_roots {
            a.push(r.as_str());
        }
        if a.len() > 1 {
            for v in a.iter_mut() {
                v.decor_mut().set_prefix("\n    ");
            }
            a.set_trailing("\n");
            a.set_trailing_comma(true);
        }
        doc.remove("roots"); // a v1 file's alias would win over the new key otherwise
        doc["render_roots"] = Item::Value(a.into());
    }
    let (og, ng) = (&old.gpu, &c.gpu);
    if og != ng {
        if !doc.contains_table("gpu") {
            doc["gpu"] = toml_edit::table();
        }
        let t = &mut doc["gpu"];
        for (k, a, b) in [
            ("temp_amber_c", og.temp_amber_c, ng.temp_amber_c),
            ("temp_red_c", og.temp_red_c, ng.temp_red_c),
            ("vram_amber_mb", og.vram_amber_mb, ng.vram_amber_mb),
            ("vram_red_mb", og.vram_red_mb, ng.vram_red_mb),
        ] {
            if a != b {
                t[k] = value(b as f64);
            }
        }
    }
    Ok(doc.to_string())
}

/// Reset settings: the defaults (window positions kept) replace the file; the old file is kept
/// as the backup. Returns the new settings.
pub fn reset(path: &Path, keep_window: &crate::config::WindowCfg) -> Result<Config, String> {
    let text = crate::config::with_window(&crate::config::default_text(), keep_window)?;
    write_atomic(path, &text, path.exists())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

/// Path of the backup kept next to the settings file.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Write `text` to `path` atomically: a temporary file in the same folder, flushed, then renamed
/// over the old one (on Windows a rename replaces the target in one step). With `backup`, the
/// previous file is first copied to `path.bak`.
pub fn write_atomic(path: &Path, text: &str, backup: bool) -> Result<(), String> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        if backup && path.exists() {
            std::fs::copy(path, backup_path(path))?;
        }
        std::fs::rename(&tmp, path)
    })();
    res.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not save {}: {e}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
    })
}

/// Save the Settings page's keys into the file at `path` (created from the header if missing).
/// Returns false when nothing needed writing.
pub fn save(path: &Path, c: &Config, backup: bool) -> Result<bool, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HEADER.to_string(),
        Err(e) => return Err(format!("cannot read the settings file: {e}")),
    };
    let new = with_settings(&text, c)?;
    if new == text {
        return Ok(false);
    }
    // never write something that would not load again
    toml::from_str::<Config>(&new).map_err(|e| format!("not saved, the result would not load: {e}"))?;
    write_atomic(path, &new, backup)?;
    Ok(true)
}

/// What to paste into an MCP client (Claude Desktop, Cursor, ...): starts `tachyon.exe mcp`.
pub fn mcp_config_json(exe: &Path) -> String {
    // written by hand to keep "command" before "args"; serde_json escapes the path
    let cmd = serde_json::to_string(&exe.to_string_lossy()).unwrap_or_default();
    format!("{{\n  \"mcpServers\": {{\n    \"tachyon\": {{\n      \"command\": {cmd},\n      \"args\": [\"mcp\"]\n    }}\n  }}\n}}")
}

/// The mode the Settings page shows: island, Hex (companion) or panel.
pub fn mode_label(m: UiMode) -> &'static str {
    match m {
        UiMode::Island => "Island",
        UiMode::Companion => "Hex",
        UiMode::Panel => "Panel",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# Tachyon 1.0 settings for this PC\n\
        poll_ms = 1000\n\
        render_roots = [\n    'C:\\work\\projects',\n]\n\
        # a comment that must stay\n\
        price_per_kwh = 0.30      # uncomment to see the energy cost\n\
        future_key = \"kept\"\n\
        mode = \"companion\"\n\
        agent_api = false\n\n\
        [stage_names]\nr = \"stage A render\"\n\n\
        [gpu]\nutil_band_low = 77\ntemp_amber_c = 80\n\n\
        [window]\nx = 1954.0\n";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tachyon_settings_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unchanged_config_leaves_the_file_byte_identical() {
        let c: Config = toml::from_str(FILE).unwrap();
        assert_eq!(with_settings(FILE, &c).unwrap(), FILE);
    }

    #[test]
    fn edits_keep_comments_unknown_keys_and_other_tables() {
        let mut c: Config = toml::from_str(FILE).unwrap();
        c.agent_api = true;
        c.poll_ms = 500;
        c.gpu.temp_red_c = 88.0;
        c.render_roots.push(r"D:\renders".into());
        c.mode = Some(UiMode::Panel);
        let new = with_settings(FILE, &c).unwrap();
        for keep in [
            "# a comment that must stay",
            "future_key = \"kept\"",
            "util_band_low = 77",
            "r = \"stage A render\"",
            "x = 1954.0",
            "# uncomment",
        ] {
            assert!(new.contains(keep), "{keep} lost:\n{new}");
        }
        let back: Config = toml::from_str(&new).unwrap();
        assert!(back.agent_api);
        assert_eq!((back.poll_ms, back.gpu.temp_red_c, back.gpu.temp_amber_c, back.mode), (500, 88.0, 80.0, Some(UiMode::Panel)));
        assert_eq!(back.render_roots, vec![r"C:\work\projects".to_string(), r"D:\renders".to_string()]);
        assert_eq!(back.gpu.util_band_low, Some(77.0));
    }

    #[test]
    fn price_off_removes_the_key_and_v1_roots_alias_is_replaced() {
        let mut c: Config = toml::from_str(FILE).unwrap();
        c.price_per_kwh = None;
        let new = with_settings(FILE, &c).unwrap();
        assert!(!new.contains("price_per_kwh"), "{new}");
        assert_eq!(toml::from_str::<Config>(&new).unwrap().price(), None);
        let v1 = "roots = ['D:\\\\old']\n";
        let mut c: Config = toml::from_str(v1).unwrap();
        c.render_roots = vec![r"E:\new".into()];
        let new = with_settings(v1, &c).unwrap();
        assert!(!new.contains("roots = ['D"), "{new}");
        assert_eq!(toml::from_str::<Config>(&new).unwrap().render_roots, vec![r"E:\new".to_string()]);
    }

    #[test]
    fn save_is_atomic_with_a_backup_of_the_previous_file() {
        let d = tmp("save");
        let p = d.join("tachyon.toml");
        std::fs::write(&p, FILE).unwrap();
        let mut c: Config = toml::from_str(FILE).unwrap();
        // nothing changed: nothing written, no backup
        assert_eq!(save(&p, &c, true), Ok(false));
        assert!(!backup_path(&p).exists());
        c.history = false;
        assert_eq!(save(&p, &c, true), Ok(true));
        assert_eq!(std::fs::read_to_string(backup_path(&p)).unwrap(), FILE);
        let now = std::fs::read_to_string(&p).unwrap();
        assert!(now.contains("history = false") && now.contains("future_key"));
        assert!(!d.join("tachyon.toml.tmp").exists());
        // a second save without backup keeps the first backup
        c.agent_api = true;
        assert_eq!(save(&p, &c, false), Ok(true));
        assert_eq!(std::fs::read_to_string(backup_path(&p)).unwrap(), FILE);
        // a missing file is created from the header
        let q = d.join("new.toml");
        assert_eq!(save(&q, &c, true), Ok(true));
        let created = std::fs::read_to_string(&q).unwrap();
        assert!(created.contains("# Tachyon settings") && created.contains("agent_api = true"), "{created}");
        // a file that does not parse is left alone
        std::fs::write(&p, "poll_ms = [").unwrap();
        assert!(save(&p, &c, true).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "poll_ms = [");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn limits_and_sanitise() {
        assert_eq!(TEMP.bump(80.0, 1), 81.0);
        assert_eq!(TEMP.bump(100.0, 1), 100.0);
        assert_eq!(PRICE.bump(0.29, 1), 0.3);
        assert_eq!(PRICE.bump(0.0, -1), 0.0);
        assert_eq!(VRAM_MB.fit(14500.0), 14592.0);
        assert_eq!(cycle(&POLL_STEPS, 1000, 1), 1500);
        assert_eq!(cycle(&POLL_STEPS, 2000, 1), 250);
        assert_eq!(cycle(&POLL_STEPS, 700, 0), 1000);
        assert_eq!(cycle(&BUBBLE_STEPS, 0, 1), 5);
        assert_eq!(cycle_clamped(&POLL_STEPS, 2000, 1), 2000);
        assert_eq!(cycle_clamped(&FORECAST_STEPS, 60, 1), 0);
        assert_eq!(ends(&POLL_STEPS, 250), (false, true));
        assert_eq!(ends(&FORECAST_STEPS, 0), (true, false));
        assert_eq!(bump_price(None, 1), Some(0.2));
        assert_eq!(bump_price(None, -1), None);
        assert_eq!(bump_price(Some(0.01), -1), None);
        assert_eq!(bump_price(Some(0.3), 1), Some(0.31));
        let mut c = Config::default();
        c.poll_ms = 10;
        c.gpu.temp_amber_c = 95.0;
        c.gpu.temp_red_c = 90.0;
        c.gpu.vram_amber_mb = 16000.0;
        c.gpu.vram_red_mb = 15600.0;
        c.price_per_kwh = Some(-1.0);
        c.currency = "  ".into();
        c.render_roots = vec![r"C:\a".into(), r"c:\A\".into(), "".into(), r"D:\b".into()];
        sanitise(&mut c);
        assert_eq!(c.poll_ms, 250);
        // values inside the limits keep their exact number (no snapping to the stepper grid)
        let mut d = Config::default();
        sanitise(&mut d);
        assert_eq!((d.gpu.vram_amber_mb, d.gpu.vram_red_mb), (14500.0, 15600.0));
        assert!(c.gpu.temp_amber_c < c.gpu.temp_red_c && c.gpu.vram_amber_mb < c.gpu.vram_red_mb);
        assert_eq!((c.price_per_kwh, c.currency.as_str()), (None, "EUR"));
        assert_eq!(c.render_roots, vec![r"C:\a".to_string(), r"D:\b".to_string()]);
    }

    #[test]
    fn roots_are_checked() {
        let d = tmp("roots");
        let ok = check_root(&[], &d).unwrap();
        assert!(Path::new(&ok).is_dir());
        assert!(check_root(std::slice::from_ref(&ok), &d).is_err(), "duplicate");
        assert!(check_root(&[], &d.join("missing")).is_err());
        assert!(check_root(&[], Path::new("relative")).is_err());
        let full: Vec<String> = (0..MAX_ROOTS).map(|i| format!("X:\\{i}")).collect();
        assert!(check_root(&full, &d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restart_only_for_the_recorder() {
        let a = Config::default();
        let run = (a.blackbox, a.blackbox_minutes);
        let mut b = a.clone();
        b.agent_api = true;
        b.history = false;
        b.poll_ms = 500;
        b.render_roots = vec!["C:\\x".into()];
        assert!(restart_needed(run, &b).is_empty());
        b.blackbox_minutes = 30;
        assert_eq!(restart_needed(run, &b), vec!["flight recorder"]);
        b.blackbox = false;
        assert_eq!(restart_needed(run, &b), vec!["flight recorder"]);
    }

    #[test]
    fn reset_keeps_the_old_file_as_backup_and_the_window_where_it_was() {
        let d = std::env::temp_dir().join(format!("tachyon_reset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("tachyon.toml");
        let mine = "# mine\nagent_api = true\npoll_ms = 250\nrender_roots = ['D:\\\\work']\n[window]\nx = 300\ny = 40\n";
        std::fs::write(&p, mine).unwrap();
        let w = crate::config::WindowCfg { x: Some(300.0), y: Some(40.0), ..Default::default() };
        let c = reset(&p, &w).unwrap();
        assert!(!c.agent_api && c.poll_ms == Config::default().poll_ms && c.render_roots.is_empty());
        assert_eq!((c.window.x, c.window.y), (Some(300.0), Some(40.0)));
        assert_eq!(std::fs::read_to_string(backup_path(&p)).unwrap(), mine);
        assert!(crate::config::load(&p).warn.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn gpu_choice_is_saved() {
        let c = Config { gpu_index: 1, ..Default::default() };
        let out = with_settings("poll_ms = 1000\n", &c).unwrap();
        assert!(out.contains("gpu_index = 1"), "{out}");
    }

    #[test]
    fn mcp_snippet_has_the_full_exe_path_and_the_mcp_argument() {
        let j = mcp_config_json(Path::new(r"C:\Users\me\Desktop\My Tools\tools\tachyon\tachyon.exe"));
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["mcpServers"]["tachyon"]["command"], r"C:\Users\me\Desktop\My Tools\tools\tachyon\tachyon.exe");
        assert_eq!(v["mcpServers"]["tachyon"]["args"], serde_json::json!(["mcp"]));
        assert!(j.contains(r#""C:\\Users\\me\\Desktop\\My Tools"#), "{j}");
        assert!(j.find("command").unwrap() < j.find("args").unwrap(), "{j}");
        let odd = mcp_config_json(Path::new(r#"D:\a "quoted" dir\tachyon.exe"#));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&odd).unwrap()["mcpServers"]["tachyon"]["command"],
            r#"D:\a "quoted" dir\tachyon.exe"#
        );
    }
}
