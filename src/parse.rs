//! Pure parsing of log tails. No I/O here, so it is fully unit-testable.
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress {
    pub frame: u64,
    pub total: Option<u64>,
    pub fps: Option<f64>,
    pub speed: Option<String>,
}

/// Decode raw bytes from a log. PowerShell 5 `>` redirects write UTF-16LE,
/// so detect that (lots of NULs at odd offsets) and decode accordingly.
pub fn decode(bytes: &[u8]) -> String {
    let n = bytes.len().min(4096);
    if n >= 4 {
        let odd_nuls = bytes[..n].iter().skip(1).step_by(2).filter(|b| **b == 0).count();
        let even_nuls = bytes[..n].iter().step_by(2).filter(|b| **b == 0).count();
        if odd_nuls * 3 > n / 2 || even_nuls * 3 > n / 2 {
            // Align to the byte phase where the NULs sit at the high byte.
            let start = if even_nuls > odd_nuls { 1 } else { 0 };
            let u: Vec<u16> = bytes[start..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return String::from_utf16_lossy(&u).trim_start_matches('\u{feff}').to_string();
        }
    }
    String::from_utf8_lossy(bytes).trim_start_matches('\u{feff}').to_string()
}

/// Split on both \n and \r: ffmpeg rewrites its stats line with carriage returns.
pub fn lines(text: &str) -> impl DoubleEndedIterator<Item = &str> {
    text.split(['\n', '\r']).map(str::trim).filter(|l| !l.is_empty())
}

/// Number following `key` (after optional spaces, '=' or ':' already part of key).
fn num_after(line: &str, key: &str) -> Option<f64> {
    let i = line.find(key)? + key.len();
    let rest = line[i..].trim_start();
    let end = rest.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.')).map(|(i, _)| i).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Number immediately before `key`, e.g. "15.2" in "S2  15.2 fps".
fn num_before(line: &str, key: &str) -> Option<f64> {
    let i = line.rfind(key)?;
    let head = line[..i].trim_end();
    let st = head.rfind(|c: char| !(c.is_ascii_digit() || c == '.')).map(|x| x + 1).unwrap_or(0);
    head[st..].parse().ok()
}

fn token_after(line: &str, key: &str) -> Option<String> {
    let i = line.find(key)? + key.len();
    let t = line[i..].split_whitespace().next()?;
    Some(t.to_string())
}

/// Find the last ffmpeg progress in a log tail. Handles the classic stats line
/// (`frame= 1234 fps= 12 q=... speed=0.5x`) and `-progress` key=value blocks.
pub fn ffmpeg_progress(text: &str) -> Option<Progress> {
    let all: Vec<&str> = lines(text).collect();
    for (idx, l) in all.iter().enumerate().rev() {
        if !l.contains("frame=") {
            continue;
        }
        let Some(frame) = num_after(l, "frame=") else { continue };
        let mut p = Progress { frame: frame as u64, ..Default::default() };
        if l.contains("fps=") {
            p.fps = num_after(l, "fps=");
            p.speed = token_after(l, "speed=");
        } else {
            // -progress output: frame=N on its own line, fps=/speed= on following lines.
            for f in all.iter().skip(idx + 1).take(12) {
                if f.starts_with("frame=") {
                    break;
                }
                if let Some(v) = f.strip_prefix("fps=") {
                    p.fps = v.trim().parse().ok();
                }
                if let Some(v) = f.strip_prefix("speed=") {
                    p.speed = Some(v.trim().to_string());
                }
            }
        }
        return Some(p);
    }
    None
}

/// Generic "N/M" progress from Python renderers / tqdm, e.g.
/// `frame 1234/7155`, `[1234/7155]`, ` 17%|##  | 1234/7155 [01:02<05:00, 11.2it/s]`.
/// Only lines that mention frame/it/s or a % bar are accepted, to avoid matching dates.
pub fn slash_progress(text: &str) -> Option<Progress> {
    for l in lines(text).rev() {
        let low = l.to_ascii_lowercase();
        if !(low.contains("frame") || low.contains("it/s") || low.contains("s/it") || low.contains("%|") || low.contains("fps")) {
            continue;
        }
        let b = l.as_bytes();
        let mut best = None;
        for (i, _) in l.match_indices('/') {
            let (mut s, mut e) = (i, i + 1);
            while s > 0 && b[s - 1].is_ascii_digit() {
                s -= 1;
            }
            while e < b.len() && b[e].is_ascii_digit() {
                e += 1;
            }
            if s == i || e == i + 1 {
                continue;
            }
            // reject dates/paths like 2026/10/24 or a/b/c
            if (s > 0 && (b[s - 1] == b'/' || b[s - 1].is_ascii_alphabetic())) || (e < b.len() && b[e] == b'/') {
                continue;
            }
            let (Ok(a), Ok(t)) = (l[s..i].parse::<u64>(), l[i + 1..e].parse::<u64>()) else { continue };
            if t > 0 && a <= t {
                best = Some((a, t));
            }
        }
        if let Some((a, t)) = best {
            let fps = num_after(&low, "fps=").or_else(|| num_before(&low, " fps")).or_else(|| {
                // tqdm: "11.2it/s"
                let i = low.find("it/s")?;
                let head = &low[..i];
                let st = head.rfind(|c: char| !(c.is_ascii_digit() || c == '.')).map(|x| x + 1).unwrap_or(0);
                head[st..].parse().ok()
            });
            return Some(Progress { frame: a, total: Some(t), fps, speed: None });
        }
    }
    None
}

/// Total frame count announced in a batch/queue log. Accepts `NF=7155`, `NF: 7155`,
/// `nf 7155`, `total_frames=7155`, `expected_frames=7155`, `frames=7155` (not ffmpeg's `frame=`).
/// The last match wins (the most recent job).
pub fn total_frames(text: &str) -> Option<u64> {
    const KEYS: [&str; 6] = ["total_frames", "expected_frames", "expected", "nframes", "nf", "frames"];
    let mut found = None;
    for l in lines(text) {
        let low = l.to_ascii_lowercase();
        for k in KEYS {
            let mut from = 0;
            while let Some(pos) = low[from..].find(k) {
                let s = from + pos;
                from = s + k.len();
                // word boundary on the left
                if s > 0 {
                    let c = low.as_bytes()[s - 1];
                    if c.is_ascii_alphanumeric() || c == b'_' {
                        continue;
                    }
                }
                let rest = &low[s + k.len()..];
                let rest2 = rest.trim_start_matches([' ', '\t']);
                let rest3 = rest2.strip_prefix('=').or_else(|| rest2.strip_prefix(':')).unwrap_or(rest2);
                // bare "nf 7155" only for the NF key; others need = or :
                if rest3.len() == rest2.len() && k != "nf" {
                    continue;
                }
                let digits: String = rest3.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<u64>() {
                    if n > 0 {
                        found = Some(n);
                    }
                }
            }
        }
    }
    found
}

/// ffmpeg header fallback: `Duration: HH:MM:SS.xx` and the first video stream's `NN fps`.
pub fn ffmpeg_header_total(head: &str) -> Option<u64> {
    let mut dur = None;
    let mut fps = None;
    for l in lines(head) {
        if dur.is_none() {
            if let Some(i) = l.find("Duration:") {
                let t = l[i + 9..].trim_start();
                let t = t.split(',').next().unwrap_or("");
                let parts: Vec<&str> = t.split(':').collect();
                if parts.len() == 3 {
                    if let (Ok(h), Ok(m), Ok(s)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>(), parts[2].parse::<f64>()) {
                        dur = Some(h * 3600.0 + m * 60.0 + s);
                    }
                }
            }
        }
        if fps.is_none() && l.contains("Video:") {
            for seg in l.split(',') {
                let seg = seg.trim();
                if let Some(v) = seg.strip_suffix(" fps") {
                    fps = v.trim().parse::<f64>().ok();
                }
            }
        }
    }
    let (d, f) = (dur?, fps?);
    Some((d * f).round() as u64)
}

/// Optional progress.json written by our own scripts.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProgressJson {
    pub job: Option<String>,
    pub frame: u64,
    pub total: Option<u64>,
    pub fps: Option<f64>,
    pub started: Option<String>,
}

pub fn progress_json(text: &str) -> Option<ProgressJson> {
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// Remaining seconds for `frame` of `total` at `fps`.
pub fn eta_secs(frame: u64, total: u64, fps: f64) -> Option<f64> {
    if fps <= 0.01 || total == 0 || frame > total {
        return None;
    }
    Some((total - frame) as f64 / fps)
}

pub fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Text after the last `> command` line: the section belonging to the stage that is
/// running now. Progress lines from earlier stages must not be shown as current.
pub fn current_section(text: &str) -> &str {
    let mut cut = 0;
    let mut pos = 0;
    for seg in text.split_inclusive(['\n', '\r']) {
        if seg.trim_start().starts_with("> ") {
            cut = pos;
        }
        pos += seg.len();
    }
    &text[cut..]
}

/// Short stage name from the last `> command` line: the script (`prepare.py`) or exe (`ffedit.exe`, `ffmpeg`).
pub fn current_stage(text: &str) -> Option<String> {
    let line = lines(text).rev().find(|l| l.starts_with("> "))?;
    let cmd = &line[2..];
    let base = |end: usize| {
        let head = &cmd[..end];
        let st = head.rfind(['\\', '/', ' ']).map(|i| i + 1).unwrap_or(0);
        head[st..].to_string()
    };
    if let Some(i) = cmd.find(".py") {
        return Some(base(i + 3));
    }
    if let Some(i) = cmd.to_ascii_lowercase().find(".exe") {
        let name = base(i + 4);
        if !name.eq_ignore_ascii_case("python.exe") {
            return Some(name);
        }
    }
    cmd.split_whitespace().next().map(|w| w.rsplit(['\\', '/']).next().unwrap_or(w).to_string())
}

/// State of a batch/queue log. Two formats are understood:
///  batch.log format: `HH:MM:SS start 02x 169 try 1 NF=1420` / `HH:MM:SS end 02x 169 try 1 exit 0 ok=True`
///  4K_night queue.log: `YYYY-MM-DD HH:MM:SS START clipb_cut_60s_r6 (try 1)` / `... DONE clipb_cut_60s_r6`
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BatchState {
    /// The job started last, if it has no matching end yet.
    pub running: Option<String>,
    pub nf: Option<u64>,
    /// Start time of the running job: (optional date "YYYY-MM-DD", "HH:MM:SS").
    pub started: Option<(Option<String>, String)>,
    /// Seconds per frame of the most recent successful job (batch.log format only).
    pub last_ok_spf: Option<f64>,
}

fn hms_secs(t: &str) -> Option<i64> {
    let v: Vec<i64> = t.split(':').map(|x| x.parse().ok()).collect::<Option<Vec<_>>>()?;
    (v.len() == 3).then(|| v[0] * 3600 + v[1] * 60 + v[2])
}

fn is_hms(t: &str) -> bool {
    t.len() == 8 && hms_secs(t).is_some()
}

pub fn batch_state(text: &str) -> Option<BatchState> {
    let mut st = BatchState::default();
    let mut any = false;
    // name -> (start secs, nf) for spf history
    let mut open: Option<(String, i64, Option<u64>, Option<String>, String)> = None;
    for l in lines(text) {
        let mut w = l.split_whitespace();
        let Some(a) = w.next() else { continue };
        let (date, time) = if is_hms(a) {
            (None, a.to_string())
        } else if a.len() == 10 && a.as_bytes().get(4) == Some(&b'-') {
            match w.next() {
                Some(t) if is_hms(t) => (Some(a.to_string()), t.to_string()),
                _ => continue,
            }
        } else {
            continue;
        };
        let Some(verb) = w.next() else { continue };
        let rest: Vec<&str> = w.collect();
        let rest_s = rest.join(" ");
        match verb {
            "start" | "START" => {
                any = true;
                let (name, nf) = match rest_s.find(" NF=") {
                    Some(i) => (rest_s[..i].to_string(), rest_s[i + 4..].trim().parse().ok()),
                    None => (rest_s.split(" (try").next().unwrap_or(&rest_s).to_string(), None),
                };
                open = Some((name, hms_secs(&time).unwrap_or(0), nf, date, time));
            }
            "end" | "DONE" | "FAILED" | "STOP" | "STOPPED" | "ABORT" => {
                any = true;
                if let Some((name, t0, nf, _, _)) = &open {
                    if rest_s.starts_with(name.as_str()) {
                        if rest_s.contains("ok=True") {
                            if let (Some(nf), Some(t1)) = (nf, hms_secs(&time)) {
                                let mut d = t1 - t0;
                                if d < 0 {
                                    d += 86400;
                                }
                                if *nf > 0 && d > 0 {
                                    st.last_ok_spf = Some(d as f64 / *nf as f64);
                                }
                            }
                        }
                        open = None;
                    }
                }
            }
            _ => {}
        }
    }
    if !any {
        return None;
    }
    if let Some((name, _, nf, date, time)) = open {
        st.running = Some(name);
        st.nf = nf;
        st.started = Some((date, time));
    }
    Some(st)
}

/// Seconds elapsed since a batch start stamp. `now` is local wall time; `ref_date` is the
/// local date used for HH:MM:SS-only stamps (the batch file's mtime date).
pub fn elapsed_since(started: &(Option<String>, String), now: chrono::NaiveDateTime, ref_date: chrono::NaiveDate) -> Option<f64> {
    use chrono::{NaiveDate, NaiveTime};
    let t = NaiveTime::parse_from_str(&started.1, "%H:%M:%S").ok()?;
    let d = match &started.0 {
        Some(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()?,
        None => ref_date,
    };
    let mut s = d.and_time(t);
    if started.0.is_none() && s > now {
        s -= chrono::Duration::days(1);
    }
    Some((now - s).num_milliseconds() as f64 / 1000.0)
}

/// One chunk log of a parallel stage, e.g. `work4k/<clip>/r_<start>.log`
/// (`<frame> <secs>` lines, `DONE <start> <end> <secs>`) or `p_<start>.log`
/// (`post <frame>` lines, `POST frames <start> <end> ...`).
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub start: u64,
    pub last: Option<u64>,
    pub end: Option<u64>,
}

/// Parse a chunk file name like `r_350.log` -> ("r", 350).
pub fn chunk_name(name: &str) -> Option<(String, u64)> {
    let stem = name.strip_suffix(".log")?;
    let (pre, num) = stem.split_once('_')?;
    if pre.is_empty() || pre.len() > 3 || !pre.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((pre.to_string(), num.parse().ok()?))
}

pub fn parse_chunk(start: u64, text: &str) -> Chunk {
    let mut c = Chunk { start, last: None, end: None };
    for l in lines(text) {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["DONE", s, e, ..] | ["POST", "frames", s, e, ..] => {
                if let (Ok(_), Ok(e)) = (s.parse::<u64>(), e.parse::<u64>()) {
                    c.end = Some(e);
                }
            }
            ["post", f, ..] => {
                if let Ok(f) = f.parse() {
                    c.last = Some(f);
                }
            }
            [f, secs] if secs.parse::<f64>().is_ok() => {
                if let Ok(f) = f.parse() {
                    c.last = Some(f);
                }
            }
            _ => {}
        }
    }
    c
}

/// Aggregate progress over a chunk set: (frames done, total, chunks finished, chunks).
/// A running chunk ends where the next one starts (or at `total`).
pub fn chunk_progress(chunks: &mut [Chunk], total: Option<u64>) -> Option<(u64, u64, usize, usize)> {
    if chunks.is_empty() {
        return None;
    }
    chunks.sort_by_key(|c| c.start);
    let n = chunks.len();
    let (mut done, mut span, mut fin) = (0u64, 0u64, 0usize);
    for i in 0..n {
        let c = &chunks[i];
        let end = c.end.or_else(|| chunks.get(i + 1).map(|x| x.start)).or(total)?;
        let s = end.saturating_sub(c.start);
        span += s;
        if c.end.is_some() {
            done += s;
            fin += 1;
        } else if let Some(l) = c.last {
            done += (l + 1).saturating_sub(c.start).min(s);
        }
    }
    let tot = total.unwrap_or(span).max(span);
    Some((done, tot, fin, n))
}

/// A resolution named in a job name, a path or a log: `3840x2160` (also `×`, `*`, ffmpeg's
/// `scale=1920:1080`), `4320p`/`2160p`/`1440p`/`1080p`/`720p`, `8k`, `4k`, `uhd`, `qhd`, `fhd`.
/// The largest explicit size wins over a word (a log may name its input and output sizes).
pub fn resolution_hint(text: &str) -> Option<(u32, u32)> {
    let low = text.to_ascii_lowercase();
    let b = low.as_bytes();
    let n = b.len();
    let mut best: Option<(u32, u32)> = None;
    let mut keep = |w: u32, h: u32| {
        let (lo, hi) = (w.min(h), w.max(h));
        if (200..=16384).contains(&lo) && (320..=16384).contains(&hi) && hi as f32 / lo as f32 <= 4.0 {
            if best.is_none_or(|(bw, bh)| (w as u64 * h as u64) > (bw as u64 * bh as u64)) {
                best = Some((w, h));
            }
        }
    };
    let digits_at = |i: usize| {
        let mut j = i;
        while j < n && b[j].is_ascii_digit() {
            j += 1;
        }
        (j, low[i..j].parse::<u32>().ok())
    };
    // explicit sizes: digits, a separator, digits (separator `x`, `*`, `×` or `:` after scale=)
    let mut i = 0;
    while i < n {
        if b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_digit()) {
            let (j, w) = digits_at(i);
            if (3..=5).contains(&(j - i)) && j < n {
                let sep = if b[j] == b'x' || b[j] == b'*' {
                    Some(j + 1)
                } else if low[j..].starts_with('×') {
                    Some(j + '×'.len_utf8())
                } else if b[j] == b':' && low[..i].ends_with("scale=") {
                    Some(j + 1)
                } else {
                    None
                };
                if let Some(k) = sep.filter(|k| *k < n && b[*k].is_ascii_digit()) {
                    let (e, h) = digits_at(k);
                    if (3..=5).contains(&(e - k))
                        && (e == n || !b[e].is_ascii_alphanumeric())
                        && let (Some(w), Some(h)) = (w, h)
                    {
                        keep(w, h);
                    }
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    if best.is_some() {
        return best;
    }
    // words: "4k" needs no digit before and no letter or digit after; "uhd" needs non-letters around
    let word = |w: &str, digit_word: bool| {
        let mut from = 0;
        while let Some(p) = low[from..].find(w) {
            let s = from + p;
            let e = s + w.len();
            from = s + 1;
            let before_ok = s == 0 || if digit_word { !b[s - 1].is_ascii_digit() } else { !b[s - 1].is_ascii_alphabetic() };
            let after_ok = e == n || !b[e].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
        }
        false
    };
    for (w, digit_word, res) in [
        ("8k", true, (7680, 4320)),
        ("4320p", true, (7680, 4320)),
        ("4k", true, (3840, 2160)),
        ("2160p", true, (3840, 2160)),
        ("uhd", false, (3840, 2160)),
        ("1440p", true, (2560, 1440)),
        ("qhd", false, (2560, 1440)),
        ("1080p", true, (1920, 1080)),
        ("fhd", false, (1920, 1080)),
        ("720p", true, (1280, 720)),
    ] {
        if word(w, digit_word) {
            return Some(res);
        }
    }
    None
}

/// The class a resolution belongs to, for matching runs: "8K", "4K", "1440p", "1080p", "720p",
/// or "WxH" for anything else.
pub fn resolution_label((w, h): (u32, u32)) -> String {
    let long = w.max(h);
    let short = w.min(h);
    match long {
        7000.. => "8K".into(),
        3800.. => "4K".into(),
        2500.. if short >= 1300 => "1440p".into(),
        1900.. => "1080p".into(),
        1270.. => "720p".into(),
        _ => format!("{w}x{h}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_hints() {
        let r = |s: &str| resolution_hint(s).map(resolution_label);
        assert_eq!(r("demo4k · 02x 169 try 1").as_deref(), Some("4K"));
        assert_eq!(r("mezz_4k_r6").as_deref(), Some("4K"));
        assert_eq!(r("C:\\work\\projects\\ladial\\work4k\\r_0.log").as_deref(), Some("4K"));
        assert_eq!(r("Stream #0:0: Video: h264, yuv420p, 3840x2160 [SAR 1:1], 25 fps").as_deref(), Some("4K"));
        assert_eq!(r("in 1920x1080 -> out 3840×2160").as_deref(), Some("4K"), "the largest size wins");
        assert_eq!(r("-vf scale=1920:1080").as_deref(), Some("1080p"));
        assert_eq!(r("shot_1080p_v2").as_deref(), Some("1080p"));
        assert_eq!(r("teaser UHD final").as_deref(), Some("4K"));
        assert_eq!(r("2560x1440").as_deref(), Some("1440p"));
        assert_eq!(r("1080x1920 vertical").as_deref(), Some("1080p"));
        assert_eq!(r("1024x1024 tiles").as_deref(), Some("1024x1024"));
        // not resolutions
        for s in ["64k buffer", "4kb", "speed 1.5x", "time=00:01:02", "frame 1234x", "seq 12x34", "2x 169 try 1", "shout", ""] {
            assert_eq!(resolution_hint(s), None, "{s}");
        }
        let mut rng = crate::fuzz::Rng::new(4);
        for _ in 0..3000 {
            let _ = resolution_hint(&String::from_utf8_lossy(&rng.mutate(b"in 1920x1080 -> 3840\xc3\x972160 scale=1:2 4k")));
        }
    }
}
