//! The Settings view of the panel: Tachyon's main `tachyon.toml` options as Aurora controls in five
//! tabs. Every change is checked, saved at once (src/settings.rs: only the changed keys, atomic,
//! with a backup) and applied live where it can be; the rest says "restart to apply".
//! The view only exists while it is open (`App::set` is None otherwise), so it costs nothing closed.
use super::*;
use tachyon::settings::{self as st, BLACKBOX_STEPS, BUBBLE_STEPS, CURRENCIES, FORECAST_STEPS, POLL_STEPS};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum Tab {
    #[default]
    General,
    Projects,
    Alerts,
    Data,
    Ai,
}

const TABS: [(Tab, &str); 5] =
    [(Tab::General, "General"), (Tab::Projects, "Projects"), (Tab::Alerts, "Alerts"), (Tab::Data, "Data"), (Tab::Ai, "AI agents")];

/// A control on the Settings view. `Btn::Set(ctl, d)`: d = -1/+1 for steppers, 0 otherwise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Ctl {
    Tab(u8),
    Mode(u8),
    Skin(u8),
    Announce,
    BubbleClose,
    Poll,
    RootAdd,
    RootDel(u8),
    TempAmber,
    TempRed,
    VramAmber,
    VramRed,
    Forecast,
    Blackbox,
    BlackboxMin,
    History,
    Price,
    Currency,
    Agent,
    CopyMcp,
    Gpu(u8),
    Startup,
    Reset,
}

/// Open-view state. Dropped when the view closes.
#[derive(Default)]
pub(super) struct SetUi {
    pub tab: Tab,
    /// the backup of the file as it was before this visit has been written
    backed_up: bool,
    msg: Option<(String, Rgb)>,
    saved_at: Option<String>,
    /// "Reset settings" was clicked once: the next click resets
    reset_armed: bool,
    /// the Startup shortcut exists (read when the view opens and after a change)
    startup: bool,
    /// keyboard focus (Tab / Shift+Tab), drawn as a ring
    pub focus: Option<Btn>,
}

/// Steppers: the value area (d = 0) is only a wheel target, not a keyboard stop.
fn is_stepper(c: Ctl) -> bool {
    matches!(
        c,
        Ctl::BubbleClose
            | Ctl::Poll
            | Ctl::TempAmber
            | Ctl::TempRed
            | Ctl::VramAmber
            | Ctl::VramRed
            | Ctl::Forecast
            | Ctl::BlackboxMin
            | Ctl::Price
    )
}

impl SetUi {
    pub fn new(tab: Tab) -> Self {
        SetUi { tab, startup: tachyon::startup::enabled(), ..Default::default() }
    }
}

const ROW: f32 = 46.0;
const MODES: [UiMode; 3] = [UiMode::Island, UiMode::Companion, UiMode::Panel];

fn secs_label(s: u64) -> String {
    if s == 0 { "Never".into() } else { format!("{s} s") }
}
fn poll_label(ms: u64) -> String {
    format!("{} s", (ms as f64 / 1000.0 * 100.0).round() / 100.0)
}
fn mins_label(m: u32) -> String {
    if m == 0 { "Off".into() } else { format!("{m} min") }
}
fn gb_label(mb: f32) -> String {
    format!("{:.2} GB", mb / 1024.0)
}

impl App {
    pub(super) fn open_settings(&mut self, tab: Option<Tab>) {
        let t = tab.or(self.set.as_ref().map(|s| s.tab)).unwrap_or_default();
        self.set = Some(Box::new(SetUi::new(t)));
        self.open_view(View::Settings);
    }

    // ---- controls -------------------------------------------------------------------------
    fn hov(&self, b: Btn) -> bool {
        self.hover == Some(b)
    }

    /// Two-state switch, right-aligned at `xr`, centred on `cy`.
    fn toggle(&mut self, g: &mut G, xr: f32, cy: f32, on: bool, b: Btn) {
        let r = R::new(xr - 40.0, cy - 11.0, 40.0, 22.0);
        let hov = self.hov(b);
        g.stat(|g| {
            if on {
                g.shade(rrect_path(r, 11.0), hgrad(r.x, r.r(), &[(0.0, CYAN, 0.75), (1.0, VIOLET, 0.75)]), &[r.x, r.y, 1.0]);
            } else {
                g.fill_rrect(r, 11.0, (255, 255, 255), if hov { 0.24 } else { 0.14 });
            }
            g.stroke_rrect(r.grow(-0.5), 10.5, (255, 255, 255), if hov { 0.55 } else { 0.30 }, 1.0);
            let kx = if on { r.r() - 11.0 } else { r.x + 11.0 };
            g.dot(kx, cy, 8.0, (255, 255, 255), if on { 1.0 } else { 0.85 });
        });
        self.hits.push((r.grow(3.0), b));
    }

