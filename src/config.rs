use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// NVML polling interval in ms (clamped to 250..=2000).
    pub poll_ms: u64,
    /// GPU index shown at start-up.
    pub gpu_index: u32,
    /// Directories searched (recursively, up to `scan_depth`) for render logs.
    /// Empty (the default) = pure GPU monitor, no render panel.
    #[serde(alias = "roots")]
    pub render_roots: Vec<String>,
    /// The render panel is shown only if a log under `render_roots` was written within this many seconds.
    pub render_visible_secs: u64,
    /// Max directory depth below each root.
    pub scan_depth: usize,
    /// A log counts as a live progress source if written within this many seconds.
    pub idle_secs: u64,
    /// A progress.json is preferred over logs when it was written within this many seconds.
    pub progress_json_fresh_secs: u64,
    /// Bytes read from the end of a log on each poll.
    pub tail_bytes: u64,
    /// Seconds between directory rescans (tailing itself happens every poll).
    pub rescan_secs: u64,
    /// A batch job (open `start` in batch.log/queue.log) older than this is treated as dead.
    pub max_job_secs: u64,
    /// Labels for parallel chunk logs by file prefix (r_0.log, p_354.log, ...).
    pub stage_names: std::collections::BTreeMap<String, String>,
    /// Query PCIe RX/TX (NVML samples each direction for ~20 ms, so this costs ~40 ms of wall time per tick).
    pub pcie: bool,
    /// Seconds between Toolhelp32 process snapshots used for the launcher chain.
    pub process_refresh_secs: u64,
    /// Show each GPU process's command line on hover (read with PROCESS_QUERY_LIMITED_INFORMATION; may show paths).
    pub process_cmdline: bool,
    /// Desktop/shell processes hidden from the GPU process list unless "show all" is on
    /// (case-insensitive file names, `*` wildcards).
    pub process_hide: Vec<String>,
    /// Glass style: "tint" (darker tinted glass, works everywhere), "blur" (experimental: also asks
    /// Windows to blur the desktop behind the shape), "auto" (= tint in v5). Older files with
    /// `glass = true/false` still load (true = auto, false = tint).
    pub glass: GlassMode,
    /// How Tachyon starts: "island" (top capsule), "companion" (Hex, the desktop companion) or
    /// "panel" (the full window). Unset = from `[window] compact` (v4 files). Switched from the menu.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<UiMode>,
    /// Flight recorder: keep the last `blackbox_minutes` of readings on disk (fixed-size files in
    /// %LOCALAPPDATA%\Tachyon\blackbox) so a crash or unexpected restart can be explained afterwards.
    pub blackbox: bool,
    pub blackbox_minutes: u32,
    /// Record finished render jobs and GPU-busy sessions in %LOCALAPPDATA%\Tachyon\history.
    pub history: bool,
    /// Electricity price per kWh, to show what a job cost. Off when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_per_kwh: Option<f64>,
    /// Currency label for `price_per_kwh` (for example "EUR" or "£").
    pub currency: String,
    /// Let local AI agents read Tachyon's live state through `tachyon mcp` (read-only, this PC only).
    pub agent_api: bool,
    /// Hex opens its bubble by itself to announce a crash report (once per report).
    pub hex_announce: bool,
    /// Hex's bubble closes by itself after this many seconds without the pointer on it (0 = stays open).
    pub bubble_close_secs: u64,
    /// Warn when VRAM would be full within this many minutes at the current rate (0 = no forecast warning).
    pub vram_forecast_minutes: u32,
    pub gpu: GpuThresholds,
    pub window: WindowCfg,
    /// "Who is using the GPU": first matching rule wins; unmatched processes are "Other".
    #[serde(rename = "process_rules")]
    pub process_rules: Vec<ProcessRule>,
}

/// `glass = "auto" | "blur" | "tint"` (booleans accepted for v4 files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlassMode {
    #[default]
    Auto,
    Blur,
    Tint,
}

