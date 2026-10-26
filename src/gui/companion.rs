//! Companion mode: Hex, a small glass hexagon that sits on the desktop, plus its Aurora speech
//! bubble. Both are separate `WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE`
//! windows presented with the same layered presenter as the island/panel (present.rs).
//!
//! Lightness: no constant frame timer. After each sprite frame one one-shot timer is armed for
//! that frame's hold (idle breath 1.5 s, render 0.5 s, hot 0.33 s, sleepy 2 s); 12 fps happens
//! only for blinks and state transitions. Paused (no timer at all) while the sprite is hidden or
//! a fullscreen app is in front; resumed at frame 0 by the 1 Hz poll. The bubble redraws on the
//! poll only while it is open.
use super::*;
use tachyon::config::UiMode;
use tachyon::hex::{self, Animator, Atlas, EdgeGeo, Inputs, Pose, Script, State};

pub(super) const HEX_CLASS: &str = "TachyonHex";
pub(super) const BUBBLE_CLASS: &str = "TachyonBubble";
const T_FRAME: usize = 10;
const T_BUBBLE_ANIM: usize = 11;
const T_BUBBLE_IDLE: usize = 12;
const BUBBLE_W: f32 = 380.0;
const BUBBLE_R: f32 = 26.0;
const BUBBLE_PAD: f32 = 44.0;
/// gap between the bubble and Hex's halo (the two tail dots sit in it)
const BUBBLE_GAP: f32 = 30.0;
const BUBBLE_ANIM_MS: f32 = 160.0;
const HIGHLIGHT: Rgb = (169, 246, 255);
const WARN: Rgb = (255, 210, 154);
const WARN_HOT: Rgb = (255, 154, 138);
const QUIET: Rgb = (205, 212, 228);
const FOOT: Rgb = (176, 184, 204);

pub(super) unsafe fn register(hinst: HINSTANCE, icon: HICON) {
    for (name, proc_) in
        [(HEX_CLASS, hex_wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT), (BUBBLE_CLASS, bubble_wndproc)]
    {
        let cls = wz(name);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(proc_),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: icon,
            hCursor: LoadCursorW(null_mut(), IDC_HAND),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: cls.as_ptr(),
            hIconSm: icon,
        };
        RegisterClassExW(&wc);
    }
}

/// A word of a spoken line: text, face, colour.
type Span = (String, Face, Rgb);

pub(super) struct Companion {
    pub hwnd: HWND,
    pub bubble: HWND,
    sc: Script,
    at: Atlas,
    key: &'static str,
    anim: Animator,
    want: State,
    pose: Pose,
    edge_geo: Option<EdgeGeo>,
    edge: Option<(u32, Vec<f32>)>,
    surf: Option<Surface>,
    scratch: Vec<[f32; 4]>,
    /// sprite window top-left (screen px)
    pub pos: POINT,
    armed: bool,
    paused: bool,
    down_at: Option<POINT>,
    inp: Inputs,
    pub frames: u64,
    // bubble
    pub b_open: bool,
    b_anim: Option<(Instant, bool)>,
    b_surf: Option<Surface>,
    b_geo: Option<Geometry>,
    b_fg: Vec<u8>,
    b_hits: Vec<(R, Link)>,
    /// lines that replace the usual ones until the bubble closes: the crash announcement or
    /// the answer to "Why is it slow?"
    b_special: Option<(Special, Vec<tachyon::voice::Line>)>,
    /// bubble shape rect in screen px, and its padding
    b_shape: RECT,
    b_pad: i32,
    b_origin: POINT,
    b_last_hover: Instant,
    b_left: bool,
    /// the bubble closes after this many seconds without the pointer (0 = stays open)
    pub b_close_secs: u64,
}

/// Links in the bubble's bottom row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Link {
    Panel,
    Report,
    Why,
    Back,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Special {
    Announce,
    Why,
}