    /// Segmented choice; returns the left edge.
    fn segments(&mut self, g: &mut G, xr: f32, cy: f32, labels: &[&str], sel: usize, b: impl Fn(u8) -> Btn) -> f32 {
        let pad = 12.0;
        let ws: Vec<f32> = labels.iter().map(|l| g.measure(l, Face::SansSemi, 11.5).0 + 2.0 * pad).collect();
        let total: f32 = ws.iter().sum::<f32>() + 4.0;
        let outer = R::new(xr - total, cy - 13.0, total, 26.0);
        g.stat(|g| {
            g.fill_rrect(outer, 13.0, (255, 255, 255), 0.10);
            g.stroke_rrect(outer.grow(-0.5), 12.5, (255, 255, 255), 0.22, 1.0);
        });
        let mut x = outer.x + 2.0;
        for (i, (l, w)) in labels.iter().zip(ws).enumerate() {
            let r = R::new(x, cy - 11.0, w, 22.0);
            let bb = b(i as u8);
            let (on, hov) = (i == sel, self.hov(bb));
            g.stat(|g| {
                if on {
                    g.fill_rrect(r, 11.0, CYAN, 0.36);
                    g.stroke_rrect(r.grow(-0.5), 10.5, (255, 255, 255), 0.40, 1.0);
                } else if hov {
                    g.fill_rrect(r, 11.0, (255, 255, 255), 0.14);
                }
                g.text(l, r.cx(), cy + 4.0, Face::SansSemi, 11.5, if on { TEXT } else { TEXT2 }, H::C, V::Base);
            });
            self.hits.push((r, bb));
            x += w;
        }
        outer.x
    }

    /// "−  value  +" stepper.
    fn stepper(&mut self, g: &mut G, xr: f32, cy: f32, val: &str, ctl: Ctl, can_dec: bool, can_inc: bool) {
        let vw = g.measure(val, Face::SansSemi, 12.5).0.max(70.0) + 16.0;
        let plus = R::new(xr - 24.0, cy - 12.0, 24.0, 24.0);
        let minus = R::new(plus.x - vw - 24.0, cy - 12.0, 24.0, 24.0);
        for (r, d, en, is_plus) in [(minus, -1i8, can_dec, false), (plus, 1, can_inc, true)] {
            let b = Btn::Set(ctl, d);
            let hov = en && self.hov(b);
            g.stat(|g| {
                g.fill_rrect(
                    r,
                    12.0,
                    (255, 255, 255),
                    if hov {
                        0.30
                    } else if en {
                        0.16
                    } else {
                        0.06
                    },
                );
                g.stroke_rrect(r.grow(-0.5), 11.5, (255, 255, 255), if en { 0.30 } else { 0.12 }, 1.0);
                let a = if en { 1.0 } else { 0.35 };
                g.line(r.cx() - 4.5, r.cy(), r.cx() + 4.5, r.cy(), TEXT, a, 1.5);
                if is_plus {
                    g.line(r.cx(), r.cy() - 4.5, r.cx(), r.cy() + 4.5, TEXT, a, 1.5);
                }
            });
            if en {
                self.hits.push((r, b));
            }
        }
        let mid = (minus.r() + plus.x) / 2.0;
        // the value also takes the wheel (see WM_MOUSEWHEEL): a hit with d = 0
        self.hits.push((R::new(minus.r(), cy - 12.0, plus.x - minus.r(), 24.0), Btn::Set(ctl, 0)));
        g.stat(|g| g.text(val, mid, cy + 4.5, Face::SansSemi, 12.5, TEXT, H::C, V::Base));
    }

    /// Glass pill button with a cyan label; returns its rect.
    fn pill(&mut self, g: &mut G, xr: f32, cy: f32, label: &str, b: Btn) -> R {
        let w = g.measure(label, Face::SansSemi, 11.5).0 + 28.0;
        let r = R::new(xr - w, cy - 13.0, w, 26.0);
        let hov = self.hov(b);
        g.stat(|g| {
            g.fill_rrect(r, 13.0, if hov { CYAN } else { (255, 255, 255) }, if hov { 0.30 } else { 0.14 });
            g.stroke_rrect(r.grow(-0.5), 12.5, (255, 255, 255), 0.32, 1.0);
            g.text(label, r.cx(), cy + 4.0, Face::SansSemi, 11.5, if hov { TEXT } else { T_CYAN }, H::C, V::Base);
        });
        self.hits.push((r, b));
        r
    }