impl GlassMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GlassMode::Auto => "auto",
            GlassMode::Blur => "blur",
            GlassMode::Tint => "tint",
        }
    }
}

impl Serialize for GlassMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GlassMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = GlassMode;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("\"auto\", \"blur\", \"tint\" or a boolean")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<GlassMode, E> {
                Ok(if v { GlassMode::Auto } else { GlassMode::Tint })
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<GlassMode, E> {
                match v.trim().to_ascii_lowercase().as_str() {
                    "auto" => Ok(GlassMode::Auto),
                    "blur" => Ok(GlassMode::Blur),
                    "tint" | "solid" | "off" => Ok(GlassMode::Tint),
                    _ => Err(E::invalid_value(serde::de::Unexpected::Str(v), &self)),
                }
            }
        }
        d.deserialize_any(V)
    }
}

/// Start mode (see `Config::mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiMode {
    Island,
    Companion,
    Panel,
}

impl UiMode {
    pub fn as_str(self) -> &'static str {
        match self {
            UiMode::Island => "island",
            UiMode::Companion => "companion",
            UiMode::Panel => "panel",
        }
    }
}

/// A category rule. Patterns are case-insensitive file names with optional `*` wildcards.
/// `names` match the process itself, `parents` match any process in its launcher chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProcessRule {
    pub category: String,
    /// Tag colour, "#RRGGBB".
    pub color: String,
    pub names: Vec<String>,
    pub parents: Vec<String>,
}

impl Default for ProcessRule {
    fn default() -> Self {
        Self { category: "Other".into(), color: "#7C8BA1".into(), names: vec![], parents: vec![] }
    }
}

fn rule(category: &str, color: &str, names: &[&str], parents: &[&str]) -> ProcessRule {
    ProcessRule {
        category: category.into(),
        color: color.into(),
        names: names.iter().map(|s| s.to_string()).collect(),
        parents: parents.iter().map(|s| s.to_string()).collect(),
    }
}

