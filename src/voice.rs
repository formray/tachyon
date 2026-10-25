//! What Hex says in its speech bubble: up to 3 short lines (render, then temperature/limits,
//! then VRAM). Minimal, precise, a little dry: numbers first, no exclamation marks.
//! Host-independent so the wording is unit-tested; the window maps tones to faces/colours.
use crate::config::{Config, Level};
use crate::monitor::Snapshot;
use crate::nvml::throttle_words;
use crate::parse;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// spoken part (semibold, white)
    Plain,
    /// quiet part (regular, dimmer)
    Quiet,
    /// a job or process name
    Name,
    /// warm (amber)
    Warm,
    /// too hot / tight (red)
    Hot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub words: Vec<(String, Tone)>,
    /// the render line (a progress track follows it)
    pub render: bool,
}

fn gb(mb: f64) -> String {
    format!("{:.1}", mb / 1024.0)
}

impl Line {
    /// The whole line as plain text.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.0.as_str()).collect()
    }
}

pub fn lines(snap: &Snapshot, cfg: &Config, now: chrono::DateTime<chrono::Local>) -> Vec<Line> {
    let mut out = vec![];
    let w = |s: &str, t: Tone| (s.to_string(), t);
    if let Some(rv) = &snap.render {
        let mut tail = String::new();
        if let Some(p) = &rv.progress {
            if let Some(t) = p.total {
                tail += &format!(" · {:.1}%", p.frame as f64 * 100.0 / t.max(1) as f64);
                if let Some(rem) = rv.fps.and_then(|f| parse::eta_secs(p.frame, t, f)) {
                    let at = now + chrono::Duration::seconds(rem as i64);
                    tail += &format!(" · ETA {}", at.format("%H:%M"));
                }
            } else {
                tail += &format!(" · frame {}", p.frame);
            }
        }
        out.push(Line { words: vec![w(&rv.job, Tone::Name), w(&format!("{tail}."), Tone::Plain)], render: true });
    }
    let Some(g) = &snap.sample else { return out };
    let mut v = vec![];
    let util = g.util_pct.unwrap_or(0);
    let power_cap = g.throttle.is_some_and(|t| throttle_words(t).iter().any(|x| x.starts_with("power")));
    match g.temp_c.map(|t| t as f32) {
        Some(t) if t >= cfg.gpu.temp_red_c => {
            v.push(w(&format!("GPU {util}%. "), Tone::Plain));
            v.push(w(&format!("{t:.0} °C. Too warm"), Tone::Hot));
            match g.fan_pct {
                Some(f) => v.push(w(&format!("; fan at {f}%."), Tone::Quiet)),
                None => v.push(w(".", Tone::Quiet)),
            }
        }
        Some(t) if t >= cfg.gpu.temp_amber_c => {
            v.push(w(&format!("GPU {util}%. "), Tone::Plain));
            v.push(w(&format!("{t:.0} °C, a bit warm."), Tone::Warm));
            if power_cap {
                v.push(w(" Power cap active.", Tone::Quiet));
            }
        }
        _ if bottleneck_line(snap).is_some() => v = bottleneck_line(snap).unwrap_or_default(),
        t => {
            let deg = t.map(|t| format!(" · {t:.0} °C")).unwrap_or_default();
            if util < 5 {
                v.push(w(&format!("GPU idle{deg}. "), Tone::Plain));
                v.push(w("Nothing to report.", Tone::Quiet));
            } else {
                v.push(w(&format!("GPU {util}%{deg}. "), Tone::Plain));
                v.push(w(if power_cap { "Power cap active." } else { "All good." }, Tone::Quiet));
            }
        }
    }
    out.push(Line { words: v, render: false });
    let ins = &snap.insights;
    // VRAM: a confident "full soon" warning first, then a tight card (with an idle hog if any)
    if let Some(wn) = &ins.forecast.warning {
        out.push(Line { words: vec![w(wn, Tone::Hot)], render: false });
        out.truncate(3);
        return out;
    }
    if let (Some(u), Some(tot)) = (g.vram_used_mb, g.vram_total_mb) {
        let lvl = cfg.gpu.vram_level(u as f32, tot as f32);
        if lvl != Level::Ok {
            let tone = if lvl == Level::Red { Tone::Hot } else { Tone::Warm };
            let tail = ins
                .forecast
                .hogs
                .first()
                .map(|h| format!("{} holds {:.1} GB without using it.", h.name, h.vram_mb / 1024.0))
                .unwrap_or("Tight.".into());
            out.push(Line {
                words: vec![w(&format!("VRAM {} of {:.0} GB. ", gb(u), tot / 1024.0), tone), w(&tail, Tone::Quiet)],
                render: false,
            });
            out.truncate(3);
            return out;
        }
    }
    // a job that finished in the last 15 minutes
    if let Some(j) = ins.last_job.as_ref().filter(|j| now.timestamp_millis() - j.end_ms < 15 * 60_000) {
        let mut words =
            vec![w(&j.name, Tone::Name), w(&format!(" finished in {}. ", crate::history::fmt_duration(j.duration_s)), Tone::Plain)];
        if let Some(vs) = &j.vs_last {
            words.push(w(vs, Tone::Quiet));
        }
        out.push(Line { words, render: false });
        out.truncate(3);
        return out;
    }
    let top = snap.procs.as_ref().and_then(|v| {
        v.iter()
            .filter(|p| p.info.vram_mb.is_some())
            .max_by(|a, b| a.info.vram_mb.partial_cmp(&b.info.vram_mb).unwrap_or(std::cmp::Ordering::Equal))
    });
    if let Some((p, mb)) = top.and_then(|p| p.info.vram_mb.map(|mb| (p, mb))) {
        let name = p.info.name.trim_end_matches(".exe").trim_end_matches(".EXE");
        out.push(Line { words: vec![w(name, Tone::Name), w(&format!(": {} GB VRAM.", gb(mb)), Tone::Plain)], render: false });
    }
    out.truncate(3);
    out
}

