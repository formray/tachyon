//! PCIe link history (1.2): the link's current generation and width against the best the GPU and
//! its slot can do, with changes logged as events. NVIDIA cards drop to Gen1 when idle to save
//! power and go back up under load; that is normal and only counted. A link that stays below its
//! generation while the GPU is busy, or below its width at any time, is reported as degraded
//! (a riser cable, a slot shared with an M.2 drive, dust or a seating problem, a BIOS setting).
//! Notable events go to `history/pcie_link.jsonl` (one line each, rotated at 1 MB).
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

/// GPU load at which the link should be at full speed...
pub const BUSY_UTIL: u8 = 50;
/// ...for this long before a lower generation counts as degraded.
pub const BUSY_HOLD_MS: i64 = 5_000;
/// A narrower link counts after this long (short retraining blips are ignored).
pub const WIDTH_HOLD_MS: i64 = 3_000;
/// Idle downshifts are written to the file at most this often (all are counted).
pub const IDLE_LOG_EVERY_MS: i64 = 10 * 60_000;
pub const FILE: &str = "pcie_link.jsonl";
const OLD: &str = "pcie_link.1.jsonl";
const MAX_FILE_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// At its best generation and width.
    Full,
    /// Lower generation while the GPU is idle or lightly used: normal power saving.
    IdleSaving,
    /// Lower generation for a few seconds of load: may still be training up.
    Settling,
    /// Narrower than the slot allows (x8 instead of x16), at any load.
    DegradedWidth,
    /// Lower generation under sustained load.
    DegradedGen,
    Unknown,
}

impl State {
    pub fn degraded(self) -> bool {
        matches!(self, State::DegradedWidth | State::DegradedGen)
    }
    pub fn words(self) -> &'static str {
        match self {
            State::Full => "at full speed",
            State::IdleSaving => "slowed down while idle to save power (normal)",
            State::Settling => "below full speed for a moment under load",
            State::DegradedWidth => "running on fewer lanes than the slot allows (degraded)",
            State::DegradedGen => "below its PCIe generation under load (degraded)",
            State::Unknown => "unknown",
        }
    }
}

pub fn link_text(g: Option<u32>, w: Option<u32>) -> String {
    match (g, w) {
        (Some(g), Some(w)) => format!("Gen{g} x{w}"),
        (Some(g), None) => format!("Gen{g}"),
        (None, Some(w)) => format!("x{w}"),
        _ => "unknown".into(),
    }
}

/// One logged change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkEvent {
    pub t_ms: i64,
    pub from: String,
    pub to: String,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_pct: Option<u8>,
    /// Idle downshifts and upshifts since the previous written idle event (coalesced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_changes: Option<u32>,
    pub sentence: String,
}

/// The link now, for the status and the bottleneck explainer.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LinkStatus {
    #[serde(rename = "gen")]
    pub gen_: Option<u32>,
    pub width: Option<u32>,
    pub max_gen: Option<u32>,
    pub max_width: Option<u32>,
    pub link: String,
    pub best: String,
    pub state: Option<State>,
    pub sentence: String,
    /// Since when the link has been in this state (Unix ms, and local time).
    pub since_ms: Option<i64>,
    pub since: Option<String>,
    /// The best link seen while the GPU was busy, this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_under_load: Option<String>,
    pub degraded_events: u32,
    pub idle_changes: u32,
}

#[derive(Debug, Default)]
pub struct LinkTracker {
    last: Option<(Option<u32>, Option<u32>)>,
    state: Option<State>,
    since: i64,
    busy_since: Option<i64>,
    narrow_since: Option<i64>,
    idle_pending: u32,
    idle_logged_at: Option<i64>,
    best_load: Option<(u32, u32)>,
    pub status: LinkStatus,
}