/// Desktop shell processes that use the GPU only to draw the desktop (hidden by default).
pub fn default_process_hide() -> Vec<String> {
    [
        "dwm.exe",
        "explorer.exe",
        "shellhost.exe",
        "sihost.exe",
        "startmenuexperiencehost.exe",
        "searchhost.exe",
        "searchapp.exe",
        "searchui.exe",
        "shellexperiencehost.exe",
        "crossdeviceresume.exe",
        "textinputhost.exe",
        "applicationframehost.exe",
        "lockapp.exe",
        "systemsettings.exe",
        "widgets.exe",
        "widgetservice.exe",
        "phoneexperiencehost.exe",
        "csrss.exe",
        "winlogon.exe",
        "fontdrvhost.exe",
        "nvidia overlay.exe",
        "nvidia share.exe",
        "nvcontainer.exe",
        "gamebar*.exe",
        "rig_monitor*.exe",
        "tachyon*.exe",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Default "who is using the GPU" rules (order matters: first match wins).
pub fn default_process_rules() -> Vec<ProcessRule> {
    vec![
        rule(
            "System",
            "#8A97AB",
            &[
                "dwm.exe",
                "csrss.exe",
                "explorer.exe",
                "searchhost.exe",
                "searchapp.exe",
                "shellexperiencehost.exe",
                "startmenuexperiencehost.exe",
                "textinputhost.exe",
                "lockapp.exe",
                "applicationframehost.exe",
                "systemsettings.exe",
                "widgets.exe",
                "phoneexperiencehost.exe",
                "gamebar*.exe",
                "nvcontainer.exe",
                "nvidia*.exe",
                "nvdisplay.container.exe",
                "rtss*.exe",
                "msiafterburner.exe",
                "rig_monitor*.exe",
                "winlogon.exe",
                "fontdrvhost.exe",
                "crossdeviceresume.exe",
                "video.ui.exe",
                "tachyon*.exe",
                "shellhost.exe",
                "sihost.exe",
            ],
            &[],
        ),
        rule(
            "Browser",
            "#3DDC97",
            &[
                "chrome.exe",
                "msedge.exe",
                "firefox.exe",
                "brave.exe",
                "opera*.exe",
                "vivaldi.exe",
                "arc.exe",
                "msedgewebview2.exe",
                "*webhelper*.exe",
                "zen.exe",
                "librewolf.exe",
            ],
            &[],
        ),
        rule(
            "AI/agent",
            "#C77DFF",
            &[
                "ollama*.exe",
                "lm studio.exe",
                "lmstudio*.exe",
                "llama-server*.exe",
                "llama-cli*.exe",
                "koboldcpp*.exe",
                "comfyui*.exe",
                "jan.exe",
                "chatgpt.exe",
                "claude.exe",
                "cursor.exe",
                "msty.exe",
                "gpt4all*.exe",
                "text-generation-webui*.exe",
                "invokeai*.exe",
                "fooocus*.exe",
            ],
            &["comfyui*.exe", "ollama*.exe", "lm studio.exe", "lmstudio*.exe", "pinokio*.exe", "stabilitymatrix*.exe", "invokeai*.exe"],
        ),
        rule(
            "Creative app",
            "#FFB347",
            &[
                "resolve.exe",
                "afterfx.exe",
                "adobe premiere pro.exe",
                "premiere*.exe",
                "photoshop.exe",
                "illustrator.exe",
                "blender*.exe",
                "obs64.exe",
                "obs32.exe",
                "c4d*.exe",
                "cinema 4d*.exe",
                "houdini*.exe",
                "unity.exe",
                "unrealeditor*.exe",
                "ffmpeg*.exe",
                "ffedit*.exe",
                "handbrake*.exe",
                "topaz*.exe",
                "touchdesigner*.exe",
                "capcut.exe",
                "nuke*.exe",
                "substance*.exe",
                "maya.exe",
                "3dsmax.exe",
                "krita.exe",
                "gimp*.exe",
                "vegas*.exe",
                "mediaencoder*.exe",
                "adobe media encoder.exe",
                "fusion.exe",
                "ableton*.exe",
            ],
            &[],
        ),
        rule(
            "Game",
            "#FF5C7A",
            &["*-win64-shipping.exe", "*_dx12.exe", "*_dx11.exe", "minecraft*.exe"],
            &[
                "steam.exe",
                "epicgameslauncher.exe",
                "battle.net.exe",
                "eadesktop.exe",
                "origin.exe",
                "galaxyclient.exe",
                "upc.exe",
                "ubisoftconnect.exe",
                "riotclientservices.exe",
                "xboxpcapp.exe",
                "gamingservices.exe",
                "playnite*.exe",
                "heroic.exe",
                "rockstarlauncher.exe",
            ],
        ),
        rule(
            "Script/Python",
            "#4FC3F7",
            &[
                "python*.exe",
                "pythonw*.exe",
                "py.exe",
                "node.exe",
                "java.exe",
                "javaw.exe",
                "deno.exe",
                "bun.exe",
                "julia.exe",
                "rscript.exe",
                "powershell.exe",
                "pwsh.exe",
                "cmd.exe",
                "conda.exe",
                "uv.exe",
                "wsl*.exe",
            ],
            &[],
        ),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GpuThresholds {
    /// Optional utilisation band: inside = cyan, above = amber. Unset (default) = no util colouring.
    pub util_band_low: Option<f32>,
    pub util_band_high: Option<f32>,
    /// Also show utilisation below the band in amber (useful for render jobs, noisy for gaming).
    pub util_below_band_amber: bool,
    pub temp_amber_c: f32,
    pub temp_red_c: f32,
    /// VRAM thresholds: the lower of the absolute MB value and the % of the card's total is used,
    /// so the same file works on 8 GB and 16 GB cards.
    pub vram_amber_mb: f32,
    pub vram_red_mb: f32,
    pub vram_amber_pct: f32,
    pub vram_red_pct: f32,
    /// Power draw at or above this % of the enforced limit is amber.
    pub power_amber_pct: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WindowCfg {
    pub always_on_top: bool,
    /// Start as the island (the small always-on-top capsule). Key kept as `compact` for older files.
    pub compact: bool,
    /// Island position: centre x and top y in screen pixels (default: top-centre of the primary screen).
    pub island_cx: Option<f32>,
    pub island_y: Option<f32>,
    /// GPU process list: also show desktop/idle graphics processes.
    pub show_all_processes: bool,
    /// Hex (companion mode) window position, top-left in screen pixels.
    pub hex_x: Option<f32>,
    pub hex_y: Option<f32>,
    /// Last outer position (remembered on exit).
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: f32,
    pub height: f32,
    pub compact_width: f32,
    pub compact_height: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_ms: 1000,
            gpu_index: 0,
            render_roots: vec![],
            render_visible_secs: 120,
            scan_depth: 4,
            idle_secs: 60,
            progress_json_fresh_secs: 30,
            tail_bytes: 64 * 1024,
            rescan_secs: 5,
            max_job_secs: 3 * 3600,
            stage_names: Default::default(),
            pcie: true,
            process_refresh_secs: 4,
            process_cmdline: false,
            process_hide: default_process_hide(),
            glass: GlassMode::Auto,
            mode: None,
            blackbox: true,
            blackbox_minutes: 15,
            history: true,
            price_per_kwh: None,
            currency: "EUR".into(),
            agent_api: false,
            hex_announce: true,
            bubble_close_secs: 8,
            vram_forecast_minutes: 15,
            gpu: GpuThresholds::default(),
            window: WindowCfg::default(),
            process_rules: default_process_rules(),
        }
    }
}

impl Default for GpuThresholds {
    fn default() -> Self {
        Self {
            util_band_low: None,
            util_band_high: None,
            util_below_band_amber: false,
            temp_amber_c: 80.0,
            temp_red_c: 83.0,
            vram_amber_mb: 14500.0,
            vram_red_mb: 15600.0,
            vram_amber_pct: 90.0,
            vram_red_pct: 97.0,
            power_amber_pct: 98.0,
        }
    }
}

impl Default for WindowCfg {
    fn default() -> Self {
        Self {
            always_on_top: true,
            compact: false,
            island_cx: None,
            island_y: None,
            show_all_processes: false,
            hex_x: None,
            hex_y: None,
            x: None,
            y: None,
            width: 600.0,
            height: 800.0,
            compact_width: 470.0,
            compact_height: 46.0,
        }
    }
}

impl Config {
    /// The mode to start in: `mode` if set, else island/panel from `[window] compact`.
    pub fn start_mode(&self) -> UiMode {
        self.mode.unwrap_or(if self.window.compact { UiMode::Island } else { UiMode::Panel })
    }
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.poll_ms.clamp(250, 2000))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Neutral,
    Amber,
    Red,
}

impl GpuThresholds {
    /// Utilisation colour in context: "below the band = amber" only applies while a render is
    /// active (render panel visible), so an idle GPU is never shown in amber.
    pub fn util_level_ctx(&self, u: f32, render_active: bool) -> Level {
        match self.util_level(u) {
            Level::Amber if !render_active && self.util_band_low.is_some_and(|lo| u < lo) => Level::Neutral,
            l => l,
        }
    }
    pub fn util_level(&self, u: f32) -> Level {
        let (Some(lo), Some(hi)) = (self.util_band_low, self.util_band_high) else { return Level::Ok };
        if u > hi {
            Level::Amber
        } else if u >= lo {
            Level::Ok
        } else if self.util_below_band_amber {
            Level::Amber
        } else {
            Level::Neutral
        }
    }
    pub fn temp_level(&self, c: f32) -> Level {
        if c >= self.temp_red_c {
            Level::Red
        } else if c >= self.temp_amber_c {
            Level::Amber
        } else {
            Level::Ok
        }
    }
    pub fn vram_level(&self, used_mb: f32, total_mb: f32) -> Level {
        let amber = self.vram_amber_mb.min(total_mb * self.vram_amber_pct / 100.0);
        let red = self.vram_red_mb.min(total_mb * self.vram_red_pct / 100.0);
        if used_mb >= red {
            Level::Red
        } else if used_mb >= amber {
            Level::Amber
        } else {
            Level::Ok
        }
    }
    pub fn power_level(&self, w: f32, limit: f32) -> Level {
        if limit > 0.0 && w >= limit * self.power_amber_pct / 100.0 { Level::Amber } else { Level::Ok }
    }
}

/// Tachyon's own folder for the flight recorder, history and exports: `TACHYON_DATA_DIR` if set,
/// else `%LOCALAPPDATA%\Tachyon` (on other systems `$XDG_DATA_HOME/tachyon` or `~/.local/share/tachyon`).
pub fn data_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("TACHYON_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("Tachyon"));
    }
    std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .map(|d| d.join("tachyon"))
}

impl Config {
    /// Electricity price and currency, when set.
    pub fn price(&self) -> Option<(f64, String)> {
        self.price_per_kwh.filter(|p| p.is_finite() && *p > 0.0).map(|p| (p, self.currency.clone()))
    }
}

/// Settings file next to the running exe: tachyon.toml, or a legacy rig_monitor.toml when only that exists.
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("TACHYON_CONFIG").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
    let p = pick_config(&dir);
    // a read-only exe folder (Program Files): the settings live in the data folder
    if !p.exists()
        && let Some(alt) = data_dir().map(|d| d.join("tachyon.toml")).filter(|a| a.exists())
    {
        return alt;
    }
    p
}

