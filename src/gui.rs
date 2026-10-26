//! Tachyon "Aurora" UI. One borderless layered window (see present.rs) that is either the
//! island capsule or the expanded panel. Each frame = software glass body (glass.rs: shadow,
//! tint, Aurora colour field, progress level) + a cached foreground (tiny-skia shapes and
//! DirectWrite text), composited and handed to UpdateLayeredWindow.
//!
//! Lightness: one WM_TIMER at the poll rate samples NVML and redraws once. Extra frames only
//! while the island <-> panel morph runs (220 ms) or while the colour field eases (600 ms,
//! started only when load/temperature move past a hysteresis step, at most one every 5 s,
//! 10 fps). No continuous animation.
#![allow(clippy::too_many_arguments, unsafe_op_in_unsafe_fn)]
mod companion;
mod settings;
mod views;
use crate::dw::{self, Dw, Face};
use crate::present::{self, Surface};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use tachyon::aurora::{Aurora, Field};
use tachyon::config::{self, Config, GlassMode, Level, UiMode};
use tachyon::glass::{self, Body, FieldLayout, Geometry, LevelFill, ShadowSpec, Shape};
use tachyon::monitor::{Island, Metric, RenderView, Sampler, Series, Snapshot, WINDOW_SECS, filter_procs, island_pos};
use tachyon::nvml::throttle_words;
use tachyon::parse;
use tachyon::theme::*;
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, Path, PathBuilder, PixmapMut, Point, Shader, SpreadMode,
    Stroke, Transform,
};
use views::View;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn level_text(l: Level) -> Rgb {
    match l {
        Level::Ok | Level::Neutral => TEXT,
        Level::Amber => T_AMBER,
        Level::Red => T_RED,
    }
}
fn wz(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
/// tiny-skia colour in our BGRA buffer order (channels swapped; rasterising is channel-agnostic).
fn col(c: Rgb, a: f32) -> Color {
    Color::from_rgba8(c.2, c.1, c.0, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct R {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}
impl R {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    fn r(&self) -> f32 {
        self.x + self.w
    }
    fn b(&self) -> f32 {
        self.y + self.h
    }
    fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    fn grow(&self, d: f32) -> Self {
        Self::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.r() && y >= self.y && y < self.b()
    }
}

#[derive(Clone, Copy)]
enum H {
    L,
    C,
    Rt,
}
/// Vertical anchor: centre of the cap height, or the baseline.
#[derive(Clone, Copy)]
enum V {
    C,
    Base,
}

fn pt(x: f32, y: f32) -> Point {
    Point::from_xy(x, y)
}

fn rrect_path(r: R, rad: f32) -> Option<Path> {
    let rad = rad.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    let k = 0.5523 * rad;
    let mut pb = PathBuilder::new();
    pb.move_to(r.x + rad, r.y);
    pb.line_to(r.r() - rad, r.y);
    pb.cubic_to(r.r() - rad + k, r.y, r.r(), r.y + rad - k, r.r(), r.y + rad);
    pb.line_to(r.r(), r.b() - rad);
    pb.cubic_to(r.r(), r.b() - rad + k, r.r() - rad + k, r.b(), r.r() - rad, r.b());
    pb.line_to(r.x + rad, r.b());
    pb.cubic_to(r.x + rad - k, r.b(), r.x, r.b() - rad + k, r.x, r.b() - rad);
    pb.line_to(r.x, r.y + rad);
    pb.cubic_to(r.x, r.y + rad - k, r.x + rad - k, r.y, r.x + rad, r.y);
    pb.close();
    pb.finish()
}

fn poly_path(pts: &[(f32, f32)], close: bool) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(pts.first()?.0, pts[0].1);
    for p in &pts[1..] {
        pb.line_to(p.0, p.1);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

fn hgrad(x0: f32, x1: f32, stops: &[(f32, Rgb, f32)]) -> Option<Shader<'static>> {
    LinearGradient::new(
        pt(x0, 0.0),
        pt(x1.max(x0 + 0.01), 0.0),
        stops.iter().map(|(p, c, a)| GradientStop::new(*p, col(*c, *a))).collect(),
        SpreadMode::Pad,
        Transform::identity(),
    )
}
fn vgrad(y0: f32, y1: f32, stops: &[(f32, Rgb, f32)]) -> Option<Shader<'static>> {
    LinearGradient::new(
        pt(0.0, y0),
        pt(0.0, y1.max(y0 + 0.01)),
        stops.iter().map(|(p, c, a)| GradientStop::new(*p, col(*c, *a))).collect(),
        SpreadMode::Pad,
        Transform::identity(),
    )
}

// ---- drawing context -------------------------------------------------------------------------
/// Paint passes: the static layer (labels, dividers, row veils, rim) is drawn into its own buffer
/// only when its fingerprint changes; each data tick copies it and draws the live values on top.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Hash,
    Static,
    Dynamic,
}

struct G<'a> {
    pm: PixmapMut<'a>,
    bw: i32,
    bh: i32,
    dw: Option<&'a mut Dw>,
    scale: f32,
    /// device-pixel origin of the glass shape inside the padded surface
    ox: f32,
    oy: f32,
    pass: Pass,
    in_static: bool,
    hash: u64,
}

impl G<'_> {
    fn tf(&self) -> Transform {
        Transform::from_row(self.scale, 0.0, 0.0, self.scale, self.ox, self.oy)
    }
    fn stat<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let old = std::mem::replace(&mut self.in_static, true);
        let r = f(self);
        self.in_static = old;
        r
    }
    /// Should a primitive draw now? In the hash pass, static primitives feed the fingerprint.
    fn on(&mut self, kind: u32, nums: &[f32], cols: &[u32], text: &str) -> bool {
        match self.pass {
            Pass::Hash => {
                if self.in_static {
                    let mut h = self.hash;
                    let mut mix = |v: u32| {
                        h = (h ^ v as u64).wrapping_mul(0x100000001b3);
                    };
                    mix(kind);
                    nums.iter().for_each(|n| mix(n.to_bits()));
                    cols.iter().for_each(|c| mix(*c));
                    text.bytes().for_each(|b| mix(b as u32));
                    self.hash = h;
                }
                false
            }
            Pass::Static => self.in_static,
            Pass::Dynamic => !self.in_static,
        }
    }
    fn ck(c: Rgb, a: f32) -> u32 {
        (c.0 as u32) << 24 | (c.1 as u32) << 16 | (c.2 as u32) << 8 | (a * 255.0) as u32
    }
    fn paint(c: Rgb, a: f32) -> Paint<'static> {
        let mut p = Paint::default();
        p.anti_alias = true;
        p.shader = Shader::SolidColor(col(c, a));
        p
    }
    fn fill(&mut self, path: Option<Path>, paint: &Paint) {
        if let Some(p) = path {
            let tf = self.tf();
            self.pm.fill_path(&p, paint, FillRule::Winding, tf, None);
        }
    }
    fn stroke(&mut self, path: Option<Path>, paint: &Paint, w: f32, round: bool) {
        if let Some(p) = path {
            let s = Stroke {
                width: w,
                line_cap: if round { LineCap::Round } else { LineCap::Butt },
                line_join: LineJoin::Round,
                ..Default::default()
            };
            let tf = self.tf();
            self.pm.stroke_path(&p, paint, &s, tf, None);
        }
    }
    fn fill_rrect(&mut self, r: R, rad: f32, c: Rgb, a: f32) {
        if !self.on(1, &[r.x, r.y, r.w, r.h, rad], &[Self::ck(c, a)], "") {
            return;
        }
        self.fill(rrect_path(r, rad), &Self::paint(c, a));
    }
    fn stroke_rrect(&mut self, r: R, rad: f32, c: Rgb, a: f32, w: f32) {
        if !self.on(2, &[r.x, r.y, r.w, r.h, rad, w], &[Self::ck(c, a)], "") {
            return;
        }
        self.stroke(rrect_path(r, rad), &Self::paint(c, a), w, false);
    }
    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, c: Rgb, a: f32, w: f32) {
        if !self.on(3, &[x1, y1, x2, y2, w], &[Self::ck(c, a)], "") {
            return;
        }
        self.stroke(poly_path(&[(x1, y1), (x2, y2)], false), &Self::paint(c, a), w, true);
    }
    fn dot(&mut self, cx: f32, cy: f32, rad: f32, c: Rgb, a: f32) {
        if !self.on(7, &[cx, cy, rad], &[Self::ck(c, a)], "") {
            return;
        }
        self.fill(PathBuilder::from_circle(cx, cy, rad), &Self::paint(c, a));
    }
    fn stroke_circle(&mut self, cx: f32, cy: f32, rad: f32, c: Rgb, a: f32, w: f32) {
        if !self.on(12, &[cx, cy, rad, w], &[Self::ck(c, a)], "") {
            return;
        }
        self.stroke(PathBuilder::from_circle(cx, cy, rad), &Self::paint(c, a), w, false);
    }
    fn fill_poly(&mut self, pts: &[(f32, f32)], c: Rgb, a: f32) {
        let flat: Vec<f32> = pts.iter().flat_map(|p| [p.0, p.1]).collect();
        if !self.on(8, &flat, &[Self::ck(c, a)], "") {
            return;
        }
        self.fill(poly_path(pts, true), &Self::paint(c, a));
    }
    /// Fill with a shader; `key` identifies it for the static fingerprint.
    fn shade(&mut self, path: Option<Path>, sh: Option<Shader<'static>>, key: &[f32]) {
        if !self.on(9, key, &[], "") {
            return;
        }
        if let Some(sh) = sh {
            let mut p = Paint::default();
            p.anti_alias = true;
            p.shader = sh;
            self.fill(path, &p);
        }
    }
    fn shade_stroke(&mut self, path: Option<Path>, sh: Option<Shader<'static>>, w: f32, key: &[f32]) {
        if !self.on(11, key, &[], "") {
            return;
        }
        if let Some(sh) = sh {
            let mut p = Paint::default();
            p.anti_alias = true;
            p.shader = sh;
            self.stroke(path, &p, w, false);
        }
    }

    // ---- text ----
    fn measure_t(&mut self, s: &str, face: Face, size: f32, track: f32) -> (f32, f32, f32, f32) {
        let sc = self.scale;
        match self.dw.as_deref_mut() {
            Some(d) => {
                let t = d.get_t(s, face, size * sc, track * sc);
                (t.adv / sc, t.asc / sc, t.desc / sc, t.cap / sc)
            }
            None => (s.chars().count() as f32 * (size * 0.55 + track), size * 0.9, size * 0.25, size * 0.7),
        }
    }
    fn measure(&mut self, s: &str, face: Face, size: f32) -> (f32, f32, f32, f32) {
        self.measure_t(s, face, size, 0.0)
    }
    fn text(&mut self, s: &str, x: f32, y: f32, face: Face, size: f32, c: Rgb, h: H, v: V) -> R {
        self.text_t(s, x, y, face, size, 0.0, c, h, v)
    }
    /// Text with letter-spacing `track` (logical px) and a 1 px soft shadow under it.
    fn text_t(&mut self, s: &str, x: f32, y: f32, face: Face, size: f32, track: f32, c: Rgb, h: H, v: V) -> R {
        if s.is_empty() {
            return R::new(x, y, 0.0, 0.0);
        }
        let (w, asc, desc, cap) = self.measure_t(s, face, size, track);
        let x0 = match h {
            H::L => x,
            H::C => x - w / 2.0,
            H::Rt => x - w,
        };
        let base = match v {
            V::C => y + cap / 2.0,
            V::Base => y,
        };
        let rect = R::new(x0, base - asc, w, asc + desc);
        if !self.on(10, &[x0, base, size, track, face as u8 as f32], &[Self::ck(c, 1.0)], s) {
            return rect;
        }
        let (sc, bw, bh) = (self.scale, self.bw, self.bh);
        let px = (self.ox + x0 * sc).round() as i32;
        let by = (self.oy + base * sc).round() as i32;
        if let Some(d) = self.dw.as_deref_mut() {
            let t = d.get_t(s, face, size * sc, track * sc);
            let sh = (sc.round() as i32).max(1);
            dw::blend(&t, self.pm.data_mut(), bw, bh, px, by + sh, (0, 0, 0), TEXT_SHADOW_A, false, 0);
            dw::blend(&t, self.pm.data_mut(), bw, bh, px, by, c, 1.0, false, 0);
        }
        rect
    }
    /// Small-caps style label: bold, tracked, secondary colour.
    fn cap(&mut self, s: &str, x: f32, y: f32, size: f32, h: H, v: V) -> R {
        self.text_t(s, x, y, Face::SansBold, size, size * 0.13, TEXT2, h, v)
    }
    /// Shorten with an ellipsis to fit `maxw`.
    fn fit(&mut self, s: &str, face: Face, size: f32, maxw: f32) -> String {
        if self.measure(s, face, size).0 <= maxw {
            return s.to_string();
        }
        let chars: Vec<char> = s.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let t: String = chars[..mid].iter().collect::<String>() + "…";
            if self.measure(&t, face, size).0 <= maxw { lo = mid } else { hi = mid - 1 }
        }
        chars[..lo].iter().collect::<String>() + "…"
    }
    /// Number + small unit on one baseline; returns the right edge.
    fn num_unit(&mut self, num: &str, unit: &str, x: f32, base: f32, face: Face, size: f32, usize_: f32, c: Rgb, h: H) -> f32 {
        let nw = self.measure(num, face, size).0;
        let uw = if unit.is_empty() { 0.0 } else { self.measure(unit, Face::SansSemi, usize_).0 + 1.0 };
        let x0 = match h {
            H::L => x,
            H::C => x - (nw + uw) / 2.0,
            H::Rt => x - nw - uw,
        };
        self.text(num, x0, base, face, size, c, H::L, V::Base);
        if !unit.is_empty() {
            self.text(unit, x0 + nw + 1.0, base, Face::SansSemi, usize_, TEXT2, H::L, V::Base);
        }
        x0 + nw + uw
    }
}