/// True when a fullscreen app (game, video, presentation) is in front on the sprite's monitor.
pub(super) unsafe fn fullscreen_in_front(near: HWND) -> bool {
    let fg = GetForegroundWindow();
    if fg.is_null() || fg == near || fg == GetDesktopWindow() || fg == GetShellWindow() || IsWindowVisible(fg) == 0 || IsIconic(fg) != 0 {
        return false;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(fg, cls.as_mut_ptr(), cls.len() as i32).max(0) as usize;
    let name = String::from_utf16_lossy(&cls[..n]);
    if matches!(name.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "#32769" | "TachyonV5" | "TachyonHex" | "TachyonBubble") {
        return false;
    }
    let mut r: RECT = std::mem::zeroed();
    GetWindowRect(fg, &mut r);
    let mon = MonitorFromWindow(near, MONITOR_DEFAULTTONEAREST);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(mon, &mut mi);
    let m = mi.rcMonitor;
    r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
}

fn present_alpha(hwnd: HWND, s: &Surface, x: i32, y: i32, alpha: u8) {
    unsafe {
        let pos = POINT { x, y };
        let size = SIZE { cx: s.w, cy: s.h };
        let src = POINT { x: 0, y: 0 };
        let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        UpdateLayeredWindow(hwnd, null_mut(), &pos, &size, s.dc, &src, 0, &bf, ULW_ALPHA);
    }
}

fn inputs(snap: &Snapshot, cfg: &Config) -> (Inputs, bool) {
    let (load, temp) = load_temp(snap);
    let progress = snap
        .render
        .as_ref()
        .and_then(|rv| rv.progress.as_ref())
        .and_then(|p| p.total.map(|t| p.frame as f32 / t.max(1) as f32))
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    (Inputs { load: load.unwrap_or(0.0), temp: temp.unwrap_or(40.0), temp_red: cfg.gpu.temp_red_c, progress }, snap.render.is_some())
}

impl Companion {
    /// Create the sprite window (hidden) for the DPI of where it will sit.
    pub unsafe fn new(hinst: HINSTANCE, cfg: &Config, snap: &Snapshot) -> Option<Companion> {
        let sc = Script::parse(hex::ATLAS_JSON)?;
        let hwnd = present::create(HEX_CLASS, "Tachyon Hex", WS_EX_TOPMOST | WS_EX_NOACTIVATE, hinst);
        if hwnd.is_null() {
            return None;
        }
        let bubble = present::create(BUBBLE_CLASS, "Tachyon Hex bubble", WS_EX_TOPMOST | WS_EX_NOACTIVATE, hinst);
        // position first (DPI depends on the monitor), then the atlas for that DPI only
        let pt = match (cfg.window.hex_x, cfg.window.hex_y) {
            (Some(x), Some(y)) => {
                let rc = RECT { left: x as i32, top: y as i32, right: x as i32 + 40, bottom: y as i32 + 40 };
                (!MonitorFromRect(&rc, MONITOR_DEFAULTTONULL).is_null()).then_some(POINT { x: x as i32, y: y as i32 })
            }
            _ => None,
        };
        let probe = pt.unwrap_or(POINT { x: 0, y: 0 });
        let mon = MonitorFromPoint(probe, MONITOR_DEFAULTTOPRIMARY);
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        let _ =
            windows_sys::Win32::UI::HiDpi::GetDpiForMonitor(mon, windows_sys::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let key = hex::scale_key(dpi_x as f32 / 96.0);
        let at = Atlas::load(key)?;
        let n = at.size() as i32;
        let pos = pt.unwrap_or_else(|| {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(mon, &mut mi);
            let w = mi.rcWork;
            POINT { x: w.right - n - 24, y: w.bottom - n - 24 }
        });
        let (inp, render) = inputs(snap, cfg);
        let mut anim = Animator::new(&sc, State::Idle);
        let want = anim.desired(render, Some(inp.load), Some(inp.temp), cfg.gpu.temp_amber_c, 0);
        // start directly in the right state (no transition on launch)
        anim = Animator::new(&sc, want);
        let pose = anim.next(&sc).0;
        Some(Companion {
            hwnd,
            bubble,
            sc,
            at,
            key,
            anim,
            want,
            pose,
            edge_geo: None,
            edge: None,
            surf: None,
            scratch: vec![],
            pos,
            armed: false,
            paused: false,
            down_at: None,
            inp,
            frames: 0,
            b_open: false,
            b_anim: None,
            b_surf: None,
            b_geo: None,
            b_fg: vec![],
            b_hits: vec![],
            b_special: None,
            b_shape: std::mem::zeroed(),
            b_pad: 0,
            b_origin: POINT { x: 0, y: 0 },
            b_last_hover: Instant::now(),
            b_close_secs: cfg.bubble_close_secs,
            b_left: true,
        })
    }

    pub fn atlas_key(&self) -> &'static str {
        self.key
    }
    /// debug: state, poll redraws
    pub fn debug(&self) -> String {
        self.anim.state.key().to_string()
    }

    pub unsafe fn show(&mut self) {
        self.draw(None);
        ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        present::set_topmost(self.hwnd, true);
        self.arm(self.pose.step.hold);
    }

    pub unsafe fn destroy(&mut self) {
        KillTimer(self.hwnd, T_FRAME);
        KillTimer(self.hwnd, T_BUBBLE_ANIM);
        KillTimer(self.hwnd, T_BUBBLE_IDLE);
        DestroyWindow(self.bubble);
        DestroyWindow(self.hwnd);
    }

    unsafe fn arm(&mut self, hold: u32) {
        let ms = (hold.max(1) as f64 * hex::TICK_MS).round() as u32;
        SetTimer(self.hwnd, T_FRAME, ms, None);
        self.armed = true;
    }

    unsafe fn hidden(&self) -> bool {
        IsWindowVisible(self.hwnd) == 0 || fullscreen_in_front(self.hwnd)
    }

    /// Compose the current pose and present it. `dump`: TACHYON_DUMP_DIR.
    unsafe fn draw(&mut self, dump: Option<&std::path::Path>) {
        let n = self.at.size();
        if self.surf.as_ref().is_none_or(|s| s.w != n as i32) {
            self.surf = Surface::new(n as i32, n as i32);
        }
        let edge = if self.pose.edge() {
            let q = (self.inp.progress * 1000.0).round() as u32;
            if self.edge.as_ref().is_none_or(|(k, _)| *k != q) {
                let g = self.edge_geo.get_or_insert_with(|| EdgeGeo::new(&self.sc, &self.at));
                self.edge = Some((q, g.alpha(self.inp.progress)));
            }
            self.edge.as_ref().map(|(_, e)| e.as_slice())
        } else {
            None
        };
        let Some(surf) = self.surf.as_mut() else { return };
        hex::compose(&self.sc, &self.at, &self.pose, &self.inp, edge, &mut self.scratch, surf.bytes());
        present::present(self.hwnd, surf, self.pos.x, self.pos.y);
        self.frames += 1;
        if let Some(dir) = dump {
            let mut v = format!("{n} {n} {} {}\n", self.pos.x, self.pos.y).into_bytes();
            v.extend_from_slice(surf.bytes());
            let _ = std::fs::write(dir.join("hex.bgra"), v);
        }
    }

    /// One-shot frame timer: advance the scheduler, draw, arm the next hold.
    pub unsafe fn on_frame(&mut self, dump: Option<&std::path::Path>) {
        KillTimer(self.hwnd, T_FRAME);
        self.armed = false;
        if self.hidden() {
            self.paused = true;
            return;
        }
        if self.want != self.anim.state && !self.anim.in_transition() {
            self.anim.go(&self.sc, self.want);
        }
        let (pose, hold) = self.anim.next(&self.sc);
        self.pose = pose;
        self.draw(dump);
        self.arm(hold);
    }

    /// 1 Hz poll: new values. Starts transitions right away and resumes after a pause. No
    /// other redraw: the next scheduled frame (at most 2 s away, 0.5 s while rendering) picks up
    /// the new colours and the edge progress.
    pub unsafe fn on_poll(&mut self, snap: &Snapshot, cfg: &Config, now_ms: u64, dump: Option<&std::path::Path>) {
        let (inp, render) = inputs(snap, cfg);
        self.inp = inp;
        let (load, temp) = load_temp(snap);
        self.want = self.anim.desired(render, load, temp, cfg.gpu.temp_amber_c, now_ms);
        if self.paused || !self.armed {
            if self.hidden() {
                return;
            }
            self.paused = false;
            self.anim.reset(&self.sc);
            self.on_frame(dump);
            return;
        }
        if self.want != self.anim.state && !self.anim.in_transition() {
            // start the transition now instead of waiting for the current hold
            self.on_frame(dump);
        }
    }

    /// Alpha of the sprite at a client point (hit test: the hexagon takes clicks, the halo
    /// and the transparent corners do not).
    fn alpha_at(&self, x: i32, y: i32) -> u8 {
        let Some(s) = self.surf.as_ref() else { return 0 };
        if x < 0 || y < 0 || x >= s.w || y >= s.h {
            return 0;
        }
        unsafe { *s.bits_ptr().add(((y * s.w + x) * 4 + 3) as usize) }
    }

    pub unsafe fn moved_to(&mut self, x: i32, y: i32) {
        self.pos = POINT { x, y };
        if self.b_open {
            self.place_bubble();
            self.present_bubble(255);
        }
    }

    /// Back onto the nearest screen when the sprite is no longer on any (monitor unplugged).
    pub unsafe fn pull_back(&mut self) {
        let n = self.at.size() as i32;
        let rc = RECT { left: self.pos.x + n / 4, top: self.pos.y + n / 4, right: self.pos.x + n * 3 / 4, bottom: self.pos.y + n * 3 / 4 };
        let visible = !MonitorFromRect(&rc, MONITOR_DEFAULTTONULL).is_null();
        let mon = MonitorFromPoint(self.pos, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let w = mi.rcWork;
        if let Some((x, y)) = tachyon::monitor::pull_back((self.pos.x, self.pos.y), (n, n), visible, (w.left, w.top, w.right, w.bottom)) {
            self.moved_to(x, y);
            self.draw(None);
        }
    }

    pub unsafe fn on_dpi(&mut self, dpi: u32) {
        let key = hex::scale_key(dpi as f32 / 96.0);
        if key != self.key {
            if let Some(at) = Atlas::load(key) {
                self.at = at;
                self.key = key;
                self.edge_geo = None;
                self.edge = None;
                self.draw(None);
            }
        }
    }

    // ---- bubble ----------------------------------------------------------------------------

    fn halo_box(&self) -> RECT {
        let p = (self.at.pad() / 2) as i32;
        let f = self.at.frame("halo_1").copied().unwrap_or(hex::FrameRect { x: 0, y: 0, w: self.at.cell, h: self.at.cell, ox: 0, oy: 0 });
        let l = self.pos.x + p + f.ox;
        let t = self.pos.y + p + f.oy;
        RECT { left: l, top: t, right: l + f.w as i32, bottom: t + f.h as i32 }
    }

    /// Bubble beside Hex on the side with more room (left by default), top 6 px above the halo,
    /// clamped to the work area with a 12 px margin.
    unsafe fn place_bubble(&mut self) {
        let k = self.at.scale;
        let (bw, bh) = (rw(&self.b_shape), rh(&self.b_shape));
        let hb = self.halo_box();
        let mon = MonitorFromPoint(POINT { x: (hb.left + hb.right) / 2, y: (hb.top + hb.bottom) / 2 }, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let (m, gap) = ((12.0 * k) as i32, (BUBBLE_GAP * k) as i32);
        let left_x = hb.left - gap - bw;
        self.b_left = left_x >= wa.left + m;
        let x = if self.b_left { left_x } else { hb.right + gap };
        let x = x.clamp(wa.left + m, (wa.right - m - bw).max(wa.left + m));
        let y = (hb.top - (6.0 * k) as i32).clamp(wa.top + m, (wa.bottom - m - bh).max(wa.top + m));
        self.b_shape = RECT { left: x, top: y, right: x + bw, bottom: y + bh };
    }

    unsafe fn present_bubble(&mut self, alpha: u8) {
        let (pad, left, shape) = (self.b_pad, self.b_left, self.b_shape);
        // the open/close ease slides 6 px toward Hex
        let slide = match self.b_anim {
            Some((t0, opening)) => {
                let t = (t0.elapsed().as_secs_f32() * 1000.0 / BUBBLE_ANIM_MS).min(1.0);
                let e = ease(t);
                let v = if opening { 1.0 - e } else { e };
                (v * 6.0 * self.at.scale) as i32
            }
            None => 0,
        };
        let dx = if left { slide } else { -slide };
        self.b_origin = POINT { x: shape.left - pad + dx, y: shape.top - pad };
        if let Some(s) = self.b_surf.as_ref() {
            present_alpha(self.bubble, s, self.b_origin.x, self.b_origin.y, alpha);
        }
    }

    pub unsafe fn toggle_bubble(&mut self, ctx: &mut BubbleCtx) {
        if self.b_open { self.close_bubble() } else { self.open_bubble(ctx) }
    }

    pub unsafe fn open_bubble(&mut self, ctx: &mut BubbleCtx) {
        self.b_open = true;
        self.b_last_hover = Instant::now();
        self.render_bubble(ctx);
        self.b_anim = Some((Instant::now(), true));
        self.present_bubble(0);
        ShowWindow(self.bubble, SW_SHOWNOACTIVATE);
        present::set_topmost(self.bubble, true);
        SetTimer(self.hwnd, T_BUBBLE_ANIM, 15, None);
        SetTimer(self.hwnd, T_BUBBLE_IDLE, 1000, None);
    }

    pub unsafe fn close_bubble(&mut self) {
        if !self.b_open {
            return;
        }
        self.b_open = false;
        self.b_special = None;
        KillTimer(self.hwnd, T_BUBBLE_IDLE);
        self.b_anim = Some((Instant::now(), false));
        SetTimer(self.hwnd, T_BUBBLE_ANIM, 15, None);
    }

    unsafe fn bubble_anim_step(&mut self) {
        let Some((t0, opening)) = self.b_anim else {
            KillTimer(self.hwnd, T_BUBBLE_ANIM);
            return;
        };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / BUBBLE_ANIM_MS).min(1.0);
        let e = ease(t);
        let a = if opening { e } else { 1.0 - e };
        self.present_bubble((a * 255.0).round() as u8);
        if t >= 1.0 {
            self.b_anim = None;
            KillTimer(self.hwnd, T_BUBBLE_ANIM);
            if !opening {
                ShowWindow(self.bubble, SW_HIDE);
                // the bubble's buffers are not needed until it opens again
                self.b_surf = None;
                self.b_geo = None;
                self.b_fg = vec![];
            } else {
                self.present_bubble(255);
            }
        }
    }

    unsafe fn bubble_idle_check(&mut self) {
        let mut p: POINT = std::mem::zeroed();
        GetCursorPos(&mut p);
        let r = self.b_shape;
        let over = p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom;
        if over {
            self.b_last_hover = Instant::now();
        } else if self.b_close_secs > 0 && self.b_last_hover.elapsed().as_secs() >= self.b_close_secs {
            self.close_bubble();
        }
    }

    /// Lay out and draw the bubble for the current values (1 Hz while open).
    pub unsafe fn render_bubble(&mut self, ctx: &mut BubbleCtx) {
        let k = self.at.scale.max(ctx.scale);
        let (fa, fb) = hex::field(self.anim.state, &self.inp);
        let lines = match &self.b_special {
            Some((_, l)) => spans_of(l.clone()),
            None => bubble_lines(ctx.snap, ctx.cfg),
        };
        let chips = bubble_chips(ctx.snap, ctx.cfg);
        // bottom row: (left text or link, right link)
        let (left_link, right_link) = match self.b_special.as_ref().map(|s| s.0) {
            Some(Special::Announce) => (None, (Link::Report, "Open report ›")),
            Some(Special::Why) => (Some((Link::Back, "‹ Back")), (Link::Panel, "Open panel ›")),
            None => (asks_why(ctx.snap).then_some((Link::Why, "Why is it slow? ›")), (Link::Panel, "Open panel ›")),
        };
        let footer = bubble_footer(ctx.snap).or_else(|| self.b_special.as_ref().map(|_| String::new()));
        // width: 380 (300..420), grows only for longer lines
        let mut need = 0.0f32;
        for (spans, _) in &lines {
            let w: f32 = spans.iter().map(|(s, f, _)| mt(ctx.dw.as_deref_mut(), s, *f, 15.0, 0.0, k)).sum();
            need = need.max(w.min(420.0 - 36.0) + 36.0);
        }
        let bw = need.max(BUBBLE_W).clamp(300.0, 420.0);
        let inner = bw - 36.0;
        // wrap each line into at most 2 rows (3 for the announcement and the "why" answer)
        let max_rows = if self.b_special.is_some() { 3 } else { 2 };
        let mut rows: Vec<(Vec<(String, Face, Rgb, f32)>, bool, bool)> = vec![]; // (words with x, first row of line, render line)
        for (spans, is_render) in &lines {
            let wrapped = wrap(ctx.dw.as_deref_mut(), spans, inner, k, max_rows);
            let nrows = wrapped.len();
            for (i, r) in wrapped.into_iter().enumerate() {
                rows.push((r, i == 0, *is_render && i + 1 == nrows));
            }
        }
        let mut h = 16.0;
        for (i, (_, first, render_end)) in rows.iter().enumerate() {
            if i > 0 {
                h += if *first { 7.0 } else { 0.0 };
            }
            h += 21.0;
            if *render_end {
                h += 9.0 + 5.0 + 3.0;
            }
        }
        let chip_h = 8.0 + 11.0 + 4.0 + 20.0 + 7.0;
        h += if rows.is_empty() { 0.0 } else { 12.0 } + chip_h;
        if footer.is_some() {
            h += 12.0 + 16.0;
        }
        h += 14.0;
        let (sw, sh) = ((bw * k).round() as i32, (h * k).round() as i32);
        let pad = (BUBBLE_PAD * k).round() as i32;
        self.b_pad = pad;
        self.b_shape = RECT { left: 0, top: 0, right: sw, bottom: sh };
        self.place_bubble();
        let (w, hh) = (sw + 2 * pad, sh + 2 * pad);
        if self.b_surf.as_ref().is_none_or(|s| s.w != w || s.h != hh) {
            self.b_surf = Surface::new(w, hh);
            self.b_geo = None;
        }
        let shape = Shape { x: pad as f32, y: pad as f32, w: sw as f32, h: sh as f32, r: BUBBLE_R * k };
        if self.b_geo.as_ref().is_none_or(|g| g.shape != shape) {
            let spec = ShadowSpec { key_dy: 18.0 * k, key_blur: 34.0 * k, key_a: 0.40, near_dy: 2.0 * k, near_blur: 6.0 * k, near_a: 0.18 };
            self.b_geo = Some(Geometry::new(w as usize, hh as usize, shape, Some(spec)));
        }
        let hb = self.halo_box();
        let hex_c = ((hb.left + hb.right) as f32 / 2.0, (hb.top + hb.bottom) as f32 / 2.0);
        let (b_shape, b_left, progress) = (self.b_shape, self.b_left, self.inp.progress);
        let (Some(geo), Some(surf)) = (self.b_geo.as_ref(), self.b_surf.as_mut()) else { return };
        let hot = self.anim.state == State::Hot;
        let c255 = |c: [f32; 3]| [c[0] * 255.0, c[1] * 255.0, c[2] * 255.0];
        let field = Field {
            b: [
                tachyon::aurora::Blob { c: c255(fa), k: 1.0 },
                tachyon::aurora::Blob { c: c255(fb), k: 1.0 },
                tachyon::aurora::Blob { c: if hot { c255(fa) } else { [255.0, 150.0, 80.0] }, k: 0.5 },
            ],
        };
        let layout = FieldLayout {
            x: shape.x,
            y: shape.y,
            w: shape.w,
            h: shape.h,
            blobs: [(0.15, 0.30, 0.42, 0.85), (0.60, 0.20, 0.42, 0.85), (0.90, 0.70, 0.32, 0.75)],
            peak: 0.40,
            gain: [1.0, 1.0, 1.0],
        };
        let tint_a = if ctx.transparency == Some(false) { TINT_OPAQUE_A } else { 0.84 };
        let body = Body { tint: (0x18, 0x1a, 0x28), tint_a, field: Some((field, layout)), level: None };
        glass::compose_body(geo, &body, surf.bytes());
        self.b_fg.clear();
        self.b_fg.resize((w * hh * 4) as usize, 0);
        self.b_hits.clear();
        let mut hits = vec![];
        for pass in [Pass::Static, Pass::Dynamic] {
            let Some(pm) = PixmapMut::from_bytes(&mut self.b_fg, w as u32, hh as u32) else { return };
            let mut g = G {
                pm,
                bw: w,
                bh: hh,
                dw: ctx.dw.as_deref_mut(),
                scale: k,
                ox: pad as f32,
                oy: pad as f32,
                pass,
                in_static: false,
                hash: 0,
            };
            let r = R::new(0.0, 0.0, bw, h);
            // fill gradient (white 16% -> 7%), specular top line and hairline
            g.stat(|g| {
                g.shade(rrect_path(r, BUBBLE_R), vgrad(0.0, h, &[(0.0, (255, 255, 255), 0.16), (1.0, (255, 255, 255), 0.07)]), &[bw, h]);
            });
            rim(&mut g, r, BUBBLE_R);
            // tail: two glass dots on the line toward Hex's centre
            let (ex, ey) = if b_left { (bw, h * 0.32) } else { (0.0, h * 0.32) };
            let (hx, hy) = ((hex_c.0 - b_shape.left as f32) / k, (hex_c.1 - b_shape.top as f32) / k);
            let (dx, dy) = (hx - ex, hy - ey);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let (ux, uy) = (dx / len, dy / len);
            for (dist, d) in [(12.0, 16.0), (24.0 + 4.0, 9.0)] {
                let (cx, cy) = (ex + ux * dist, ey + uy * dist);
                g.dot(cx, cy, d / 2.0, (40, 44, 64), 0.80);
                g.dot(cx, cy, d / 2.0, (255, 255, 255), 0.12);
                g.stat(|g| g.stroke_circle(cx, cy, d / 2.0 - 0.5, (255, 255, 255), 0.35, 1.0));
            }
            let x0 = 18.0;
            let mut y = 16.0;
            for (i, (words, first, render_end)) in rows.iter().enumerate() {
                if i > 0 && *first {
                    y += 7.0;
                }
                let base = y + 15.5;
                for (s, f, c, x) in words {
                    g.text(s, x0 + x, base, *f, 15.0, *c, H::L, V::Base);
                }
                y += 21.0;
                if *render_end {
                    y += 9.0;
                    let tr = R::new(x0, y, inner, 5.0);
                    g.fill_rrect(tr, 3.0, (255, 255, 255), 0.20);
                    let p = progress.clamp(0.0, 1.0);
                    if p > 0.0 {
                        let fr = R::new(x0, y, (inner * p).max(5.0), 5.0);
                        let ca = (c255(fa)[0] as u8, c255(fa)[1] as u8, c255(fa)[2] as u8);
                        g.shade(rrect_path(fr.grow(3.0), 5.5), hgrad(fr.x, fr.r(), &[(0.0, ca, 0.18), (1.0, (169, 139, 255), 0.22)]), &[]);
                        g.shade(rrect_path(fr, 3.0), hgrad(fr.x, fr.r(), &[(0.0, ca, 1.0), (1.0, (169, 139, 255), 1.0)]), &[]);
                    }
                    y += 5.0 + 3.0;
                }
            }
            if !rows.is_empty() {
                y += 12.0;
            }
            // chips: 4 equal columns, 7 px gap
            let cw = (inner - 3.0 * 7.0) / 4.0;
            for (i, (label, val, unit, c)) in chips.iter().enumerate() {
                let cr = R::new(x0 + i as f32 * (cw + 7.0), y, cw, chip_h);
                g.stat(|g| {
                    g.fill_rrect(cr, 13.0, (255, 255, 255), 0.10);
                    g.shade_stroke(
                        rrect_path(cr.grow(-0.5), 12.5),
                        vgrad(cr.y, cr.b(), &[(0.0, (255, 255, 255), 0.30), (0.4, (255, 255, 255), 0.05), (1.0, (255, 255, 255), 0.03)]),
                        1.0,
                        &[cr.x, cr.y, cr.w, cr.h],
                    );
                    g.text_t(label, cr.x + 9.0, cr.y + 8.0 + 8.0, Face::SansBold, 9.0, 9.0 * 0.13, (200, 206, 222), H::L, V::Base);
                });
                let vb = cr.y + 8.0 + 11.0 + 4.0 + 15.5;
                let vw = g.measure(val, Face::SansSemi, 16.0).0;
                g.text(val, cr.x + 9.0, vb, Face::SansSemi, 16.0, *c, H::L, V::Base);
                g.text(unit, cr.x + 9.0 + vw + 1.0, vb, Face::Sans, 10.5, QUIET, H::L, V::Base);
            }
            y += chip_h;
            if let Some(f) = &footer {
                y += 12.0;
                let fb_ = y + 11.5;
                let (rl, link) = right_link;
                let lw = g.measure(link, Face::SansSemi, 11.5).0;
                match left_link {
                    Some((ll, label)) => {
                        let r = g.text(label, x0, fb_, Face::SansSemi, 11.5, HIGHLIGHT, H::L, V::Base);
                        if pass == Pass::Dynamic {
                            hits.push((r.grow(4.0), ll));
                        }
                    }
                    None => {
                        let left = g.fit(f, Face::SansSemi, 11.5, inner - lw - 12.0);
                        g.text(&left, x0, fb_, Face::SansSemi, 11.5, FOOT, H::L, V::Base);
                    }
                }
                let lr = g.text(link, x0 + inner, fb_, Face::SansSemi, 11.5, HIGHLIGHT, H::Rt, V::Base);
                if pass == Pass::Dynamic {
                    hits.push((lr.grow(4.0), rl));
                }
            }
        }
        self.b_hits = hits;
        glass::over(surf.bytes(), &self.b_fg);
        if let Some(dir) = ctx.dump {
            let mut v = format!("{w} {hh} {} {}\n", self.b_shape.left - pad, self.b_shape.top - pad).into_bytes();
            v.extend_from_slice(surf.bytes());
            let _ = std::fs::write(dir.join("bubble.bgra"), v);
        }
        if self.b_anim.is_none() && self.b_open {
            self.present_bubble(255);
        }
    }

    /// Bubble-local point -> the link under it.
    fn bubble_link_at(&self, cx: i32, cy: i32) -> Option<Link> {
        let k = self.at.scale;
        let shape_x = self.b_shape.left - self.b_origin.x;
        let shape_y = self.b_shape.top - self.b_origin.y;
        let (x, y) = ((cx - shape_x) as f32 / k, (cy - shape_y) as f32 / k);
        self.b_hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, l)| *l)
    }
}