/// tachyon.toml wins; a legacy rig_monitor.toml is used only if tachyon.toml does not exist.
pub fn pick_config(dir: &Path) -> PathBuf {
    let new = dir.join("tachyon.toml");
    let old = dir.join("rig_monitor.toml");
    if !new.exists() && old.exists() { old } else { new }
}

pub const HEADER: &str = "# Tachyon settings. Most of these are on the Settings page (right-click menu); or edit and restart Tachyon.\n\
# render_roots = [] keeps it a pure GPU monitor. [window] is updated automatically on exit.\n\
# Set util_band_low/util_band_high under [gpu] to colour GPU load by a target band.\n\
# glass = \"tint\" (default look), \"blur\" (experimental desktop blur) or \"auto\".\n\
# mode = \"island\", \"companion\" (Hex) or \"panel\" picks how Tachyon starts (also in the right-click menu).\n\
# blackbox and history keep their files in %LOCALAPPDATA%\\Tachyon. Nothing leaves this PC.\n\
# price_per_kwh = 0.30 shows what each job cost. agent_api = true lets local AI agents read the state (tachyon mcp).\n";

/// Biggest settings file Tachyon reads; anything larger is treated as damaged.
pub const MAX_CONFIG_BYTES: u64 = 1 << 20;

/// What loading the settings found.
#[derive(Debug)]
pub struct Loaded {
    pub cfg: Config,
    /// The file in use (it may have moved to the data folder when the exe's folder is read-only).
    pub path: PathBuf,
    /// A message for the panel (damaged file recovered, file could not be written, ...).
    pub warn: Option<String>,
    /// No settings file existed: this is the first start.
    pub first_run: bool,
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `<name>.corrupt-YYYYmmdd-HHMMSS` next to the file, never an existing name.
pub fn corrupt_copy_path(path: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let base = format!("{}.corrupt-{stamp}", file_name(path));
    let mut p = path.with_file_name(&base);
    let mut n = 2;
    while p.exists() {
        p = path.with_file_name(format!("{base}-{n}"));
        n += 1;
    }
    p
}

/// The settings file written on a first start (and by Reset settings).
pub fn default_text() -> String {
    format!("{HEADER}{}", toml::to_string_pretty(&Config::default()).unwrap_or_default())
}

/// Read the settings file without ever failing: missing = first start (the defaults are
/// written), damaged = the file is kept under a new name and the defaults are used and written.
/// When the exe's folder is read-only the file goes to the data folder instead.
pub fn load(path: &Path) -> Loaded {
    let read = std::fs::metadata(path).and_then(|m| {
        if m.len() > MAX_CONFIG_BYTES {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("larger than {} KB", MAX_CONFIG_BYTES >> 10)))
        } else {
            std::fs::read_to_string(path)
        }
    });
    let mut out = Loaded { cfg: Config::default(), path: path.to_path_buf(), warn: None, first_run: false };
    let damaged = match read {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(c) => {
                out.cfg = c;
                return out;
            }
            Err(e) => e.message().to_string() + &e.span().map(|r| format!(" (at byte {})", r.start)).unwrap_or_default(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.first_run = true;
            if let Err(e) = crate::settings::write_atomic(path, &default_text(), false) {
                let alt = data_dir().map(|d| d.join("tachyon.toml")).filter(|a| a != path);
                let moved = alt.filter(|a| {
                    a.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok())
                        && crate::settings::write_atomic(a, &default_text(), false).is_ok()
                });
                match moved {
                    Some(a) => out.path = a,
                    None => out.warn = Some(format!("Settings are not saved: {e}")),
                }
            }
            return out;
        }
        Err(e) => e.to_string(),
    };
    let keep = corrupt_copy_path(path);
    let kept = std::fs::copy(path, &keep).is_ok();
    let rewrote = kept && crate::settings::write_atomic(path, &default_text(), false).is_ok();
    out.warn = Some(if rewrote {
        format!("{} could not be read ({damaged}); started with defaults. Your file is kept as {}.", file_name(path), file_name(&keep))
    } else {
        format!("{} could not be read ({damaged}); using defaults this time.", file_name(path))
    });
    out
}

