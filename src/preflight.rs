//! Pre-render check: "will this job fit?". From the job history (what similar past runs needed)
//! and the card right now (what other apps hold), predict the job's peak VRAM and duration, and
//! warn early when it will likely run out of memory. Thin or inconsistent history gives no
//! verdict rather than a false alarm. Read-only, pure, and cheap: it only looks at records.
use crate::forecast::{Hog, fmt_eta};
use crate::history::{JobKind, JobRecord, job_key};
use serde::Serialize;

/// Runs of the same kind looked at (newest first).
pub const MAX_RUNS: usize = 5;
/// Below this confidence there is no verdict.
pub const MIN_CONFIDENCE: f64 = 0.4;
/// Hex only speaks up about an out-of-memory verdict from at least two consistent runs (or one
/// that matched very closely).
pub const ALARM_CONFIDENCE: f64 = 0.65;
/// A past run that reached this share of the card filled it: its measured need is a lower bound.
const FILLED: f32 = 0.97;
/// Above this share of the card the job "should fit, but only just".
const TIGHT: f32 = 0.95;

/// The job to check.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub name: String,
    pub project: Option<String>,
    /// Resolution class ("4K", "1080p", ... or "WxH"), see `parse::resolution_label`.
    pub resolution: Option<String>,
    pub frames: Option<u64>,
}

/// The card right now.
#[derive(Debug, Clone, Default)]
pub struct LiveVram {
    pub total_mb: Option<f32>,
    pub used_mb: Option<f32>,
    /// What the job's own processes already hold (0 before it starts).
    pub job_now_mb: f32,
    /// Background apps holding VRAM without using the GPU.
    pub hogs: Vec<Hog>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    Fits,
    Tight,
    WontFit,
    Unknown,
}