/// What the bubble needs from the app.
pub(super) struct BubbleCtx<'a> {
    pub snap: &'a Snapshot,
    pub cfg: &'a Config,
    pub dw: Option<&'a mut Dw>,
    pub scale: f32,
    pub transparency: Option<bool>,
    pub dump: Option<&'a std::path::Path>,
}

fn mt(dw: Option<&mut Dw>, s: &str, face: Face, size: f32, track: f32, k: f32) -> f32 {
    match dw {
        Some(d) => d.get_t(s, face, size * k, track * k).adv / k,
        None => s.chars().count() as f32 * (size * 0.55 + track),
    }
}

/// Greedy word wrap over styled spans: at most `max_rows` rows, the rest ellipsised.
fn wrap(mut dw: Option<&mut Dw>, spans: &[Span], maxw: f32, k: f32, max_rows: usize) -> Vec<Vec<(String, Face, Rgb, f32)>> {
    let mut rows: Vec<Vec<(String, Face, Rgb, f32)>> = vec![vec![]];
    let mut x = 0.0f32;
    for (text, face, c) in spans {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            // next word including its trailing spaces
            let end = rest.find(' ').map(|i| rest[i..].find(|ch| ch != ' ').map(|j| i + j).unwrap_or(rest.len())).unwrap_or(rest.len());
            let (word, tail) = rest.split_at(end);
            rest = tail;
            let ww = mt(dw.as_deref_mut(), word, *face, 15.0, 0.0, k);
            let trimmed = mt(dw.as_deref_mut(), word.trim_end(), *face, 15.0, 0.0, k);
            if x + trimmed > maxw && x > 0.0 {
                if rows.len() == max_rows {
                    if let Some(last) = rows[max_rows - 1].last_mut() {
                        last.0 = format!("{}…", last.0.trim_end());
                    }
                    return rows;
                }
                rows.push(vec![]);
                x = 0.0;
            }
            rows.last_mut().unwrap().push((word.to_string(), *face, *c, x));
            x += ww;
        }
    }
    rows
}