/// Load the config; if the file is missing, write the defaults.
/// Returns the config plus an optional warning to show in the UI.
pub fn load_or_create(path: &Path) -> (Config, Option<String>) {
    let l = load(path);
    (l.cfg, l.warn)
}

/// Update only the `[window]` table of an existing toml text, keeping comments and other keys.
pub fn with_window(text: &str, w: &WindowCfg) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    if !doc.contains_table("window") {
        doc["window"] = toml_edit::table();
    }
    let t = &mut doc["window"];
    t["always_on_top"] = toml_edit::value(w.always_on_top);
    t["compact"] = toml_edit::value(w.compact);
    if let Some(x) = w.x {
        t["x"] = toml_edit::value(x.round() as f64);
    }
    if let Some(y) = w.y {
        t["y"] = toml_edit::value(y.round() as f64);
    }
    t["width"] = toml_edit::value(w.width.round() as f64);
    t["height"] = toml_edit::value(w.height.round() as f64);
    if let Some(v) = w.island_cx {
        t["island_cx"] = toml_edit::value(v.round() as f64);
    }
    if let Some(v) = w.island_y {
        t["island_y"] = toml_edit::value(v.round() as f64);
    }
    if let Some(v) = w.hex_x {
        t["hex_x"] = toml_edit::value(v.round() as f64);
    }
    if let Some(v) = w.hex_y {
        t["hex_y"] = toml_edit::value(v.round() as f64);
    }
    t["show_all_processes"] = toml_edit::value(w.show_all_processes);
    Ok(doc.to_string())
}