    /// Row label and hint on the left, a divider under it; returns the row's centre line.
    fn row(&mut self, g: &mut G, x0: f32, xr: f32, y: f32, label: &str, hint: &str, ctl_w: f32) -> f32 {
        let maxw = (xr - x0 - ctl_w - 14.0).max(60.0);
        let l = g.fit(label, Face::SansSemi, 12.5, maxw);
        let h = g.fit(hint, Face::Sans, 11.0, maxw);
        g.stat(|g| {
            g.text(&l, x0, y + 20.0, Face::SansSemi, 12.5, TEXT, H::L, V::Base);
            if !h.is_empty() {
                g.text(&h, x0, y + 36.0, Face::Sans, 11.0, TEXT2, H::L, V::Base);
            }
            g.line(x0, y + ROW - 0.5, xr, y + ROW - 0.5, (255, 255, 255), DIVIDER_A, 1.0);
        });
        y + ROW / 2.0
    }

    fn note(&mut self, g: &mut G, x0: f32, w: f32, mut y: f32, text: &str, c: Rgb, rows: usize) -> f32 {
        for row in views::wrap_g(g, text, Face::Sans, 11.5, w, rows) {
            g.stat(|g| g.text(&row, x0, y + 13.0, Face::Sans, 11.5, c, H::L, V::Base));
            y += 16.0;
        }
        y
    }