/// Glass rim + sheen over the body (static): a bright top edge fading down, a soft top sheen.
fn rim(g: &mut G, r: R, rad: f32) {
    g.stat(|g| {
        let key = [r.x, r.y, r.w, r.h, rad];
        g.shade(
            rrect_path(r, rad),
            vgrad(r.y, r.y + (r.h * 0.38).min(110.0), &[(0.0, (255, 255, 255), 0.10), (1.0, (255, 255, 255), 0.0)]),
            &key,
        );
        g.shade_stroke(
            rrect_path(r.grow(-0.5), rad - 0.5),
            vgrad(r.y, r.b(), &[(0.0, (255, 255, 255), 0.55), (0.35, (255, 255, 255), 0.16), (1.0, (255, 255, 255), 0.12)]),
            1.0,
            &key,
        );
    });
}

/// 60 s mini sparkline (legend).
fn mini_spark(g: &mut G, r: R, s: &Series, now: f64, lo: f32, hi: f32, c0: Rgb, c1: Rgb) {
    if s.pts.len() < 2 {
        g.line(r.x, r.cy(), r.r(), r.cy(), c0, 0.6, 1.5);
        return;
    }
    let pts: Vec<(f32, f32)> = s
        .pts
        .iter()
        .map(|&(t, v)| {
            let x = r.r() - ((now - t) / WINDOW_SECS) as f32 * r.w;
            (x.max(r.x), r.b() - ((v - lo) / (hi - lo).max(1.0)).clamp(0.0, 1.0) * r.h)
        })
        .collect();
    if !g.on(12, &[r.x, r.y, pts.len() as f32], &[], "") {
        return;
    }
    if let Some(sh) = hgrad(r.x, r.r(), &[(0.0, c0, 1.0), (1.0, c1, 1.0)]) {
        let mut p = Paint::default();
        p.anti_alias = true;
        p.shader = sh;
        g.stroke(poly_path(&pts, false), &p, 1.6, true);
    }
}

// ---- app ---------------------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Close,
    Pin,
    Island,
    Gpu,
    ShowAll,
    Report,
    History,
    Back,
    Export,
    Why,
    Settings,
    Set(settings::Ctl, i8),
}

const PANEL_W: f32 = 480.0;
const PANEL_R: f32 = 30.0;
const FIELD_H: f32 = 230.0;
const TOP_BAND: f32 = 50.0;
const ISLAND_H: f32 = 46.0;
const ISLAND_RH: f32 = 62.0;
const PAD_ISLAND: f32 = 18.0;
const PAD_PANEL: f32 = 30.0;
const ROW_H: f32 = 34.0;
const CHIP_H: f32 = 38.0;
const CHIP_STEP: f32 = 45.0;

/// Eased morph between the island and the panel (a 15 ms timer runs only while active).
struct Anim {
    from: (RECT, f32),
    to: (RECT, f32),
    start: Instant,
    dur: Duration,
    to_island: bool,
}

/// Resolved glass style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Glass {
    Tint,
    Blur,
}

struct App {
    cfg: Config,
    path: std::path::PathBuf,
    warn: Option<String>,
    /// First start only: how to find Settings and the modes.
    hint: Option<String>,
    s: Sampler,
    hwnd: HWND,
    scale: f32,
    glass: Glass,
    transparency: Option<bool>,
    blur_api: bool,
    surf: Option<Surface>,
    geo: Option<Geometry>,
    layer: Vec<u8>,
    layer_hash: u64,
    layer_redraws: u64,
    fg: Vec<u8>,
    fg_valid: bool,
    /// cached glass body (shadow + tint + field + level) and what it was built from
    bg: Vec<u8>,
    bg_key: Option<(Body, Shape, i32, i32)>,
    aurora: Aurora,
    clock: Instant,
    dw: Option<Dw>,
    hits: Vec<(R, Btn)>,
    hover: Option<Btn>,
    pressed: Option<Btn>,
    tracking: bool,
    island_state: Island,
    /// screen rect of the glass shape (without the shadow padding)
    shape: RECT,
    shown: bool,
    panel_h: f32,
    panel_fixed_h: f32,
    anim: Option<Anim>,
    /// island: mouse-down point (screen) while deciding between click and drag
    down_at: Option<POINT>,
    // diagnostics (TACHYON_DEBUG=1 / RIG_DEBUG=1)
    debug: bool,
    frames: u64,
    ease_frames: u64,
    paint_total: Duration,
    paint_last: Duration,
    skipped: u64,
    started: Instant,
    last_cpu: (Instant, f64),
    mem_log: Vec<(&'static str, f64, f64)>,
    dump: Option<std::path::PathBuf>,
    /// current UI mode; in companion mode Hex is shown and the main window only as the panel
    mode: UiMode,
    comp: Option<companion::Companion>,
    /// the main window is hidden (companion mode without the panel open)
    main_hidden: bool,
    /// panel view: main, crash report or job history
    view: View,
    /// the bottleneck's numbers are shown under the sentence
    show_why: bool,
    /// finished-job counter and the jobs read for the History view
    hist_cache: Option<(u64, Vec<tachyon::history::JobRecord>)>,
    export_msg: Option<String>,
    /// Settings view state, only while it is open
    set: Option<Box<settings::SetUi>>,
    /// flight recorder (on, minutes) as started: changing it needs a restart
    boot_blackbox: (bool, u32),
    /// the poll timer's current interval
    s_poll_ms: u64,
}

static mut APP: *mut App = null_mut();
#[allow(static_mut_refs)]
/// After a panic (the log is written): tell the user once, without any technical noise.
pub fn crash_box(log: Option<&std::path::Path>) {
    let where_ = log.map(|p| format!("\n\nA short report was saved to:\n{}", p.display())).unwrap_or_default();
    let text = wz(&format!("Tachyon hit an internal error and closed. Start it again to carry on.{where_}"));
    let title = wz("Tachyon");
    unsafe {
        MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND);
    }
}

/// Posted by a second start to the running Tachyon: show yourself.
const WM_TACHYON_SHOW: u32 = WM_APP + 1;
const MAIN_CLASS: &str = "TachyonV5";

/// Mutex name for one settings file: the same file means the same Tachyon.
pub(crate) fn instance_name(config: &std::path::Path) -> String {
    let key = config.to_string_lossy().to_lowercase();
    let h = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    format!("Local\\TachyonInstance-{h:016x}")
}

/// One Tachyon per settings file. A second start shows the running one and exits.
unsafe fn single_instance(config: &std::path::Path) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let sa = tachyon::winsec::Private::new();
    let name = wz(&instance_name(config));
    let h = CreateMutexW(sa.as_ref().map_or(null(), |s| s.ptr()), 0, name.as_ptr());
    if h.is_null() {
        return true; // cannot tell: better two than none
    }
    if GetLastError() != ERROR_ALREADY_EXISTS {
        return true; // ours for the life of the process (never closed)
    }
    let cls = wz(MAIN_CLASS);
    let other = FindWindowW(cls.as_ptr(), null());
    if !other.is_null() {
        PostMessageW(other, WM_TACHYON_SHOW, 0, 0);
    }
    false
}

/// Shown in the panel on the very first start.
const WELCOME: &str = "Welcome to Tachyon. Right-click it (or use the gear) for Settings. Modes: island at the top of the screen, Hex on the desktop, or this panel.";

fn app() -> Option<&'static mut App> {
    unsafe { APP.as_mut() }
}

fn temps(cfg: &Config) -> (f32, f32) {
    (cfg.gpu.temp_amber_c, cfg.gpu.temp_red_c)
}
fn load_temp(s: &Snapshot) -> (Option<f32>, Option<f32>) {
    match &s.sample {
        Some(g) => (g.util_pct.map(|u| u as f32), g.temp_c.map(|t| t as f32)),
        None => (None, None),
    }
}

pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let debug = ["TACHYON_DEBUG", "RIG_DEBUG"].iter().any(|k| std::env::var(k).is_ok_and(|v| v != "0"));
        if debug {
            attach_console();
        }
        let m0 = mem_mb();
        let mut mem_log = vec![("start", m0.0, m0.1)];
        let path = config::config_path();
        if !single_instance(&path) {
            return;
        }
        let loaded = config::load(&path);
        let (cfg, mut warn, first_run, path) = (loaded.cfg, loaded.warn, loaded.first_run, loaded.path);
        if let Some(log) = tachyon::crashlog::logs_dir().and_then(|d| tachyon::crashlog::take_last(&d)) {
            warn.get_or_insert_with(|| format!("Tachyon closed after an internal error last time. Report: {}", log.display()));
        }
        if std::env::var_os("TACHYON_TEST_PANIC").is_some() {
            panic!("test panic (TACHYON_TEST_PANIC)");
        }
        let mut dw = Dw::new();
        if let (Some(d), Some(path)) = (dw.as_mut(), std::env::var_os("TACHYON_DWTEST")) {
            let _ = std::fs::write(
                path,
                dw::debug_pgm(&d.get("lIl|H", Face::Sans, std::env::var("TACHYON_DWPX").ok().and_then(|v| v.parse().ok()).unwrap_or(14.0))),
            );
        }
        let m1 = mem_mb();
        mem_log.push(("DirectWrite + fonts", m1.0, m1.1));
        let mut s = Sampler::new(cfg.clone());
        s.tick();
        let m2 = mem_mb();
        mem_log.push(("NVML + first sample", m2.0, m2.1));
        let hinst = GetModuleHandleW(null());
        let cls = wz(MAIN_CLASS);
        let icon = LoadIconW(hinst, 1 as _);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: icon,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: cls.as_ptr(),
            hIconSm: icon,
        };
        RegisterClassExW(&wc);
        companion::register(hinst, icon);
        let transparency = present::transparency_effects();
        let glass = if cfg.glass == GlassMode::Blur { Glass::Blur } else { Glass::Tint };
        let (lt, tt) = load_temp(&s.snap);
        let (amb, red) = temps(&cfg);
        let aurora = Aurora::new(lt, tt, amb, red);
        let a = Box::into_raw(Box::new(App {
            cfg,
            path,
            warn,
            hint: first_run.then(|| WELCOME.to_string()),
            s,
            hwnd: null_mut(),
            scale: 1.0,
            glass,
            transparency,
            blur_api: false,
            surf: None,
            geo: None,
            layer: vec![],
            layer_hash: 0,
            layer_redraws: 0,
            fg: vec![],
            fg_valid: false,
            bg: vec![],
            bg_key: None,
            aurora,
            clock: Instant::now(),
            dw,
            hits: vec![],
            hover: None,
            pressed: None,
            tracking: false,
            island_state: Island::Idle,
            shape: std::mem::zeroed(),
            shown: false,
            panel_h: 600.0,
            panel_fixed_h: 520.0,
            anim: None,
            down_at: None,
            debug,
            frames: 0,
            ease_frames: 0,
            paint_total: Duration::ZERO,
            paint_last: Duration::ZERO,
            skipped: 0,
            started: Instant::now(),
            last_cpu: (Instant::now(), cpu_secs()),
            mem_log,
            dump: std::env::var_os("TACHYON_DUMP_DIR").map(Into::into),
            mode: UiMode::Panel,
            comp: None,
            main_hidden: false,
            view: match std::env::var("TACHYON_VIEW").as_deref() {
                Ok("report") => View::Report,
                Ok("history") => View::History,
                Ok("settings") => View::Settings,
                _ => View::Main,
            },
            show_why: false,
            hist_cache: None,
            export_msg: None,
            set: None,
            boot_blackbox: (false, 0),
            s_poll_ms: 0,
        }));
        APP = a;
        let ap = &mut *a;
        ap.boot_blackbox = (ap.cfg.blackbox, ap.cfg.blackbox_minutes);
        ap.s_poll_ms = ap.cfg.poll_ms;
        if ap.view == View::Settings {
            let tab = match std::env::var("TACHYON_SETTINGS_TAB").as_deref() {
                Ok("projects") => settings::Tab::Projects,
                Ok("alerts") => settings::Tab::Alerts,
                Ok("data") => settings::Tab::Data,
                Ok("ai") => settings::Tab::Ai,
                _ => settings::Tab::General,
            };
            ap.set = Some(Box::new(settings::SetUi::new(tab)));
        }
        // the first start, and a start with something to say, open the panel where the message is
        let speak = first_run || ap.warn.is_some();
        let start = if speak && std::env::var_os("TACHYON_VIEW").is_none() { UiMode::Panel } else { ap.cfg.start_mode() };
        ap.mode = start;
        if start != UiMode::Companion {
            ap.cfg.window.compact = start == UiMode::Island;
        }
        let island = start == UiMode::Island;
        let hwnd = present::create(MAIN_CLASS, "Tachyon", if island { WS_EX_NOACTIVATE } else { 0 }, hinst);
        if hwnd.is_null() {
            return;
        }
        ap.hwnd = hwnd;
        let dpi = GetDpiForWindow(hwnd);
        if dpi > 0 {
            ap.scale = dpi as f32 / 96.0;
        }
        ap.island_state = Island::from_render(ap.s.snap.render.is_some());
        if !island {
            // panel height comes from its content: one layout pass first
            ap.shape = ap.panel_rect_from_cfg();
            ap.frame(true);
        }
        ap.shape = if island { ap.island_rect_from_cfg() } else { ap.panel_rect_from_cfg() };
        if start == UiMode::Companion {
            ap.cfg.window.compact = false;
            ap.comp = companion::Companion::new(hinst, &ap.cfg, &ap.s.snap);
            match ap.comp.as_mut() {
                Some(c) => {
                    ap.main_hidden = true;
                    c.show();
                }
                None => ap.mode = UiMode::Panel,
            }
        }
        if !ap.main_hidden {
            ap.apply_mode(island);
            ap.frame(true);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            ap.shown = true;
        }
        SetTimer(hwnd, 1, ap.cfg.poll_interval().as_millis() as u32, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE};
    let h = GetStdHandle(STD_ERROR_HANDLE);
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn cpu_secs() -> f64 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let (mut a, mut b, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) =
            (std::mem::zeroed(), std::mem::zeroed(), std::mem::zeroed(), std::mem::zeroed());
        GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
        let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 1e7;
        f(k) + f(u)
    }
}

/// (working set MB, private bytes MB)
fn mem_mb() -> (f64, f64) {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut m: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        m.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m as *mut _ as *mut PROCESS_MEMORY_COUNTERS, m.cb);
        (m.WorkingSetSize as f64 / 1048576.0, m.PrivateUsage as f64 / 1048576.0)
    }
}

/// Trim the working set (pages fault back in on demand). Same effect as SetProcessWorkingSetSize(-1, -1).
fn trim_ws() {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32)
}

/// True when nothing of the window can be seen: minimised, cloaked, or fully covered by an
/// opaque window above it in z-order (best-effort heuristic; layered/transparent windows ignored).
unsafe fn hidden(hwnd: HWND) -> bool {
    if IsIconic(hwnd) != 0 || IsWindowVisible(hwnd) == 0 {
        return true;
    }
    let cloaked = |h: HWND| {
        let mut v = 0u32;
        DwmGetWindowAttribute(h, DWMWA_CLOAKED as u32, &mut v as *mut _ as *mut _, 4) == 0 && v != 0
    };
    if cloaked(hwnd) {
        return true;
    }
    let mut me: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut me);
    let mut h = GetWindow(hwnd, GW_HWNDPREV);
    let mut n = 0;
    while !h.is_null() && n < 128 {
        n += 1;
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if IsWindowVisible(h) != 0 && IsIconic(h) == 0 && ex & (WS_EX_LAYERED | WS_EX_TRANSPARENT) == 0 && !cloaked(h) {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(h, &mut r);
            if r.left <= me.left && r.top <= me.top && r.right >= me.right && r.bottom >= me.bottom {
                return true;
            }
        }
        h = GetWindow(h, GW_HWNDPREV);
    }
    false
}

