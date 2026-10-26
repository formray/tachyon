//! Panel additions in 1.0: the insights block on the main view (bottleneck, VRAM forecast, crash
//! banner, last job) and the two extra views, the crash report and the job history.
use super::*;
use tachyon::history::{JobKind, JobRecord, fmt_duration};
use tachyon::report::{Cause, gb};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum View {
    #[default]
    Main,
    Report,
    History,
    Settings,
}

/// Word wrap with the real font; the last row is ellipsised when the text does not fit.
pub(super) fn wrap_g(g: &mut G, s: &str, face: Face, size: f32, maxw: f32, max_rows: usize) -> Vec<String> {
    let mut rows: Vec<String> = vec![];
    let mut cur = String::new();
    let words: Vec<&str> = s.split(' ').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        let cand = if cur.is_empty() { w.to_string() } else { format!("{cur} {w}") };
        if cur.is_empty() || g.measure(&cand, face, size).0 <= maxw {
            cur = cand;
            continue;
        }
        if rows.len() + 1 == max_rows {
            let rest = words[i..].join(" ");
            rows.push(g.fit(&format!("{cur} {rest}"), face, size, maxw));
            return rows;
        }
        rows.push(std::mem::replace(&mut cur, w.to_string()));
    }
    if !cur.is_empty() {
        rows.push(g.fit(&cur, face, size, maxw));
    }
    rows
}

fn hm(ms: i64) -> String {
    tachyon::telemetry::Sample { t_ms: ms, ..Default::default() }.local().format("%H:%M").to_string()
}

fn hms(ms: i64) -> String {
    tachyon::telemetry::Sample { t_ms: ms, ..Default::default() }.local().format("%H:%M:%S").to_string()
}

fn day(ms: i64) -> String {
    tachyon::telemetry::Sample { t_ms: ms, ..Default::default() }.local().format("%a %d %b").to_string()
}

impl App {
    /// A text link ("Open report ›") at the right edge; returns its rect.
    fn link(&mut self, g: &mut G, label: &str, xr: f32, base: f32, b: Btn) -> R {
        let hov = self.hover == Some(b);
        let r = g.text(label, xr, base, Face::SansSemi, 11.5, if hov { TEXT } else { T_CYAN }, H::Rt, V::Base);
        self.hits.push((r.grow(4.0), b));
        r
    }

    /// Open a panel view from anywhere (menu, bubble, links).
    pub(super) fn open_view(&mut self, v: View) {
        self.view = v;
        if v == View::Settings {
            self.hint = None; // found it
        }
        if v == View::Settings && self.set.is_none() {
            self.set = Some(Box::new(super::settings::SetUi::new(Default::default())));
        } else if v != View::Settings {
            self.set = None;
        }
        if v == View::History {
            self.refresh_history();
        }
        self.export_msg = None;
        unsafe {
            if self.main_hidden {
                self.open_panel_from_companion();
            } else if self.island() {
                self.toggle_island();
            }
            self.fg_valid = false;
            self.frame(true);
        }
    }

    pub(super) fn refresh_history(&mut self) {
        let done = self.s.snap.insights.jobs_done;
        if self.hist_cache.as_ref().is_some_and(|(d, _)| *d == done) {
            return;
        }
        let jobs = self.s.engine.history_dir().map(|d| tachyon::history::recent(&d, 30)).unwrap_or_default();
        self.hist_cache = Some((done, jobs));
    }

    pub(super) fn export_blackbox(&mut self) {
        let now = tachyon::telemetry::now_ms();
        self.export_msg = Some(match self.s.engine.export(now, &self.s.snap.insights) {
            Ok((csv, json)) => {
                let name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                format!("Saved {} and {} in Tachyon\\exports.", name(&csv), name(&json))
            }
            Err(e) => format!("Export failed: {e}"),
        });
        self.fg_valid = false;
        unsafe { self.frame(true) };
    }