/// The bottleneck in one line, when it is a problem worth saying unasked (likely or sure).
fn bottleneck_line(snap: &Snapshot) -> Option<Vec<(String, Tone)>> {
    let v = snap.insights.bottleneck.as_ref().filter(|v| v.kind.is_problem() && v.confidence >= 0.5)?;
    let tone = if matches!(v.kind, crate::explain::Kind::Vram | crate::explain::Kind::Thermal) { Tone::Warm } else { Tone::Plain };
    Some(vec![(first_sentence(&v.sentence).to_string(), tone), (format!(" ({})", v.confidence_word()), Tone::Quiet)])
}

/// Up to and including the first full stop that ends a sentence.
pub fn first_sentence(s: &str) -> &str {
    match s.find(". ") {
        Some(i) => &s[..=i],
        None => s,
    }
}

/// Hex's answer to "Why is it slow?": the verdict and the numbers behind it.
pub fn why_lines(v: Option<&crate::explain::Verdict>) -> Vec<Line> {
    let w = |s: &str, t: Tone| (s.to_string(), t);
    let Some(v) = v else {
        return vec![Line {
            words: vec![w("No reading yet. ", Tone::Plain), w("Ask again in a few seconds.", Tone::Quiet)],
            render: false,
        }];
    };
    let mut out = vec![Line {
        words: vec![w(first_sentence(&v.sentence), Tone::Plain), w(&format!(" ({})", v.confidence_word()), Tone::Quiet)],
        render: false,
    }];
    if let Some(p) = &v.process {
        out.push(Line { words: vec![w("Main process: ", Tone::Quiet), w(p, Tone::Name)], render: false });
    }
    if let Some(e) = v.evidence.first() {
        out.push(Line { words: vec![w(e, Tone::Quiet)], render: false });
    }
    out.truncate(3);
    out
}