/// Windows build number (RtlGetVersion is not subject to the compatibility manifest lie).
fn os_build() -> u32 {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    #[repr(C)]
    struct Ver {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    unsafe {
        let ntdll = GetModuleHandleW(wz("ntdll.dll").as_ptr());
        let Some(f) = GetProcAddress(ntdll, c"RtlGetVersion".as_ptr() as *const u8) else { return 0 };
        let f: unsafe extern "system" fn(*mut Ver) -> i32 = std::mem::transmute(f);
        let mut v: Ver = std::mem::zeroed();
        v.size = size_of::<Ver>() as u32;
        if f(&mut v) != 0 {
            return 0;
        }
        if v.major > 10 { 99999 } else { v.build }
    }
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}
fn lerp_rect(a: RECT, b: RECT, t: f32) -> RECT {
    let l = |x: i32, y: i32| (x as f32 + (y - x) as f32 * t).round() as i32;
    RECT { left: l(a.left, b.left), top: l(a.top, b.top), right: l(a.right, b.right), bottom: l(a.bottom, b.bottom) }
}
fn rw(r: &RECT) -> i32 {
    r.right - r.left
}
fn rh(r: &RECT) -> i32 {
    r.bottom - r.top
}

impl App {
    fn island(&self) -> bool {
        self.cfg.window.compact
    }
    fn render_active(&self) -> bool {
        self.s.snap.render.is_some()
    }
    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    fn measure_t(&mut self, s: &str, face: Face, size: f32, track: f32) -> f32 {
        let sc = self.scale;
        match self.dw.as_mut() {
            Some(d) => d.get_t(s, face, size * sc, track * sc).adv / sc,
            None => s.chars().count() as f32 * (size * 0.55 + track),
        }
    }

    /// Island size (logical) for the current state; templates use the widest digits so the
    /// capsule does not jitter as values change.
    fn island_size(&mut self) -> (f32, f32) {
        match self.island_state {
            Island::Idle => {
                let cap = |a: &mut App, s: &str| a.measure_t(s, Face::SansBold, 9.5, 9.5 * 0.13) + 4.0;
                let big = self.measure_t("100", Face::SansBold, 20.0, 0.0) + self.measure_t("%", Face::SansSemi, 11.0, 0.0) + 1.0;
                let m = |a: &mut App, n: &str, u: &str| {
                    a.measure_t(n, Face::SansSemi, 14.0, 0.0) + a.measure_t(u, Face::SansSemi, 11.0, 0.0) + 1.0
                };
                let w = 18.0
                    + 8.0
                    + 14.0
                    + big
                    + 14.0
                    + cap(self, "TEMP")
                    + m(self, "100", "°")
                    + 14.0
                    + cap(self, "VRAM")
                    + m(self, "99.9", " GB")
                    + 14.0
                    + cap(self, "PWR")
                    + m(self, "999", " W")
                    + 18.0;
                (w.ceil(), ISLAND_H)
            }
            Island::Rendering => {
                let pct = self.measure_t("100", Face::SansBold, 26.0, 0.0) + self.measure_t(".0%", Face::SansSemi, 13.0, 0.0) + 2.0;
                let right = self.measure_t("100% · 100° · 10h 59m left", Face::Sans, 11.0, 0.0).max(self.measure_t(
                    "00:00",
                    Face::SansBold,
                    16.0,
                    0.0,
                ));
                ((24.0 + pct + 16.0 + 200.0 + 16.0 + right + 24.0).ceil(), ISLAND_RH)
            }
        }
    }

    unsafe fn work_area_at(&self, x: i32, y: i32) -> (i32, i32, i32, i32) {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let w = mi.rcWork;
        (w.left, w.top, w.right, w.bottom)
    }

    /// Island rect from the saved anchor (centre x, top y).
    unsafe fn island_rect_from_cfg(&mut self) -> RECT {
        let (lw, lh) = self.island_size();
        let (w, h) = ((lw * self.scale).round() as i32, (lh * self.scale).round() as i32);
        let wc = &self.cfg.window;
        let work = match (wc.island_cx, wc.island_y) {
            (Some(x), Some(y)) => self.work_area_at(x as i32, y as i32 + 5),
            _ => self.work_area_at(0, 0),
        };
        let (x, y) = island_pos(work, (w, h), wc.island_cx, wc.island_y);
        RECT { left: x, top: y, right: x + w, bottom: y + h }
    }

    fn panel_size_px(&self) -> (i32, i32) {
        ((PANEL_W * self.scale).round() as i32, (self.panel_h * self.scale).round() as i32)
    }

    unsafe fn panel_rect_from_cfg(&self) -> RECT {
        let wc = &self.cfg.window;
        let (w, h) = self.panel_size_px();
        if let (Some(x), Some(y)) = (wc.x, wc.y) {
            let rc = RECT { left: x as i32, top: y as i32, right: x as i32 + 60, bottom: y as i32 + 40 };
            if !MonitorFromRect(&rc, MONITOR_DEFAULTTONULL).is_null() {
                return RECT { left: x as i32, top: y as i32, right: x as i32 + w, bottom: y as i32 + h };
            }
        }
        let (l, t, r, b) = self.work_area_at(0, 0);
        let (x, y) = ((l + r - w) / 2, ((t + b - h) / 2).max(t + 10));
        RECT { left: x, top: y, right: x + w, bottom: y + h }
    }

    fn pad_px(&self) -> i32 {
        let p = if self.island() && self.anim.is_none() { PAD_ISLAND } else { PAD_PANEL };
        (p * self.scale).round() as i32
    }

    /// Where the shape should be now: the current position (it may have been dragged) with the
    /// size of the current state; the island keeps its centre x and top, the panel its top-left.
    unsafe fn target_shape(&mut self) -> RECT {
        if let (Some(_), true) = (&self.surf, self.shown) {
            let mut wr: RECT = std::mem::zeroed();
            if GetWindowRect(self.hwnd, &mut wr) != 0 {
                let old_pad = self.geo.as_ref().map(|g| g.shape.x.round() as i32).unwrap_or(self.pad_px());
                let w = rw(&self.shape);
                self.shape = RECT {
                    left: wr.left + old_pad,
                    top: wr.top + old_pad,
                    right: wr.left + old_pad + w,
                    bottom: wr.top + old_pad + rh(&self.shape),
                };
            }
        }
        let cur = self.shape;
        if self.island() {
            let (lw, lh) = self.island_size();
            let (w, h) = ((lw * self.scale).round() as i32, (lh * self.scale).round() as i32);
            let cx = (cur.left + cur.right) / 2;
            RECT { left: cx - w / 2, top: cur.top, right: cx - w / 2 + w, bottom: cur.top + h }
        } else {
            let (w, h) = self.panel_size_px();
            RECT { left: cur.left, top: cur.top, right: cur.left + w, bottom: cur.top + h }
        }
    }

    fn remember_geometry(&mut self) {
        if self.anim.is_some() {
            return;
        }
        let r = self.shape;
        let w = &mut self.cfg.window;
        if w.compact {
            w.island_cx = Some(((r.left + r.right) / 2) as f32);
            w.island_y = Some(r.top as f32);
        } else {
            w.x = Some(r.left as f32);
            w.y = Some(r.top as f32);
            w.width = PANEL_W;
            w.height = self.panel_h.round();
        }
    }

    /// Island: no activation, always on top. Panel: activates normally; on top when pinned.
    unsafe fn apply_mode(&mut self, island: bool) {
        present::set_ex_flag(self.hwnd, WS_EX_NOACTIVATE, island);
        present::set_topmost(self.hwnd, island || self.cfg.window.always_on_top);
    }

    /// Start the eased morph to the island or the panel.
    fn toggle_island(&mut self) {
        if self.anim.is_some() {
            return;
        }
        if self.mode == UiMode::Companion {
            if self.main_hidden {
                self.open_panel_from_companion();
            } else {
                self.hide_main_in_companion();
            }
            return;
        }
        unsafe {
            let _ = self.target_shape();
        }
        self.remember_geometry();
        let to_island = !self.island();
        if to_island {
            self.view = View::Main;
            self.set = None;
        }
        unsafe {
            let from = (self.shape, self.cur_radius());
            self.cfg.window.compact = to_island;
            self.island_state = Island::from_render(self.render_active());
            self.hover = None;
            self.fg_valid = false;
            let to = if to_island { self.island_rect_from_cfg() } else { self.panel_rect_from_cfg() };
            let tr = if to_island { rh(&to) as f32 / 2.0 } else { PANEL_R * self.scale };
            self.start_anim(from, (to, tr), to_island);
        }
        config::save_window(&self.path, &self.cfg.window);
    }

    fn cur_radius(&self) -> f32 {
        if self.island() { rh(&self.shape) as f32 / 2.0 } else { PANEL_R * self.scale }
    }

    unsafe fn start_anim(&mut self, from: (RECT, f32), to: (RECT, f32), to_island: bool) {
        self.anim = Some(Anim { from, to, start: Instant::now(), dur: Duration::from_millis(220), to_island });
        if !to_island {
            self.apply_mode(false);
        }
        present::set_topmost(self.hwnd, true);
        SetTimer(self.hwnd, 2, 15, None);
        self.anim_step();
    }

    unsafe fn anim_step(&mut self) {
        let Some(an) = &self.anim else { return };
        let t = (an.start.elapsed().as_secs_f32() / an.dur.as_secs_f32()).min(1.0);
        if t >= 1.0 {
            let to_island = an.to_island;
            self.shape = an.to.0;
            KillTimer(self.hwnd, 2);
            self.anim = None;
            self.apply_mode(to_island);
            self.fg_valid = false;
            self.frame(true);
            if to_island {
                // the panel's buffers and its text rasters are gone now; give the pages back
                if let Some(d) = self.dw.as_mut() {
                    d.trim();
                }
                self.bg.shrink_to_fit();
                trim_ws();
            }
            return;
        }
        self.frame(false);
    }

    fn toggle_pin(&mut self) {
        let w = &mut self.cfg.window;
        w.always_on_top = !w.always_on_top;
        unsafe {
            if !self.island() {
                self.apply_mode(false);
            }
            self.fg_valid = false;
            self.frame(false);
        }
        config::save_window(&self.path, &self.cfg.window);
    }

    fn toggle_show_all(&mut self) {
        self.cfg.window.show_all_processes = !self.cfg.window.show_all_processes;
        config::save_window(&self.path, &self.cfg.window);
        unsafe { self.frame(true) };
    }

    fn select_gpu(&mut self, i: u32) {
        if i != self.s.selected {
            self.s.selected = i;
            self.s.tick();
            unsafe { self.frame(true) };
        }
    }

    /// Right-click menu on Hex or its bubble.
    fn menu_from(&mut self, owner: HWND, x: i32, y: i32) {
        self.menu_owned(owner, x, y, true);
    }

    fn menu(&mut self, x: i32, y: i32, full: bool) {
        self.menu_owned(self.hwnd, x, y, full);
    }

    fn menu_owned(&mut self, owner: HWND, x: i32, y: i32, full: bool) {
        unsafe {
            let m = CreatePopupMenu();
            if full {
                let open_label = if self.mode == UiMode::Companion {
                    if self.main_hidden { "Open panel" } else { "Hide panel" }
                } else if self.island() {
                    "Open panel"
                } else {
                    "Collapse to island"
                };
                AppendMenuW(m, MF_STRING, 1, wz(open_label).as_ptr());
                AppendMenuW(m, MF_STRING | if self.cfg.window.always_on_top { MF_CHECKED } else { 0 }, 2, wz("Pin panel on top").as_ptr());
                AppendMenuW(
                    m,
                    MF_STRING | if self.cfg.window.show_all_processes { MF_CHECKED } else { 0 },
                    4,
                    wz("Show all GPU processes").as_ptr(),
                );
                AppendMenuW(m, MF_SEPARATOR, 0, null());
                for (id, md, label) in [
                    (10, UiMode::Island, "Mode: island"),
                    (11, UiMode::Companion, "Mode: companion (Hex)"),
                    (12, UiMode::Panel, "Mode: panel"),
                ] {
                    let fl = MF_STRING | if self.mode == md { MF_CHECKED } else { 0 };
                    AppendMenuW(m, fl, id, wz(label).as_ptr());
                }
                AppendMenuW(m, MF_SEPARATOR, 0, null());
            }
            for (i, n) in self.s.snap.gpus.iter().enumerate() {
                let fl = MF_STRING | if i as u32 == self.s.snap.selected { MF_CHECKED } else { 0 };
                AppendMenuW(m, fl, 100 + i, wz(&format!("GPU {i}: {n}")).as_ptr());
            }
            if full {
                AppendMenuW(m, MF_SEPARATOR, 0, null());
                if self.s.snap.insights.data_dir.is_some() {
                    AppendMenuW(m, MF_STRING, 20, wz("Crash report").as_ptr());
                    AppendMenuW(m, MF_STRING | if self.cfg.history { 0 } else { MF_GRAYED }, 21, wz("Job history").as_ptr());
                    AppendMenuW(m, MF_STRING | if self.cfg.blackbox { 0 } else { MF_GRAYED }, 22, wz("Export flight recorder").as_ptr());
                    AppendMenuW(m, MF_SEPARATOR, 0, null());
                }
                AppendMenuW(m, MF_STRING, 23, wz("Settings…").as_ptr());
                AppendMenuW(m, MF_STRING, 3, wz("Close Tachyon").as_ptr());
            }
            SetForegroundWindow(owner);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, x, y, 0, owner, null());
            DestroyMenu(m);
            match cmd {
                1 => self.toggle_island(),
                2 => self.toggle_pin(),
                3 => {
                    PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
                }
                4 => self.toggle_show_all(),
                10 => self.set_mode(UiMode::Island),
                11 => self.set_mode(UiMode::Companion),
                12 => self.set_mode(UiMode::Panel),
                20 => self.open_view(View::Report),
                21 => self.open_view(View::History),
                22 => {
                    self.open_view(View::Report);
                    self.export_blackbox();
                }
                23 => self.open_settings(None),
                c if c >= 100 => self.select_gpu((c - 100) as u32),
                _ => {}
            }
        }
    }

    fn click(&mut self, b: Btn) {
        match b {
            Btn::Close => unsafe {
                PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
            },
            Btn::Pin => self.toggle_pin(),
            Btn::Island => self.toggle_island(),
            Btn::ShowAll => self.toggle_show_all(),
            Btn::Report => self.open_view(View::Report),
            Btn::History => self.open_view(View::History),
            Btn::Back => self.open_view(View::Main),
            Btn::Export => self.export_blackbox(),
            Btn::Settings => {
                if self.view == View::Settings {
                    self.open_view(View::Main)
                } else {
                    self.open_settings(None)
                }
            }
            Btn::Set(c, d) => self.settings_click(c, d),
            Btn::Why => {
                self.show_why = !self.show_why;
                self.fg_valid = false;
                unsafe { self.frame(true) };
            }
            Btn::Gpu => {
                if let Some((r, _)) = self.hits.iter().find(|(_, x)| *x == Btn::Gpu) {
                    let p = POINT { x: self.shape.left + (r.x * self.scale) as i32, y: self.shape.top + (r.b() * self.scale) as i32 };
                    self.menu(p.x, p.y, false);
                }
            }
        }
    }

    /// Client px -> logical coordinates relative to the glass shape.
    fn to_logical(&self, x: i32, y: i32) -> (f32, f32) {
        let pad = self.geo.as_ref().map(|g| g.shape.x).unwrap_or(0.0);
        ((x as f32 - pad) / self.scale, (y as f32 - pad) / self.scale)
    }
    fn inside_shape(&self, cx: i32, cy: i32) -> bool {
        self.geo.as_ref().is_some_and(|g| g.shape.sd(cx as f32 + 0.5, cy as f32 + 0.5) <= 0.5)
    }

    fn btn_at(&self, x: f32, y: f32) -> Option<Btn> {
        self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, b)| *b)
    }

    fn on_timer(&mut self) {
        self.s.tick();
        unsafe {
            // the island grows/shrinks when a render starts or ends
            let st = Island::from_render(self.render_active());
            if self.island() && !self.main_hidden && st != self.island_state && self.anim.is_none() {
                let _ = self.target_shape();
                let from = (self.shape, self.cur_radius());
                self.remember_geometry();
                self.island_state = st;
                let to = self.island_rect_from_cfg();
                self.fg_valid = false;
                self.start_anim(from, (to, rh(&to) as f32 / 2.0), true);
            }
            self.island_state = st;
            let (l, t) = load_temp(&self.s.snap);
            let (amb, red) = temps(&self.cfg);
            let now = self.now_ms();
            if self.aurora.update(l, t, amb, red, now) && !self.main_hidden {
                SetTimer(self.hwnd, 3, 100, None);
            }
            if self.anim.is_none() {
                self.frame(true);
            }
        }
        if self.comp.is_some() {
            self.hex_poll();
        }
        if self.s.snap.insights.report_new && !self.main_hidden && !self.island() && self.anim.is_none() {
            // the banner is on screen: Hex does not need to announce it again
            self.s.engine.mark_announced(&mut self.s.snap.insights);
        }
        let ticks = self.s.snap.ticks;
        if ticks == 3 {
            let m = mem_mb();
            self.mem_log.push(("after first paints", m.0, m.1));
            trim_ws();
            let m = mem_mb();
            self.mem_log.push(("after working-set trim", m.0, m.1));
            if self.debug {
                for (k, ws, pv) in &self.mem_log {
                    eprintln!("[tachyon] mem {k:<24} WS {ws:6.1} MB  private {pv:6.1} MB");
                }
                eprintln!(
                    "[tachyon] {} config {} | mode {} (hex atlas {}) | windows build {} | glass {} (config \"{}\", blur api {}) | transparency effects {} | layered ULW_ALPHA | scale {:.2} text {}",
                    env!("CARGO_PKG_VERSION"),
                    self.path.display(),
                    self.mode.as_str(),
                    self.comp.as_ref().map(|c| c.atlas_key()).unwrap_or("-"),
                    os_build(),
                    match self.glass {
                        Glass::Tint => "tint",
                        Glass::Blur => "blur",
                    },
                    self.cfg.glass.as_str(),
                    self.blur_api,
                    match self.transparency {
                        Some(true) => "on",
                        Some(false) => "off",
                        None => "unknown",
                    },
                    self.scale,
                    if self.dw.is_some() { "DirectWrite" } else { "unavailable" }
                );
            }
        }
        if self.debug && ticks.is_multiple_of(10) {
            let (ws, pv) = mem_mb();
            let now = Instant::now();
            let cpu = cpu_secs();
            let pct = (cpu - self.last_cpu.1) / (now - self.last_cpu.0).as_secs_f64().max(1e-3) * 100.0;
            self.last_cpu = (now, cpu);
            eprintln!(
                "[tachyon] t={:.0}s ticks {} hex-frames {} ({}) frames {} ease-frames {} skipped {} layer-redraws {} glyph-rasters {} | frame avg {:.2} ms last {:.2} ms | poll {:.1} ms | WS {:.1} MB private {:.1} MB | CPU {:.2} s total, {:.2}% last 10 ticks",
                self.started.elapsed().as_secs_f64(),
                ticks,
                self.comp.as_ref().map(|c| c.frames).unwrap_or(0),
                self.comp.as_ref().map(|c| c.debug()).unwrap_or_default(),
                self.frames,
                self.ease_frames,
                self.skipped,
                self.layer_redraws,
                self.dw.as_ref().map(|d| d.rasterised).unwrap_or(0),
                self.paint_total.as_secs_f64() * 1000.0 / self.frames.max(1) as f64,
                self.paint_last.as_secs_f64() * 1000.0,
                self.s.snap.poll_cost_ms,
                ws,
                pv,
                cpu,
                pct
            );
        }
    }

    fn on_ease_timer(&mut self) {
        unsafe {
            if self.anim.is_none() {
                self.ease_frames += 1;
                self.frame(false);
            }
            if !self.aurora.animating() || self.main_hidden {
                KillTimer(self.hwnd, 3);
            }
        }
    }

    /// Glass body for the current state.
    fn body(&mut self, g: &Geometry, field: Field) -> Body {
        let sh = g.shape;
        let (tint, tint_a) = match (self.glass, self.transparency) {
            (_, Some(false)) => (TINT, TINT_OPAQUE_A),
            (Glass::Blur, _) => (BLUR_TINT, BLUR_TINT_A),
            (Glass::Tint, _) => (TINT, TINT_A),
        };
        let panel_like = sh.h > 120.0 * self.scale;
        let rendering = !panel_like && self.island() && self.island_state == Island::Rendering && self.anim.is_none();
        let layout = if panel_like {
            let fh = (FIELD_H * self.scale).min(sh.h);
            FieldLayout {
                x: sh.x - 0.1 * sh.w,
                y: sh.y - 0.2 * fh,
                w: sh.w * 1.2,
                h: fh * 1.2,
                blobs: [(0.22, 0.70, 0.33, 0.55), (0.55, 0.45, 0.42, 0.65), (0.88, 0.72, 0.28, 0.50)],
                peak: FIELD_PEAK,
                gain: [1.0, 1.0, 1.0],
            }
        } else {
            FieldLayout {
                x: sh.x - 0.1 * sh.w,
                y: sh.y - 0.3 * sh.h,
                w: sh.w * 1.2,
                h: sh.h * 1.6,
                blobs: if rendering {
                    [(0.18, 0.55, 0.40, 0.70), (0.55, 0.40, 0.45, 0.80), (0.85, 0.50, 0.40, 0.80)]
                } else {
                    [(0.18, 0.55, 0.40, 0.70), (0.55, 0.40, 0.45, 0.80), (0.92, 0.60, 0.30, 0.70)]
                },
                peak: FIELD_PEAK,
                gain: if rendering { [0.0, 0.0, 0.7] } else { [1.0, 1.0, 1.0] },
            }
        };
        let level = if rendering {
            self.s
                .snap
                .render
                .as_ref()
                .and_then(|rv| rv.progress.as_ref())
                .and_then(|p| p.total.map(|t| p.frame as f32 / t.max(1) as f32))
                .map(|f| LevelFill { end: sh.x + sh.w * f.clamp(0.0, 1.0), c0: CYAN, a0: 0.04, c1: VIOLET, a1: 0.55 })
        } else {
            None
        };
        Body { tint, tint_a, field: Some((field, layout)), level }
    }

    /// Build and present one frame. `data`: the values changed (redraw the foreground).
    unsafe fn frame(&mut self, data: bool) {
        if self.main_hidden {
            return;
        }
        if self.shown && self.anim.is_none() && hidden(self.hwnd) {
            self.skipped += 1;
            return;
        }
        let t0 = Instant::now();
        let (shape, rad) = match &self.anim {
            Some(an) => {
                let t = ease((an.start.elapsed().as_secs_f32() / an.dur.as_secs_f32()).min(1.0));
                (lerp_rect(an.from.0, an.to.0, t), an.from.1 + (an.to.1 - an.from.1) * t)
            }
            None => {
                let s = self.target_shape();
                let r = if self.island() { rh(&s) as f32 / 2.0 } else { PANEL_R * self.scale };
                (s, r)
            }
        };
        self.shape = if self.anim.is_none() { shape } else { self.shape };
        let pad = self.pad_px();
        let (sw, sh) = (rw(&shape).max(8), rh(&shape).max(8));
        let (w, h) = (sw + 2 * pad, sh + 2 * pad);
        if self.surf.as_ref().is_none_or(|s| s.w != w || s.h != h) {
            self.surf = Surface::new(w, h);
            self.layer = vec![0u8; (w * h * 4) as usize];
            self.fg = vec![0u8; (w * h * 4) as usize];
            self.layer_hash = 0;
            self.fg_valid = false;
        }
        let gshape = Shape { x: pad as f32, y: pad as f32, w: sw as f32, h: sh as f32, r: rad };
        if self.geo.as_ref().is_none_or(|g| g.w != w as usize || g.h != h as usize || g.shape != gshape) {
            let k = self.scale;
            let big = sh as f32 > 120.0 * k;
            let spec = if big {
                ShadowSpec { key_dy: 12.0 * k, key_blur: 24.0 * k, key_a: 0.34, near_dy: 2.0 * k, near_blur: 6.0 * k, near_a: 0.20 }
            } else {
                ShadowSpec { key_dy: 5.0 * k, key_blur: 11.0 * k, key_a: 0.32, near_dy: 1.0 * k, near_blur: 3.0 * k, near_a: 0.22 }
            };
            self.geo = Some(Geometry::new(w as usize, h as usize, gshape, Some(spec)));
            if self.glass == Glass::Blur {
                self.blur_api = present::set_blur(self.hwnd, Some((pad, pad, sw, sh, rad.round() as i32)));
            }
        }
        let Some(geo) = self.geo.take() else { return };
        let field = self.aurora.step(self.now_ms()).0;
        let body = self.body(&geo, field);
        let Some(mut surf) = self.surf.take() else {
            self.geo = Some(geo);
            return;
        };
        // the body only changes while the field eases, the progress moves, or the size changes
        let key = (body, geo.shape, w, h);
        if self.bg_key != Some(key) {
            self.bg.resize((w * h * 4) as usize, 0);
            glass::compose_body(&geo, &body, &mut self.bg);
            self.bg_key = Some(key);
        }
        surf.bytes().copy_from_slice(&self.bg);
        self.geo = Some(geo);
        if self.anim.is_none() {
            if data || !self.fg_valid {
                self.draw_fg(w, h, pad as f32);
                self.fg_valid = true;
            }
            glass::over(surf.bytes(), &self.fg);
        }
        present::present(self.hwnd, &surf, shape.left - pad, shape.top - pad);
        if let Some(dir) = &self.dump {
            // preview aid: the premultiplied BGRA frame exactly as handed to UpdateLayeredWindow
            let mut v = format!("{w} {h} {} {}\n", shape.left - pad, shape.top - pad).into_bytes();
            v.extend_from_slice(surf.bytes());
            let _ = std::fs::write(dir.join(if self.island() { "island.bgra" } else { "panel.bgra" }), v);
        }
        self.surf = Some(surf);
        self.frames += 1;
        self.paint_last = t0.elapsed();
        self.paint_total += self.paint_last;
        // the panel's height follows its content: if the layout asked for another height, reflow once
        if self.anim.is_none() && !self.island() && (self.panel_h * self.scale).round() as i32 != sh {
            self.frame(true);
        }
    }

    /// Foreground = cached static layer + live values (transparent where nothing is drawn).
    unsafe fn draw_fg(&mut self, w: i32, h: i32, pad: f32) {
        let mut layer = std::mem::take(&mut self.layer);
        let mut fg = std::mem::take(&mut self.fg);
        let hash = self.pass(&mut fg, w, h, pad, Pass::Hash);
        if hash != self.layer_hash {
            layer.iter_mut().for_each(|b| *b = 0);
            self.pass(&mut layer, w, h, pad, Pass::Static);
            self.layer_hash = hash;
            self.layer_redraws += 1;
        }
        fg.copy_from_slice(&layer);
        self.pass(&mut fg, w, h, pad, Pass::Dynamic);
        self.layer = layer;
        self.fg = fg;
    }

    /// One layout pass; returns the static-layer fingerprint.
    unsafe fn pass(&mut self, buf: &mut [u8], w: i32, h: i32, pad: f32, pass: Pass) -> u64 {
        let Some(pm) = PixmapMut::from_bytes(buf, w as u32, h as u32) else { return 0 };
        let (lw, lh) = ((w as f32 - 2.0 * pad) / self.scale, (h as f32 - 2.0 * pad) / self.scale);
        let mut dw = self.dw.take();
        let hash = {
            let mut g = G {
                pm,
                bw: w,
                bh: h,
                dw: dw.as_mut(),
                scale: self.scale,
                ox: pad,
                oy: pad,
                pass,
                in_static: false,
                hash: 0xcbf29ce484222325,
            };
            let sc = self.scale;
            g.stat(|g| g.on(0, &[lw, lh, sc], &[], ""));
            self.hits.clear();
            let snap = std::mem::take(&mut self.s.snap);
            if self.island() {
                self.draw_island(&mut g, &snap, lw, lh);
            } else {
                self.draw_panel(&mut g, &snap, lw, lh);
            }
            self.s.snap = snap;
            g.hash
        };
        self.dw = dw;
        hash
    }
}