    /// Main view: what Tachyon worked out, between the render block and the metric rows.
    pub(super) fn draw_insights(&mut self, g: &mut G, s: &Snapshot, x0: f32, xr: f32, mut y: f32) -> f32 {
        let i = &s.insights;
        let w = xr - x0;
        let recent_report = i.report.as_ref().filter(|r| i.report_new || tachyon::telemetry::now_ms() - r.created_ms < 24 * 3600 * 1000);
        if let Some(r) = recent_report {
            let row = R::new(x0 - 8.0, y, w + 16.0, 30.0);
            g.stat(|g| g.fill_rrect(row, 15.0, AMBER, 0.16));
            let lr = self.link(g, "Open report ›", xr - 4.0, y + 19.5, Btn::Report);
            let head = g.fit(&r.headline, Face::SansSemi, 12.5, lr.x - x0 - 26.0);
            g.stat(|g| {
                g.dot(x0 + 3.0, y + 15.0, 3.5, AMBER, 1.0);
                g.text(&head, x0 + 13.0, y + 19.5, Face::SansSemi, 12.5, T_AMBER, H::L, V::Base)
            });
            y += 38.0;
        }
        if let Some(v) = &i.bottleneck {
            // live pass: the confidence word changes without moving anything
            let cap = format!("WHAT IS HOLDING IT BACK · {}", v.confidence_word().to_uppercase());
            g.cap(&cap, x0, y + 10.0, 9.5, H::L, V::Base);
            let why = if self.show_why { "Hide numbers" } else { "Why? ›" };
            self.link(g, why, xr, y + 10.5, Btn::Why);
            y += 18.0;
            let c = if v.kind.is_problem() { T_AMBER } else { TEXT };
            // At least two rows, so a sentence that rewraps does not move the static layer below.
            let rows = wrap_g(g, &v.sentence, Face::Sans, 12.5, w, 3);
            let n = rows.len().max(2);
            for (k, row) in rows.iter().enumerate() {
                g.text(row, x0, y + 13.0 + 17.0 * k as f32, Face::Sans, 12.5, c, H::L, V::Base);
            }
            y += 17.0 * n as f32;
            if self.show_why {
                let mut ev = v.evidence.clone();
                if let Some(p) = &v.process {
                    ev.insert(0, format!("Main process: {p}"));
                }
                for e in ev {
                    let e = g.fit(&e, Face::Sans, 11.0, w);
                    g.text(&e, x0, y + 12.0, Face::Sans, 11.0, TEXT2, H::L, V::Base);
                    y += 15.0;
                }
            }
            y += 6.0;
        }
        let vram_note = i.forecast.warning.clone().map(|t| (t, T_RED)).or_else(|| i.forecast.hog_sentence().map(|t| (t, TEXT2)));
        if let Some((t, c)) = vram_note {
            for row in wrap_g(g, &t, Face::SansSemi, 12.0, w, 2) {
                g.text(&row, x0, y + 13.0, Face::SansSemi, 12.0, c, H::L, V::Base);
                y += 16.0;
            }
            y += 6.0;
        }
        let job_line = match (&i.live_job, &i.last_job) {
            (Some(l), _) => Some((
                format!("Recording {}: {}", if l.kind == JobKind::Render { "job" } else { "GPU session" }, l.name),
                fmt_duration(l.elapsed_s),
            )),
            (None, Some(j)) => {
                Some((format!("{} finished in {}", j.name, fmt_duration(j.duration_s)), j.vs_last.clone().unwrap_or_default()))
            }
            _ => None,
        };
        if self.cfg.history {
            let lr = self.link(g, "History ›", xr, y + 13.0, Btn::History);
            if let Some((a, b)) = job_line {
                let a = g.fit(&a, Face::SansSemi, 12.0, lr.x - x0 - 10.0);
                let ar = g.text(&a, x0, y + 13.0, Face::SansSemi, 12.0, TEXT, H::L, V::Base);
                if !b.is_empty() {
                    let b = g.fit(&format!(" · {b}"), Face::Sans, 11.5, (lr.x - ar.r() - 10.0).max(0.0));
                    g.text(&b, ar.r(), y + 13.0, Face::Sans, 11.5, TEXT2, H::L, V::Base);
                }
            } else {
                g.stat(|g| g.text("Finished jobs are listed in History.", x0, y + 13.0, Face::Sans, 11.5, TEXT2, H::L, V::Base));
            }
            y += 24.0;
        }
        y
    }