fn gb(mb: f64) -> String {
    format!("{:.1}", mb / 1024.0)
}

/// The spoken lines (tachyon::voice) with this window's faces and colours.
fn bubble_lines(snap: &Snapshot, cfg: &Config) -> Vec<(Vec<Span>, bool)> {
    spans_of(tachyon::voice::lines(snap, cfg, chrono::Local::now()))
}

/// Offer "Why is it slow?" while the GPU is working or something is holding it back.
fn asks_why(snap: &Snapshot) -> bool {
    snap.sample.as_ref().and_then(|g| g.util_pct).is_some_and(|u| u >= 30)
        || snap.insights.bottleneck.as_ref().is_some_and(|v| v.kind.is_problem())
}

fn spans_of(lines: Vec<tachyon::voice::Line>) -> Vec<(Vec<Span>, bool)> {
    use tachyon::voice::Tone;
    lines
        .into_iter()
        .map(|l| {
            let spans = l
                .words
                .into_iter()
                .map(|(t, tone)| match tone {
                    Tone::Plain => (t, Face::SansSemi, TEXT),
                    Tone::Quiet => (t, Face::Sans, QUIET),
                    Tone::Name => (t, Face::SansSemi, HIGHLIGHT),
                    Tone::Warm => (t, Face::SansSemi, WARN),
                    Tone::Hot => (t, Face::SansSemi, WARN_HOT),
                })
                .collect();
            (spans, l.render)
        })
        .collect()
}