impl App {
    /// Island capsule. Idle: status dot, GPU load, TEMP, VRAM, PWR. Rendering: progress %, job,
    /// frames/fps, ETA clock (the glass behind fills up as a progress level).
    fn draw_island(&mut self, g: &mut G, s: &Snapshot, lw: f32, lh: f32) {
        let shape = R::new(0.0, 0.0, lw, lh);
        rim(g, shape, lh / 2.0);
        let cy = lh / 2.0;
        let th = self.cfg.gpu.clone();
        let ra = s.render.is_some();
        let gs = s.sample.clone();
        if let (Some(rv), Island::Rendering) = (&s.render, self.island_state) {
            let frac = rv.progress.as_ref().and_then(|p| p.total.map(|t| p.frame as f32 / t.max(1) as f32));
            // live edge of the progress level
            if let Some(f) = frac {
                let ex = (lw * f.clamp(0.0, 1.0)).max(1.0);
                if ex < lw - 2.0 {
                    let mut gl = Paint::default();
                    gl.anti_alias = true;
                    if g.on(13, &[ex], &[], "") {
                        if let Some(sh) = hgrad(ex - 14.0, ex + 2.0, &[(0.0, VIOLET, 0.0), (1.0, VIOLET, 0.35)]) {
                            gl.shader = sh;
                            g.fill(rrect_path(R::new(ex - 14.0, 1.0, 16.0, lh - 2.0), 0.0), &gl);
                        }
                    }
                    g.line(ex, 1.5, ex, lh - 1.5, (255, 255, 255), 0.55, 1.0);
                }
            }
            let x = 24.0;
            let pctw = g.measure("100", Face::SansBold, 26.0).0 + g.measure(".0%", Face::SansSemi, 13.0).0 + 2.0;
            let base = cy + 9.0;
            match frac {
                Some(f) => {
                    let v = format!("{:.1}", (f * 100.0).min(100.0));
                    let (a, b) = v.split_once('.').unwrap_or((&v, "0"));
                    g.num_unit(a, &format!(".{b}%"), x, base, Face::SansBold, 26.0, 13.0, TEXT, H::L);
                }
                None => {
                    g.text("—", x, base, Face::SansBold, 26.0, TEXT, H::L, V::Base);
                }
            }
            let jx = x + pctw + 16.0;
            let job = match &rv.stage {
                Some(st) => format!("{} · {st}", rv.job),
                None => rv.job.clone(),
            };
            let job = g.fit(&job, Face::SansBold, 14.0, 200.0);
            g.text(&job, jx, cy - 3.0, Face::SansBold, 14.0, TEXT, H::L, V::Base);
            let mut sub = match rv.progress.as_ref() {
                Some(p) => match p.total {
                    Some(t) => format!("{} / {t}", p.frame),
                    None => format!("frame {}", p.frame),
                },
                None => "working…".into(),
            };
            if let Some(f) = rv.fps {
                sub += &format!(" · {f:.1} fps");
            }
            let sub = g.fit(&sub, Face::Sans, 11.5, 200.0);
            g.text(&sub, jx, cy + 14.0, Face::Sans, 11.5, TEXT2, H::L, V::Base);
            let rx = lw - 24.0;
            let rem = match (&rv.progress, rv.fps) {
                (Some(p), Some(f)) => p.total.and_then(|t| parse::eta_secs(p.frame, t, f)),
                _ => None,
            };
            let clock = rem.map(|r| (chrono::Local::now() + chrono::Duration::seconds(r as i64)).format("%H:%M").to_string());
            g.text(clock.as_deref().unwrap_or("—"), rx, cy - 3.0, Face::SansBold, 16.0, TEXT, H::Rt, V::Base);
            let mut parts = vec![];
            if let Some(gs) = &gs {
                if let Some(u) = gs.util_pct {
                    parts.push(format!("{u}%"));
                }
                if let Some(t) = gs.temp_c {
                    parts.push(format!("{t}°"));
                }
            }
            if let Some(r) = rem {
                parts.push(format!("{} left", parse::fmt_dur(r)));
            }
            let tc = gs.as_ref().and_then(|gs| gs.temp_c).map(|t| level_text(th.temp_level(t as f32))).unwrap_or(TEXT2);
            let tc = if tc == TEXT { TEXT2 } else { tc };
            g.text(&parts.join(" · "), rx, cy + 14.0, Face::Sans, 11.0, tc, H::Rt, V::Base);
            return;
        }
        // idle
        let mut x = 18.0;
        let Some(gs) = gs else {
            g.dot(x + 4.0, cy, 4.0, T_AMBER, 1.0);
            g.text("NVML unavailable · retrying", x + 22.0, cy, Face::SansSemi, 13.0, T_AMBER, H::L, V::C);
            return;
        };
        let lv = |l: Option<Level>| l.map(level_text).unwrap_or(TEXT);
        let worst = [
            gs.util_pct.map(|u| th.util_level_ctx(u as f32, ra)),
            gs.temp_c.map(|t| th.temp_level(t as f32)),
            match (gs.vram_used_mb, gs.vram_total_mb) {
                (Some(u), Some(t)) => Some(th.vram_level(u as f32, t as f32)),
                _ => None,
            },
        ]
        .into_iter()
        .flatten()
        .fold(Level::Ok, |a, b| if matches!(b, Level::Red) || (matches!(b, Level::Amber) && !matches!(a, Level::Red)) { b } else { a });
        let dotc = match worst {
            Level::Red => RED,
            Level::Amber => AMBER,
            _ => (201, 255, 244),
        };
        g.dot(x + 4.0, cy, 8.0, dotc, 0.16);
        g.dot(x + 4.0, cy, 5.5, dotc, 0.30);
        g.dot(x + 4.0, cy, 4.0, dotc, 1.0);
        x += 8.0 + 14.0;
        let base = cy + 7.0;
        // columns come from the widest values, so the labels sit still (static layer) as values change
        let bigw = g.measure("100", Face::SansBold, 20.0).0 + g.measure("%", Face::SansSemi, 11.0).0 + 1.0;
        let u = gs.util_pct.map(|u| u.to_string()).unwrap_or("—".into());
        g.num_unit(&u, "%", x, base, Face::SansBold, 20.0, 11.0, lv(gs.util_pct.map(|u| th.util_level_ctx(u as f32, ra))), H::L);
        x += bigw + 14.0;
        let vram = match (gs.vram_used_mb, gs.vram_total_mb) {
            (Some(u), Some(t)) => Some((u as f32, t as f32)),
            _ => None,
        };
        let items = [
            ("TEMP", gs.temp_c.map(|t| t.to_string()).unwrap_or("—".into()), "°", "100", lv(gs.temp_c.map(|t| th.temp_level(t as f32)))),
            (
                "VRAM",
                vram.map(|(u, _)| format!("{:.1}", u / 1024.0)).unwrap_or("—".into()),
                " GB",
                "99.9",
                lv(vram.map(|(u, t)| th.vram_level(u, t))),
            ),
            (
                "PWR",
                gs.power_w.map(|w| format!("{w:.0}")).unwrap_or("—".into()),
                " W",
                "999",
                lv(match (gs.power_w, gs.power_limit_w) {
                    (Some(w), Some(l)) => Some(th.power_level(w as f32, l as f32)),
                    _ => None,
                }),
            ),
        ];
        for (lab, val, unit, tmpl, c) in items {
            let lr = g.stat(|g| g.cap(lab, x, base - 1.0, 9.5, H::L, V::Base));
            let vx = lr.r() + 4.0;
            g.num_unit(&val, unit, vx, base - 1.0, Face::SansSemi, 14.0, 11.0, c, H::L);
            x = vx + g.measure(tmpl, Face::SansSemi, 14.0).0 + g.measure(unit, Face::SansSemi, 11.0).0 + 1.0 + 14.0;
        }
    }