    /// Header shared by the two extra views: "‹ Back", a title in the field and an action.
    pub(super) fn view_header(&mut self, g: &mut G, x0: f32, xr: f32, big: &str, caption: &str, action: Option<(&str, Btn)>) {
        let hov = self.hover == Some(Btn::Back);
        let br = g.text("‹ Back", x0, 62.0, Face::SansSemi, 12.0, if hov { TEXT } else { T_CYAN }, H::L, V::Base);
        self.hits.push((br.grow(5.0), Btn::Back));
        if let Some((label, b)) = action {
            self.link(g, label, xr, 62.0, b);
        }
        let hero_base = FIELD_H - 44.0;
        g.stat(|g| {
            g.text_t(big, x0 - 3.0, hero_base, Face::SansLight, 72.0, -72.0 * 0.03, TEXT, H::L, V::Base);
            g.cap(caption, x0, FIELD_H - 24.0, 10.0, H::L, V::Base);
        });
    }

    fn max_panel_h(&self) -> f32 {
        unsafe {
            let (_, t, _, b) = self.work_area_at(self.shape.left + 10, self.shape.top + 10);
            (b - t) as f32 / self.scale - 24.0
        }
    }

    pub(super) fn draw_report_view(&mut self, g: &mut G, s: &Snapshot, lw: f32) -> f32 {
        let (x0, xr) = (24.0, lw - 24.0);
        let w = xr - x0;
        let max_y = self.max_panel_h() - 30.0;
        let Some(r) = s.insights.report.clone() else {
            self.view_header(g, x0, xr, "All clear", "NO CRASH REPORT", Some(("Export recording ›", Btn::Export)));
            let mut y = FIELD_H + 10.0;
            let msg = "Tachyon writes a report here when your PC restarts unexpectedly or Tachyon stops without closing. It keeps the last minutes of readings in a small rolling file to explain what happened.";
            for row in wrap_g(g, msg, Face::Sans, 12.5, w, 4) {
                g.stat(|g| g.text(&row, x0, y + 13.0, Face::Sans, 12.5, TEXT2, H::L, V::Base));
                y += 18.0;
            }
            if let Some(n) = s.insights.note.clone() {
                for row in wrap_g(g, &n, Face::Sans, 12.0, w, 2) {
                    g.stat(|g| g.text(&row, x0, y + 13.0, Face::Sans, 12.0, T_AMBER, H::L, V::Base));
                    y += 17.0;
                }
            }
            return self.export_note(g, x0, w, y + 6.0);
        };
        let cap = match r.cause {
            Cause::Restart => format!("UNEXPECTED RESTART · {}", day(r.stopped_ms).to_uppercase()),
            Cause::AppClosed => format!("TACHYON STOPPED · {}", day(r.stopped_ms).to_uppercase()),
        };
        self.view_header(g, x0, xr, &hm(r.stopped_ms), &cap, Some(("Export ›", Btn::Export)));
        let mut y = FIELD_H + 6.0;
        g.stat(|g| {
            let mut y2 = y;
            for row in wrap_g(g, &r.headline, Face::SansSemi, 14.0, w, 2) {
                g.text(&row, x0, y2 + 14.0, Face::SansSemi, 14.0, TEXT, H::L, V::Base);
                y2 += 19.0;
            }
            for row in wrap_g(g, &r.just_before, Face::Sans, 12.5, w, 3) {
                g.text(&row, x0, y2 + 14.0, Face::Sans, 12.5, TEXT2, H::L, V::Base);
                y2 += 17.0;
            }
            y = y2 + 10.0;
        });
        if let Some(d) = &r.diagnosis {
            y = self.draw_diagnosis(g, d, x0, xr, y);
        }
        // peaks: two columns of label / value rows
        let p = &r.peaks_last_minute;
        let mut cells: Vec<(&str, String)> = vec![];
        if let Some(u) = p.gpu_util {
            cells.push(("GPU", format!("{u}%")));
        }
        if let Some(t) = p.temp {
            cells.push(("Temperature", format!("{t} °C")));
        }
        if let Some(pw) = p.power_w {
            cells.push(("Power", p.power_limit_w.map(|l| format!("{pw:.0} / {l:.0} W")).unwrap_or(format!("{pw:.0} W"))));
        }
        if let Some(v) = p.vram_mb {
            cells.push(("VRAM", p.vram_total_mb.map(|t| format!("{:.1} / {}", v / 1024.0, gb(t))).unwrap_or(gb(v))));
        }
        if let Some(c) = p.cpu {
            cells.push(("CPU", p.cpu_core_max.map(|m| format!("{c:.0}% · core {m:.0}%")).unwrap_or(format!("{c:.0}%"))));
        }
        if let Some(m) = p.ram_used_mb {
            cells.push(("RAM", p.ram_total_mb.map(|t| format!("{:.1} / {}", m / 1024.0, gb(t))).unwrap_or(gb(m))));
        }
        if !cells.is_empty() {
            g.stat(|g| g.cap("PEAKS IN THE LAST MINUTE", x0, y + 10.0, 9.5, H::L, V::Base));
            y += 16.0;
            let colw = (w - 26.0) / 2.0;
            for (k, (label, val)) in cells.iter().enumerate() {
                let cx = x0 + (k % 2) as f32 * (colw + 26.0);
                let ry = y + (k / 2) as f32 * 28.0;
                g.stat(|g| {
                    g.line(cx, ry + 27.5, cx + colw, ry + 27.5, (255, 255, 255), DIVIDER_A, 1.0);
                    g.text(label, cx, ry + 18.0, Face::Sans, 12.0, TEXT2, H::L, V::Base);
                    g.text(val, cx + colw, ry + 18.0, Face::SansSemi, 12.5, TEXT, H::Rt, V::Base);
                });
            }
            y += cells.len().div_ceil(2) as f32 * 28.0 + 12.0;
        }
        if let Some(h) = &r.vram_holder {
            let t = format!("Most VRAM: {} ({}), {}", h.name, h.category, gb(h.vram_mb));
            let t = g.fit(&t, Face::Sans, 12.0, w);
            g.stat(|g| g.text(&t, x0, y + 12.0, Face::Sans, 12.0, TEXT, H::L, V::Base));
            y += 22.0;
        }
        // timeline, newest last; events are highlighted
        g.stat(|g| g.cap("TIMELINE", x0, y + 10.0, 9.5, H::L, V::Base));
        y += 18.0;
        let event_times: Vec<i64> = r.events.iter().map(|e| e.event.t_ms).collect();
        let budget = (((max_y - y - 70.0) / 17.0).floor() as usize).max(4);
        let skip = r.timeline.len().saturating_sub(budget);
        if skip > 0 {
            g.stat(|g| g.text(&format!("{skip} earlier lines in the export"), x0, y + 12.0, Face::Sans, 11.0, TEXT2, H::L, V::Base));
            y += 16.0;
        }
        for l in r.timeline.iter().skip(skip) {
            let ev = event_times.contains(&l.t_ms) || l.text.starts_with("Windows started");
            let tm = hms(l.t_ms);
            let txt = g.fit(&l.text, Face::Sans, 11.5, w - 64.0);
            g.stat(|g| {
                g.text(&tm, x0, y + 12.0, Face::SansSemi, 11.5, if ev { T_AMBER } else { TEXT2 }, H::L, V::Base);
                g.text(&txt, x0 + 64.0, y + 12.0, Face::Sans, 11.5, if ev { T_AMBER } else { TEXT }, H::L, V::Base);
            });
            y += 17.0;
        }
        if r.events.is_empty() {
            let note = r.events_note.clone().unwrap_or_else(|| "No matching Windows events were logged.".into());
            for row in wrap_g(g, &note, Face::Sans, 11.5, w, 2) {
                g.stat(|g| g.text(&row, x0, y + 14.0, Face::Sans, 11.5, TEXT2, H::L, V::Base));
                y += 16.0;
            }
        }
        self.export_note(g, x0, w, y + 8.0)
    }