/// GPU %, TEMP °, VRAM used/total GB, POWER W.
fn bubble_chips(snap: &Snapshot, cfg: &Config) -> [(String, String, String, Rgb); 4] {
    let g = snap.sample.clone().unwrap_or_default();
    let dash = || "—".to_string();
    let temp_c = g.temp_c.map(|t| t as f32);
    let tc = match temp_c {
        Some(t) if t >= cfg.gpu.temp_red_c => WARN_HOT,
        Some(t) if t >= cfg.gpu.temp_amber_c => WARN,
        _ => TEXT,
    };
    [
        ("GPU".into(), g.util_pct.map(|u| u.to_string()).unwrap_or_else(dash), "%".into(), TEXT),
        ("TEMP".into(), g.temp_c.map(|t| t.to_string()).unwrap_or_else(dash), "°".into(), tc),
        (
            "VRAM".into(),
            g.vram_used_mb.map(gb).unwrap_or_else(dash),
            g.vram_total_mb.map(|t| format!("/{:.0}", t / 1024.0)).unwrap_or_default(),
            TEXT,
        ),
        ("POWER".into(), g.power_w.map(|w| format!("{w:.0}")).unwrap_or_else(dash), "W".into(), TEXT),
    ]
}

/// `RTX 5080 · 2692 MHz · fan 64%`
fn bubble_footer(snap: &Snapshot) -> Option<String> {
    let name = snap.gpus.get(snap.selected as usize)?;
    let short = name.replace("NVIDIA ", "").replace("GeForce ", "");
    let mut parts = vec![short];
    if let Some(g) = &snap.sample {
        if let Some(c) = g.clk_gfx_mhz {
            parts.push(format!("{c} MHz"));
        }
        if let Some(f) = g.fan_pct {
            parts.push(format!("fan {f}%"));
        }
    }
    Some(parts.join(" · "))
}