/// Set the top-level `mode` key, keeping comments and everything else.
pub fn with_mode(text: &str, m: UiMode) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    doc["mode"] = toml_edit::value(m.as_str());
    Ok(doc.to_string())
}

/// Persist the start mode (best effort).
pub fn save_mode(path: &Path, m: UiMode) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| HEADER.to_string());
    if let Ok(new) = with_mode(&text, m) {
        if new != text {
            let _ = crate::settings::write_atomic(path, &new, false);
        }
    }
}

/// Persist window state (best effort).
pub fn save_window(path: &Path, w: &WindowCfg) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| HEADER.to_string());
    if let Ok(new) = with_window(&text, w) {
        if new != text {
            let _ = crate::settings::write_atomic(path, &new, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_roundtrip_and_partial_file() {
        let s = toml::to_string_pretty(&Config::default()).unwrap();
        let c: Config = toml::from_str(&s).unwrap();
        assert_eq!(c.gpu.temp_red_c, 83.0);
        let p: Config = toml::from_str("idle_secs = 90\n[gpu]\nutil_band_low = 77\n").unwrap();
        assert_eq!(p.idle_secs, 90);
        assert_eq!(p.gpu.util_band_low, Some(77.0));
        assert_eq!(p.gpu.util_band_high, None);
        // no render roots by default (pure GPU monitor)
        assert!(p.render_roots.is_empty());
        // 1.0 keys: recorder and history on, agent API and prices off
        assert!(p.blackbox && p.history && !p.agent_api);
        assert_eq!((p.blackbox_minutes, p.price()), (15, None));
        let q: Config = toml::from_str("price_per_kwh = 0.3\ncurrency = \"£\"\nagent_api = true\n").unwrap();
        assert_eq!(q.price(), Some((0.3, "£".to_string())));
        assert!(q.agent_api);
        assert!(!s.contains("price_per_kwh"), "unset price is not written");
        // Settings-page keys keep the 1.0 behaviour by default
        assert!(p.hex_announce);
        assert_eq!((p.bubble_close_secs, p.vram_forecast_minutes), (8, 15));
    }

    #[test]
    fn glass_mode_accepts_v4_booleans_and_v5_names() {
        let g = |t: &str| toml::from_str::<Config>(t).unwrap().glass;
        assert_eq!(g(""), GlassMode::Auto);
        assert_eq!(g("glass = true"), GlassMode::Auto);
        assert_eq!(g("glass = false"), GlassMode::Tint);
        assert_eq!(g("glass = 'blur'"), GlassMode::Blur);
        assert_eq!(g("glass = \"TINT\""), GlassMode::Tint);
        assert!(toml::from_str::<Config>("glass = 'frosty'").is_err());
        let s = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(s.contains("glass = \"auto\""), "{s}");
    }

    #[test]
    fn mode_defaults_from_compact_and_saves_at_top_level() {
        let c: Config = toml::from_str("[window]\ncompact = true\n").unwrap();
        assert_eq!(c.start_mode(), UiMode::Island);
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.start_mode(), UiMode::Panel);
        let c: Config = toml::from_str("mode = \"companion\"\n[window]\ncompact = true\n").unwrap();
        assert_eq!(c.start_mode(), UiMode::Companion);
        assert!(toml::from_str::<Config>("mode = \"mascot\"").is_err());
        let text = "# keep me\npoll_ms = 500\n\n[window]\ncompact = true\n";
        let new = with_mode(text, UiMode::Companion).unwrap();
        assert!(new.starts_with("# keep me"), "{new}");
        let c: Config = toml::from_str(&new).unwrap();
        assert_eq!((c.mode, c.poll_ms, c.window.compact), (Some(UiMode::Companion), 500, true));
        let mut w = c.window.clone();
        w.hex_x = Some(1500.4);
        w.hex_y = Some(800.0);
        let c: Config = toml::from_str(&with_window(&new, &w).unwrap()).unwrap();
        assert_eq!((c.window.hex_x, c.window.hex_y, c.mode), (Some(1500.0), Some(800.0), Some(UiMode::Companion)));
    }

    #[test]
    fn tachyon_toml_with_legacy_fallback() {
        let dir = std::env::temp_dir().join(format!("tachyon_pick_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(pick_config(&dir).file_name().unwrap(), "tachyon.toml"); // nothing yet: new name
        std::fs::write(dir.join("rig_monitor.toml"), "").unwrap();
        assert_eq!(pick_config(&dir).file_name().unwrap(), "rig_monitor.toml"); // legacy only
        std::fs::write(dir.join("tachyon.toml"), "").unwrap();
        assert_eq!(pick_config(&dir).file_name().unwrap(), "tachyon.toml"); // both: new wins
        let _ = std::fs::remove_dir_all(&dir);
    }
}