impl Fit {
    pub fn label(self) -> &'static str {
        match self {
            Fit::Fits => "should fit",
            Fit::Tight => "tight",
            Fit::WontFit => "likely out of memory",
            Fit::Unknown => "no prediction yet",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preflight {
    pub job: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub fit: Fit,
    /// 0..1; below `MIN_CONFIDENCE` the fit is `Unknown`.
    pub confidence: f64,
    pub confidence_word: &'static str,
    /// One plain sentence, e.g. "At 4K this job needs ~15.9 GB of VRAM; 14.6 GB is free. Likely out of memory, or very slow."
    pub sentence: String,
    /// What the prediction rests on.
    pub evidence: Vec<String>,
    /// VRAM the job itself is expected to need (median of past runs), and the most a run needed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub need_mb: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub need_high_mb: Option<f32>,
    /// Expected card total at the job's peak (other apps now + the job's need).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicted_mb: Option<f32>,
    /// VRAM available to the job: card total minus what other apps hold now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_mb: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_mb: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    /// Past runs used.
    pub matches: usize,
    /// What closing idle VRAM hogs would free, when it matters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hog_note: Option<String>,
}

impl Preflight {
    /// Worth telling the user without being asked.
    pub fn alarming(&self) -> bool {
        self.fit == Fit::WontFit && self.confidence >= ALARM_CONFIDENCE
    }

    /// Short line for the panel, e.g. "Pre-check: 4K needs ~15.9 GB, 14.6 GB free · likely out of memory".
    pub fn short_line(&self) -> String {
        match (self.need_mb, self.free_mb) {
            (Some(n), Some(f)) if self.fit != Fit::Unknown => {
                let at = self.resolution.as_ref().map(|r| format!("{r} ")).unwrap_or_default();
                format!("Pre-check: {at}needs ~{}, {} free · {}", gb(n), gb(f), self.fit.label())
            }
            _ => format!("Pre-check: {}", self.fit.label()),
        }
    }
}

fn gb(mb: f32) -> String {
    format!("{:.1} GB", mb / 1024.0)
}

fn tokens(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty() && !matches!(*t, "try" | "run" | "attempt" | "final" | "v"))
        .map(str::to_string)
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Jaccard similarity of the name tokens.
fn similar(a: &str, b: &str) -> f64 {
    let (x, y) = (tokens(a), tokens(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let inter = x.iter().filter(|t| y.contains(t)).count();
    inter as f64 / (x.len() + y.len() - inter) as f64
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

fn eq_ci(a: &Option<String>, b: &Option<String>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x.eq_ignore_ascii_case(y))
}

/// Which past runs count, and how much: (tier weight, what the match is, runs newest first,
/// runs of the same job at another resolution).
fn matches<'a>(q: &Query, history: &'a [JobRecord]) -> (f64, &'static str, Vec<&'a JobRecord>, Vec<&'a JobRecord>) {
    let res_ok = |r: &JobRecord| match (&q.resolution, &r.profile.hints.resolution) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    };
    let keys = [job_key(JobKind::Render, &q.name), job_key(JobKind::Gpu, &q.name)];
    let newest = |f: &dyn Fn(&JobRecord) -> bool| -> Vec<&'a JobRecord> { history.iter().rev().filter(|r| f(r)).take(MAX_RUNS).collect() };
    let same: Vec<&JobRecord> = newest(&|r| keys.contains(&r.key) && res_ok(r));
    let other_res: Vec<&JobRecord> = newest(&|r| keys.contains(&r.key) && !res_ok(r));
    if !same.is_empty() {
        return (1.0, "this job", same, other_res);
    }
    if q.project.is_some() {
        let proj = newest(&|r| r.kind == JobKind::Render && eq_ci(&r.profile.hints.project, &q.project) && res_ok(r));
        if !proj.is_empty() {
            return (0.75, "jobs in this project", proj, other_res);
        }
    }
    let sim = newest(&|r| r.kind == JobKind::Render && similar(&r.name, &q.name) >= 0.5 && res_ok(r));
    (0.6, "similar jobs", sim, other_res)
}

/// The prediction for `q`. `history` is oldest first, as `history::recent` returns it.
pub fn predict(q: &Query, history: &[JobRecord], live: &LiveVram) -> Preflight {
    let (tier, what, runs, other_res) = matches(q, history);
    let mut out = Preflight {
        job: q.name.clone(),
        resolution: q.resolution.clone(),
        fit: Fit::Unknown,
        confidence: 0.0,
        confidence_word: "low",
        sentence: String::new(),
        evidence: vec![],
        need_mb: None,
        need_high_mb: None,
        predicted_mb: None,
        free_mb: None,
        total_mb: live.total_mb,
        duration_s: None,
        matches: runs.len(),
        hog_note: None,
    };
    if !other_res.is_empty() {
        let rs: Vec<String> = other_res
            .iter()
            .filter_map(|r| r.profile.hints.resolution.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        out.evidence.push(format!("Runs of this job at {} were not counted (different resolution).", rs.join(", ")));
    }

    // duration: seconds per frame when the frame counts are known, else the same job's median time
    let mut spf: Vec<f64> =
        runs.iter().filter_map(|r| r.profile.hints.frames.filter(|f| *f > 0).map(|f| r.duration_s / f as f64)).collect();
    out.duration_s = match (q.frames, median(&mut spf)) {
        (Some(f), Some(s)) => Some(s * f as f64),
        _ if tier == 1.0 => median(&mut runs.iter().map(|r| r.duration_s).collect::<Vec<_>>()),
        _ => None,
    };

    // what the job itself needed: its own processes (1.1 records), or the card's peak minus where it started
    let mut need: Vec<f64> = vec![];
    let mut card_only = 0;
    let mut filled = false;
    for r in &runs {
        let own = r.profile.own_vram_mb.or_else(|| match (r.peak_vram_mb, r.profile.start_vram_mb) {
            (Some(p), Some(s)) if p > s => {
                card_only += 1;
                Some(p - s)
            }
            _ => None,
        });
        if let Some(n) = own {
            need.push(n as f64);
        }
        let total = r.profile.vram_total_mb.or(live.total_mb);
        if let (Some(p), Some(t)) = (r.peak_vram_mb, total)
            && p >= FILLED * t
        {
            filled = true;
        }
    }
    // records from before 1.1 only know the card's peak: assume the same apps are open as then
    let peak_only = need.is_empty();
    if peak_only {
        need = runs.iter().filter_map(|r| r.peak_vram_mb.map(|p| p as f64)).collect();
        card_only = need.len();
    }
    let n = need.len();
    let high = need.iter().cloned().fold(f64::NAN, f64::max);
    let mid = median(&mut need);

    let count = match n {
        0 => 0.0,
        1 => 0.6,
        2 => 0.8,
        _ => 0.95,
    };
    let cv = if n >= 2 {
        let m = need.iter().sum::<f64>() / n as f64;
        (need.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n as f64).sqrt() / m.max(1.0)
    } else {
        0.0
    };
    let consistency = if cv < 0.08 {
        1.0
    } else if cv < 0.2 {
        0.85
    } else {
        0.65
    };
    let mut conf = tier * count * consistency;
    if card_only * 2 > n {
        conf *= 0.8;
    }
    if q.resolution.is_none() || runs.iter().any(|r| r.profile.hints.resolution.is_none()) {
        conf *= 0.9;
    }
    out.confidence = (conf.min(0.9) * 100.0).round() / 100.0;
    out.confidence_word = if out.confidence >= 0.75 {
        "high"
    } else if out.confidence >= 0.55 {
        "medium"
    } else {
        "low"
    };

    let dur = out.duration_s.map(|d| format!(" Expected to take {}.", fmt_eta(d))).unwrap_or_default();
    let (Some(mid), Some(total), Some(used)) = (mid, live.total_mb, live.used_mb) else {
        out.sentence = if runs.is_empty() {
            "Not enough history for this job yet: Tachyon learns from each finished run.".into()
        } else {
            format!("Past runs did not record their VRAM, so there is no memory prediction.{dur}")
        };
        return out;
    };
    let others = (used - live.job_now_mb).max(0.0);
    let free = (total - others).max(0.0);
    let predicted = if peak_only { mid as f32 } else { others + mid as f32 };
    out.need_mb = Some(mid as f32);
    out.need_high_mb = Some(high as f32);
    out.predicted_mb = Some(predicted);
    out.free_mb = Some(free);

    let res = q.resolution.as_ref().map(|r| format!(" at {r}")).unwrap_or_default();
    let newest = runs
        .first()
        .map(|r| {
            chrono::DateTime::from_timestamp_millis(r.end_ms)
                .map(|d| d.with_timezone(&chrono::Local).format("%-d %b %H:%M").to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let range = if n >= 2 && (high - mid).abs() > 50.0 {
        format!("{}–{}", gb(need.iter().cloned().fold(f64::INFINITY, f64::min) as f32), gb(high as f32))
    } else {
        format!("~{}", gb(mid as f32))
    };
    out.evidence.insert(
        0,
        format!(
            "{} past run{} of {what}{res} (newest {newest}) needed {range}{}.",
            n,
            if n == 1 { "" } else { "s" },
            if peak_only { " on the whole card" } else { "" }
        ),
    );
    if peak_only {
        out.evidence.push("Older records only have the card's total, so this assumes the same apps are open as then.".into());
    } else {
        out.evidence.push(format!("Other apps hold {} now; the card has {}.", gb(others), gb(total)));
    }
    if filled {
        out.evidence.push("A past run filled the card, so it may need even more than it got.".into());
    }
    if n >= 2 && cv >= 0.2 {
        out.evidence.push("Past runs differed a lot, so this is a rough estimate.".into());
    }

    if out.confidence < MIN_CONFIDENCE {
        out.sentence = format!(
            "Only {n} {} past run{} to go on, so no verdict yet (it would need ~{}).{dur}",
            if tier < 1.0 { "loosely similar" } else { "comparable" },
            if n == 1 { "" } else { "s" },
            gb(mid as f32)
        );
        return out;
    }
    out.fit = if filled {
        if predicted >= FILLED * total { Fit::WontFit } else { Fit::Tight }
    } else if predicted > total {
        Fit::WontFit
    } else if predicted > TIGHT * total {
        Fit::Tight
    } else {
        Fit::Fits
    };
    let at = q.resolution.as_ref().map(|r| format!("At {r} this job")).unwrap_or_else(|| "This job".into());
    let need_s = if peak_only {
        format!("{at} filled the card to ~{} last time", gb(mid as f32))
    } else {
        format!("{at} needs ~{} of VRAM", gb(mid as f32))
    };
    out.sentence = match out.fit {
        Fit::WontFit if filled && !peak_only => {
            format!("{at} filled the card last time; with {} free it will likely run out of memory, or be very slow.", gb(free))
        }
        Fit::WontFit => format!("{need_s}; {} is free. Likely out of memory, or very slow.", gb(free)),
        Fit::Tight => format!("{need_s}; {} is free. It should fit, but only just.", gb(free)),
        _ => format!("{need_s}; {} is free. It should fit.", gb(free)),
    } + &dur;

    if matches!(out.fit, Fit::WontFit | Fit::Tight) && !live.hogs.is_empty() {
        let freed: f32 = live.hogs.iter().map(|h| h.vram_mb).sum();
        let who = match live.hogs.len() {
            1 => format!("{} holds", live.hogs[0].name),
            2 => format!("{} and {} hold", live.hogs[0].name, live.hogs[1].name),
            k => format!("{} and {} other apps hold", live.hogs[0].name, k - 1),
        };
        let enough = if predicted - freed <= TIGHT * total { ", enough for this job" } else { "" };
        out.hog_note = Some(format!(
            "{who} {} without using the GPU: closing {} would leave {} free{enough}.",
            gb(freed),
            if live.hogs.len() == 1 { "it" } else { "them" },
            gb(free + freed)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{JobHints, JobProfile};

    fn run(name: &str, res: Option<&str>, own: Option<f32>, peak: Option<f32>, frames: Option<u64>, dur: f64, i: i64) -> JobRecord {
        JobRecord {
            kind: JobKind::Render,
            name: name.into(),
            key: job_key(JobKind::Render, name),
            start_ms: 1_790_000_000_000 + i * 3_600_000,
            end_ms: 1_790_000_000_000 + i * 3_600_000 + (dur * 1000.0) as i64,
            duration_s: dur,
            avg_gpu: Some(95.0),
            peak_gpu: Some(99),
            avg_fps: None,
            energy_wh: None,
            energy_source: None,
            peak_vram_mb: peak,
            peak_temp: None,
            power_throttled_s: 0.0,
            thermal_throttled_s: 0.0,
            bottleneck: None,
            cost: None,
            currency: None,
            vs_last: None,
            profile: JobProfile {
                hints: JobHints { project: Some("ladial".into()), resolution: res.map(Into::into), frames },
                own_vram_mb: own,
                start_vram_mb: peak.map(|_| 1500.0),
                vram_total_mb: Some(16376.0),
            },
        }
    }

    fn q(name: &str, res: Option<&str>) -> Query {
        Query { name: name.into(), project: Some("ladial".into()), resolution: res.map(Into::into), frames: Some(240) }
    }

    fn live(used: f32, hogs: Vec<Hog>) -> LiveVram {
        LiveVram { total_mb: Some(16376.0), used_mb: Some(used), job_now_mb: 0.0, hogs }
    }

    fn chrome() -> Hog {
        Hog { name: "chrome.exe".into(), category: "Browser".into(), vram_mb: 1230.0, idle_secs: 300.0 }
    }

    #[test]
    fn the_4k_case_warns_early() {
        let h: Vec<JobRecord> = (0..3)
            .map(|i| run(&format!("demo4k try {i}"), Some("4K"), Some(16280.0 + i as f32 * 20.0), Some(16300.0), Some(240), 1800.0, i))
            .collect();
        // 1.8 GB held by other apps (chrome 1.2 of it): 14.6 GB free
        let p = predict(&q("demo4k try 3", Some("4K")), &h, &live(1800.0, vec![chrome()]));
        assert_eq!(p.fit, Fit::WontFit, "{p:#?}");
        assert!(p.alarming());
        assert_eq!(p.matches, 3);
        assert_eq!(p.confidence_word, "high");
        assert!(p.sentence.starts_with("At 4K this job filled the card last time; with 14.2 GB free"), "{}", p.sentence);
        assert!(p.sentence.contains("Expected to take ~30 min"), "{}", p.sentence);
        assert!(
            p.hog_note
                .as_deref()
                .unwrap()
                .starts_with("chrome.exe holds 1.2 GB without using the GPU: closing it would leave 15.4 GB free"),
            "{:?}",
            p.hog_note
        );
        assert!(p.evidence[0].starts_with("3 past runs of this job at 4K"), "{:?}", p.evidence);
    }

    #[test]
    fn needs_more_than_is_free() {
        let h: Vec<JobRecord> =
            (0..3).map(|i| run("shot 12", Some("4K"), Some(15900.0 + i as f32 * 30.0), Some(15000.0), Some(100), 600.0, i)).collect();
        let mut l = live(1800.0, vec![]);
        l.used_mb = Some(1800.0);
        let p = predict(&q("shot 12", Some("4K")), &h, &l);
        // 15.6 GB peak each time is below "filled" (97%): the plain rule applies
        assert_eq!(p.fit, Fit::WontFit);
        assert_eq!(
            p.sentence,
            "At 4K this job needs ~15.6 GB of VRAM; 14.2 GB is free. Likely out of memory, or very slow. Expected to take ~24 min."
        );
        assert_eq!(p.short_line(), "Pre-check: 4K needs ~15.6 GB, 14.2 GB free · likely out of memory");
        // the same job at 1080p fits, and the 4K runs are left out
        let small: Vec<JobRecord> = (3..6).map(|i| run("shot 12", Some("1080p"), Some(6000.0), Some(7500.0), None, 300.0, i)).collect();
        let all: Vec<JobRecord> = h.iter().cloned().chain(small).collect();
        let p = predict(&q("shot 12", Some("1080p")), &all, &l);
        assert_eq!(p.fit, Fit::Fits, "{p:#?}");
        assert!(p.evidence.iter().any(|e| e.contains("at 4K were not counted")), "{:?}", p.evidence);
        assert_eq!(p.duration_s, Some(300.0));
    }

    #[test]
    fn tight_and_fits() {
        let h = vec![
            run("a", Some("1080p"), Some(13800.0), Some(15000.0), None, 60.0, 0),
            run("a", Some("1080p"), Some(13900.0), Some(15000.0), None, 60.0, 1),
        ];
        assert_eq!(predict(&q("a", Some("1080p")), &h, &live(1800.0, vec![])).fit, Fit::Tight);
        assert_eq!(predict(&q("a", Some("1080p")), &h, &live(600.0, vec![])).fit, Fit::Fits);
        // VRAM the job already holds is not counted twice
        let mut l = live(15500.0, vec![]);
        l.job_now_mb = 13900.0;
        assert_eq!(predict(&q("a", Some("1080p")), &h, &l).fit, Fit::Fits);
    }

    #[test]
    fn no_false_alarms_on_thin_history() {
        // nothing to go on
        let p = predict(&q("new job", Some("4K")), &[], &live(15000.0, vec![]));
        assert_eq!((p.fit, p.matches), (Fit::Unknown, 0));
        assert!(p.sentence.starts_with("Not enough history"));
        // one loosely similar run in another project: no verdict
        let mut r = run("teaser cut", None, Some(16000.0), Some(16300.0), None, 60.0, 0);
        r.profile.hints.project = Some("other".into());
        let p = predict(&Query { name: "teaser cut b".into(), ..Default::default() }, &[r], &live(3000.0, vec![]));
        assert_eq!(p.fit, Fit::Unknown, "{p:#?}");
        assert!(p.confidence < MIN_CONFIDENCE && !p.alarming());
        assert!(p.sentence.starts_with("Only 1 loosely similar past run"), "{}", p.sentence);
        // a single run of the same job: a verdict, but not loud enough to announce
        let h = vec![run("x", Some("4K"), Some(15900.0), Some(15000.0), None, 60.0, 0)];
        let p = predict(&q("x", Some("4K")), &h, &live(2000.0, vec![]));
        assert_eq!(p.fit, Fit::WontFit);
        assert!(!p.alarming(), "{}", p.confidence);
        // wildly different runs lower the confidence
        let h: Vec<JobRecord> = [4000.0, 15000.0, 9000.0]
            .iter()
            .enumerate()
            .map(|(i, v)| run("y", Some("4K"), Some(*v), Some(16000.0), None, 60.0, i as i64))
            .collect();
        let p = predict(&q("y", Some("4K")), &h, &live(2000.0, vec![]));
        assert!(p.confidence < 0.65 && !p.alarming(), "{p:#?}");
        // no live VRAM reading: no memory verdict
        let p = predict(&q("x", Some("4K")), &[run("x", Some("4K"), Some(9000.0), None, None, 60.0, 0)], &LiveVram::default());
        assert_eq!(p.fit, Fit::Unknown);
    }

    #[test]
    fn older_records_and_similar_names() {
        // 1.0 records: no profile, only the card peak
        let mut h: Vec<JobRecord> = (0..3).map(|i| run("intro", None, None, Some(16200.0), None, 120.0, i)).collect();
        for r in &mut h {
            r.profile = JobProfile::default();
        }
        let p = predict(&Query { name: "intro try 4".into(), ..Default::default() }, &h, &live(2500.0, vec![]));
        assert_eq!(p.fit, Fit::WontFit, "{p:#?}");
        assert!(p.sentence.starts_with("This job filled the card to ~15.8 GB last time"), "{}", p.sentence);
        assert!(p.evidence.iter().any(|e| e.contains("assumes the same apps")));
        // same project, other job name
        let h = vec![
            run("shot 1", Some("4K"), Some(9000.0), None, Some(100), 100.0, 0),
            run("shot 2", Some("4K"), Some(9100.0), None, Some(100), 100.0, 1),
        ];
        let p = predict(
            &Query { name: "shot 3".into(), project: Some("Ladial".into()), resolution: Some("4K".into()), frames: Some(300) },
            &h,
            &live(1000.0, vec![]),
        );
        assert_eq!(p.fit, Fit::Fits);
        assert!(p.evidence[0].contains("of jobs in this project at 4K"), "{:?}", p.evidence);
        assert!((p.duration_s.unwrap() - 300.0).abs() < 1.0, "s/frame × frames");
        assert_eq!(similar("Shot 12 final v2", "shot 12 v3"), 0.5);
    }
}
