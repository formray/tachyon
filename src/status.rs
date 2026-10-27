//! One status scale for every verdict Tachyon shows: all fine, worth a look, act now. The GUI
//! colours the verdict dot and accent from it (green, amber, red) and always prints the words too,
//! so the colour is never the only signal.
use crate::markers::Tone;
use crate::preflight::Fit;
use crate::report::Severity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Nothing to do.
    Fine,
    /// Something to check when convenient.
    Look,
    /// Act before the next job.
    Act,
}

impl Status {
    /// The words shown next to the coloured dot.
    pub fn word(self) -> &'static str {
        match self {
            Status::Fine => "All fine",
            Status::Look => "Worth a look",
            Status::Act => "Act now",
        }
    }

    /// The worse of two.
    pub fn max(self, o: Status) -> Status {
        std::cmp::max(self, o)
    }
}

impl From<Severity> for Status {
    /// A crash report: a plain close is fine, a crash or a restart is worth a look, a bluescreen
    /// or a hardware error means act now.
    fn from(s: Severity) -> Self {
        match s {
            Severity::Note => Status::Fine,
            Severity::Warning => Status::Look,
            Severity::Critical => Status::Act,
        }
    }
}

impl From<Tone> for Status {
    /// A marked job: nothing held it back (power limit under full load is normal), or something
    /// to check (heat, VRAM nearly full, a slow PCIe link).
    fn from(t: Tone) -> Self {
        match t {
            Tone::Note => Status::Fine,
            Tone::Warning => Status::Look,
        }
    }
}

impl From<Fit> for Status {
    /// The pre-render check: will not fit means act now (before starting it).
    fn from(f: Fit) -> Self {
        match f {
            Fit::Fits | Fit::Unknown => Status::Fine,
            Fit::Tight => Status::Look,
            Fit::WontFit => Status::Act,
        }
    }
}

/// The live bottleneck: a problem worth a look (decoder, CPU, VRAM, heat, PCIe) or fine.
pub fn of_bottleneck(problem: bool) -> Status {
    if problem { Status::Look } else { Status::Fine }
}

/// The PCIe link: slower than the slot allows under load is worth a look.
pub fn of_pcie(degraded: bool) -> Status {
    if degraded { Status::Look } else { Status::Fine }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_mapping_for_every_verdict() {
        assert_eq!(Status::from(Severity::Note), Status::Fine);
        assert_eq!(Status::from(Severity::Warning), Status::Look);
        assert_eq!(Status::from(Severity::Critical), Status::Act);
        assert_eq!(Status::from(Tone::Note), Status::Fine);
        assert_eq!(Status::from(Tone::Warning), Status::Look);
        assert_eq!(Status::from(Fit::Fits), Status::Fine);
        assert_eq!(Status::from(Fit::Unknown), Status::Fine);
        assert_eq!(Status::from(Fit::Tight), Status::Look);
        assert_eq!(Status::from(Fit::WontFit), Status::Act);
        assert_eq!(of_bottleneck(true), Status::Look);
        assert_eq!(of_pcie(false), Status::Fine);
        assert_eq!(Status::Fine.max(Status::Act), Status::Act);
        // every status has words, all different
        let w: Vec<&str> = [Status::Fine, Status::Look, Status::Act].iter().map(|s| s.word()).collect();
        assert_eq!(w, ["All fine", "Worth a look", "Act now"]);
    }

    /// The real 21:38 stop is "all fine"; the real demo jobs too.
    #[test]
    fn the_real_reports_map_to_fine() {
        let mut r: crate::report::Report = serde_json::from_str(include_str!("../tests/samples/report_2138_v12draft.json")).unwrap();
        r.revise();
        assert_eq!(Status::from(r.severity()), Status::Fine);
        let reps: Vec<crate::markers::JobReport> =
            include_str!("../tests/samples/marked_jobs_2026-10-26.jsonl").lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        assert!(reps.len() >= 2);
        for j in &reps {
            assert_eq!(Status::from(j.verdict().1), Status::Fine, "{}", j.job);
        }
    }
}