    // ---- the view -------------------------------------------------------------------------
    pub(super) fn draw_settings_view(&mut self, g: &mut G, lw: f32, gpus: &[String]) -> f32 {
        let (x0, xr) = (24.0, lw - 24.0);
        let w = xr - x0;
        let Some(su) = self.set.as_ref() else { return FIELD_H };
        let (tab, saved_at, msg, startup, armed) = (su.tab, su.saved_at.clone(), su.msg.clone(), su.startup, su.reset_armed);
        let restart = st::restart_needed(self.boot_blackbox, &self.cfg);
        let cap = match (&saved_at, restart.is_empty()) {
            (_, false) => format!("SAVED · RESTART TACHYON TO APPLY THE {}", restart.join(", ").to_uppercase()),
            (Some(t), true) => format!("SAVED TO TACHYON.TOML · {t}"),
            (None, true) => "CHANGES ARE SAVED AS YOU MAKE THEM".into(),
        };
        self.view_header(g, x0, xr, "Settings", &cap, None);
        // tabs
        let mut y = FIELD_H + 8.0;
        let labels: Vec<&str> = TABS.iter().map(|t| t.1).collect();
        let sel = TABS.iter().position(|t| t.0 == tab).unwrap_or(0);
        // left-aligned with the text column
        let tw: f32 = labels.iter().map(|l| g.measure(l, Face::SansSemi, 11.5).0 + 24.0).sum::<f32>() + 4.0;
        self.segments(g, x0 - 6.0 + tw, y + 14.0, &labels, sel, |i| Btn::Set(Ctl::Tab(i), 0));
        y += 36.0;
        let c = self.cfg.clone();
        match tab {
            Tab::General => {
                let mode = self.mode;
                let cy = self.row(g, x0, xr, y, "Display mode", "Island, Hex on the desktop, or the panel", 190.0);
                let msel = MODES.iter().position(|m| *m == mode).unwrap_or(2);
                let ml: Vec<&str> = MODES.iter().map(|m| st::mode_label(*m)).collect();
                self.segments(g, xr, cy, &ml, msel, |i| Btn::Set(Ctl::Mode(i), 0));
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Companion skin", "How Hex looks; applies at once", 190.0);
                let ssel = HexSkin::ALL.iter().position(|k| *k == c.hex_skin).unwrap_or(0);
                let sl: Vec<&str> = HexSkin::ALL.iter().map(|k| k.label()).collect();
                self.segments(g, xr, cy, &sl, ssel, |i| Btn::Set(Ctl::Skin(i), 0));
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Hex announces crash reports", "Opens its bubble once after an unexpected stop", 50.0);
                self.toggle(g, xr, cy, c.hex_announce, Btn::Set(Ctl::Announce, 0));
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Hex bubble closes after", "Without the pointer on it", 130.0);
                let (a, b) = st::ends(&BUBBLE_STEPS, c.bubble_close_secs);
                self.stepper(g, xr, cy, &secs_label(c.bubble_close_secs), Ctl::BubbleClose, a, b);
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Refresh", "How often the GPU is read; slower is lighter", 130.0);
                self.stepper(g, xr, cy, &poll_label(c.poll_ms), Ctl::Poll, c.poll_ms > POLL_STEPS[0], c.poll_ms < POLL_STEPS[4]);
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Start with Windows", "A shortcut in your Startup folder; no admin rights", 50.0);
                self.toggle(g, xr, cy, startup, Btn::Set(Ctl::Startup, 0));
                y += ROW;
                let gpus = gpus.to_vec();
                if gpus.len() > 1 {
                    let sel = (self.s.selected as usize).min(gpus.len() - 1);
                    let name = gpus[sel].trim_start_matches("NVIDIA ").to_string();
                    let cy = self.row(g, x0, xr, y, "GPU", &name, 60.0 * gpus.len().min(4) as f32);
                    let labels: Vec<String> = (0..gpus.len().min(4)).map(|i| format!("GPU {i}")).collect();
                    let lr: Vec<&str> = labels.iter().map(|l| l.as_str()).collect();
                    self.segments(g, xr, cy, &lr, sel, |i| Btn::Set(Ctl::Gpu(i), 0));
                    y += ROW;
                }
            }
            Tab::Projects => {
                y = self.note(
                    g,
                    x0,
                    w,
                    y,
                    "Tachyon shows render progress from the logs in these folders, up to four levels down. No folders: a pure GPU monitor.",
                    TEXT2,
                    2,
                ) + 6.0;
                for (i, r) in c.render_roots.iter().enumerate() {
                    let row = R::new(x0 - 8.0, y, w + 16.0, 34.0);
                    let b = Btn::Set(Ctl::RootDel(i as u8), 0);
                    let hov = self.hov(b);
                    let p = g.fit(r, Face::Sans, 12.0, w - 44.0);
                    g.stat(|g| {
                        g.fill_rrect(row, 14.0, (255, 255, 255), ROW_A);
                        g.text(&p, x0, y + 21.5, Face::Sans, 12.0, TEXT, H::L, V::Base);
                    });
                    let xr2 = R::new(xr - 22.0, y + 6.0, 22.0, 22.0);
                    g.stat(|g| {
                        g.fill_rrect(xr2, 11.0, if hov { RED } else { (255, 255, 255) }, if hov { 0.55 } else { 0.14 });
                        g.line(xr2.cx() - 3.5, xr2.cy() - 3.5, xr2.cx() + 3.5, xr2.cy() + 3.5, TEXT, 1.0, 1.3);
                        g.line(xr2.cx() - 3.5, xr2.cy() + 3.5, xr2.cx() + 3.5, xr2.cy() - 3.5, TEXT, 1.0, 1.3);
                    });
                    self.hits.push((xr2, b));
                    y += 40.0;
                }
                if c.render_roots.is_empty() {
                    g.stat(|g| g.text("No project folders.", x0, y + 16.0, Face::SansSemi, 12.5, TEXT2, H::L, V::Base));
                    y += 26.0;
                }
                y += 6.0;
                if c.render_roots.len() < st::MAX_ROOTS {
                    self.pill(g, xr, y + 14.0, "Add folder…", Btn::Set(Ctl::RootAdd, 0));
                }
                y += 34.0;
            }
            Tab::Alerts => {
                let g2 = &c.gpu;
                let cy = self.row(g, x0, xr, y, "Temperature: amber from", "The reading turns amber and Hex warms up", 130.0);
                self.stepper(
                    g,
                    xr,
                    cy,
                    &format!("{:.0} °C", g2.temp_amber_c),
                    Ctl::TempAmber,
                    g2.temp_amber_c > st::TEMP.min as f32,
                    g2.temp_amber_c < g2.temp_red_c - 1.0,
                );
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Temperature: red from", "The reading turns red", 130.0);
                self.stepper(
                    g,
                    xr,
                    cy,
                    &format!("{:.0} °C", g2.temp_red_c),
                    Ctl::TempRed,
                    g2.temp_red_c > g2.temp_amber_c + 1.0,
                    g2.temp_red_c < st::TEMP.max as f32,
                );
                y += ROW;
                let cy = self.row(g, x0, xr, y, "VRAM: amber from", "Or 90% of the card, if lower", 130.0);
                self.stepper(
                    g,
                    xr,
                    cy,
                    &gb_label(g2.vram_amber_mb),
                    Ctl::VramAmber,
                    g2.vram_amber_mb > st::VRAM_MB.min as f32,
                    g2.vram_amber_mb < g2.vram_red_mb - st::VRAM_MB.step as f32,
                );
                y += ROW;
                let cy = self.row(g, x0, xr, y, "VRAM: red from", "Or 97% of the card, if lower", 130.0);
                self.stepper(
                    g,
                    xr,
                    cy,
                    &gb_label(g2.vram_red_mb),
                    Ctl::VramRed,
                    g2.vram_red_mb > g2.vram_amber_mb + st::VRAM_MB.step as f32,
                    g2.vram_red_mb < st::VRAM_MB.max as f32,
                );
                y += ROW;
                let cy = self.row(g, x0, xr, y, "VRAM forecast", "Warn when VRAM would be full within", 130.0);
                let (a, b) = st::ends(&FORECAST_STEPS, c.vram_forecast_minutes);
                self.stepper(g, xr, cy, &mins_label(c.vram_forecast_minutes), Ctl::Forecast, a, b);
                y += ROW;
            }
            Tab::Data => {
                let cy = self.row(g, x0, xr, y, "Flight recorder", "Keeps the last minutes on disk to explain a crash", 50.0);
                self.toggle(g, xr, cy, c.blackbox, Btn::Set(Ctl::Blackbox, 0));
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Recorder keeps", "Fixed-size files, about 0.2 MB per 15 min", 130.0);
                let (a, b) = st::ends(&BLACKBOX_STEPS, c.blackbox_minutes);
                self.stepper(g, xr, cy, &mins_label(c.blackbox_minutes), Ctl::BlackboxMin, a, b);
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Job history", "Finished jobs with energy and a comparison", 50.0);
                self.toggle(g, xr, cy, c.history, Btn::Set(Ctl::History, 0));
                y += ROW;
                let price = c.price_per_kwh.map(|p| format!("{p:.2} / kWh")).unwrap_or("Off".into());
                let cy = self.row(g, x0, xr, y, "Energy price", "Shows what each job cost; the wheel works too", 150.0);
                self.stepper(g, xr, cy, &price, Ctl::Price, c.price_per_kwh.is_some(), c.price_per_kwh.is_none_or(|p| p < st::PRICE.max));
                y += ROW;
                let cy = self.row(g, x0, xr, y, "Currency", "Click to change", 90.0);
                let cur = c.currency.clone();
                self.pill(g, xr, cy, &cur, Btn::Set(Ctl::Currency, 0));
                y += ROW;
                let dir = self.s.engine.history_dir().and_then(|d| d.parent().map(|p| p.display().to_string()));
                let cy = self.row(g, x0, xr, y, "Reset settings", "Back to the defaults; the old file is kept as .bak", 120.0);
                self.pill(g, xr, cy, if armed { "Click to confirm" } else { "Reset…" }, Btn::Set(Ctl::Reset, 0));
                y += ROW;
                if let Some(d) = dir {
                    y = self.note(
                        g,
                        x0,
                        w,
                        y + 2.0,
                        &format!("Stored in {d}. Nothing leaves this PC. To remove Tachyon: quit it, delete tachyon.exe, its tachyon.toml and that folder."),
                        TEXT2,
                        3,
                    );
                }
            }
            Tab::Ai => {
                let cy = self.row(g, x0, xr, y, "Agent API", "Lets AI apps on this PC read Tachyon's live data", 50.0);
                self.toggle(g, xr, cy, c.agent_api, Btn::Set(Ctl::Agent, 0));
                y += ROW;
                let (dot, st_txt) = match (c.agent_api, self.s.agent_live()) {
                    (true, true) => (CYAN, "On: publishing live data for tachyon.exe mcp"),
                    (true, false) => (AMBER, "On, but the shared memory could not be created"),
                    _ => ((150, 160, 180), "Off: tachyon.exe mcp answers that the agent API is off"),
                };
                g.stat(|g| {
                    g.dot(x0 + 4.0, y + 13.0, 4.0, dot, 1.0);
                    g.text(st_txt, x0 + 16.0, y + 17.0, Face::Sans, 11.5, TEXT2, H::L, V::Base);
                });
                y += 30.0;
                let cy = self.row(g, x0, xr, y, "MCP client config", "For Claude Desktop, Cursor and other MCP apps", 150.0);
                self.pill(g, xr, cy, "Copy MCP config", Btn::Set(Ctl::CopyMcp, 0));
                y += ROW + 6.0;
                let exe = std::env::current_exe().unwrap_or_default();
                let snippet = st::mcp_config_json(&exe);
                let lines: Vec<String> = snippet.lines().map(|l| l.to_string()).collect();
                let bh = 12.0 + 15.0 * lines.len() as f32;
                let r = R::new(x0 - 8.0, y, w + 16.0, bh);
                g.stat(|g| {
                    g.fill_rrect(r, 14.0, (0, 0, 0), 0.22);
                    g.stroke_rrect(r.grow(-0.5), 13.5, (255, 255, 255), 0.12, 1.0);
                });
                for (k, l) in lines.iter().enumerate() {
                    let t = g.fit(l, Face::Mono, 10.5, w - 8.0);
                    g.stat(|g| g.text(&t, x0 + 2.0, y + 17.0 + 15.0 * k as f32, Face::Mono, 10.5, T_CYAN, H::L, V::Base));
                }
                y += bh + 8.0;
                y = self.note(
                    g,
                    x0,
                    w,
                    y,
                    "Read-only: the seven tools only read GPU readings, the flight recorder and the job history. An agent cannot change settings or touch the GPU, and nothing leaves this PC.",
                    TEXT2,
                    3,
                );
            }
        }
        if let Some((m, c)) = msg {
            y = self.note(g, x0, w, y + 4.0, &m, c, 2);
        }
        // keyboard focus ring (dynamic layer: moving it does not redraw the static one)
        let focus = self.set.as_ref().and_then(|s| s.focus);
        if let Some(r) = focus.and_then(|f| self.hits.iter().rev().find(|h| h.1 == f).map(|h| h.0)) {
            let rad = (r.h / 2.0).min(14.0) + 3.0;
            g.stroke_rrect(r.grow(3.0), rad, CYAN, 0.95, 2.0);
        }
        y
    }