/// The one-time crash announcement.
pub fn announcement_lines(r: &crate::report::Report) -> Vec<Line> {
    vec![
        Line { words: vec![(r.headline.clone(), Tone::Warm)], render: false },
        Line { words: vec![(r.just_before.clone(), Tone::Quiet)], render: false },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{ProcView, RenderView};
    use crate::nvml::{GpuSample, ProcInfo};
    use crate::parse::Progress;

    fn snap(temp: u32, util: u32, vram: f64) -> Snapshot {
        let mut s = Snapshot::default();
        s.sample = Some(GpuSample {
            util_pct: Some(util),
            temp_c: Some(temp),
            vram_used_mb: Some(vram),
            vram_total_mb: Some(16384.0),
            fan_pct: Some(78),
            ..Default::default()
        });
        s
    }

    #[test]
    fn voice_lines() {
        let cfg = Config::default();
        let now = chrono::Local::now();
        let t = |s: &Snapshot| lines(s, &cfg, now).iter().map(|l| l.text()).collect::<Vec<_>>();
        assert_eq!(t(&snap(64, 82, 4000.0)), ["GPU 82% · 64 °C. All good."]);
        assert_eq!(t(&snap(41, 2, 4000.0)), ["GPU idle · 41 °C. Nothing to report."]);
        let warm = lines(&snap(81, 82, 4000.0), &cfg, now);
        assert_eq!(warm[0].text(), "GPU 82%. 81 °C, a bit warm.");
        assert_eq!(warm[0].words[1].1, Tone::Warm);
        assert_eq!(t(&snap(84, 90, 4000.0)), ["GPU 90%. 84 °C. Too warm; fan at 78%."]);
        assert_eq!(t(&snap(60, 90, 15600.0))[1], "VRAM 15.2 of 16 GB. Tight.");
        let mut s = snap(60, 90, 9000.0);
        s.procs = Some(vec![ProcView {
            info: ProcInfo {
                pid: 1,
                name: "Cyberpunk2077.exe".into(),
                vram_mb: Some(6246.4),
                sm_pct: None,
                enc_pct: None,
                dec_pct: None,
                kind: "G",
            },
            chain: vec![],
            category: "Game".into(),
            color: (0, 0, 0),
            cmdline: None,
        }]);
        s.render = Some(RenderView {
            job: "demo4k".into(),
            stage: None,
            source_name: "r_0.log".into(),
            progress: Some(Progress { frame: 948, total: Some(1420), fps: None, speed: None }),
            fps: None,
            job_elapsed: None,
            job_est_total: None,
        });
        let l = lines(&s, &cfg, now);
        assert_eq!(l.len(), 3);
        assert!(l[0].render && l[0].text() == "demo4k · 66.8%.", "{}", l[0].text());
        assert_eq!(l[2].text(), "Cyberpunk2077: 6.1 GB VRAM.");
        assert_eq!(l[2].words[0].1, Tone::Name);
        assert!(l.iter().all(|l| !l.text().contains('!')));
    }

    fn verdict(kind: crate::explain::Kind, conf: f32, sentence: &str) -> crate::explain::Verdict {
        crate::explain::Verdict {
            kind,
            confidence: conf,
            sentence: sentence.into(),
            process: Some("ffmpeg.exe (Video)".into()),
            evidence: vec!["GPU 41% · decoder 93% · encoder 0%".into()],
        }
    }

    #[test]
    fn bottleneck_forecast_and_job_lines() {
        use crate::explain::Kind;
        let cfg = Config::default();
        let now = chrono::Local::now();
        let t = |s: &Snapshot| lines(s, &cfg, now).iter().map(|l| l.text()).collect::<Vec<_>>();
        let mut s = snap(64, 41, 4000.0);
        s.insights.bottleneck = Some(verdict(
            Kind::Decode,
            0.8,
            "Waiting on video decoding: the decoder is at 93% while the GPU cores sit at 41%. A lighter codec would help.",
        ));
        assert_eq!(t(&s), ["Waiting on video decoding: the decoder is at 93% while the GPU cores sit at 41%. (sure)"]);
        // not a problem, or not confident: the usual calm line
        s.insights.bottleneck = Some(verdict(Kind::GpuBound, 0.9, "The GPU is the limit."));
        assert_eq!(t(&s), ["GPU 41% · 64 °C. All good."]);
        s.insights.bottleneck = Some(verdict(Kind::Decode, 0.4, "Waiting on video decoding."));
        assert_eq!(t(&s), ["GPU 41% · 64 °C. All good."]);
        // heat still wins over a bottleneck
        let mut hot = snap(84, 41, 4000.0);
        hot.insights.bottleneck = Some(verdict(Kind::Decode, 0.8, "Waiting on video decoding."));
        assert!(t(&hot)[0].contains("Too warm"));
        // the VRAM forecast warning comes before anything else about VRAM
        let mut f = snap(64, 90, 15600.0);
        f.insights.forecast.warning = Some("At this rate VRAM will be full in ~3 min (python.exe is growing).".into());
        let l = lines(&f, &cfg, now);
        assert_eq!(l[1].text(), "At this rate VRAM will be full in ~3 min (python.exe is growing).");
        assert_eq!(l[1].words[0].1, Tone::Hot);
        // tight card with an idle hog
        let mut h = snap(64, 90, 15600.0);
        h.insights.forecast.hogs =
            vec![crate::forecast::Hog { name: "chrome.exe".into(), category: "Browser".into(), vram_mb: 1228.8, idle_secs: 300.0 }];
        assert_eq!(t(&h)[1], "VRAM 15.2 of 16 GB. chrome.exe holds 1.2 GB without using it.");
        // a job that just finished, with the comparison
        let mut j = snap(64, 3, 4000.0);
        j.insights.last_job = Some(
            serde_json::from_value(serde_json::json!({
                "kind": "render", "name": "Shot 12", "key": "render:shot 12", "start_ms": 0, "end_ms": now.timestamp_millis() - 60_000,
                "duration_s": 1140.0, "avg_gpu": null, "peak_gpu": null, "avg_fps": null, "energy_wh": null, "energy_source": null,
                "peak_vram_mb": null, "peak_temp": null, "power_throttled_s": 360.0, "thermal_throttled_s": 0.0, "bottleneck": null,
                "vs_last": "14% slower than last time; GPU spent 6 min power-throttled."
            }))
            .unwrap(),
        );
        assert_eq!(t(&j)[1], "Shot 12 finished in 19 min. 14% slower than last time; GPU spent 6 min power-throttled.");
    }

    #[test]
    fn why_and_announcement() {
        use crate::explain::Kind;
        let v = verdict(Kind::Decode, 0.6, "Waiting on video decoding: the decoder is at 93%. More text.");
        let l = why_lines(Some(&v));
        assert_eq!(l[0].text(), "Waiting on video decoding: the decoder is at 93%. (likely)");
        assert_eq!(l[1].text(), "Main process: ffmpeg.exe (Video)");
        assert_eq!(l[2].text(), "GPU 41% · decoder 93% · encoder 0%");
        assert!(why_lines(None)[0].text().starts_with("No reading yet."));
    }
}