impl LinkTracker {
    /// One reading (about once a second). Returns the events worth writing down.
    pub fn push(
        &mut self,
        t_ms: i64,
        gen_: Option<u32>,
        width: Option<u32>,
        max_gen: Option<u32>,
        max_width: Option<u32>,
        util: Option<u8>,
    ) -> Vec<LinkEvent> {
        let mut out = vec![];
        if gen_.is_none() && width.is_none() {
            return out;
        }
        let busy = util.is_some_and(|u| u >= BUSY_UTIL);
        self.busy_since = if busy { Some(self.busy_since.unwrap_or(t_ms)) } else { None };
        let narrow = matches!((width, max_width), (Some(w), Some(m)) if w < m);
        self.narrow_since = if narrow { Some(self.narrow_since.unwrap_or(t_ms)) } else { None };
        let slow = matches!((gen_, max_gen), (Some(g), Some(m)) if g < m);
        let state = if narrow && self.narrow_since.is_some_and(|s| t_ms - s >= WIDTH_HOLD_MS) {
            State::DegradedWidth
        } else if slow && busy {
            if self.busy_since.is_some_and(|s| t_ms - s >= BUSY_HOLD_MS) { State::DegradedGen } else { State::Settling }
        } else if slow {
            State::IdleSaving
        } else if max_gen.is_none() && max_width.is_none() {
            State::Unknown
        } else if narrow {
            State::Settling
        } else {
            State::Full
        };
        if busy && let (Some(g), Some(w)) = (gen_, width) {
            self.best_load = Some(match self.best_load {
                Some((bg, bw)) if (bg, bw) >= (g, w) => (bg, bw),
                _ => (g, w),
            });
        }
        let link = link_text(gen_, width);
        let prev_link = self.last.map(|(g, w)| link_text(g, w));
        let prev_state = self.state;
        if prev_state != Some(state) {
            self.since = t_ms;
            let from = prev_link.clone().unwrap_or_else(|| "start".into());
            let best = link_text(max_gen, max_width);
            let sentence = match state {
                State::DegradedWidth => {
                    format!("The PCIe link dropped to {link} (this GPU and slot can do {best}): fewer lanes than the slot allows.")
                }
                State::DegradedGen => format!(
                    "The PCIe link stays at {link} under load (GPU {}%), below the {best} this GPU and slot can do.",
                    util.unwrap_or(0)
                ),
                State::Full if prev_state.is_some_and(State::degraded) => format!("The PCIe link is back to full speed ({link})."),
                State::Full => format!("PCIe link at full speed ({link})."),
                State::IdleSaving => format!("The PCIe link slowed to {link} while the GPU is idle (normal power saving)."),
                State::Settling => format!("The PCIe link is at {link} under load, training up."),
                State::Unknown => format!("PCIe link {link}."),
            };
            let ev = |idle: Option<u32>| LinkEvent {
                t_ms,
                from: from.clone(),
                to: link.clone(),
                state,
                gpu_pct: util,
                idle_changes: idle,
                sentence: sentence.clone(),
            };
            let notable = state.degraded() || prev_state.is_some_and(State::degraded) || prev_state.is_none();
            if state.degraded() {
                self.status.degraded_events += 1;
            }
            if notable {
                out.push(ev(None));
            } else if matches!(state, State::IdleSaving) || (matches!(state, State::Full) && prev_state == Some(State::IdleSaving)) {
                // the normal idle dance: counted, written now and then
                self.idle_pending += 1;
                self.status.idle_changes += 1;
                if self.idle_logged_at.is_none_or(|l| t_ms - l >= IDLE_LOG_EVERY_MS) {
                    out.push(ev(Some(self.idle_pending)));
                    self.idle_pending = 0;
                    self.idle_logged_at = Some(t_ms);
                }
            }
        }
        self.state = Some(state);
        self.last = Some((gen_, width));
        self.status = LinkStatus {
            gen_,
            width,
            max_gen,
            max_width,
            link: link.clone(),
            best: link_text(max_gen, max_width),
            state: Some(state),
            sentence: match state {
                State::Full | State::Unknown => format!("PCIe {link}, {}.", state.words()),
                _ => format!("PCIe {link} (best {}), {}.", link_text(max_gen, max_width), state.words()),
            },
            since_ms: Some(self.since),
            since: Some(
                crate::telemetry::Sample { t_ms: self.since, ..Default::default() }.local().format("%Y-%m-%d %H:%M:%S").to_string(),
            ),
            best_under_load: self.best_load.map(|(g, w)| link_text(Some(g), Some(w))),
            degraded_events: self.status.degraded_events,
            idle_changes: self.status.idle_changes,
        };
        out
    }
}

