//! Panel additions in 1.0: the insights block on the main view (bottleneck, VRAM forecast, crash
//! banner, last job) and the two extra views, the crash report and the job history.
use super::*;
use tachyon::history::{JobKind, JobRecord, fmt_duration};
use tachyon::preflight::Fit;
use tachyon::report::{Cause, Severity, gb};
use tachyon::status::Status;

/// Type scale: body, headings/verdicts/values, and the small-caps labels. Line heights and gaps.
pub(super) const FS: f32 = 12.5;
pub(super) const FS_H: f32 = 14.0;
pub(super) const FS_CAP: f32 = 9.5;
pub(super) const LH: f32 = 18.0;
pub(super) const LH_H: f32 = 20.0;
pub(super) const GAP: f32 = 18.0;
pub(super) const GAP_S: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum View {
    #[default]
    Main,
    Report,
    History,
    Job,
    Settings,
}

/// Word wrap with the real font; the last row is ellipsised when the text does not fit.
pub(super) fn wrap_g(g: &mut G, s: &str, face: Face, size: f32, maxw: f32, max_rows: usize) -> Vec<String> {
    let mut rows: Vec<String> = vec![];
    let mut cur = String::new();
    // a unit stays with its number ("57 W" never breaks between the two)
    let mut glued: Vec<String> = vec![];
    for w in s.split(' ').filter(|w| !w.is_empty()) {
        let unit = matches!(w.trim_end_matches([',', '.', ';', ')']), "W" | "GB" | "MB" | "Wh" | "%" | "°C" | "s" | "MHz" | "min" | "h");
        match glued.last_mut() {
            Some(prev) if unit && prev.chars().last().is_some_and(|c| c.is_ascii_digit()) => {
                prev.push(' ');
                prev.push_str(w);
            }
            _ => glued.push(w.to_string()),
        }
    }
    let words: Vec<&str> = glued.iter().map(String::as_str).collect();
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
        let r = g.text(label, xr, base, Face::SansSemi, FS, if hov { TEXT } else { T_CYAN }, H::Rt, V::Base);
        self.hits.push((r.grow(4.0), b));
        r
    }

    /// Open a panel view from anywhere (menu, bubble, links).
    pub(super) fn open_view(&mut self, v: View) {
        if v != self.view {
            // each view opens at its first level of detail
            self.show_others = false;
            self.show_why = false;
            self.show_earlier = false;
            self.show_note = false;
        }
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

    /// The marked-job reports, newest last (cached until another job finishes).
    pub(super) fn refresh_marked(&mut self) {
        let done = self.s.snap.insights.marked_done;
        if self.marked_cache.as_ref().is_some_and(|(d, _)| *d == done) {
            return;
        }
        let reps = self.s.engine.history_dir().map(|d| tachyon::markers::reports(&d, 10)).unwrap_or_default();
        self.marked_cache = Some((done, reps));
    }

    /// Main view: what Tachyon worked out, between the render block and the metric rows.
    pub(super) fn draw_insights(&mut self, g: &mut G, s: &Snapshot, x0: f32, xr: f32, mut y: f32) -> f32 {
        let i = &s.insights;
        let w = xr - x0;
        // a report worth a look: a restart, or Tachyon crashing; a plain close is not news
        let recent_report = i
            .report
            .as_ref()
            .filter(|r| i.report_new || tachyon::telemetry::now_ms() - r.created_ms < 24 * 3600 * 1000)
            .filter(|r| r.severity() != Severity::Note);
        if let Some(r) = recent_report {
            let (tc, ac) = status_colours(Status::from(r.severity()));
            let lw_ = g.measure("Open report ›", Face::SansSemi, FS).0;
            let rows = wrap_all(g, &r.headline, Face::SansSemi, FS, w - lw_ - 30.0);
            let h = rows.len() as f32 * LH + 12.0;
            let row = R::new(x0 - 8.0, y, w + 16.0, h);
            g.stat(|g| g.fill_rrect(row, 15.0, ac, 0.16));
            self.link(g, "Open report ›", xr - 4.0, y + 6.0 + 13.5, Btn::Report);
            g.stat(|g| {
                g.dot(x0 + 3.0, y + 6.0 + 9.0, 3.5, ac, 1.0);
                for (k, row) in rows.iter().enumerate() {
                    g.text(row, x0 + 13.0, y + 6.0 + 13.5 + LH * k as f32, Face::SansSemi, FS, tc, H::L, V::Base);
                }
            });
            y += h + GAP_S;
        }
        if let Some(v) = &i.bottleneck {
            // live pass: the confidence word changes without moving anything
            let cap = format!("WHAT IS HOLDING IT BACK · {}", v.confidence_word().to_uppercase());
            g.cap(&cap, x0, y + 10.0, FS_CAP, H::L, V::Base);
            let why = if self.show_why { "Hide numbers" } else { "Why? ›" };
            self.link(g, why, xr, y + 10.5, Btn::Why);
            y += 18.0;
            let c = status_colours(tachyon::status::of_bottleneck(v.kind.is_problem())).0;
            // At least two rows, so a sentence that rewraps does not move the static layer below.
            let rows = wrap_all(g, &v.sentence, Face::Sans, FS, w);
            let n = rows.len().max(2);
            for (k, row) in rows.iter().enumerate() {
                g.text(row, x0, y + 13.5 + LH * k as f32, Face::Sans, FS, c, H::L, V::Base);
            }
            y += LH * n as f32;
            if self.show_why {
                let mut ev = v.evidence.clone();
                if let Some(p) = &v.process {
                    ev.insert(0, format!("Main process: {p}"));
                }
                y += 2.0;
                y = bullets(g, &ev, x0, y, w, TEXT2, false);
            }
            y += GAP_S;
        }
        // the pre-render check: the verdict when it matters, a short line when the job should fit
        if let Some(p) = i.preflight.as_ref().filter(|p| p.fit != Fit::Unknown) {
            let (text, c) = match Status::from(p.fit) {
                Status::Fine => (p.short_line(), TEXT2),
                st => (p.sentence.clone(), status_colours(st).0),
            };
            y = para_live(g, &text, x0, y, w, Face::SansSemi, FS, LH, c);
            if let Some(h) = p.hog_note.as_ref().filter(|_| p.fit != Fit::Fits) {
                y = para_live(g, h, x0, y, w, Face::Sans, FS, LH, TEXT2);
            }
            y += GAP_S;
        }
        // an idle-hog note is already part of the check when it warns
        let pf_hogs = i.preflight.as_ref().is_some_and(|p| p.hog_note.is_some() && p.fit != Fit::Fits);
        let vram_note = i
            .forecast
            .warning
            .clone()
            .map(|t| (t, status_colours(Status::Act).0))
            .or_else(|| i.forecast.hog_sentence().filter(|_| !pf_hogs).map(|t| (t, TEXT2)));
        if let Some((t, c)) = vram_note {
            y = para_live(g, &t, x0, y, w, Face::SansSemi, FS, LH, c);
            y += GAP_S;
        }
        let job_line = match (&i.live_job, &i.last_job) {
            _ if !i.marked_open.is_empty() => {
                let m = &i.marked_open[i.marked_open.len() - 1];
                Some((format!("Recording marked job: {}", m.job), fmt_duration(m.running_s)))
            }
            (Some(l), _) => Some((
                format!("Recording {}: {}", if l.kind == JobKind::Render { "job" } else { "GPU session" }, l.name),
                fmt_duration(l.elapsed_s),
            )),
            (None, Some(j)) => {
                Some((format!("{} finished in {}", j.name, fmt_duration(j.duration_s)), j.vs_last.clone().unwrap_or_default()))
            }
            _ => None,
        };
        // links on the right: the marked-job report (when there is one) and the history
        self.refresh_marked();
        let has_marked = self.marked_cache.as_ref().is_some_and(|(_, r)| !r.is_empty());
        let mut lx = xr;
        if self.cfg.history {
            let lr = self.link(g, "History ›", lx, y + 13.5, Btn::History);
            lx = lr.x - 14.0;
        }
        if has_marked {
            let lr = self.link(g, "Job report ›", lx, y + 13.5, Btn::Job);
            lx = lr.x - 14.0;
        }
        let room = (lx - x0 - 4.0).max(60.0);
        if self.cfg.history || has_marked {
            match job_line {
                Some((a, b)) => {
                    let rows = wrap_all(g, &a, Face::SansSemi, FS, room);
                    for (k, row) in rows.iter().enumerate() {
                        g.text(row, x0, y + 13.5 + LH * k as f32, Face::SansSemi, FS, TEXT, H::L, V::Base);
                    }
                    y += LH * rows.len() as f32;
                    if !b.is_empty() {
                        y = para_live(g, &b, x0, y, room, Face::Sans, FS, LH, TEXT2);
                    }
                }
                None => {
                    let t = if self.cfg.history { "Finished jobs are listed in History." } else { "Marked jobs have a report." };
                    y = para(g, t, x0, y, room, Face::Sans, FS, LH, TEXT2);
                }
            }
            y += GAP_S;
        }
        y
    }

    /// Header shared by the extra views: "‹ Back", a title in the field and an action.
    pub(super) fn view_header(&mut self, g: &mut G, x0: f32, xr: f32, big: &str, caption: &str, action: Option<(&str, Btn)>) {
        let hov = self.hover == Some(Btn::Back);
        let br = g.text("‹ Back", x0, 62.0, Face::SansSemi, FS, if hov { TEXT } else { T_CYAN }, H::L, V::Base);
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

    /// A full-width quiet row that opens or closes a level of detail ("Show evidence ›").
    fn toggle_row(&mut self, g: &mut G, label: &str, open: bool, x0: f32, xr: f32, y: f32, b: Btn) -> f32 {
        let r = R::new(x0 - 8.0, y, xr - x0 + 16.0, 30.0);
        let hov = self.hover == Some(b);
        g.fill_rrect(r, 15.0, (255, 255, 255), if hov { 0.14 } else { ROW_A });
        g.text(label, x0 + 4.0, y + 19.5, Face::SansSemi, FS, if hov { TEXT } else { T_CYAN }, H::L, V::Base);
        g.text(if open { "Hide" } else { "Show" }, xr - 4.0, y + 19.5, Face::Sans, FS, TEXT2, H::Rt, V::Base);
        self.hits.push((r, b));
        y + 30.0
    }

    pub(super) fn draw_report_view(&mut self, g: &mut G, s: &Snapshot, lw: f32) -> f32 {
        let (x0, xr) = (24.0, lw - 24.0);
        let w = xr - x0;
        // the window adds its shadow pad around the panel: keep the whole window on the screen
        let max_y = self.max_panel_h() - 60.0;
        let Some(r) = s.insights.report.clone() else {
            self.view_header(g, x0, xr, "All clear", "NO CRASH REPORT", Some(("Export recording ›", Btn::Export)));
            let mut y = FIELD_H + 10.0;
            let msg = "Tachyon writes a report here when your PC restarts unexpectedly or Tachyon itself crashes. It keeps the last minutes of readings in a small rolling file to explain what happened.";
            y = para(g, msg, x0, y, w, Face::Sans, FS, LH, TEXT2);
            if let Some(n) = s.insights.note.clone() {
                y = para(g, &n, x0, y + GAP_S, w, Face::Sans, FS, LH, T_AMBER);
            }
            return self.export_note(g, x0, w, y + GAP_S);
        };
        let st = Status::from(r.severity());
        let tc = status_colours(st).0;
        let top = r.diagnosis.as_ref().and_then(|d| d.top()).map(|c| c.suspect);
        let what = match (r.cause, top) {
            (Cause::Restart, _) => "UNEXPECTED RESTART",
            (Cause::AppClosed, Some(tachyon::diagnose::Suspect::Application)) => "TACHYON CRASHED",
            (Cause::AppClosed, Some(tachyon::diagnose::Suspect::Closed) | None) => "TACHYON CLOSED",
            (Cause::AppClosed, _) => "TACHYON STOPPED",
        };
        let cap = format!("{what} · {}", day(r.stopped_ms).to_uppercase());
        self.view_header(g, x0, xr, &hm(r.stopped_ms), &cap, Some(("Export ›", Btn::Export)));
        let mut y = FIELD_H + 6.0;
        // 1. the verdict: the status (dot and words), what happened, is the PC OK, what to do
        y = status_tag(g, st, x0, y);
        y = para(g, &r.verdict(), x0 + 16.0, y, w - 16.0, Face::SansSemi, FS_H, LH_H, tc);
        y = para(g, &r.just_before, x0 + 16.0, y + 2.0, w - 16.0, Face::Sans, FS, LH, TEXT2);
        y += GAP;
        // 2. the top cause as a card with one action; the others folded
        if let Some(d) = &r.diagnosis {
            if self.show_why {
                // evidence open: the cause shrinks to one line to leave the room to the evidence
                if let Some(c) = d.top() {
                    let share = d.share(c);
                    let pw = g.measure(&share, Face::SansSemi, FS).0 + 12.0;
                    let rows = wrap_all(g, &c.label, Face::SansSemi, FS, w - pw);
                    g.stat(|g| {
                        for (k, row) in rows.iter().enumerate() {
                            g.text(row, x0, y + 13.5 + LH * k as f32, Face::SansSemi, FS, TEXT, H::L, V::Base);
                        }
                        g.text(&share, xr, y + 13.5, Face::SansSemi, FS, TEXT2, H::Rt, V::Base);
                    });
                    y += LH * rows.len() as f32 + GAP_S;
                }
            } else {
                y = self.draw_diagnosis(g, d, r.card_action(), st, x0, xr, y);
            }
        }
        // 3. the raw evidence, hidden until asked for
        y = self.toggle_row(g, "Evidence, peaks and timeline", self.show_why, x0, xr, y, Btn::Why);
        if self.show_why {
            y = self.draw_evidence(g, &r, x0, xr, y + GAP_S, max_y);
        }
        self.export_note(g, x0, w, y + GAP_S)
    }

    /// The top cause as a card (label, its share, plain words, one action); the others behind a
    /// row. Shares are percentages only when causes compete, otherwise a word ("certain").
    fn draw_diagnosis(
        &mut self,
        g: &mut G,
        d: &tachyon::diagnose::Diagnosis,
        action: Option<String>,
        st: Status,
        x0: f32,
        xr: f32,
        mut y: f32,
    ) -> f32 {
        let w = xr - x0;
        let Some(c) = d.top() else {
            y = para(g, &d.sentence, x0, y, w, Face::Sans, FS, LH, TEXT2);
            return y + GAP;
        };
        let closed = c.suspect == tachyon::diagnose::Suspect::Closed;
        let cap = if closed {
            "WHAT HAPPENED".to_string()
        } else {
            format!("{} CAUSE", if d.confidence == "likely" { "MOST LIKELY" } else { "POSSIBLE" })
        };
        g.stat(|g| g.cap(&cap, x0, y + 10.0, FS_CAP, H::L, V::Base));
        y += 18.0;
        let (tc, ac) = status_colours(st);
        let competing = d.competing();
        let pad = 14.0;
        let iw = w - 2.0 * pad + 16.0;
        let share = d.share(c);
        let pw = g.measure(&share, Face::SansSemi, FS_H).0 + 12.0;
        let label_rows = wrap_all(g, &c.label, Face::SansSemi, FS_H, iw - pw);
        let plain_rows = wrap_all(g, c.suspect.plain(), Face::Sans, FS, iw);
        let act_rows = action.map(|a| wrap_all(g, &a, Face::Sans, FS, iw)).unwrap_or_default();
        let bar_h = if competing { 10.0 } else { 4.0 };
        let act_h = if act_rows.is_empty() { 0.0 } else { 6.0 + act_rows.len() as f32 * LH };
        let h = pad + label_rows.len() as f32 * LH_H + bar_h + plain_rows.len() as f32 * LH + act_h + pad - 4.0;
        let card = R::new(x0 - 8.0, y, w + 16.0, h);
        let (lx, frac) = (x0 - 8.0 + pad, c.percent as f32 / 100.0);
        g.stat(|g| {
            g.fill_rrect(card, 18.0, (255, 255, 255), ROW_A);
            g.fill_rrect(R::new(card.x, card.y + 14.0, 3.0, card.h - 28.0), 1.5, ac, 0.9);
            let mut yy = y + pad;
            for (k, row) in label_rows.iter().enumerate() {
                g.text(row, lx, yy + 15.0, Face::SansSemi, FS_H, if st == Status::Fine { TEXT } else { tc }, H::L, V::Base);
                if k == 0 {
                    g.text(&share, card.r() - pad, yy + 15.0, Face::SansSemi, FS_H, TEXT2, H::Rt, V::Base);
                }
                yy += LH_H;
            }
            // a share bar only when causes compete
            if competing {
                g.fill_rrect(R::new(lx, yy + 2.0, iw, 3.0), 1.5, (255, 255, 255), 0.12);
                g.fill_rrect(R::new(lx, yy + 2.0, (iw * frac).max(3.0), 3.0), 1.5, ac, 0.85);
            }
            yy += bar_h;
            for row in &plain_rows {
                g.text(row, lx, yy + 13.5, Face::Sans, FS, TEXT2, H::L, V::Base);
                yy += LH;
            }
            if !act_rows.is_empty() {
                yy += 6.0;
                for row in &act_rows {
                    g.text(row, lx, yy + 13.5, Face::Sans, FS, TEXT, H::L, V::Base);
                    yy += LH;
                }
            }
        });
        y += h + GAP_S;
        let others: Vec<&tachyon::diagnose::Ranked> = d.causes.iter().skip(1).take(3).collect();
        if !others.is_empty() {
            let label = format!("Other possible causes ({})", others.len());
            y = self.toggle_row(g, &label, self.show_others, x0, xr, y, Btn::Others);
            if self.show_others {
                y += GAP_S;
                for o in others {
                    let share = d.share(o);
                    let pw = g.measure(&share, Face::SansSemi, FS).0 + 12.0;
                    let rows = wrap_all(g, &o.label, Face::SansSemi, FS, w - pw);
                    g.stat(|g| {
                        for (k, row) in rows.iter().enumerate() {
                            g.text(row, x0, y + 13.5 + LH * k as f32, Face::SansSemi, FS, TEXT, H::L, V::Base);
                        }
                        g.text(&share, xr, y + 13.5, Face::SansSemi, FS, TEXT2, H::Rt, V::Base);
                    });
                    y += LH * rows.len() as f32;
                    y = para(g, o.suspect.plain(), x0, y, w, Face::Sans, FS, LH, TEXT2);
                    y += GAP_S;
                }
            }
        }
        y + GAP_S
    }

    /// Behind "Evidence": the top cause's evidence, the notes, the peaks, the timeline up to the
    /// stop and, apart, what happened after it.
    fn draw_evidence(&mut self, g: &mut G, r: &tachyon::report::Report, x0: f32, xr: f32, mut y: f32, max_y: f32) -> f32 {
        let w = xr - x0;
        // what the estimates rest on: a small (i) on the first row, its text on demand
        if let Some(note) = r.diagnosis.as_ref().map(|d| d.note.clone()).filter(|n| !n.is_empty()) {
            self.info_icon(g, xr - 8.0, y + 7.0);
            if self.show_note {
                y = para(g, &note, x0, y, w - 28.0, Face::Sans, FS, LH, TEXT2);
                y += GAP_S;
            }
        }
        if let Some(d) = &r.diagnosis {
            let mut ev: Vec<String> = d.top().map(|c| c.evidence.clone()).unwrap_or_default();
            ev.extend(d.hints.iter().cloned());
            // "the PC kept running" is already the verdict of a plain close
            if d.top().is_none_or(|c| c.suspect != tachyon::diagnose::Suspect::Closed) {
                ev.extend(d.normal.iter().cloned());
            }
            if !ev.is_empty() {
                g.stat(|g| g.cap("WHAT TACHYON SAW", x0, y + 10.0, FS_CAP, H::L, V::Base));
                y += 18.0;
                // as many bullets as leave room for the timeline (at least one)
                let fit = (((max_y - y - 6.0 * LH) / (2.0 * LH)).floor().max(1.0)) as usize;
                let shown: Vec<String> = ev.iter().take(fit.min(3)).cloned().collect();
                y = bullets(g, &shown, x0, y, w, TEXT2, true);
                if ev.len() > shown.len() {
                    y = para(
                        g,
                        &format!("+{} more in the export", ev.len() - shown.len()),
                        x0 + 14.0,
                        y,
                        w - 14.0,
                        Face::Sans,
                        FS,
                        LH,
                        TEXT2,
                    );
                }
                y += GAP;
            }
        }
        // peaks: two columns; numbers right-aligned on one edge, units in their own column
        let p = &r.peaks_last_minute;
        let mut cells: Vec<(&str, String, String)> = vec![];
        if let Some(u) = p.gpu_util {
            cells.push(("GPU", u.to_string(), "%".into()));
        }
        if let Some(t) = p.temp {
            cells.push(("Temperature", t.to_string(), "°C".into()));
        }
        if let Some(pw) = p.power_w {
            cells.push(("Power", format!("{pw:.0}"), p.power_limit_w.map(|l| format!("/ {l:.0} W")).unwrap_or("W".into())));
        }
        if let Some(v) = p.vram_mb {
            cells.push((
                "VRAM",
                format!("{:.1}", v / 1024.0),
                p.vram_total_mb.map(|t| format!("/ {:.1} GB", t / 1024.0)).unwrap_or("GB".into()),
            ));
        }
        if let Some(c) = p.cpu {
            cells.push(("CPU", format!("{c:.0}"), "%".into()));
        }
        if let Some(m) = p.cpu_core_max {
            cells.push(("Busiest core", format!("{m:.0}"), "%".into()));
        }
        if let Some(m) = p.ram_used_mb {
            cells.push((
                "RAM",
                format!("{:.1}", m / 1024.0),
                p.ram_total_mb.map(|t| format!("/ {:.1} GB", t / 1024.0)).unwrap_or("GB".into()),
            ));
        }
        let grid_h = 16.0 + cells.len().div_ceil(2) as f32 * 30.0 + GAP;
        if !cells.is_empty() && y + grid_h + 4.0 * LH <= max_y {
            g.stat(|g| g.cap("PEAKS IN THE LAST MINUTE BEFORE THE STOP", x0, y + 10.0, FS_CAP, H::L, V::Base));
            y += 16.0;
            y = num_grid(g, &cells, x0, y, w);
            y += GAP;
        }
        // timeline up to the stop: times in one column, text wrapped beside them
        g.stat(|g| g.cap("TIMELINE UP TO THE STOP", x0, y + 10.0, FS_CAP, H::L, V::Base));
        y += 18.0;
        let event_times: Vec<i64> =
            r.events.iter().filter(|e| e.event.is_gpu() || e.event.bugcheck().is_some()).map(|e| e.event.t_ms).collect();
        let tcol = g.measure("00:00:00", Face::SansSemi, FS).0 + 14.0;
        // what must still fit under the timeline: the "after" block and the closing note
        let after_h = if r.after_events.is_empty() { 0.0 } else { GAP + 18.0 + (r.after_events.len().min(3) + 1) as f32 * (LH + 2.0) };
        let budget = (((max_y - y - after_h.min(2.0 * LH) - 2.0 * LH) / (2.0 * LH + 2.0)).floor().max(1.0)) as usize;
        let skip = r.timeline.len().saturating_sub(budget);
        if skip > 0 {
            y = para(
                g,
                &if skip == 1 {
                    "1 earlier line is in the export (Export ›).".to_string()
                } else {
                    format!("{skip} earlier lines are in the export (Export ›).")
                },
                x0,
                y,
                w,
                Face::Sans,
                FS,
                LH,
                TEXT2,
            );
        }
        for l in r.timeline.iter().skip(skip) {
            let ev = event_times.contains(&l.t_ms);
            let tm = hms(l.t_ms);
            let rows = wrap_all(g, &l.text, Face::Sans, FS, w - tcol);
            g.stat(|g| {
                g.text(&tm, x0, y + 13.5, Face::SansSemi, FS, if ev { T_AMBER } else { TEXT2 }, H::L, V::Base);
                for (k, row) in rows.iter().enumerate() {
                    g.text(row, x0 + tcol, y + 13.5 + LH * k as f32, Face::Sans, FS, if ev { T_AMBER } else { TEXT }, H::L, V::Base);
                }
            });
            y += LH * rows.len() as f32 + 2.0;
        }
        if r.events.is_empty() && y + LH <= max_y {
            let note = r.events_note.clone().unwrap_or_else(|| "Windows logged nothing relevant before the stop.".into());
            y = para(g, &note, x0, y + 2.0, w, Face::Sans, FS, LH, TEXT2);
        }
        if !r.after_events.is_empty() && y + after_h > max_y {
            let t = format!("{} unrelated events after the stop are in the export.", r.after_events.len());
            y = para(g, &t, x0, y + GAP_S, w, Face::Sans, FS, LH, TEXT2);
        } else if !r.after_events.is_empty() {
            y += GAP;
            g.stat(|g| g.cap("AFTER THE STOP · NOT RELATED", x0, y + 10.0, FS_CAP, H::L, V::Base));
            y += 18.0;
            for e in r.after_events.iter().take(3) {
                let rows = wrap_all(g, &e.text, Face::Sans, FS, w - tcol);
                let tm = hms(e.event.t_ms);
                g.stat(|g| {
                    g.text(&tm, x0, y + 13.5, Face::SansSemi, FS, TEXT2, H::L, V::Base);
                    for (k, row) in rows.iter().enumerate() {
                        g.text(row, x0 + tcol, y + 13.5 + LH * k as f32, Face::Sans, FS, TEXT2, H::L, V::Base);
                    }
                });
                y += LH * rows.len() as f32 + 2.0;
            }
            if r.after_events.len() > 3 {
                y = para(g, &format!("{} more in the export.", r.after_events.len() - 3), x0, y, w, Face::Sans, FS, LH, TEXT2);
            }
            y = para(g, "Logged after Tachyon stopped, so not used as evidence.", x0, y + 2.0, w, Face::Sans, FS, LH, TEXT2);
        }
        y
    }

    /// The small (i) icon: opens or closes a note (`show_note`).
    fn info_icon(&mut self, g: &mut G, cx: f32, cy: f32) {
        let hov = self.hover == Some(Btn::Info) || self.show_note;
        g.stat(|g| {
            g.dot(cx, cy, 8.0, (255, 255, 255), if hov { 0.22 } else { 0.12 });
            g.text("i", cx, cy + 4.5, Face::SansBold, 11.0, if hov { TEXT } else { T_CYAN }, H::C, V::Base);
        });
        self.hits.push((R::new(cx - 12.0, cy - 12.0, 24.0, 24.0), Btn::Info));
    }

    fn export_note(&mut self, g: &mut G, x0: f32, w: f32, y: f32) -> f32 {
        match self.export_msg.clone() {
            Some(m) => para(g, &m, x0, y, w, Face::Sans, FS, LH, T_CYAN),
            None => y,
        }
    }

    /// The last marked job (`tachyon mark`): a verdict, the numbers, the PCIe link and who held
    /// the VRAM; the clock-limit reasons and earlier jobs behind rows.
    pub(super) fn draw_job_view(&mut self, g: &mut G, _s: &Snapshot, lw: f32) -> f32 {
        let (x0, xr) = (24.0, lw - 24.0);
        let w = xr - x0;
        self.refresh_marked();
        let reps = self.marked_cache.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
        let Some(j) = reps.last().cloned() else {
            self.view_header(g, x0, xr, "No jobs", "MARKED JOBS", None);
            let msg = "Wrap a render or a script with  tachyon mark start --job NAME  and  tachyon mark end --job NAME  and its report appears here: how busy the GPU was, VRAM, power, energy, heat and the PCIe link.";
            let y = para(g, msg, x0, FIELD_H + 10.0, w, Face::Sans, FS, LH, TEXT2);
            return y;
        };
        let big = fmt_duration(j.duration_s);
        let cap = format!("MARKED JOB · {} · {}–{}", day(j.start_ms).to_uppercase(), hm(j.start_ms), hm(j.end_ms));
        self.view_header(g, x0, xr, &big, &cap, Some(("History ›", Btn::History)));
        let mut y = FIELD_H + 6.0;
        let (verdict, tone) = j.verdict();
        let st = Status::from(tone);
        y = para(g, &j.job, x0, y, w, Face::SansSemi, FS_H, LH_H, TEXT);
        y = status_tag(g, st, x0, y + 4.0);
        y = para(g, &verdict, x0 + 16.0, y, w - 16.0, Face::Sans, FS, LH, status_colours(st).0);
        y += GAP;
        // against the previous comparable job: the same name first, else the last marked job
        let cmp = j.compare_in(&reps);
        let cap = match &cmp {
            Some(c) if c.same_job => format!("VS THE LAST RUN · {} {}", day(c.prev_start_ms).to_uppercase(), hm(c.prev_start_ms)),
            Some(c) => format!("VS THE LAST MARKED JOB · {}", c.prev_job.to_uppercase()),
            None => "VS LAST TIME".to_string(),
        };
        let cap_rows = wrap_all(g, &cap, Face::SansBold, FS_CAP, w);
        for row in &cap_rows {
            g.stat(|g| g.cap(row, x0, y + 10.0, FS_CAP, H::L, V::Base));
            y += 16.0;
        }
        y += 2.0;
        y = match &cmp {
            Some(c) => para(g, &c.sentence(), x0, y, w, Face::Sans, FS, LH, TEXT),
            None => para(g, tachyon::markers::FIRST_RUN, x0, y, w, Face::Sans, FS, LH, TEXT2),
        };
        y += GAP;
        // the numbers, aligned; limits as a share of the job, in plain words
        let f1 = |v: Option<f32>| v.map(|v| format!("{v:.0}")).unwrap_or("—".into());
        let gb1 = |v: Option<f32>| v.map(|v| format!("{:.1}", v / 1024.0)).unwrap_or("—".into());
        let pct = |v: f64| tachyon::markers::share(v, j.duration_s).trim_end_matches('%').to_string();
        let cells: Vec<(&str, String, String)> = vec![
            ("GPU average", f1(j.gpu_util_avg), "%".into()),
            ("GPU peak", j.gpu_util_peak.map(|v| v.to_string()).unwrap_or("—".into()), "%".into()),
            ("Power average", f1(j.power_avg_w), "W".into()),
            ("Power peak", f1(j.power_peak_w), j.power_limit_w.map(|l| format!("/ {l:.0} W")).unwrap_or("W".into())),
            ("VRAM peak", gb1(j.vram_peak_mb), j.vram_total_mb.map(|t| format!("/ {:.1} GB", t / 1024.0)).unwrap_or("GB".into())),
            ("Energy", j.energy_wh.map(|e| format!("{e:.1}")).unwrap_or("—".into()), "Wh".into()),
            ("Temp. average", f1(j.temp_avg_c), "°C".into()),
            ("Temp. peak", j.temp_peak_c.map(|v| v.to_string()).unwrap_or("—".into()), "°C".into()),
            ("At power limit", pct(j.power_limited_s), "%".into()),
            ("Slowed by heat", pct(j.thermal_limited_s), "%".into()),
        ];
        g.stat(|g| g.cap("THE NUMBERS", x0, y + 10.0, FS_CAP, H::L, V::Base));
        y += 16.0;
        y = num_grid(g, &cells, x0, y, w);
        y = para(g, "Limits: share of the job's time the clocks were held back.", x0, y + 4.0, w, Face::Sans, FS, LH, TEXT2);
        y += GAP;
        // the PCIe link
        let degraded = j.pcie["degraded_events"].as_u64().unwrap_or(0) > 0;
        g.stat(|g| g.cap("PCIE LINK (GPU TO PC)", x0, y + 10.0, FS_CAP, H::L, V::Base));
        y += 18.0;
        y = para(g, &j.pcie_sentence(), x0, y, w, Face::Sans, FS, LH, status_colours(tachyon::status::of_pcie(degraded)).0);
        y += GAP;
        // who held the VRAM: three rows with a bar, the rest folded
        if !j.top_vram_processes.is_empty() {
            g.stat(|g| g.cap("VRAM BY PROCESS (PEAK)", x0, y + 10.0, FS_CAP, H::L, V::Base));
            y += 18.0;
            let maxv = j.top_vram_processes.iter().map(|p| p.1).fold(1.0f32, f32::max);
            let room = ((self.max_panel_h() - 60.0 - y - 130.0) / (LH + 4.0)).floor().max(3.0) as usize;
            let n = if self.show_others { j.top_vram_processes.len().min(room) } else { 3.min(j.top_vram_processes.len()) };
            let vw = g.measure("00.0 GB", Face::SansSemi, FS).0;
            for (name, mb) in j.top_vram_processes.iter().take(n) {
                let val = if *mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") };
                let nw = w - vw - 120.0;
                let name_rows = wrap_all(g, name, Face::Sans, FS, nw);
                let bx = x0 + nw + 10.0;
                g.stat(|g| {
                    for (k, row) in name_rows.iter().enumerate() {
                        g.text(row, x0, y + 13.5 + LH * k as f32, Face::Sans, FS, TEXT, H::L, V::Base);
                    }
                    g.fill_rrect(R::new(bx, y + 8.0, 100.0, 4.0), 2.0, (255, 255, 255), 0.12);
                    g.fill_rrect(R::new(bx, y + 8.0, (100.0 * mb / maxv).max(3.0), 4.0), 2.0, CYAN, 0.85);
                    g.text(&val, xr, y + 13.5, Face::SansSemi, FS, TEXT, H::Rt, V::Base);
                });
                y += LH * name_rows.len() as f32 + 4.0;
            }
            if j.top_vram_processes.len() > 3 {
                let label = if self.show_others {
                    "Fewer processes".to_string()
                } else {
                    format!("{} more processes", j.top_vram_processes.len() - 3)
                };
                y = self.toggle_row(g, &label, self.show_others, x0, xr, y + 4.0, Btn::Others);
            }
            y += GAP;
        }
        // two folds: why the clocks were held back, and the earlier marked jobs
        if !j.clock_limit_reasons.is_empty() {
            y = self.toggle_row(g, "What held the clocks back", self.show_why, x0, xr, y, Btn::Why);
            if self.show_why {
                y += GAP_S;
                let glossed: Vec<String> = j.clock_limit_reasons.iter().map(|r| tachyon::markers::reason_gloss(r)).collect();
                y = bullets(g, &glossed, x0, y, w, TEXT2, true);
            }
            y += GAP_S;
        }
        let earlier: Vec<String> = reps
            .iter()
            .rev()
            .skip(1)
            .map(|r| {
                let e = r.energy_wh.map(|e| format!(" · {e:.1} Wh")).unwrap_or_default();
                format!("{} · {} {} · {}{e}", r.job, day(r.start_ms), hm(r.start_ms), fmt_duration(r.duration_s))
            })
            .collect();
        if !earlier.is_empty() {
            y = self.toggle_row(g, &format!("Earlier marked jobs ({})", earlier.len()), self.show_earlier, x0, xr, y, Btn::Earlier);
            if self.show_earlier {
                y += GAP_S;
                y = bullets(g, &earlier, x0, y, w, TEXT2, true);
            }
        }
        y
    }

    pub(super) fn draw_history_view(&mut self, g: &mut G, s: &Snapshot, lw: f32) -> f32 {
        let (x0, xr) = (24.0, lw - 24.0);
        let w = xr - x0;
        self.refresh_history();
        self.refresh_marked();
        // the marked jobs are listed too, instead of the automatic job recorded for the same work
        let marked = self.marked_cache.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
        let jobs: Vec<JobRecord> =
            tachyon::markers::merge_history(self.hist_cache.as_ref().map(|(_, j)| j.clone()).unwrap_or_default(), &marked);
        let total_wh: f64 = jobs.iter().filter_map(|j| j.energy_wh).sum();
        let cap =
            if total_wh > 0.0 { format!("JOBS RECORDED · {:.2} KWH IN TOTAL", total_wh / 1000.0) } else { "JOBS RECORDED".to_string() };
        let has_marked = self.marked_cache.as_ref().is_some_and(|(_, r)| !r.is_empty());
        self.view_header(g, x0, xr, &jobs.len().to_string(), &cap, has_marked.then_some(("Job report ›", Btn::Job)));
        let mut y = FIELD_H + 6.0;
        if !self.cfg.history {
            return para(g, "History is off (history = false in tachyon.toml).", x0, y, w, Face::Sans, FS, LH, TEXT2);
        }
        if let Some(l) = &s.insights.live_job {
            let t = format!("Recording now: {} · {}", l.name, fmt_duration(l.elapsed_s));
            y = para_live(g, &t, x0, y, w, Face::SansSemi, FS, LH, T_CYAN);
            y += GAP_S;
        }
        if jobs.is_empty() {
            let msg = "Nothing yet. A render job, or a stretch of a minute or more with the GPU busy, is recorded here when it finishes, with its energy use and a comparison with the previous run.";
            return para(g, msg, x0, y, w, Face::Sans, FS, LH, TEXT2);
        }
        let max_y = self.max_panel_h() - 30.0;
        let mut shown = 0usize;
        for j in jobs.iter().rev() {
            let third_rows = j.vs_last.as_ref().map(|v| wrap_g(g, v, Face::Sans, FS, w - 8.0, 2).len()).unwrap_or(1).max(1);
            let right = format!("{} · {} {}", fmt_duration(j.duration_s), day(j.start_ms), hm(j.start_ms));
            // a long name wraps beside the time (at most two rows), never cut short mid-word
            let rw = g.measure(&right, Face::Sans, FS).0;
            let name_rows = wrap_g(g, &j.name, Face::SansSemi, FS, w - rw - 22.0, 2);
            let extra = 18.0 * (name_rows.len().max(1) - 1) as f32;
            let row_h = 48.0 + 18.0 * third_rows as f32 + extra;
            if shown > 0 && y + row_h > max_y {
                break;
            }
            shown += 1;
            let r = R::new(x0 - 8.0, y, w + 16.0, row_h);
            let mut meta = vec![];
            if j.key.starts_with(tachyon::markers::MARKED_KEY) {
                meta.push("marked job".to_string());
            }
            if let Some(a) = j.avg_gpu {
                meta.push(format!("GPU {a:.0}%"));
            }
            if let Some(e) = j.energy_wh {
                meta.push(format!("{e:.0} Wh"));
            }
            if let Some(v) = j.peak_vram_mb {
                meta.push(format!("VRAM {}", gb(v)));
            }
            if let Some(t) = j.peak_temp {
                meta.push(format!("{t} °C"));
            }
            if let Some(f) = j.avg_fps {
                meta.push(format!("{f:.1} fps"));
            }
            if let (Some(c), Some(cur)) = (j.cost, &j.currency) {
                meta.push(format!("{c:.2} {cur}"));
            }
            if j.thermal_throttled_s >= 30.0 {
                meta.push(format!("slowed by heat {}", tachyon::markers::share(j.thermal_throttled_s, j.duration_s)));
            } else if j.power_throttled_s >= 30.0 {
                meta.push(format!("at power limit {}", tachyon::markers::share(j.power_throttled_s, j.duration_s)));
            }
            let third =
                j.vs_last.clone().map(|v| (v, T_CYAN)).or_else(|| j.bottleneck.clone().map(|b| (format!("Main limit: {b}"), TEXT2)));
            g.stat(|g| {
                g.fill_rrect(r, 16.0, (255, 255, 255), ROW_A);
                g.text(&right, r.r() - 12.0, y + 21.0, Face::Sans, FS, TEXT2, H::Rt, V::Base);
                for (k, row) in name_rows.iter().enumerate() {
                    g.text(row, x0 + 4.0, y + 21.0 + 18.0 * k as f32, Face::SansSemi, FS, TEXT, H::L, V::Base);
                }
                let y = y + extra;
                // whole items only: the facts that fit, most useful first
                let m = fit_items(g, &meta, Face::Sans, FS, w - 8.0);
                g.text(&m, x0 + 4.0, y + 39.0, Face::Sans, FS, TEXT2, H::L, V::Base);
                if let Some((t, c)) = &third {
                    for (k, row) in wrap_g(g, t, Face::Sans, FS, w - 8.0, 2).iter().enumerate() {
                        g.text(row, x0 + 4.0, y + 57.0 + 18.0 * k as f32, Face::Sans, FS, *c, H::L, V::Base);
                    }
                }
            });
            y += row_h + 8.0;
        }
        let n = shown;
        if jobs.len() > n {
            y = para(g, &format!("{} older jobs in history\\jobs.jsonl", jobs.len() - n), x0, y, w, Face::Sans, FS, LH, TEXT2);
            y += 8.0;
        }
        y - 8.0
    }
}

/// The one colour mapping for a status: (text, accent). All fine keeps white text with a green
/// dot or accent; worth a look is amber; act now is red. The words always go with the colour.
pub(super) fn status_colours(s: Status) -> (Rgb, Rgb) {
    match s {
        Status::Fine => (TEXT, GREEN),
        Status::Look => (T_AMBER, AMBER),
        Status::Act => (T_RED, RED),
    }
}

/// The status as a coloured dot and its words ("ALL FINE"); returns the y under it.
pub(super) fn status_tag(g: &mut G, st: Status, x0: f32, y: f32) -> f32 {
    let ac = status_colours(st).1;
    g.stat(|g| {
        g.dot(x0 + 4.0, y + 7.0, 4.0, ac, 1.0);
        g.cap(&st.word().to_uppercase(), x0 + 16.0, y + 10.5, FS_CAP, H::L, V::Base);
    });
    y + 18.0
}

/// All rows of a wrapped text: never cut.
pub(super) fn wrap_all(g: &mut G, s: &str, face: Face, size: f32, maxw: f32) -> Vec<String> {
    wrap_g(g, s, face, size, maxw, usize::MAX)
}

/// A wrapped paragraph in the static layer; returns the y under it.
pub(super) fn para(g: &mut G, s: &str, x: f32, mut y: f32, w: f32, face: Face, size: f32, lh: f32, c: Rgb) -> f32 {
    let base = lh - (lh - size) / 2.0 - 2.0;
    for row in wrap_all(g, s, face, size, w) {
        g.stat(|g| g.text(&row, x, y + base, face, size, c, H::L, V::Base));
        y += lh;
    }
    y
}

/// The same for text that changes every second (live layer).
pub(super) fn para_live(g: &mut G, s: &str, x: f32, mut y: f32, w: f32, face: Face, size: f32, lh: f32, c: Rgb) -> f32 {
    let base = lh - (lh - size) / 2.0 - 2.0;
    for row in wrap_all(g, s, face, size, w) {
        g.text(&row, x, y + base, face, size, c, H::L, V::Base);
        y += lh;
    }
    y
}

/// At most three bullets, each wrapped with a hanging indent; "+N more" when there are more.
pub(super) fn bullets(g: &mut G, items: &[String], x: f32, mut y: f32, w: f32, c: Rgb, stat: bool) -> f32 {
    for it in items.iter().take(3) {
        let rows = wrap_all(g, it, Face::Sans, FS, w - 14.0);
        let draw = |g: &mut G| {
            g.dot(x + 3.0, y + 9.0, 2.0, c, 0.9);
            for (k, row) in rows.iter().enumerate() {
                g.text(row, x + 14.0, y + 13.5 + LH * k as f32, Face::Sans, FS, c, H::L, V::Base);
            }
        };
        if stat {
            g.stat(draw)
        } else {
            draw(g)
        }
        y += LH * rows.len() as f32 + 2.0;
    }
    if items.len() > 3 {
        let t = format!("+{} more in the export", items.len() - 3);
        y = if stat {
            para(g, &t, x + 14.0, y, w - 14.0, Face::Sans, FS, LH, TEXT2)
        } else {
            para_live(g, &t, x + 14.0, y, w - 14.0, Face::Sans, FS, LH, TEXT2)
        };
    }
    y
}

/// Two columns of label / number / unit: the numbers end on one edge, the units start on one
/// edge, so values and units line up down each column.
pub(super) fn num_grid(g: &mut G, cells: &[(&str, String, String)], x0: f32, mut y: f32, w: f32) -> f32 {
    let gap = 26.0;
    let colw = (w - gap) / 2.0;
    let uw = cells.iter().map(|(_, _, u)| g.measure(u, Face::Sans, FS).0).fold(0.0f32, f32::max);
    for (k, (label, val, unit)) in cells.iter().enumerate() {
        let cx = x0 + (k % 2) as f32 * (colw + gap);
        let ry = y + (k / 2) as f32 * 30.0;
        let num_r = cx + colw - uw - 5.0;
        g.stat(|g| {
            g.line(cx, ry + 29.5, cx + colw, ry + 29.5, (255, 255, 255), DIVIDER_A, 1.0);
            g.text(label, cx, ry + 20.0, Face::Sans, FS, TEXT2, H::L, V::Base);
            g.text(val, num_r, ry + 20.0, Face::SansSemi, FS_H, TEXT, H::Rt, V::Base);
            g.text(unit, num_r + 5.0, ry + 20.0, Face::Sans, FS, TEXT2, H::L, V::Base);
        });
    }
    y += cells.len().div_ceil(2) as f32 * 30.0;
    y
}

/// Join whole items with " · " while they fit.
pub(super) fn fit_items(g: &mut G, items: &[String], face: Face, size: f32, maxw: f32) -> String {
    let mut out = String::new();
    for it in items {
        let cand = if out.is_empty() { it.clone() } else { format!("{out} · {it}") };
        if g.measure(&cand, face, size).0 > maxw {
            break;
        }
        out = cand;
    }
    out
}