    /// The ranked probable causes: label, share and a bar each; the top one's advice; the
    /// evidence behind each and what looked normal behind "Evidence ›".
    fn draw_diagnosis(&mut self, g: &mut G, d: &tachyon::diagnose::Diagnosis, x0: f32, xr: f32, mut y: f32) -> f32 {
        let w = xr - x0;
        let cap = format!("PROBABLE CAUSE · {}", d.confidence.to_uppercase());
        g.stat(|g| g.cap(&cap, x0, y + 10.0, 9.5, H::L, V::Base));
        if !d.causes.is_empty() {
            let label = if self.show_why { "Hide evidence" } else { "Evidence ›" };
            self.link(g, label, xr, y + 10.5, Btn::Why);
        }
        y += 18.0;
        if d.causes.is_empty() {
            for row in wrap_g(g, &d.sentence, Face::Sans, 12.0, w, 2) {
                g.stat(|g| g.text(&row, x0, y + 13.0, Face::Sans, 12.0, TEXT2, H::L, V::Base));
                y += 17.0;
            }
            return y + 8.0;
        }
        for (k, c) in d.causes.iter().take(3).enumerate() {
            let (col, face) = if k == 0 { (T_AMBER, Face::SansSemi) } else { (TEXT, Face::Sans) };
            let pct = format!("{}%", c.percent);
            let lab = g.fit(&c.label, face, 12.5, w - 48.0);
            let frac = c.percent as f32 / 100.0;
            g.stat(|g| {
                g.text(&lab, x0, y + 13.0, face, 12.5, col, H::L, V::Base);
                g.text(&pct, xr, y + 13.0, Face::SansSemi, 12.5, col, H::Rt, V::Base);
                g.fill_rrect(R::new(x0, y + 19.0, w, 3.0), 1.5, (255, 255, 255), 0.12);
                g.fill_rrect(R::new(x0, y + 19.0, (w * frac).max(3.0), 3.0), 1.5, if k == 0 { AMBER } else { CYAN }, 0.85);
            });
            y += 27.0;
            if k == 0 {
                for row in wrap_g(g, &format!("Try: {}", c.advice), Face::Sans, 11.5, w, 3) {
                    g.stat(|g| g.text(&row, x0, y + 12.0, Face::Sans, 11.5, TEXT2, H::L, V::Base));
                    y += 16.0;
                }
                y += 4.0;
            }
            if self.show_why {
                for e in &c.evidence {
                    let e = g.fit(&format!("· {e}"), Face::Sans, 11.0, w);
                    g.stat(|g| g.text(&e, x0, y + 12.0, Face::Sans, 11.0, TEXT2, H::L, V::Base));
                    y += 15.0;
                }
                y += 4.0;
            }
        }
        if self.show_why {
            for n in d.normal.iter().chain(std::iter::once(&d.note)) {
                for row in wrap_g(g, n, Face::Sans, 11.0, w, 2) {
                    g.stat(|g| g.text(&row, x0, y + 12.0, Face::Sans, 11.0, T_CYAN, H::L, V::Base));
                    y += 15.0;
                }
            }
        }
        y + 8.0
    }