pub fn append(dir: &Path, events: &[LinkEvent]) -> std::io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let p = dir.join(FILE);
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
        std::fs::rename(&p, dir.join(OLD))?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
    let mut buf = String::new();
    for e in events {
        buf.push_str(&serde_json::to_string(e).map_err(std::io::Error::other)?);
        buf.push('\n');
    }
    f.write_all(buf.as_bytes())
}

/// Logged events, oldest first (the rotated file too), at most `n` (the newest).
pub fn recent(dir: &Path, n: usize) -> Vec<LinkEvent> {
    let mut v = vec![];
    for name in [OLD, FILE] {
        let Ok(f) = std::fs::File::open(dir.join(name)) else { continue };
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            if line.len() < 4096
                && let Ok(e) = serde_json::from_str::<LinkEvent>(&line)
            {
                v.push(e);
            }
        }
    }
    v.sort_by_key(|e| e.t_ms);
    let k = v.len().saturating_sub(n);
    v.split_off(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_downshift_is_normal_degraded_links_are_events() {
        let mut t = LinkTracker::default();
        let m = (Some(5), Some(16));
        let ev = std::cell::RefCell::new(vec![]);
        let step = |t_: &mut LinkTracker, ms: i64, g: u32, w: u32, u: u8| {
            let e = t_.push(ms, Some(g), Some(w), m.0, m.1, Some(u));
            ev.borrow_mut().extend(e.clone());
            e
        };
        // start at idle: Gen1 x16 is power saving; the first state is written
        let e = step(&mut t, 0, 1, 16, 1);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].state, State::IdleSaving);
        // load: Gen5 x16 within a second, then idle again many times: counted, written rarely
        for k in 0..20 {
            step(&mut t, 1000 + k * 20_000, 5, 16, 99);
            step(&mut t, 11_000 + k * 20_000, 1, 16, 2);
        }
        assert_eq!(t.status.idle_changes, 40);
        assert!(ev.borrow().len() <= 2, "{ev:#?}");
        assert_eq!(t.status.state, Some(State::IdleSaving));
        assert_eq!(t.status.best_under_load.as_deref(), Some("Gen5 x16"));
        // under load the link stays at Gen1: settling, then degraded after 5 s
        let base = 1_000_000;
        assert!(step(&mut t, base, 1, 16, 95).is_empty());
        assert_eq!(t.status.state, Some(State::Settling));
        let e = step(&mut t, base + 6_000, 1, 16, 95);
        assert_eq!(e[0].state, State::DegradedGen);
        assert!(e[0].sentence.contains("stays at Gen1 x16 under load") && e[0].sentence.contains("Gen5 x16"), "{}", e[0].sentence);
        // recovered
        let e = step(&mut t, base + 7_000, 5, 16, 95);
        assert_eq!(e[0].state, State::Full);
        assert!(e[0].sentence.contains("back to full speed"));
        // x8: narrower than the slot, even at idle, after 3 s
        assert!(step(&mut t, base + 10_000, 5, 8, 3).is_empty() || t.status.state != Some(State::DegradedWidth));
        let e = step(&mut t, base + 14_000, 5, 8, 3);
        assert_eq!(e[0].state, State::DegradedWidth);
        assert!(e[0].sentence.contains("x8") && e[0].sentence.contains("Gen5 x16"));
        assert_eq!(t.status.degraded_events, 2);
        // the file keeps them
        let d = std::env::temp_dir().join(format!("tachyon_pcie_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let ev = ev.into_inner();
        append(&d, &ev).unwrap();
        let back = recent(&d, 100);
        assert_eq!(back, ev);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_maximum_is_not_called_degraded() {
        let mut t = LinkTracker::default();
        for i in 0..10 {
            t.push(i * 1000, Some(3), Some(8), None, None, Some(99));
        }
        assert_eq!(t.status.state, Some(State::Unknown));
        assert_eq!(t.status.degraded_events, 0);
        assert!(t.push(20_000, None, None, None, None, None).is_empty());
    }
}