    /// Keyboard: Tab / Shift+Tab move between controls, Space or Enter press, Left/Right step a
    /// value or move between tabs and segments, Ctrl+Tab changes tab, Esc goes back.
    /// Returns true when the key was used.
    pub(super) fn settings_key(&mut self, vk: u16, shift: bool, ctrl: bool) -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB};
        let stops: Vec<Btn> = {
            let mut v: Vec<Btn> = vec![];
            for (_, b) in &self.hits {
                if let Btn::Set(c, d) = *b {
                    if (d == 0 && is_stepper(c)) || v.contains(b) {
                        continue;
                    }
                    v.push(*b);
                }
            }
            v
        };
        let Some(su) = self.set.as_mut() else { return false };
        let cur = su.focus.and_then(|f| stops.iter().position(|b| *b == f));
        let tab_i = TABS.iter().position(|t| t.0 == su.tab).unwrap_or(0) as i32;
        match vk {
            VK_TAB if ctrl => {
                let n = TABS.len() as i32;
                let i = (tab_i + if shift { -1 } else { 1 }).rem_euclid(n);
                su.focus = Some(Btn::Set(Ctl::Tab(i as u8), 0));
                self.settings_click(Ctl::Tab(i as u8), 0);
            }
            VK_TAB => {
                if stops.is_empty() {
                    return true;
                }
                let n = stops.len() as i32;
                let i = match cur {
                    Some(c) => (c as i32 + if shift { -1 } else { 1 }).rem_euclid(n),
                    None if shift => n - 1,
                    None => 0,
                };
                su.focus = Some(stops[i as usize]);
            }
            VK_RETURN | VK_SPACE => {
                if let Some(Btn::Set(c, d)) = su.focus {
                    self.settings_click(c, d);
                    return true;
                }
                return false;
            }
            VK_LEFT | VK_RIGHT => {
                let by: i32 = if vk == VK_RIGHT { 1 } else { -1 };
                match su.focus {
                    Some(Btn::Set(c, _)) if is_stepper(c) => {
                        self.settings_click(c, by as i8);
                        return true;
                    }
                    Some(Btn::Set(Ctl::Tab(i), _)) => {
                        let j = (i as i32 + by).rem_euclid(TABS.len() as i32) as u8;
                        su.focus = Some(Btn::Set(Ctl::Tab(j), 0));
                        self.settings_click(Ctl::Tab(j), 0);
                        return true;
                    }
                    Some(Btn::Set(Ctl::Mode(i), _)) => {
                        let j = (i as i32 + by).clamp(0, 2) as u8;
                        su.focus = Some(Btn::Set(Ctl::Mode(j), 0));
                        self.settings_click(Ctl::Mode(j), 0);
                        return true;
                    }
                    _ => return false,
                }
            }
            VK_ESCAPE => {
                self.open_view(View::Main);
                return true;
            }
            _ => return false,
        }
        self.fg_valid = false;
        unsafe { self.frame(true) };
        true
    }

    // ---- actions --------------------------------------------------------------------------
    pub(super) fn settings_click(&mut self, ctl: Ctl, d: i8) {
        let Some(su) = self.set.as_mut() else { return };
        su.msg = None;
        if ctl != Ctl::Reset {
            su.reset_armed = false;
        }
        let d = d as i32;
        let mut changed = true;
        let c = &mut self.cfg;
        match ctl {
            Ctl::Tab(i) => {
                su.tab = TABS.get(i as usize).map(|t| t.0).unwrap_or_default();
                changed = false;
            }
            Ctl::Mode(i) => {
                let m = MODES[(i as usize).min(2)];
                c.mode = Some(m);
                if m != self.mode {
                    // set_mode saves the mode itself; the panel stays open in every mode
                    self.set_mode(m);
                    if m != UiMode::Panel {
                        self.open_view(View::Settings);
                    }
                }
                changed = false;
            }
            Ctl::Skin(i) => {
                let k = HexSkin::ALL[(i as usize).min(HexSkin::ALL.len() - 1)];
                changed = k != c.hex_skin;
                c.hex_skin = k;
            }
            Ctl::Announce => c.hex_announce = !c.hex_announce,
            Ctl::BubbleClose => c.bubble_close_secs = st::cycle_clamped(&BUBBLE_STEPS, c.bubble_close_secs, d),
            Ctl::Poll => c.poll_ms = st::cycle_clamped(&POLL_STEPS, c.poll_ms, d),
            Ctl::RootAdd => {
                changed = false;
                if let Some(dir) = pick_folder(self.hwnd) {
                    match st::check_root(&c.render_roots, &dir) {
                        Ok(s) => {
                            c.render_roots.push(s);
                            changed = true;
                        }
                        Err(e) => su.msg = Some((e, T_AMBER)),
                    }
                }
            }
            Ctl::RootDel(i) => {
                if (i as usize) < c.render_roots.len() {
                    c.render_roots.remove(i as usize);
                } else {
                    changed = false;
                }
            }
            Ctl::TempAmber => c.gpu.temp_amber_c = st::TEMP.bump(c.gpu.temp_amber_c as f64, d) as f32,
            Ctl::TempRed => c.gpu.temp_red_c = st::TEMP.bump(c.gpu.temp_red_c as f64, d) as f32,
            Ctl::VramAmber => c.gpu.vram_amber_mb = st::VRAM_MB.bump(c.gpu.vram_amber_mb as f64, d) as f32,
            Ctl::VramRed => c.gpu.vram_red_mb = st::VRAM_MB.bump(c.gpu.vram_red_mb as f64, d) as f32,
            Ctl::Forecast => c.vram_forecast_minutes = st::cycle_clamped(&FORECAST_STEPS, c.vram_forecast_minutes, d),
            Ctl::Blackbox => c.blackbox = !c.blackbox,
            Ctl::BlackboxMin => c.blackbox_minutes = st::cycle_clamped(&BLACKBOX_STEPS, c.blackbox_minutes, d),
            Ctl::History => c.history = !c.history,
            Ctl::Price => c.price_per_kwh = st::bump_price(c.price_per_kwh, d),
            Ctl::Currency => {
                let i = CURRENCIES.iter().position(|x| x.eq_ignore_ascii_case(&c.currency)).map(|i| i + 1).unwrap_or(0);
                c.currency = CURRENCIES[i % CURRENCIES.len()].to_string();
            }
            Ctl::Agent => c.agent_api = !c.agent_api,
            Ctl::Gpu(i) => {
                c.gpu_index = i as u32;
                self.s.selected = i as u32;
                self.s.tick();
            }
            Ctl::Startup => {
                changed = false;
                let exe = std::env::current_exe().unwrap_or_default();
                let on = !su.startup;
                match tachyon::startup::set(on, &exe) {
                    Ok(()) => {
                        su.startup = tachyon::startup::enabled();
                        su.msg = Some((
                            if on {
                                "Tachyon starts when you sign in to Windows (Startup folder shortcut)."
                            } else {
                                "Tachyon no longer starts with Windows."
                            }
                            .into(),
                            T_CYAN,
                        ));
                    }
                    Err(e) => su.msg = Some((format!("Could not change it: {e}"), T_AMBER)),
                }
            }
            Ctl::Reset => {
                changed = false;
                if !su.reset_armed {
                    su.reset_armed = true;
                    su.msg = Some((
                        "Click Reset again to go back to the defaults. Your current file is kept as tachyon.toml.bak.".into(),
                        T_AMBER,
                    ));
                } else {
                    su.reset_armed = false;
                    match st::reset(&self.path, &c.window) {
                        Ok(fresh) => {
                            *c = fresh;
                            su.msg = Some(("Settings reset. The previous file is kept as tachyon.toml.bak.".into(), T_CYAN));
                            su.saved_at = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                            su.backed_up = true;
                            self.s.apply(sampler_cfg(&self.cfg, self.demo.is_some()));
                            if let Some(cp) = self.comp.as_mut() {
                                cp.b_close_secs = self.cfg.bubble_close_secs;
                                unsafe { cp.set_skin(self.cfg.hex_skin) };
                            }
                        }
                        Err(e) => su.msg = Some((e, T_AMBER)),
                    }
                }
            }
            Ctl::CopyMcp => {
                changed = false;
                let exe = std::env::current_exe().unwrap_or_default();
                su.msg = Some(if copy_text(self.hwnd, &st::mcp_config_json(&exe)) {
                    ("Copied. Paste it into your MCP client's settings and restart the client.".into(), T_CYAN)
                } else {
                    ("Could not open the clipboard; copy the text above by hand.".into(), T_AMBER)
                });
            }
        }
        // steppers with d = 0 are the wheel targets: no change on a plain click
        if d == 0
            && matches!(
                ctl,
                Ctl::BubbleClose
                    | Ctl::Poll
                    | Ctl::TempAmber
                    | Ctl::TempRed
                    | Ctl::VramAmber
                    | Ctl::VramRed
                    | Ctl::Forecast
                    | Ctl::BlackboxMin
                    | Ctl::Price
            )
        {
            changed = false;
        }
        if changed {
            self.settings_changed();
        }
        self.fg_valid = false;
        unsafe { self.frame(true) };
    }

    /// Mouse wheel over a stepper's value.
    pub(super) fn settings_wheel(&mut self, up: bool) {
        if let Some(Btn::Set(ctl, _)) = self.hover {
            if matches!(
                ctl,
                Ctl::BubbleClose
                    | Ctl::Poll
                    | Ctl::TempAmber
                    | Ctl::TempRed
                    | Ctl::VramAmber
                    | Ctl::VramRed
                    | Ctl::Forecast
                    | Ctl::BlackboxMin
                    | Ctl::Price
            ) {
                self.settings_click(ctl, if up { 1 } else { -1 });
            }
        }
    }

    /// Validate, save and apply what can be applied now.
    fn settings_changed(&mut self) {
        let old_poll = self.s_poll_ms;
        st::sanitise(&mut self.cfg);
        let backup = self.set.as_ref().is_some_and(|s| !s.backed_up);
        let res = st::save(&self.path, &self.cfg, backup);
        if let Some(su) = self.set.as_mut() {
            match res {
                Ok(wrote) => {
                    su.backed_up |= wrote;
                    su.saved_at = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                }
                Err(e) => su.msg = Some((e, T_AMBER)),
            }
        }
        self.s.apply(sampler_cfg(&self.cfg, self.demo.is_some()));
        if let Some(c) = self.comp.as_mut() {
            c.b_close_secs = self.cfg.bubble_close_secs;
            // the companion skin applies live
            unsafe { c.set_skin(self.cfg.hex_skin) };
        }
        if self.cfg.poll_ms != old_poll {
            self.s_poll_ms = self.cfg.poll_ms;
            unsafe {
                SetTimer(self.hwnd, 1, self.cfg.poll_interval().as_millis() as u32, None);
            }
        }
    }
}

/// The system folder picker (SHBrowseForFolder with the new dialog style). None = cancelled.
fn pick_folder(owner: HWND) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
    use windows_sys::Win32::UI::Shell::{BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW, SHGetPathFromIDListW};
    unsafe {
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let title = wz("Choose a project or render folder for Tachyon to watch");
        let mut bi: BROWSEINFOW = std::mem::zeroed();
        bi.hwndOwner = owner;
        bi.lpszTitle = title.as_ptr();
        bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let ok = SHGetPathFromIDListW(pidl, buf.as_mut_ptr()) != 0;
        CoTaskMemFree(pidl as *const _);
        if !ok {
            return None;
        }
        let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }
}

/// Put text on the clipboard as Unicode.
fn copy_text(owner: HWND, s: &str) -> bool {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    let w: Vec<u16> = s.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(owner) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        let mut ok = false;
        if !h.is_null() {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                GlobalUnlock(h);
                ok = !SetClipboardData(CF_UNICODETEXT, h as _).is_null();
            }
            if !ok {
                GlobalFree(h);
            }
        }
        CloseClipboard();
        ok
    }
}