    fn export_note(&mut self, g: &mut G, x0: f32, w: f32, mut y: f32) -> f32 {
        if let Some(m) = self.export_msg.clone() {
            for row in wrap_g(g, &m, Face::Sans, 11.5, w, 2) {
                g.stat(|g| g.text(&row, x0, y + 12.0, Face::Sans, 11.5, T_CYAN, H::L, V::Base));
                y += 16.0;
            }
        }
        y
    }

    pub(super) fn draw_history_view(&mut self, g: &mut G, s: &Snapshot, lw: f32) -> f32 {
        let (x0, xr) = (24.0, lw - 24.0);
        let w = xr - x0;
        self.refresh_history();
        let jobs: Vec<JobRecord> = self.hist_cache.as_ref().map(|(_, j)| j.clone()).unwrap_or_default();
        let total_wh: f64 = jobs.iter().filter_map(|j| j.energy_wh).sum();
        let cap =
            if total_wh > 0.0 { format!("JOBS RECORDED · {:.2} KWH IN TOTAL", total_wh / 1000.0) } else { "JOBS RECORDED".to_string() };
        self.view_header(g, x0, xr, &jobs.len().to_string(), &cap, None);
        let mut y = FIELD_H + 6.0;
        if !self.cfg.history {
            g.stat(|g| g.text("History is off (history = false in tachyon.toml).", x0, y + 14.0, Face::Sans, 12.5, TEXT2, H::L, V::Base));
            return y + 24.0;
        }
        if let Some(l) = &s.insights.live_job {
            let t = format!("Recording now: {} · {}", l.name, fmt_duration(l.elapsed_s));
            let t = g.fit(&t, Face::SansSemi, 12.5, w);
            g.text(&t, x0, y + 14.0, Face::SansSemi, 12.5, T_CYAN, H::L, V::Base);
            y += 24.0;
        }
        if jobs.is_empty() {
            let msg = "Nothing yet. A render job, or a stretch of a minute or more with the GPU busy, is recorded here when it finishes, with its energy use and a comparison with the previous run.";
            for row in wrap_g(g, msg, Face::Sans, 12.5, w, 4) {
                g.stat(|g| g.text(&row, x0, y + 14.0, Face::Sans, 12.5, TEXT2, H::L, V::Base));
                y += 18.0;
            }
            return y;
        }
        let max_y = self.max_panel_h() - 30.0;
        let row_h = 62.0;
        let n = (((max_y - y) / (row_h + 7.0)).floor() as usize).clamp(1, jobs.len());
        for j in jobs.iter().rev().take(n) {
            let r = R::new(x0 - 8.0, y, w + 16.0, row_h);
            let right = format!("{} · {} {}", fmt_duration(j.duration_s), day(j.start_ms), hm(j.start_ms));
            let mut meta = vec![];
            if let Some(a) = j.avg_gpu {
                meta.push(format!("GPU {a:.0}%"));
            }
            if let Some(f) = j.avg_fps {
                meta.push(format!("{f:.1} fps"));
            }
            if let Some(e) = j.energy_wh {
                meta.push(format!("{e:.0} Wh"));
            }
            if let (Some(c), Some(cur)) = (j.cost, &j.currency) {
                meta.push(format!("{c:.2} {cur}"));
            }
            if let Some(t) = j.peak_temp {
                meta.push(format!("{t} °C"));
            }
            if let Some(v) = j.peak_vram_mb {
                meta.push(format!("VRAM {}", gb(v)));
            }
            if j.thermal_throttled_s >= 30.0 {
                meta.push(format!("heat limit {}", fmt_duration(j.thermal_throttled_s)));
            } else if j.power_throttled_s >= 30.0 {
                meta.push(format!("power cap {}", fmt_duration(j.power_throttled_s)));
            }
            let third =
                j.vs_last.clone().map(|v| (v, T_CYAN)).or_else(|| j.bottleneck.clone().map(|b| (format!("Main limit: {b}"), TEXT2)));
            g.stat(|g| {
                g.fill_rrect(r, 16.0, (255, 255, 255), ROW_A);
                let rr = g.text(&right, r.r() - 12.0, y + 20.0, Face::Sans, 11.5, TEXT2, H::Rt, V::Base);
                let name = g.fit(&j.name, Face::SansBold, 12.5, rr.x - x0 - 10.0);
                g.text(&name, x0 + 4.0, y + 20.0, Face::SansBold, 12.5, TEXT, H::L, V::Base);
                let m = g.fit(&meta.join(" · "), Face::Sans, 11.5, w - 8.0);
                g.text(&m, x0 + 4.0, y + 37.0, Face::Sans, 11.5, TEXT2, H::L, V::Base);
                if let Some((t, c)) = &third {
                    let t = g.fit(t, Face::SansSemi, 11.5, w - 8.0);
                    g.text(&t, x0 + 4.0, y + 54.0, Face::SansSemi, 11.5, *c, H::L, V::Base);
                }
            });
            y += row_h + 7.0;
        }
        if jobs.len() > n {
            g.stat(|g| {
                g.text(
                    &format!("{} older jobs in history\\jobs.jsonl", jobs.len() - n),
                    x0,
                    y + 10.0,
                    Face::Sans,
                    11.0,
                    TEXT2,
                    H::L,
                    V::Base,
                )
            });
            y += 16.0;
        }
        y - 7.0
    }
}