// ---- window procedures ----------------------------------------------------------------------

unsafe extern "system" fn hex_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(a) = app() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    if a.comp.as_ref().is_none_or(|c| c.hwnd != hwnd) {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    match msg {
        WM_NCHITTEST => {
            let (sx, sy) = lparam_xy(lp);
            let mut p = POINT { x: sx, y: sy };
            ScreenToClient(hwnd, &mut p);
            let c = a.comp.as_ref().unwrap();
            return if c.alpha_at(p.x, p.y) >= 96 { HTCLIENT as LRESULT } else { HTTRANSPARENT as LRESULT };
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let mut p: POINT = std::mem::zeroed();
            GetCursorPos(&mut p);
            a.comp.as_mut().unwrap().down_at = Some(p);
            SetCapture(hwnd);
            return 0;
        }
        WM_MOUSEMOVE => {
            let c = a.comp.as_mut().unwrap();
            if let Some(d) = c.down_at {
                let mut p: POINT = std::mem::zeroed();
                GetCursorPos(&mut p);
                if (p.x - d.x).abs() > 4 || (p.y - d.y).abs() > 4 {
                    c.down_at = None;
                    ReleaseCapture();
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
                }
            }
            return 0;
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            if a.comp.as_mut().unwrap().down_at.take().is_some() {
                a.hex_toggle_bubble();
            }
            return 0;
        }
        WM_MOVE => {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut r);
            a.comp.as_mut().unwrap().moved_to(r.left, r.top);
        }
        WM_EXITSIZEMOVE => {
            let c = a.comp.as_ref().unwrap();
            a.cfg.window.hex_x = Some(c.pos.x as f32);
            a.cfg.window.hex_y = Some(c.pos.y as f32);
            config::save_window(&a.path, &a.cfg.window);
        }
        WM_CONTEXTMENU => {
            let (x, y) = lparam_xy(lp);
            a.menu_from(hwnd, x, y);
            return 0;
        }
        WM_TIMER => {
            match wp {
                T_FRAME => {
                    let dump = a.dump.clone();
                    a.comp.as_mut().unwrap().on_frame(dump.as_deref());
                }
                T_BUBBLE_ANIM => a.comp.as_mut().unwrap().bubble_anim_step(),
                T_BUBBLE_IDLE => a.comp.as_mut().unwrap().bubble_idle_check(),
                _ => {}
            }
            return 0;
        }
        WM_DPICHANGED => {
            a.comp.as_mut().unwrap().on_dpi((wp & 0xffff) as u32);
            return 0;
        }
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_CLOSE => {
            PostMessageW(a.hwnd, WM_CLOSE, 0, 0);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe extern "system" fn bubble_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(a) = app() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    if a.comp.as_ref().is_none_or(|c| c.bubble != hwnd) {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    match msg {
        WM_NCHITTEST => {
            let (sx, sy) = lparam_xy(lp);
            let c = a.comp.as_ref().unwrap();
            let r = c.b_shape;
            return if sx >= r.left && sx < r.right && sy >= r.top && sy < r.bottom {
                HTCLIENT as LRESULT
            } else {
                HTTRANSPARENT as LRESULT
            };
        }
        WM_SETCURSOR => {
            let mut p: POINT = std::mem::zeroed();
            GetCursorPos(&mut p);
            ScreenToClient(hwnd, &mut p);
            let link = a.comp.as_ref().unwrap().bubble_link_at(p.x, p.y).is_some();
            SetCursor(LoadCursorW(null_mut(), if link { IDC_HAND } else { IDC_ARROW }));
            return 1;
        }
        WM_MOUSEMOVE => {
            a.comp.as_mut().unwrap().b_last_hover = Instant::now();
            return 0;
        }
        WM_LBUTTONUP => {
            let (cx, cy) = lparam_xy(lp);
            match a.comp.as_ref().unwrap().bubble_link_at(cx, cy) {
                Some(Link::Panel) => {
                    a.comp.as_mut().unwrap().close_bubble();
                    a.open_view(super::View::Main);
                }
                Some(Link::Report) => {
                    a.comp.as_mut().unwrap().close_bubble();
                    a.open_view(super::View::Report);
                }
                Some(Link::Why) => {
                    let lines = tachyon::voice::why_lines(a.s.engine.why_now().as_ref());
                    a.bubble_special(Some((Special::Why, lines)));
                }
                Some(Link::Back) => a.bubble_special(None),
                None => a.comp.as_mut().unwrap().close_bubble(),
            }
            return 0;
        }
        WM_CONTEXTMENU => {
            let (x, y) = lparam_xy(lp);
            a.menu_from(hwnd, x, y);
            return 0;
        }
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            return 0;
        }
        WM_ERASEBKGND => return 1,
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

impl App {
    fn bubble_ctx_parts(&mut self) -> (Option<&mut Companion>, BubbleCtx<'_>) {
        let ctx = BubbleCtx {
            snap: &self.s.snap,
            cfg: &self.cfg,
            dw: self.dw.as_mut(),
            scale: self.scale,
            transparency: self.transparency,
            dump: self.dump.as_deref(),
        };
        (self.comp.as_mut(), ctx)
    }

    pub(super) fn hex_toggle_bubble(&mut self) {
        let (c, mut ctx) = self.bubble_ctx_parts();
        if let Some(c) = c {
            unsafe { c.toggle_bubble(&mut ctx) };
        }
    }

    /// Replace the bubble's lines (or go back to the usual ones) and redraw it.
    pub(super) fn bubble_special(&mut self, sp: Option<(Special, Vec<tachyon::voice::Line>)>) {
        let (c, mut ctx) = self.bubble_ctx_parts();
        if let Some(c) = c {
            c.b_special = sp;
            c.b_last_hover = Instant::now();
            unsafe {
                if c.b_open {
                    c.render_bubble(&mut ctx);
                } else {
                    c.open_bubble(&mut ctx);
                }
            }
        }
    }

    /// Hex tells the user about a new crash report, once.
    fn hex_announce(&mut self) {
        let Some(r) = self.s.snap.insights.report.clone() else { return };
        self.bubble_special(Some((Special::Announce, tachyon::voice::announcement_lines(&r))));
        if let Some(c) = self.comp.as_mut() {
            // give it longer than the usual idle time before it closes by itself
            c.b_last_hover = Instant::now() + Duration::from_secs(20);
        }
        self.s.engine.mark_announced(&mut self.s.snap.insights);
    }

    /// Poll tick in companion mode: sprite state/colours, and the bubble while it is open.
    pub(super) fn hex_poll(&mut self) {
        if self.s.snap.insights.report_new && self.cfg.hex_announce {
            self.hex_announce();
        }
        let now = self.now_ms();
        let dump = self.dump.clone();
        let (c, mut ctx) = self.bubble_ctx_parts();
        if let Some(c) = c {
            unsafe {
                c.on_poll(ctx.snap, ctx.cfg, now, dump.as_deref());
                if c.b_open && c.b_anim.is_none() {
                    c.render_bubble(&mut ctx);
                }
            }
        }
    }

    /// Switch the UI mode (menu). Island/panel use the main window; companion hides it and
    /// shows Hex. The choice is saved as the start mode.
    pub(super) fn set_mode(&mut self, m: UiMode) {
        unsafe {
            if m == UiMode::Companion {
                if self.comp.is_none() {
                    let hinst = GetModuleHandleW(null());
                    self.comp = Companion::new(hinst, &self.cfg, &self.s.snap);
                }
                if self.comp.is_none() {
                    return;
                }
                if !self.main_hidden {
                    let _ = self.target_shape();
                    self.remember_geometry();
                }
                self.mode = m;
                self.main_hidden = true;
                ShowWindow(self.hwnd, SW_HIDE);
                self.cfg.window.compact = false;
                if let Some(c) = self.comp.as_mut() {
                    c.show();
                }
            } else {
                if let Some(mut c) = self.comp.take() {
                    c.destroy();
                }
                self.mode = m;
                let island = m == UiMode::Island;
                self.show_main(island);
            }
            config::save_mode(&self.path, m);
            config::save_window(&self.path, &self.cfg.window);
        }
    }

    /// Show the main window directly as the island or the panel (no morph).
    pub(super) unsafe fn show_main(&mut self, island: bool) {
        if self.anim.is_some() {
            KillTimer(self.hwnd, 2);
            self.anim = None;
        }
        self.cfg.window.compact = island;
        self.island_state = Island::from_render(self.render_active());
        self.main_hidden = false;
        self.hover = None;
        self.fg_valid = false;
        self.geo = None;
        if !island {
            self.shape = self.panel_rect_from_cfg();
            self.frame(true);
        }
        self.shape = if island { self.island_rect_from_cfg() } else { self.panel_rect_from_cfg() };
        self.apply_mode(island);
        self.frame(true);
        ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        self.shown = true;
    }

    /// Bubble "Open panel ›": the panel opens next to Hex's mode (Hex stays).
    pub(super) fn open_panel_from_companion(&mut self) {
        unsafe {
            self.show_main(false);
            SetForegroundWindow(self.hwnd);
        }
    }

    /// In companion mode the panel's island button / title double-click just hides the panel.
    pub(super) fn hide_main_in_companion(&mut self) {
        unsafe {
            let _ = self.target_shape();
            self.remember_geometry();
            config::save_window(&self.path, &self.cfg.window);
            self.main_hidden = true;
            self.view = super::View::Main;
            self.set = None;
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}