    /// Glass circle button with an icon.
    fn circle_btn(&mut self, g: &mut G, cx: f32, cy: f32, b: Btn, on: bool) {
        let r = R::new(cx - 12.0, cy - 12.0, 24.0, 24.0);
        let hov = self.hover == Some(b);
        g.stat(|g| {
            let (c, a) = if on {
                (CYAN, 0.38)
            } else if hov && b == Btn::Close {
                (RED, 0.55)
            } else {
                ((255, 255, 255), if hov { 0.30 } else { 0.18 })
            };
            g.fill_rrect(r, 12.0, c, a);
            g.stroke_rrect(r.grow(-0.5), 11.5, (255, 255, 255), 0.30, 1.0);
            match b {
                Btn::Close => {
                    g.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, TEXT, 1.0, 1.4);
                    g.line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, TEXT, 1.0, 1.4);
                }
                Btn::Island => {
                    g.stroke_rrect(R::new(cx - 6.0, cy - 3.0, 12.0, 6.0), 3.0, TEXT, 1.0, 1.3);
                }
                Btn::Pin => {
                    g.dot(cx, cy - 2.0, 3.2, TEXT, 1.0);
                    g.line(cx, cy + 1.0, cx, cy + 6.0, TEXT, 1.0, 1.4);
                }
                Btn::Settings => {
                    // gear: a ring with six teeth
                    g.stroke_circle(cx, cy, 3.6, TEXT, 1.0, 1.5);
                    for k in 0..6 {
                        let a = k as f32 * std::f32::consts::PI / 3.0 + 0.26;
                        let (s, c) = a.sin_cos();
                        g.line(cx + c * 5.4, cy + s * 5.4, cx + c * 7.2, cy + s * 7.2, TEXT, 1.0, 1.9);
                    }
                }
                _ => {}
            }
        });
        self.hits.push((r, b));
    }

    /// Expanded panel: Aurora field with the hero numeral, then lists and dividers.
    fn draw_panel(&mut self, g: &mut G, s: &Snapshot, lw: f32, lh: f32) {
        let panel = R::new(0.0, 0.0, lw, lh);
        rim(g, panel, PANEL_R);
        let th = self.cfg.gpu.clone();
        let ra = s.render.is_some();
        let x0 = 24.0;
        let xr = lw - 24.0;
        // ---- top row
        let tr = g.stat(|g| g.text("Tachyon", x0, 34.0, Face::SansSemi, 13.0, TEXT, H::L, V::Base));
        let mut bx = xr - 12.0;
        let set_on = self.view == View::Settings;
        for (b, on) in [(Btn::Close, false), (Btn::Island, false), (Btn::Pin, self.cfg.window.always_on_top), (Btn::Settings, set_on)] {
            self.circle_btn(g, bx, 30.0, b, on);
            bx -= 30.0;
        }
        let mut sx = tr.r() + 8.0;
        if s.gpus.len() > 1 {
            let label = format!("GPU {}", s.selected);
            let w = g.measure(&label, Face::SansSemi, 11.0).0 + 26.0;
            let r = R::new(sx, 20.0, w, 20.0);
            let hov = self.hover == Some(Btn::Gpu);
            g.stat(|g| {
                g.fill_rrect(r, 10.0, (255, 255, 255), if hov { 0.26 } else { 0.14 });
                g.text(&label, r.x + 9.0, 34.0, Face::SansSemi, 11.0, T_CYAN, H::L, V::Base);
                let (tx, ty) = (r.r() - 10.0, r.cy());
                g.fill_poly(&[(tx - 3.5, ty - 1.5), (tx + 3.5, ty - 1.5), (tx, ty + 2.5)], TEXT, 1.0);
            });
            self.hits.push((r, Btn::Gpu));
            sx = r.r() + 8.0;
        }
        let gpu = s.gpus.get(s.selected as usize).map(|n| n.trim_start_matches("NVIDIA ").trim_start_matches("GeForce ").to_string());
        let sub =
            format!("{} · driver {}", gpu.unwrap_or_else(|| "no NVIDIA GPU".into()), if s.driver.is_empty() { "—" } else { &s.driver });
        let sub = g.fit(&sub, Face::Sans, 12.0, (bx - 6.0 - sx).max(40.0));
        g.stat(|g| g.text(&sub, sx, 34.0, Face::Sans, 12.0, TEXT2, H::L, V::Base));
        if self.view != View::Main {
            let y = match self.view {
                View::Report => self.draw_report_view(g, s, lw),
                View::Settings => self.draw_settings_view(g, lw, &s.gpus),
                _ => self.draw_history_view(g, s, lw),
            };
            self.panel_fixed_h = y;
            self.panel_h = (y + 18.0).ceil().max(FIELD_H + 80.0);
            return;
        }
        // ---- legend: load and heat with their 60 s traces
        let ly = 60.0;
        let lr = g.stat(|g| g.text("load", x0, ly + 4.0, Face::Sans, 10.5, TEXT2, H::L, V::Base));
        mini_spark(g, R::new(lr.r() + 6.0, ly - 5.0, 44.0, 10.0), s.hist.get(Metric::Util), s.uptime, 0.0, 100.0, CYAN, VIOLET);
        let hr = g.stat(|g| g.text("heat", lr.r() + 64.0, ly + 4.0, Face::Sans, 10.5, TEXT2, H::L, V::Base));
        mini_spark(
            g,
            R::new(hr.r() + 6.0, ly - 5.0, 44.0, 10.0),
            s.hist.get(Metric::Temp),
            s.uptime,
            30.0,
            95.0,
            (255, 176, 103),
            (255, 95, 95),
        );
        // ---- hero
        let gs = s.sample.clone();
        let hero_base = FIELD_H - 44.0;
        match &gs {
            Some(gs) => {
                let u = gs.util_pct.map(|u| u.to_string()).unwrap_or("—".into());
                let c = gs.util_pct.map(|u| level_text(th.util_level_ctx(u as f32, ra))).unwrap_or(TEXT);
                let r = g.text_t(&u, x0 - 4.0, hero_base, Face::SansLight, 96.0, -96.0 * 0.04, c, H::L, V::Base);
                g.text("%", r.r() + 6.0, hero_base, Face::Sans, 28.0, TEXT2, H::L, V::Base);
                let mut l = "GPU LOAD".to_string();
                if let Some(m) = gs.mem_ctrl_pct {
                    l += &format!(" · MEM CTRL {m}%");
                }
                g.cap(&l, x0, FIELD_H - 24.0, 10.0, H::L, V::Base);
                // side: temperature and power
                let tb = FIELD_H - 104.0;
                let tc = gs.temp_c.map(|t| level_text(th.temp_level(t as f32))).unwrap_or(TEXT);
                g.num_unit(&gs.temp_c.map(|t| t.to_string()).unwrap_or("—".into()), " °C", xr, tb, Face::SansSemi, 24.0, 12.0, tc, H::Rt);
                g.stat(|g| g.cap("TEMPERATURE", xr, tb + 15.0, 9.5, H::Rt, V::Base));
                let pb = FIELD_H - 54.0;
                let pc = match (gs.power_w, gs.power_limit_w) {
                    (Some(w), Some(l)) => level_text(th.power_level(w as f32, l as f32)),
                    _ => TEXT,
                };
                let pu = gs.power_limit_w.map(|l| format!(" / {l:.0} W")).unwrap_or(" W".into());
                g.num_unit(
                    &gs.power_w.map(|w| format!("{w:.0}")).unwrap_or("—".into()),
                    &pu,
                    xr,
                    pb,
                    Face::SansSemi,
                    24.0,
                    12.0,
                    pc,
                    H::Rt,
                );
                g.stat(|g| g.cap("POWER", xr, pb + 15.0, 9.5, H::Rt, V::Base));
            }
            None => {
                g.text_t("—", x0, hero_base, Face::SansLight, 96.0, 0.0, T_AMBER, H::L, V::Base);
                g.cap("NO GPU READINGS · TACHYON KEEPS TRYING", x0, FIELD_H - 24.0, 10.0, H::L, V::Base);
                if let Some(e) = s.error.clone() {
                    for (i, line) in views::wrap_g(g, &e, Face::Sans, 11.0, lw - 2.0 * x0, 2).iter().enumerate() {
                        g.text(line, x0, 96.0 + 15.0 * i as f32, Face::Sans, 11.0, TEXT2, H::L, V::Base);
                    }
                }
            }
        }
        // ---- body
        let mut y = FIELD_H + 4.0;
        for (msg, col) in [(self.warn.clone(), T_AMBER), (self.hint.clone(), T_CYAN)] {
            let Some(m) = msg else { continue };
            for line in views::wrap_g(g, &m, Face::Sans, 12.0, lw - 2.0 * x0, 3) {
                g.stat(|g| g.text(&line, x0, y + 12.0, Face::Sans, 12.0, col, H::L, V::Base));
                y += 17.0;
            }
            y += 4.0;
        }
        if let Some(rv) = &s.render {
            y = render_block(g, rv, R::new(16.0, y, lw - 32.0, 0.0)) + 12.0;
        }
        y = self.draw_insights(g, s, x0, xr, y + 4.0);
        // ---- metric list: two columns, dividers
        y += 4.0;
        let colw = (xr - x0 - 26.0) / 2.0;
        let cols = [x0, x0 + colw + 26.0];
        let dash = |v: Option<u32>| v.map(|v| v.to_string()).unwrap_or("—".into());
        let gsr = gs.as_ref();
        let vram = gsr.and_then(|g| g.vram_used_mb.zip(g.vram_total_mb));
        let vc = vram.map(|(u, t)| level_text(th.vram_level(u as f32, t as f32))).unwrap_or(TEXT);
        let words = gsr.and_then(|g| g.throttle).map(throttle_words);
        let is_red = |x: &str| x.starts_with("thermal") || x == "power brake" || x == "hardware slowdown";
        let (lim, limc) = match &words {
            None => ("not reported".to_string(), TEXT2),
            Some(w) if w.is_empty() => ("none".to_string(), T_CYAN),
            Some(w) => {
                let c = if w.iter().any(|x| is_red(x)) {
                    T_RED
                } else if w.contains(&"power cap") {
                    T_AMBER
                } else {
                    TEXT
                };
                (w.join(", "), c)
            }
        };
        let up = s.uptime as u64;
        let rows: [[(&str, String, String, Rgb); 2]; 4] = [
            [
                (
                    "VRAM",
                    vram.map(|(u, _)| format!("{:.1}", u / 1024.0)).unwrap_or("—".into()),
                    vram.map(|(_, t)| format!("/ {:.0} GB", t / 1024.0)).unwrap_or_default(),
                    vc,
                ),
                ("Core", dash(gsr.and_then(|g| g.clk_gfx_mhz)), "MHz".into(), TEXT),
            ],
            [
                ("Memory", dash(gsr.and_then(|g| g.clk_mem_mhz)), "MHz".into(), TEXT),
                ("Fan", dash(gsr.and_then(|g| g.fan_pct)), "%".into(), TEXT),
            ],
            [
                ("Enc / dec", format!("{} / {}", dash(gsr.and_then(|g| g.enc_pct)), dash(gsr.and_then(|g| g.dec_pct))), "%".into(), TEXT),
                ("Limiter", lim, String::new(), limc),
            ],
            [
                (
                    "PCIe rx / tx",
                    match gsr.map(|g| (g.pcie_rx_mbs, g.pcie_tx_mbs)) {
                        Some((Some(a), Some(b))) => format!("{a:.0} / {b:.0}"),
                        _ => "—".into(),
                    },
                    "MB/s".into(),
                    TEXT,
                ),
                ("Session", format!("{:02}:{:02}:{:02}", up / 3600, up / 60 % 60, up % 60), String::new(), TEXT),
            ],
        ];
        for (ri, row) in rows.iter().enumerate() {
            let ry = y + ri as f32 * ROW_H;
            for (ci, (label, val, unit, c)) in row.iter().enumerate() {
                let cx = cols[ci];
                let lr = g.stat(|g| {
                    g.line(cx, ry + ROW_H - 0.5, cx + colw, ry + ROW_H - 0.5, (255, 255, 255), DIVIDER_A, 1.0);
                    g.text(label, cx, ry + 21.0, Face::Sans, 12.0, TEXT2, H::L, V::Base)
                });
                let uw = if unit.is_empty() { 0.0 } else { g.measure(unit, Face::SansSemi, 11.0).0 + 4.0 };
                let v = g.fit(val, Face::SansSemi, 13.5, (colw - (lr.w + 12.0) - uw).max(20.0));
                let unit = if unit.is_empty() { String::new() } else { format!(" {unit}") };
                g.num_unit(&v, &unit, cx + colw, ry + 21.0, Face::SansSemi, 13.5, 11.0, *c, H::Rt);
            }
        }
        y += rows.len() as f32 * ROW_H + 16.0;
        // ---- who is using the GPU
        let show_all = self.cfg.window.show_all_processes;
        let (vis, hidden) = match &s.procs {
            Some(v) => filter_procs(v, show_all, &self.cfg.process_hide),
            None => (vec![], 0),
        };
        g.stat(|g| g.cap("USING THE GPU", x0, y + 10.0, 9.5, H::L, V::Base));
        let tl = if show_all {
            "showing all · hide idle".to_string()
        } else if hidden > 0 {
            format!("{hidden} idle hidden · show all")
        } else {
            "show all".into()
        };
        let tw = g.measure(&tl, Face::Sans, 11.0).0;
        let trr = R::new(xr - tw - 16.0, y - 2.0, tw + 16.0, 18.0);
        let hov = self.hover == Some(Btn::ShowAll);
        g.fill_rrect(trr, 9.0, (255, 255, 255), if hov { 0.18 } else { 0.08 });
        g.text(&tl, trr.cx(), y + 10.5, Face::Sans, 11.0, if show_all { T_CYAN } else { TEXT2 }, H::C, V::Base);
        self.hits.push((trr, Btn::ShowAll));
        y += 22.0;
        self.panel_fixed_h = y + 14.0;
        let max_rows = unsafe {
            let (_, t, _, b) = self.work_area_at(self.shape.left + 10, self.shape.top + 10);
            (((b - t) as f32 / self.scale - 24.0 - self.panel_fixed_h) / CHIP_STEP).floor().clamp(1.0, 10.0) as usize
        };
        match &s.procs {
            None => {
                let msg = if s.sample.is_none() { "no GPU readings yet" } else { "process list not supported by this card/driver" };
                g.stat(|g| g.text(msg, x0, y + 16.0, Face::Sans, 12.0, TEXT2, H::L, V::Base));
                y += 28.0;
            }
            Some(_) if vis.is_empty() => {
                let msg = if hidden > 0 { "only idle desktop processes are using the GPU" } else { "no processes on this GPU" };
                g.stat(|g| g.text(msg, x0, y + 16.0, Face::Sans, 12.0, TEXT2, H::L, V::Base));
                y += 28.0;
            }
            Some(_) => {
                let n = vis.len().min(max_rows);
                for p in vis.iter().take(n) {
                    let r = R::new(x0, y, xr - x0, CHIP_H);
                    g.stat(|g| {
                        g.fill_rrect(r, 14.0, (255, 255, 255), ROW_A);
                        g.shade_stroke(
                            rrect_path(r.grow(-0.25), 13.75),
                            vgrad(r.y, r.b(), &[(0.0, (255, 255, 255), 0.25), (0.5, (255, 255, 255), 0.0)]),
                            0.5,
                            &[r.x, r.y, r.w],
                        );
                    });
                    let ic = R::new(r.x + 12.0, r.y + 8.0, 22.0, 22.0);
                    let pc = p.color;
                    g.shade(
                        rrect_path(ic, 7.0),
                        LinearGradient::new(
                            pt(ic.x, ic.y),
                            pt(ic.r(), ic.b()),
                            vec![GradientStop::new(0.0, col(text_tint(pc), 1.0)), GradientStop::new(1.0, col(pc, 1.0))],
                            SpreadMode::Pad,
                            Transform::identity(),
                        ),
                        &[ic.x, ic.y, pc.0 as f32, pc.1 as f32, pc.2 as f32],
                    );
                    let pct = |v: Option<u32>| v.map(|m| format!("{m}%"));
                    let mut val = p
                        .info
                        .vram_mb
                        .map(|m| if m >= 1024.0 { format!("{:.1} GB", m / 1024.0) } else { format!("{m:.0} MB") })
                        .unwrap_or("—".into());
                    if let Some(sm) = pct(p.info.sm_pct) {
                        val += &format!(" · {sm}");
                    }
                    let vr = g.text(&val, r.r() - 12.0, r.cy() + 4.5, Face::SansSemi, 12.5, TEXT, H::Rt, V::Base);
                    let nx = ic.r() + 10.0;
                    let avail = vr.x - 10.0 - nx;
                    let name = g.fit(&p.info.name, Face::SansBold, 12.5, (avail * 0.6).max(40.0));
                    let nr = g.text(&name, nx, r.cy() + 4.5, Face::SansBold, 12.5, TEXT, H::L, V::Base);
                    let mut meta = vec![];
                    if let Some(l) = p.chain.first() {
                        meta.push(l.trim_end_matches(".exe").to_string());
                    }
                    meta.push(p.category.clone());
                    meta.push(p.info.kind.to_string());
                    match (p.info.enc_pct.filter(|v| *v > 0), p.info.dec_pct.filter(|v| *v > 0)) {
                        (Some(e), Some(d)) => meta.push(format!("enc {e}% dec {d}%")),
                        (Some(e), None) => meta.push(format!("enc {e}%")),
                        (None, Some(d)) => meta.push(format!("dec {d}%")),
                        _ => {}
                    }
                    if let Some(cl) = &p.cmdline {
                        meta.push(cl.clone());
                    }
                    let meta = format!(" · {}", meta.join(" · "));
                    let meta = g.fit(&meta, Face::Sans, 11.0, (vr.x - 10.0 - nr.r()).max(10.0));
                    g.text(&meta, nr.r(), r.cy() + 4.5, Face::Sans, 11.0, TEXT2, H::L, V::Base);
                    y += CHIP_STEP;
                }
                if vis.len() > n {
                    g.text(&format!("+{} more", vis.len() - n), x0 + 4.0, y + 8.0, Face::Sans, 11.0, TEXT2, H::L, V::Base);
                    y += 16.0;
                }
                y -= CHIP_STEP - CHIP_H;
            }
        }
        self.panel_h = (y + 18.0).ceil();
    }
}

/// Render job block (translucent row with job, ETA, progress track and stats). Returns its bottom.
fn render_block(g: &mut G, rv: &RenderView, r0: R) -> f32 {
    let has_job_line = rv.job_elapsed.is_some();
    let r = R::new(r0.x, r0.y, r0.w, if has_job_line { 100.0 } else { 84.0 });
    g.stat(|g| {
        g.fill_rrect(r, 18.0, (255, 255, 255), 0.10);
        g.shade_stroke(
            rrect_path(r.grow(-0.5), 17.5),
            vgrad(r.y, r.b(), &[(0.0, (255, 255, 255), 0.30), (0.4, (255, 255, 255), 0.0)]),
            1.0,
            &[r.x, r.y, r.h],
        );
    });
    let (x0, xr) = (r.x + 16.0, r.r() - 16.0);
    let frac = rv.progress.as_ref().and_then(|p| p.total.map(|t| p.frame as f32 / t.max(1) as f32));
    let rem = match (&rv.progress, rv.fps) {
        (Some(p), Some(f)) => p.total.and_then(|t| parse::eta_secs(p.frame, t, f)),
        _ => None,
    };
    let eta = rem.map(|s| format!("ETA {}", (chrono::Local::now() + chrono::Duration::seconds(s as i64)).format("%H:%M")));
    let er = g.text(eta.as_deref().unwrap_or(""), xr, r.y + 28.0, Face::SansBold, 15.0, TEXT, H::Rt, V::Base);
    let jw = (er.x - 12.0 - x0).max(40.0);
    let job = g.fit(&rv.job, Face::SansBold, 14.0, jw * 0.6);
    let jr = g.text(&job, x0, r.y + 28.0, Face::SansBold, 14.0, TEXT, H::L, V::Base);
    let mut meta = vec![];
    if let Some(st) = &rv.stage {
        meta.push(st.clone());
    }
    if !rv.source_name.is_empty() {
        meta.push(rv.source_name.clone());
    }
    if !meta.is_empty() {
        let m = g.fit(&meta.join(" · "), Face::Sans, 12.0, (er.x - 12.0 - jr.r() - 8.0).max(10.0));
        g.text(&m, jr.r() + 8.0, r.y + 28.0, Face::Sans, 12.0, TEXT2, H::L, V::Base);
    }
    // progress track
    let tr = R::new(x0, r.y + 40.0, xr - x0, 6.0);
    g.stat(|g| g.fill_rrect(tr, 3.0, (255, 255, 255), 0.18));
    if let Some(f) = frac {
        let fr = R::new(tr.x, tr.y, (tr.w * f.clamp(0.0, 1.0)).max(tr.h), tr.h);
        g.fill_rrect(fr.grow(2.5), 5.5, (143, 217, 255), 0.18);
        g.shade(rrect_path(fr, 3.0), hgrad(fr.x, fr.r(), &[(0.0, CYAN, 1.0), (1.0, VIOLET, 1.0)]), &[fr.w]);
    }
    let sb = r.y + 64.0;
    let left = match &rv.progress {
        Some(p) => match p.total {
            Some(t) => format!("{} / {t} frames", p.frame),
            None => format!("frame {}", p.frame),
        },
        None => "working…".into(),
    };
    g.text(&left, x0, sb, Face::Sans, 11.5, TEXT2, H::L, V::Base);
    if let Some(f) = frac {
        g.text(&format!("{:.1}%", f * 100.0), r.cx(), sb, Face::Sans, 11.5, TEXT2, H::C, V::Base);
    }
    let mut right = vec![];
    if let Some(f) = rv.fps {
        right.push(format!("{f:.1} fps"));
    }
    if let Some(s) = rem {
        right.push(format!("{} left", parse::fmt_dur(s)));
    }
    g.text(&right.join(" · "), xr, sb, Face::Sans, 11.5, TEXT2, H::Rt, V::Base);
    if let Some(el) = rv.job_elapsed {
        let mut j = format!("job {}", parse::fmt_dur(el));
        if let Some(est) = rv.job_est_total {
            let at = chrono::Local::now() + chrono::Duration::seconds((est - el).max(0.0) as i64);
            j += &format!(" of ~{} · done ~{}", parse::fmt_dur(est), at.format("%H:%M"));
        }
        g.text(&j, x0, r.y + 84.0, Face::Sans, 11.5, TEXT2, H::L, V::Base);
    }
    r.b()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(a) = app() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    if a.hwnd.is_null() {
        a.hwnd = hwnd;
    }
    match msg {
        WM_TACHYON_SHOW => {
            a.hint = Some("Tachyon was already running: here it is. Right-click for Settings.".into());
            a.fg_valid = false;
            if a.mode == UiMode::Companion {
                if a.main_hidden {
                    a.open_panel_from_companion();
                }
            } else if a.cfg.window.compact {
                a.toggle_island();
            }
            a.frame(true);
            SetForegroundWindow(hwnd);
            return 0;
        }
        WM_NCHITTEST => {
            if a.anim.is_some() {
                return HTCLIENT as LRESULT;
            }
            let (sx, sy) = lparam_xy(lp);
            let mut p = POINT { x: sx, y: sy };
            ScreenToClient(hwnd, &mut p);
            // the shadow padding never takes clicks
            if !a.inside_shape(p.x, p.y) {
                return HTTRANSPARENT as LRESULT;
            }
            if a.island() {
                return HTCLIENT as LRESULT;
            }
            let (x, y) = a.to_logical(p.x, p.y);
            if y < TOP_BAND && a.btn_at(x, y).is_none() {
                return HTCAPTION as LRESULT;
            }
            return HTCLIENT as LRESULT;
        }
        // double-click on the panel's top band goes back to the island
        WM_NCLBUTTONDBLCLK if wp as u32 == HTCAPTION => {
            a.toggle_island();
            return 0;
        }
        WM_CONTEXTMENU => {
            let (mut x, mut y) = lparam_xy(lp);
            if x == -1 && y == -1 {
                x = a.shape.left + 20;
                y = a.shape.top + 20;
            }
            a.menu(x, y, true);
            return 0;
        }
        WM_MOUSEMOVE => {
            if !a.tracking {
                let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TrackMouseEvent(&mut t);
                a.tracking = true;
            }
            if let Some(d) = a.down_at {
                let mut p: POINT = std::mem::zeroed();
                GetCursorPos(&mut p);
                if (p.x - d.x).abs() > 4 || (p.y - d.y).abs() > 4 {
                    // island drag: hand over to the system move loop (WM_EXITSIZEMOVE saves the position)
                    a.down_at = None;
                    ReleaseCapture();
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
                }
                return 0;
            }
            let (cx, cy) = lparam_xy(lp);
            let (x, y) = a.to_logical(cx, cy);
            let hb = a.btn_at(x, y);
            if hb != a.hover {
                a.hover = hb;
                a.frame(false);
                a.fg_valid = false;
                a.frame(false);
            }
            return 0;
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            a.tracking = false;
            if a.hover.is_some() {
                a.hover = None;
                a.fg_valid = false;
                a.frame(false);
            }
            return 0;
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            if a.anim.is_some() {
                return 0;
            }
            if a.island() {
                let mut p: POINT = std::mem::zeroed();
                GetCursorPos(&mut p);
                a.down_at = Some(p);
                SetCapture(hwnd);
                return 0;
            }
            let (cx, cy) = lparam_xy(lp);
            let (x, y) = a.to_logical(cx, cy);
            a.pressed = a.btn_at(x, y);
            if a.pressed.is_some() {
                SetCapture(hwnd);
            }
            return 0;
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            if a.island() {
                if a.down_at.take().is_some() {
                    a.toggle_island(); // a click (not a drag) expands
                }
                return 0;
            }
            let (cx, cy) = lparam_xy(lp);
            let (x, y) = a.to_logical(cx, cy);
            if let (Some(p), Some(b)) = (a.pressed.take(), a.btn_at(x, y)) {
                if p == b {
                    a.click(b);
                }
            }
            return 0;
        }
        WM_KEYDOWN if a.view == View::Settings && !a.island() => {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
            let shift = GetKeyState(VK_SHIFT as i32) < 0;
            let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
            if a.settings_key(wp as u16, shift, ctrl) {
                return 0;
            }
        }
        WM_KEYDOWN if !a.island() && wp as u16 == 0xBC && windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x11) < 0 => {
            // Ctrl+, opens Settings (as in most apps)
            a.open_settings(None);
            return 0;
        }
        WM_MOUSEWHEEL if a.view == View::Settings && !a.island() => {
            let delta = ((wp >> 16) & 0xffff) as u16 as i16;
            a.settings_wheel(delta > 0);
            return 0;
        }
        WM_EXITSIZEMOVE => {
            let _ = a.target_shape();
            a.remember_geometry();
            config::save_window(&a.path, &a.cfg.window);
        }
        WM_TIMER => {
            match wp {
                2 => a.anim_step(),
                3 => a.on_ease_timer(),
                _ => a.on_timer(),
            }
            return 0;
        }
        WM_PAINT => {
            // layered windows are presented with UpdateLayeredWindow; nothing to paint here
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_DPICHANGED => {
            a.scale = ((wp & 0xffff) as f32 / 96.0).max(0.5);
            a.geo = None;
            a.fg_valid = false;
            a.frame(true);
            return 0;
        }
        WM_SETTINGCHANGE => {
            // "Transparency effects" toggled in Settings
            let t = present::transparency_effects();
            if t != a.transparency {
                a.transparency = t;
                a.frame(false);
            }
        }
        WM_CLOSE => {
            if !a.main_hidden {
                let _ = a.target_shape();
                a.remember_geometry();
            }
            config::save_window(&a.path, &a.cfg.window);
            a.s.close();
            if let Some(mut c) = a.comp.take() {
                c.destroy();
            }
            DestroyWindow(hwnd);
            return 0;
        }
        WM_ENDSESSION if wp != 0 => {
            a.remember_geometry();
            config::save_window(&a.path, &a.cfg.window);
            a.s.close();
        }
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            KillTimer(hwnd, 2);
            KillTimer(hwnd, 3);
            PostQuitMessage(0);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
