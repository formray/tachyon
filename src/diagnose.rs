//! Automatic crash diagnosis: after an unexpected restart (or a GPU driver failure), plain rules
//! weigh what the flight recorder saw just before (temperature, power, load, clocks), the Windows
//! events (bug check codes, NVIDIA driver resets, GPU timeout reports, WHEA hardware errors,
//! application crashes), the GPU's overclock indicators and the memory speed, and rank the
//! probable causes with a rough percentage, the evidence and one piece of advice each.
//! Transparent and conservative: every point comes from a listed piece of evidence, part of the
//! weight is always kept for "something else", the top cause is never above 85 % and the wording
//! is "likely" or "possibly", never certain. Read-only: it never changes a setting.
use crate::events::{APP_ERROR, KERNEL_POWER, LIVE_KERNEL, SysEvent, WHEA};
use crate::nvml::OcInfo;
use crate::report::{Cause, Holder, Peaks};
use crate::sysinfo::{RamSpeed, RamState};
use crate::telemetry::Sample;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suspect {
    Ram,
    GpuOverclock,
    Power,
    Heat,
    DriverTimeout,
    Application,
    /// The whole PC lost power or was switched off: the wall, the PSU switch, a manual power-off
    /// or a reset from the motherboard, not something the GPU did.
    PowerCut,
}

const ALL: [Suspect; 7] =
    [Suspect::Ram, Suspect::GpuOverclock, Suspect::Power, Suspect::Heat, Suspect::DriverTimeout, Suspect::Application, Suspect::PowerCut];

impl Suspect {
    pub fn label(self) -> &'static str {
        match self {
            Suspect::Ram => "Unstable RAM or memory overclock (EXPO/XMP)",
            Suspect::GpuOverclock => "GPU core or memory overclock",
            Suspect::Power => "GPU power delivery or the power supply",
            Suspect::Heat => "Overheating",
            Suspect::DriverTimeout => "GPU driver timeout (TDR) from a long GPU task",
            Suspect::Application => "A bug in the application",
            Suspect::PowerCut => "A whole-system power loss or a manual power-off",
        }
    }
    /// Short label for the panel and Hex.
    pub fn short(self) -> &'static str {
        match self {
            Suspect::Ram => "unstable RAM / EXPO",
            Suspect::GpuOverclock => "GPU overclock",
            Suspect::Power => "power / PSU",
            Suspect::Heat => "overheating",
            Suspect::DriverTimeout => "driver timeout (TDR)",
            Suspect::Application => "application bug",
            Suspect::PowerCut => "power cut / switched off",
        }
    }
    fn idx(self) -> usize {
        ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// What else is known about the PC, beyond the report.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub oc: Option<OcInfo>,
    /// The overclock reading was taken after the restart, not before the crash.
    pub oc_after: bool,
    pub ram: Option<RamSpeed>,
    /// Minutes the clock seemed to jump at the restart (positive: the clock after the restart ran
    /// ahead of the real time), from the recorder, the Event Log or a Windows clock correction.
    pub clock_shift_min: Option<i64>,
    /// How long the BIOS/UEFI took at the last start (ms, Windows' "Last BIOS time").
    pub firmware_boot_ms: Option<u32>,
    /// The restart diagnosed is the current boot, so the RAM speed and BIOS time read now are
    /// the ones right after it.
    pub just_after: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranked {
    pub suspect: Suspect,
    pub label: String,
    /// Rough share of the evidence, rounded to 5 %.
    pub percent: u8,
    pub evidence: Vec<String>,
    pub advice: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnosis {
    /// Most probable first; only causes with 10 % or more.
    pub causes: Vec<Ranked>,
    /// "likely" or "possibly" ("unclear" when there is too little to go on).
    pub confidence: String,
    pub sentence: String,
    /// What looked normal (rules a cause down).
    pub normal: Vec<String>,
    pub note: String,
    /// Other things worth knowing, not causes (e.g. the BIOS settings were reset).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

impl Diagnosis {
    pub fn top(&self) -> Option<&Ranked> {
        self.causes.first()
    }
    /// One line for Hex: "Likely cause: unstable RAM / EXPO (45%)."
    pub fn short_line(&self) -> String {
        match self.top() {
            Some(t) => {
                let w = if self.confidence == "likely" { "Likely" } else { "Possible" };
                format!("{w} cause: {} ({}%).", t.suspect.short(), t.percent)
            }
            None => "No clear cause in the evidence.".into(),
        }
    }
}

/// Weight kept for causes Tachyon cannot see.
const RESERVE: f32 = 1.0;
/// Below this much total evidence there is no ranking.
const MIN_EVIDENCE: f32 = 1.5;
const TOP_CAP: u8 = 85;

#[derive(Default)]
struct Board {
    s: [f32; 7],
    ev: [Vec<String>; 7],
}

impl Board {
    fn add(&mut self, who: Suspect, pts: f32, why: &str) {
        let i = who.idx();
        self.s[i] += pts;
        if !why.is_empty() && !self.ev[i].iter().any(|e| e == why) {
            self.ev[i].push(why.to_string());
        }
    }
    fn get(&self, who: Suspect) -> f32 {
        self.s[who.idx()]
    }
}

/// Name of a bug check code, for the evidence lines.
pub fn bugcheck_name(code: u32) -> Option<&'static str> {
    Some(match code {
        0x0A => "IRQL_NOT_LESS_OR_EQUAL",
        0x19 => "BAD_POOL_HEADER",
        0x1A => "MEMORY_MANAGEMENT",
        0x1E => "KMODE_EXCEPTION_NOT_HANDLED",
        0x3B => "SYSTEM_SERVICE_EXCEPTION",
        0x4E => "PFN_LIST_CORRUPT",
        0x50 => "PAGE_FAULT_IN_NONPAGED_AREA",
        0x7E => "SYSTEM_THREAD_EXCEPTION_NOT_HANDLED",
        0x9F => "DRIVER_POWER_STATE_FAILURE",
        0x10E => "VIDEO_MEMORY_MANAGEMENT_INTERNAL",
        0x113 => "VIDEO_DXGKRNL_FATAL_ERROR",
        0x116 => "VIDEO_TDR_FAILURE",
        0x117 => "VIDEO_TDR_TIMEOUT_DETECTED",
        0x119 => "VIDEO_SCHEDULER_INTERNAL_ERROR",
        0x124 => "WHEA_UNCORRECTABLE_ERROR",
        0x12B => "FAULTY_HARDWARE_CORRUPTED_PAGE",
        0x139 => "KERNEL_SECURITY_CHECK_FAILURE",
        0xC2 => "BAD_POOL_CALLER",
        0xD1 => "DRIVER_IRQL_NOT_LESS_OR_EQUAL",
        0xEF => "CRITICAL_PROCESS_DIED",
        _ => return None,
    })
}

fn bc_text(code: u32) -> String {
    match bugcheck_name(code) {
        Some(n) => format!("blue screen 0x{code:X} ({n})"),
        None => format!("blue screen 0x{code:X}"),
    }
}

/// 1 for the first event of a kind, a quarter more for each repeat (at most two).
fn repeat_factor(n: usize) -> f32 {
    1.0 + 0.25 * (n.saturating_sub(1).min(2)) as f32
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

/// What the recorder saw in the last minute, when the GPU was idle: (peak load %, peak W,
/// limit W, peak °C). Idle = at most 15 % load and a quarter of the power limit (80 W without one).
pub fn idle_before(peaks: &Peaks, last_minute: &[Sample]) -> Option<(u8, f32, Option<f32>, Option<u8>)> {
    let p = if last_minute.is_empty() { peaks.clone() } else { crate::report::peaks(last_minute) };
    let u = p.gpu_util?;
    let w = p.power_w?;
    let cap = p.power_limit_w.filter(|l| *l > 0.0).map_or(80.0, |l| l * 0.25);
    (u <= 15 && w <= cap).then_some((u, w, p.power_limit_w, p.temp))
}

fn idle_text(i: (u8, f32, Option<f32>, Option<u8>)) -> String {
    let (u, w, l, t) = i;
    let lim = l.map(|l| format!(" of its {l:.0} W limit")).unwrap_or_default();
    let temp = t.map(|t| format!(", {t} °C")).unwrap_or_default();
    format!("GPU at most {u}% busy, {w:.0} W{lim}{temp}")
}

/// Rank the probable causes. `last_minute` are the samples just before the stop (may be empty
/// for an older report: then only `peaks` are used). None when there is nothing to explain
/// (Tachyon was just closed and Windows logged nothing relevant).
pub fn diagnose(
    cause: Cause,
    events: &[SysEvent],
    events_readable: bool,
    peaks: &Peaks,
    last_minute: &[Sample],
    holder: Option<&Holder>,
    ctx: &Context,
) -> Option<Diagnosis> {
    let mut b = Board::default();
    let mut normal: Vec<String> = vec![];
    let count = |f: &dyn Fn(&SysEvent) -> bool| events.iter().filter(|e| f(e)).count();

    // --- blue screens (Kernel-Power 41 and BugCheck 1001 report the same code: count it once)
    let mut codes: Vec<u32> = events.iter().filter_map(|e| e.bugcheck()).collect();
    codes.sort_unstable();
    codes.dedup();
    let mut memory_evidence = false;
    let mut gpu_evidence = false;
    for &c in &codes {
        let t = bc_text(c);
        match c {
            0x1A | 0x4E | 0x12B | 0x19 => {
                b.add(Suspect::Ram, 3.0, &format!("{t}: system memory contents were corrupted"));
                memory_evidence = true;
            }
            0x50 | 0x3B | 0x0A | 0xD1 | 0x139 | 0xC2 | 0x7E | 0x1E => {
                b.add(Suspect::Ram, 2.0, &format!("{t}: often unstable RAM, sometimes a faulty driver"));
                memory_evidence = true;
            }
            0x124 => {
                b.add(Suspect::Ram, 2.0, &format!("{t}: the hardware reported a fatal error (CPU, memory or an unstable overclock)"));
                b.add(Suspect::Power, 1.5, &format!("{t}: can also be unstable power"));
                memory_evidence = true;
            }
            0x116 | 0x117 => {
                b.add(Suspect::GpuOverclock, 2.0, &format!("{t}: the GPU stopped responding and could not be reset"));
                b.add(Suspect::DriverTimeout, 2.0, &format!("{t}: a GPU timeout"));
                b.add(Suspect::Power, 1.0, &format!("{t}: can also be the GPU's power supply"));
                gpu_evidence = true;
            }
            0x10E | 0x113 | 0x119 => {
                b.add(Suspect::GpuOverclock, 1.5, &format!("{t}: a GPU driver failure"));
                b.add(Suspect::DriverTimeout, 1.0, &t);
                b.add(Suspect::Ram, 0.5, &t);
                gpu_evidence = true;
            }
            _ => {}
        }
    }

    // --- power loss or a hard freeze: Kernel-Power 41 without any bug check code
    let kp41 = count(&|e| e.provider == KERNEL_POWER && e.id == 41);
    let hard_off = kp41 > 0 && codes.is_empty();
    let idle = idle_before(peaks, last_minute);
    if hard_off {
        match idle {
            // the GPU was resting: it did not overload the PSU; the whole PC lost power or was switched off
            Some(i) => {
                let it = idle_text(i);
                b.add(
                    Suspect::PowerCut,
                    3.0,
                    &format!("Windows went off with no blue screen (Kernel-Power 41, code 0) while the PC was idle ({it})"),
                );
                b.add(
                    Suspect::Power,
                    1.0,
                    &format!(
                        "a power-off without a blue screen can be the PSU, but the GPU drew only {:.0} W, far from a load that trips it",
                        i.1
                    ),
                );
                b.add(Suspect::Ram, 0.5, "a hard freeze can also be unstable RAM, but it is rare at idle");
                normal.push(format!("The GPU was idle just before the stop ({it})."));
            }
            None => {
                b.add(Suspect::Power, 3.0, "Windows lost power or froze with no blue screen (Kernel-Power 41, code 0)");
                b.add(Suspect::Ram, 1.0, "a hard freeze can also be unstable RAM");
                b.add(Suspect::Heat, 0.5, "");
                if last_minute.is_empty() && peaks.gpu_util.is_none() {
                    b.add(Suspect::PowerCut, 1.0, "with no blue screen, the PC may also have lost mains power or been switched off");
                }
            }
        }
    }

    // --- WHEA hardware errors
    for (id, ram, power, gpu, why) in [
        (47u32, 3.0f32, 0.0f32, 0.0f32, "memory errors (WHEA-Logger 47)"),
        (19, 2.0, 0.0, 0.0, "corrected CPU or interconnect errors (WHEA-Logger 19), on Ryzen often from memory or Infinity Fabric speed"),
        (18, 2.0, 1.5, 0.0, "fatal hardware errors (WHEA-Logger 18)"),
        (17, 0.0, 0.5, 0.5, "PCIe link errors (WHEA-Logger 17)"),
    ] {
        let n = count(&|e| e.provider == WHEA && e.id == id);
        if n > 0 {
            let f = repeat_factor(n);
            let why = format!("{}: {why}", plural(n, "event"));
            if ram > 0.0 {
                b.add(Suspect::Ram, ram * f, &why);
                memory_evidence = true;
            }
            if power > 0.0 {
                b.add(Suspect::Power, power * f, &why);
            }
            if gpu > 0.0 {
                b.add(Suspect::GpuOverclock, gpu * f, &why);
            }
        }
    }

    // --- NVIDIA driver and GPU timeouts
    let nv = |id: u32| count(&|e| e.provider == "nvlddmkm" && e.id == id);
    let n153 = nv(153);
    if n153 > 0 {
        let f = repeat_factor(n153);
        let times = if n153 == 1 { "once".to_string() } else { format!("{n153} times") };
        let why = format!("the NVIDIA driver reset the GPU {times} after it stopped responding (nvlddmkm 153)");
        b.add(Suspect::GpuOverclock, 1.5 * f, &why);
        b.add(Suspect::DriverTimeout, 1.5 * f, &why);
        b.add(Suspect::Ram, 0.5, "GPU resets can also follow general system instability");
        gpu_evidence = true;
    }
    let n1314 = nv(13) + nv(14);
    if n1314 > 0 {
        let why =
            format!("{} (nvlddmkm 13/14): often a GPU memory overclock, sometimes the application", plural(n1314, "graphics engine error"));
        b.add(Suspect::GpuOverclock, 2.0 * repeat_factor(n1314), &why);
        b.add(Suspect::Application, 1.0, &why);
        gpu_evidence = true;
    }
    let nother = count(&|e| e.provider == "nvlddmkm" && !matches!(e.id, 13 | 14 | 153));
    if nother > 0 {
        b.add(Suspect::GpuOverclock, 0.5, &format!("{} logged by the NVIDIA driver", plural(nother, "other error")));
        gpu_evidence = true;
    }
    let n4101 = count(&|e| e.provider == "Display" && e.id == 4101);
    if n4101 > 0 {
        let why = format!("{} (Display 4101)", plural(n4101, "GPU timeout"));
        b.add(Suspect::DriverTimeout, 2.0 * repeat_factor(n4101), &why);
        b.add(Suspect::GpuOverclock, 1.0, &why);
        gpu_evidence = true;
    }
    let lk141 = count(&|e| e.provider == LIVE_KERNEL && e.id == 0x141);
    let lk117 = count(&|e| e.provider == LIVE_KERNEL && e.id == 0x117);
    if lk141 + lk117 > 0 {
        let why = format!("{} saved by Windows (LiveKernelEvent 141/117)", plural(lk141 + lk117, "GPU timeout report"));
        b.add(Suspect::DriverTimeout, 2.0 * repeat_factor(lk141 + lk117), &why);
        b.add(Suspect::GpuOverclock, if lk141 > 0 { 1.5 } else { 1.0 }, &why);
        gpu_evidence = true;
    }

    // --- an application crashed
    let main_proc = holder.map(|h| h.name.to_ascii_lowercase());
    let mut crashed: Vec<String> = events
        .iter()
        .filter(|e| e.provider == APP_ERROR)
        .filter_map(|e| e.code.clone())
        .filter(|a| !a.eq_ignore_ascii_case("tachyon.exe"))
        .collect();
    crashed.dedup();
    for a in crashed.iter().take(3) {
        if main_proc.as_deref() == Some(&a.to_ascii_lowercase()) {
            b.add(Suspect::Application, 3.0, &format!("{a}, the main GPU process, crashed (Application Error 1000)"));
        } else {
            b.add(Suspect::Application, 1.5, &format!("{a} crashed (Application Error 1000)"));
        }
    }

    // --- what the flight recorder saw in the last minute
    if let Some(t) = peaks.temp {
        if t >= 90 || peaks.thermal_throttled_s >= 10 {
            b.add(Suspect::Heat, 2.5, &format!("the GPU reached {t} °C and slowed down from heat for {} s", peaks.thermal_throttled_s));
        } else if t >= 85 {
            b.add(Suspect::Heat, 1.0, &format!("the GPU was warm ({t} °C)"));
        } else if t < 80 {
            normal.push(format!("GPU temperature was normal (at most {t} °C)."));
        }
    }
    let power_frac = match (peaks.power_w, peaks.power_limit_w) {
        (Some(p), Some(l)) if l > 0.0 => Some((p, l, p / l)),
        _ => None,
    };
    if let Some((p, l, f)) = power_frac {
        if f >= 0.95 {
            b.add(Suspect::Power, 1.0, &format!("the GPU drew {p:.0} W of its {l:.0} W limit just before"));
            if hard_off {
                b.add(Suspect::Power, 1.5, "a power-off at full GPU load often points to the PSU or its cables");
            }
        } else if f >= 0.9 && hard_off {
            b.add(Suspect::Power, 1.0, &format!("the GPU drew {p:.0} W of {l:.0} W when the PC went off"));
        } else if f < 0.6 {
            normal.push(format!("GPU power was moderate ({p:.0} of {l:.0} W)."));
        }
    }
    let busy = peaks.gpu_util.is_some_and(|u| u >= 95);
    if busy && b.get(Suspect::DriverTimeout) > 0.0 {
        b.add(Suspect::DriverTimeout, 1.0, "the GPU was fully busy: one long GPU task can exceed Windows' 2-second timeout");
    }
    if let (Some(c), Some(l)) = (peaks.commit_mb, peaks.commit_limit_mb)
        && l > 0.0
        && c / l >= 0.95
    {
        b.add(Suspect::Application, 1.0, "the system memory commit was nearly full");
    }

    // --- GPU overclock indicators
    let when = if ctx.oc_after { " (read after the restart)" } else { "" };
    match &ctx.oc {
        Some(oc) if !oc.is_empty() => {
            if let Some(c) = oc.core_offset_mhz.filter(|c| *c > 0) {
                b.add(Suspect::GpuOverclock, 2.0, &format!("GPU core clock offset +{c} MHz{when}"));
                if gpu_evidence {
                    b.add(Suspect::GpuOverclock, 1.0, "GPU driver errors together with a GPU overclock");
                }
            }
            if let Some(m) = oc.mem_offset_mhz.filter(|m| *m > 0) {
                b.add(Suspect::GpuOverclock, 2.0, &format!("GPU memory clock offset +{m} MHz{when}"));
                if gpu_evidence && oc.core_offset_mhz.unwrap_or(0) <= 0 {
                    b.add(Suspect::GpuOverclock, 1.0, "GPU driver errors together with a GPU memory overclock");
                }
            }
            if let Some(p) = oc.power_limit_pct().filter(|p| *p > 101.0) {
                b.add(Suspect::GpuOverclock, 0.5, &format!("GPU power limit raised to {p:.0}%{when}"));
                b.add(Suspect::Power, 0.5, &format!("GPU power limit raised to {p:.0}%"));
            }
            let stock = oc.core_offset_mhz.is_some_and(|c| c <= 0)
                && oc.mem_offset_mhz.is_some_and(|m| m <= 0)
                && oc.power_limit_pct().is_none_or(|p| p <= 101.0);
            if stock {
                normal.push(format!("The GPU runs at stock settings (no clock offsets, default power limit){when}."));
                b.s[Suspect::GpuOverclock.idx()] *= 0.5;
            }
            // with no offset readings, a boost clock far above the rated one is a weak hint
            if oc.core_offset_mhz.is_none()
                && let Some(rated) = oc.rated_boost_mhz.filter(|r| *r > 0)
                && let Some(peak) = last_minute.iter().filter_map(|s| s.clk_sm).max()
                && peak as f32 > rated as f32 * 1.2
            {
                b.add(
                    Suspect::GpuOverclock,
                    0.5,
                    &format!("the GPU clock reached {peak} MHz, {:.0}% above its rated boost", (peak as f32 / rated as f32 - 1.0) * 100.0),
                );
            }
        }
        _ => {}
    }

    // --- memory speed
    if let Some(r) = &ctx.ram {
        let c = r.configured_mts.unwrap_or(0);
        let k = r.kit_mts().unwrap_or(0);
        let std = r.rated_mts.map(|j| format!(", standard {j}")).unwrap_or_default();
        // read now, after the restart: the BIOS settings may differ from the day of the crash
        let fast = match r.state() {
            RamState::BeyondRating => {
                Some((1.5, 2.0, format!("the RAM runs at {c} MT/s now, faster than the kit's rated EXPO/XMP {k} (beyond its rating)")))
            }
            RamState::Profile => Some((0.5, 1.0, format!("the RAM runs an EXPO/XMP profile: {c} MT/s now (kit rated {k}{std})"))),
            RamState::AboveStandard => match r.rated_mts.filter(|j| c > j + 100) {
                Some(j) => {
                    Some((1.5, 2.0, format!("the RAM runs at {c} MT/s now, above its standard {j} (EXPO/XMP or a manual overclock)")))
                }
                None => Some((1.0, 1.5, format!("the RAM runs at {c} MT/s now, above the speed most CPUs officially support"))),
            },
            RamState::Standard => {
                if c > 0 {
                    normal.push(format!("The RAM runs at a standard speed now ({} at {c} MT/s).", r.kind));
                }
                None
            }
            // slower than standard: no RAM overclock now (the BIOS hint below says why)
            RamState::BelowStandard => None,
        };
        if let Some((w, wm, ev)) = fast {
            b.add(Suspect::Ram, w, &ev);
            if memory_evidence {
                let why = if r.state() == RamState::BeyondRating {
                    "memory-related errors with the RAM beyond its rated speed"
                } else {
                    "memory-related errors while a fast memory profile is in use"
                };
                b.add(Suspect::Ram, wm, why);
            }
        }
        if let Some(cr) = r.crowded() {
            b.add(Suspect::Ram, if memory_evidence { 1.0 } else { 0.5 }, &cr);
        }
    }

    // --- nothing logged at all
    if cause == Cause::Restart && events_readable && events.iter().all(|e| e.clock_change_s().is_some()) {
        let who = if idle.is_some() { Suspect::PowerCut } else { Suspect::Power };
        b.add(who, 1.5, "no blue screen or driver error was logged: the PC just went off");
        b.add(Suspect::Ram, 0.5, "");
    }

    // --- the BIOS settings were reset (RAM below its standard speed, the clock jumped, a slow BIOS start)
    let mut hints = vec![];
    let mut signs = vec![];
    if let Some(r) = ctx.ram.as_ref().filter(|r| r.state() == RamState::BelowStandard) {
        let kit = r.kit_mts().map(|k| format!(" and the kit's EXPO/XMP {k}")).unwrap_or_default();
        signs.push(format!(
            "the RAM runs at {} MT/s now, below its {} MT/s standard{kit}",
            r.configured_mts.unwrap_or(0),
            r.rated_mts.unwrap_or(0)
        ));
    }
    // Windows correcting its clock by 30 min or more after the restart: it had run ahead (or behind) by that much
    let corrected = events.iter().filter_map(|e| e.clock_change_s()).filter(|d| d.abs() >= 1800).max_by_key(|d| d.abs()).map(|d| -d / 60);
    if let Some(m) = ctx.clock_shift_min.or(corrected).filter(|m| m.abs() >= 30) {
        let h = (m.abs() as f32 / 60.0 * 2.0).round() / 2.0;
        let dir = if m > 0 { "ahead" } else { "behind" };
        let amount = if (h - h.round()).abs() < 0.01 { format!("{h:.0} h") } else { format!("{h:.1} h") };
        signs.push(format!("the clock after the restart was about {amount} {dir}"));
    }
    if ctx.just_after
        && let Some(f) = ctx.firmware_boot_ms.filter(|f| *f >= 45_000)
    {
        signs.push(format!("the BIOS took {} s to start (memory training after a reset takes longer)", f / 1000));
    }
    let ram_reset = ctx.ram.as_ref().is_some_and(|r| r.state() == RamState::BelowStandard);
    if !signs.is_empty() && (ram_reset || signs.len() >= 2 || (ctx.just_after && cause == Cause::Restart)) {
        let when = if ctx.just_after && cause == Cause::Restart { " at this restart" } else { "" };
        let what = if ram_reset { "BIOS likely reset to safe defaults (EXPO/XMP off)" } else { "BIOS settings possibly reset" };
        hints.push(format!(
            "{what}{when}: {}. Check the BIOS: turn EXPO/XMP back on if you use it, and the date and time.",
            signs.join("; ")
        ));
        if hard_off && ctx.just_after {
            b.add(
                Suspect::PowerCut,
                0.5,
                "the BIOS settings were reset at the same time, as after a power cut at the board, a CMOS clear or a BIOS FlashBack reset",
            );
        }
    } else if !signs.is_empty() {
        hints.push(format!("Worth knowing: {}.", signs.join("; ")));
    }

    let total: f32 = b.s.iter().sum();
    let relevant = !codes.is_empty() || hard_off || gpu_evidence || memory_evidence || !crashed.is_empty();
    if cause == Cause::AppClosed && !relevant {
        return None;
    }
    let note = "Tachyon only reads: it changes no settings. These are estimates from the evidence listed, not certainties.".to_string();
    if !events_readable {
        normal.push("The Windows event log could not be read, so this rests on the readings only.".into());
    }
    if total < MIN_EVIDENCE {
        return Some(Diagnosis {
            causes: vec![],
            confidence: "unclear".into(),
            sentence: "Not enough evidence for a probable cause.".into(),
            normal,
            note,
            hints,
        });
    }
    let reserve = RESERVE + if events_readable { 0.0 } else { 2.0 };
    let mut ranked: Vec<(Suspect, f32)> = ALL.iter().map(|s| (*s, b.get(*s))).filter(|(_, v)| *v > 0.0).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let denom = total + reserve;
    let mut causes = vec![];
    for (s, v) in ranked {
        let pct = ((v / denom * 100.0 / 5.0).round() * 5.0).clamp(0.0, TOP_CAP as f32) as u8;
        if pct < 10 {
            continue;
        }
        causes.push(Ranked {
            suspect: s,
            label: s.label().into(),
            percent: pct,
            evidence: b.ev[s.idx()].clone(),
            advice: advice(s, ctx, peaks, &crashed),
        });
    }
    let (top, second) = (causes.first().map_or(0, |c| c.percent), causes.get(1).map_or(0, |c| c.percent));
    let confidence = if top >= 40 && top >= second + 10 { "likely" } else { "possibly" };
    let sentence = match (causes.first(), causes.get(1)) {
        (Some(a), Some(b2)) if confidence == "possibly" => format!(
            "Possible cause: {}, about {}%; or {}, about {}%.",
            lower_first(&a.label),
            a.percent,
            lower_first(&b2.label),
            b2.percent
        ),
        (Some(a), _) => format!(
            "{} cause: {}, about {}%.",
            if confidence == "likely" { "Likely" } else { "Possible" },
            lower_first(&a.label),
            a.percent
        ),
        _ => "Not enough evidence for a probable cause.".into(),
    };
    Some(Diagnosis {
        confidence: if causes.is_empty() { "unclear".into() } else { confidence.into() },
        causes,
        sentence,
        normal,
        note,
        hints,
    })
}

fn lower_first(s: &str) -> String {
    // keep acronyms (GPU, RAM) as they are
    let mut c = s.chars();
    match c.next() {
        Some(f) if s.split(' ').next().is_some_and(|w| w.chars().skip(1).all(|x| x.is_lowercase())) => f.to_lowercase().chain(c).collect(),
        _ => s.to_string(),
    }
}

fn advice(s: Suspect, ctx: &Context, peaks: &Peaks, crashed: &[String]) -> String {
    match s {
        Suspect::Ram => {
            const TEST: &str = "a memory test (TestMem5, OCCT or MemTest86)";
            let Some(r) = ctx.ram.as_ref().filter(|r| r.configured_mts.is_some()) else {
                return format!("Run {TEST}; if you use EXPO/XMP or a CPU overclock, try the BIOS defaults for a while.");
            };
            let c = r.configured_mts.unwrap_or(0);
            let four = if r.crowded().is_some() { " Four modules are harder to run fast than two." } else { "" };
            match r.state() {
                RamState::BeyondRating => format!(
                    "Set the RAM back to the kit's rated speed ({} MT/s) or lower in the BIOS, then run {TEST}.{four}",
                    r.kit_mts().unwrap_or(0)
                ),
                RamState::Profile | RamState::AboveStandard => format!(
                    "The RAM runs at {c} MT/s now; the settings may have changed since this crash. If you already lowered it and it has been stable since, no action is needed. Otherwise run {TEST}, and only if it finds errors try one step slower ({c} → {} MT/s) or EXPO/XMP off.{four}",
                    c.saturating_sub(400)
                ),
                RamState::Standard => format!("The RAM runs at a standard speed ({c} MT/s), so run {TEST} to check the modules themselves."),
                RamState::BelowStandard => format!(
                    "The RAM runs at {c} MT/s now, below its standard: EXPO/XMP is off, which is the safe setting. Run {TEST} if crashes come back; turn EXPO back on in the BIOS when you want the speed again."
                ),
            }
        }

        Suspect::GpuOverclock => {
            "Put the GPU back to stock (in MSI Afterburner or similar: Reset), memory offset first, and see whether the crashes stop.".into()
        }
        Suspect::Power => "Check that the GPU power cables are fully seated (one cable per connector, no splitters) and the PSU has headroom; for a test, set the GPU power limit to 90%.".into(),
        Suspect::Heat => format!(
            "Clean the dust filters and fans and check the case airflow{}.",
            peaks.temp.map(|t| format!("; the GPU reached {t} °C")).unwrap_or_default()
        ),
        Suspect::DriverTimeout => "One GPU task ran longer than Windows allows (2 s). Smaller batches or tiles usually fix it; if it happens in many apps, reinstall the driver cleanly.".into(),
        Suspect::PowerCut => "If nobody switched it off: check the wall socket, the power strip and the PSU's switch and cable, and what changed at that moment (something plugged in or unplugged, e.g. a USB stick in the rear BIOS FlashBack port, or the power or reset button). If it happens again at idle with nothing touched, have the PSU tested.".into(),
        Suspect::Application => match crashed.first() {
            Some(a) => format!("The crash came from {a}: update it and its GPU libraries (CUDA, PyTorch and so on) and check its log."),
            None => "Update the application and its GPU libraries and check its log.".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{KERNEL_POWER, WER_SYSTEM};

    const T: i64 = 1_790_000_000_000;

    fn ev(dt_s: i64, provider: &str, id: u32, code: Option<&str>) -> SysEvent {
        SysEvent { t_ms: T + dt_s * 1000, provider: provider.into(), id, level: 2, code: code.map(str::to_string), record: 0 }
    }

    fn peaks(util: u8, temp: u8, power: f32) -> Peaks {
        Peaks {
            gpu_util: Some(util),
            temp: Some(temp),
            power_w: Some(power),
            power_limit_w: Some(360.0),
            vram_mb: Some(14_900.0),
            vram_total_mb: Some(16_303.0),
            ..Default::default()
        }
    }

    fn python() -> Holder {
        Holder { name: "python.exe".into(), category: "AI/agent".into(), vram_mb: 12_000.0 }
    }

    fn stock() -> OcInfo {
        OcInfo {
            core_offset_mhz: Some(0),
            mem_offset_mhz: Some(0),
            power_limit_w: Some(360.0),
            default_power_limit_w: Some(360.0),
            rated_boost_mhz: Some(2617),
        }
    }

    fn ram(rated: u32, conf: u32) -> RamSpeed {
        RamSpeed { kind: "DDR5".into(), rated_mts: Some(rated), configured_mts: Some(conf), modules: 2, part: None }
    }

    fn pct(d: &Diagnosis, s: Suspect) -> u8 {
        d.causes.iter().find(|c| c.suspect == s).map_or(0, |c| c.percent)
    }

    /// The case this was modelled on: nvlddmkm 153 resets, then a 0x1A blue screen with python.exe
    /// on the GPU; EXPO 6000 RAM (fixed at 5600) and an Afterburner memory overclock as runner-up.
    /// Giuseppe's PC: a 6000 EXPO kit (4 modules from two kits) now at 5600, crash days earlier.
    #[test]
    fn expo_kit_at_or_below_its_rating_is_a_profile_not_an_overclock() {
        let events = vec![ev(-300, "nvlddmkm", 153, None), ev(60, KERNEL_POWER, 41, Some("0x0000001A"))];
        let kit = |conf| RamSpeed { modules: 4, part: Some("CMK32GX5M2B6000Z30".into()), ..ram(4800, conf) };
        let ctx = Context { oc: None, oc_after: true, ram: Some(kit(5600)), ..Default::default() };
        let d = diagnose(Cause::Restart, &events, true, &peaks(92, 66, 264.0), &[], None, &ctx).unwrap();
        let r = d.causes.iter().find(|c| c.suspect == Suspect::Ram).expect("ram");
        let ev = r.evidence.join(" | ");
        assert!(ev.contains("EXPO/XMP profile: 5600 MT/s now (kit rated 6000, standard 4800)"), "{ev}");
        assert!(ev.contains("4 DDR5 modules at 5600 MT/s, from 2 separate kits"), "{ev}");
        assert!(!ev.contains("above its") && !ev.contains("beyond"), "{ev}");
        assert!(r.advice.contains("If you already lowered it and it has been stable since, no action is needed"), "{}", r.advice);
        assert!(r.advice.contains("only if it finds errors try one step slower (5600 → 5200"), "{}", r.advice);
        assert!(!r.advice.starts_with("Run the RAM one step slower"), "{}", r.advice);
        // beyond the kit's rating: back to the rated speed
        let ctx = Context { ram: Some(kit(6400)), ..ctx };
        let d = diagnose(Cause::Restart, &events, true, &peaks(92, 66, 264.0), &[], None, &ctx).unwrap();
        let r = d.causes.iter().find(|c| c.suspect == Suspect::Ram).expect("ram");
        assert!(r.evidence.iter().any(|e| e.contains("faster than the kit's rated EXPO/XMP 6000")), "{:?}", r.evidence);
        assert!(r.advice.starts_with("Set the RAM back to the kit's rated speed (6000 MT/s)"), "{}", r.advice);
    }

    #[test]
    fn real_case_expo_ram_first_gpu_memory_oc_second() {
        let events = vec![
            ev(-300, "nvlddmkm", 153, None),
            ev(-120, "nvlddmkm", 153, None),
            ev(-20, "nvlddmkm", 153, None),
            ev(60, KERNEL_POWER, 41, Some("0x0000001A")),
            ev(70, WER_SYSTEM, 1001, Some("0x0000001a")),
        ];
        let ctx = Context {
            oc: Some(OcInfo { mem_offset_mhz: Some(1000), ..stock() }),
            oc_after: false,
            ram: Some(ram(4800, 6000)),
            ..Default::default()
        };
        let d = diagnose(Cause::Restart, &events, true, &peaks(92, 66, 264.0), &[], Some(&python()), &ctx).unwrap();
        assert_eq!(d.causes[0].suspect, Suspect::Ram, "{d:#?}");
        assert_eq!(d.causes[1].suspect, Suspect::GpuOverclock, "{d:#?}");
        assert!(d.causes[0].percent >= 40 && d.causes[0].percent <= TOP_CAP, "{d:#?}");
        assert_eq!(d.confidence, "likely");
        assert!(d.sentence.starts_with("Likely cause: unstable RAM or memory overclock (EXPO/XMP), about "), "{}", d.sentence);
        let ev0 = d.causes[0].evidence.join(" | ");
        assert!(ev0.contains("0x1A (MEMORY_MANAGEMENT)") && ev0.contains("6000 MT/s now, above its standard 4800"), "{ev0}");
        assert!(d.causes[0].advice.contains("6000 → 5600"), "{}", d.causes[0].advice);
        assert!(d.causes[1].evidence.iter().any(|e| e.contains("memory clock offset +1000")));
        assert!(d.normal.iter().any(|n| n.contains("normal (at most 66 °C)")));
        assert!(!d.causes.iter().any(|c| c.suspect == Suspect::Heat));
        assert_eq!(d.short_line(), format!("Likely cause: unstable RAM / EXPO ({}%).", d.causes[0].percent));
        // the total never claims certainty
        assert!(d.causes.iter().map(|c| c.percent as u32).sum::<u32>() <= 100);
        // the same events with RAM back at 5600 and the GPU at stock: RAM is still the top suspect
        // (0x1A) but with less weight
        let ctx2 = Context { oc: Some(stock()), oc_after: true, ram: Some(ram(4800, 5600)), ..Default::default() };
        let d2 = diagnose(Cause::Restart, &events, true, &peaks(92, 66, 264.0), &[], Some(&python()), &ctx2).unwrap();
        assert!(pct(&d2, Suspect::GpuOverclock) < pct(&d, Suspect::GpuOverclock));
        assert!(d2.normal.iter().any(|n| n.contains("stock settings") && n.contains("after the restart")));
    }

    #[test]
    fn power_off_at_full_load_points_to_the_psu() {
        let events = vec![ev(40, KERNEL_POWER, 41, None), ev(41, "EventLog", 6008, None)];
        let ctx = Context { oc: Some(stock()), ram: Some(ram(4800, 4800)), ..Default::default() };
        let d = diagnose(Cause::Restart, &events, true, &peaks(99, 72, 356.0), &[], Some(&python()), &ctx).unwrap();
        assert_eq!(d.causes[0].suspect, Suspect::Power, "{d:#?}");
        assert_eq!(d.confidence, "likely");
        assert!(d.causes[0].advice.contains("power cables"));
        assert!(d.normal.iter().any(|n| n.contains("standard speed")));
    }

    fn idle_minute() -> Vec<Sample> {
        (0..60)
            .map(|i| Sample {
                t_ms: T - 60_000 + i * 1000,
                util: Some(if i % 20 == 0 { 3 } else { 0 }),
                temp: Some(32 + (i % 2) as u8),
                power_w: Some(14.0 + (i % 10) as f32),
                power_limit_w: Some(360.0),
                throttle: Some(0x400),
                ..Default::default()
            })
            .collect()
    }

    /// 26 Oct 2026: the PC went off at idle (a USB stick in the BIOS FlashBack port); Kernel-Power
    /// 41 with code 0, no WHEA, no dump; the BIOS came back on its defaults (RAM 3600, clock +1 h).
    #[test]
    fn idle_power_loss_points_to_a_power_cut_not_the_gpu() {
        let events = vec![ev(190, KERNEL_POWER, 41, None), ev(191, "EventLog", 6008, None)];
        let kit = RamSpeed { modules: 4, part: Some("CMK32GX5M2B6000Z30".into()), ..ram(4800, 3600) };
        let ctx = Context {
            oc: Some(stock()),
            oc_after: true,
            ram: Some(kit),
            clock_shift_min: Some(60),
            firmware_boot_ms: Some(52_000),
            just_after: true,
        };
        let lm = idle_minute();
        let p = crate::report::peaks(&lm);
        let d = diagnose(Cause::Restart, &events, true, &p, &lm, None, &ctx).unwrap();
        assert_eq!(d.causes[0].suspect, Suspect::PowerCut, "{d:#?}");
        assert!(d.causes[0].evidence[0].contains("while the PC was idle (GPU at most 3% busy, 23 W of its 360 W limit, 33 °C)"), "{d:#?}");
        assert!(d.causes[0].advice.contains("BIOS FlashBack port"));
        assert!(pct(&d, Suspect::Power) <= 20 && pct(&d, Suspect::Power) < d.causes[0].percent, "{d:#?}");
        assert!(
            d.causes.iter().find(|c| c.suspect == Suspect::Power).is_none_or(|c| c.evidence.iter().any(|e| e.contains("drew only 23 W")))
        );
        assert!(d.normal.iter().any(|n| n.starts_with("The GPU was idle just before the stop")), "{:?}", d.normal);
        let h = d.hints.join(" | ");
        assert!(h.starts_with("BIOS likely reset to safe defaults (EXPO/XMP off) at this restart: the RAM runs at 3600 MT/s now, below its 4800 MT/s standard and the kit's EXPO/XMP 6000"), "{h}");
        assert!(h.contains("the clock after the restart was about 1 h ahead") && h.contains("the BIOS took 52 s"), "{h}");
        assert!(!d.causes.iter().any(|c| c.suspect == Suspect::Ram && c.evidence.iter().any(|e| e.contains("EXPO/XMP profile"))));
        // the same stop at full GPU load still points at the PSU / power delivery
        let mut busy = idle_minute();
        for s in &mut busy {
            s.util = Some(99);
            s.power_w = Some(352.0);
        }
        let d2 = diagnose(Cause::Restart, &events, true, &crate::report::peaks(&busy), &busy, None, &ctx).unwrap();
        assert_eq!(d2.causes[0].suspect, Suspect::Power, "{d2:#?}");
        assert!(pct(&d2, Suspect::PowerCut) < 10, "{d2:#?}");
        // a Windows clock correction after the restart counts as the clock jump too
        let mut ev2 = events.clone();
        ev2.push(SysEvent { code: Some("-3600".into()), ..ev(900, crate::events::KERNEL_GENERAL, 1, None) });
        let ctx3 = Context { clock_shift_min: None, ram: Some(ram(4800, 4800)), ..ctx.clone() };
        let d3 = diagnose(Cause::Restart, &ev2, true, &p, &lm, None, &ctx3).unwrap();
        assert!(
            d3.hints.iter().any(|h| h.contains("about 1 h ahead") && h.starts_with("BIOS settings possibly reset at this restart")),
            "{:?}",
            d3.hints
        );
        // RAM below its standard on an older crash: noted, without "at this restart"
        let ctx4 = Context { just_after: false, clock_shift_min: None, ..ctx };
        let d4 = diagnose(Cause::Restart, &[ev(30, KERNEL_POWER, 41, Some("0x0000001A"))], true, &peaks(92, 66, 264.0), &[], None, &ctx4)
            .unwrap();
        assert!(d4.hints[0].starts_with("BIOS likely reset to safe defaults (EXPO/XMP off): the RAM runs at 3600"), "{:?}", d4.hints);
        let r = d4.causes.iter().find(|c| c.suspect == Suspect::Ram).unwrap();
        assert!(r.advice.contains("EXPO/XMP is off, which is the safe setting"), "{}", r.advice);
    }

    #[test]
    fn heat_when_hot_and_throttling() {
        let mut p = peaks(98, 93, 300.0);
        p.thermal_throttled_s = 40;
        let d = diagnose(Cause::Restart, &[ev(30, KERNEL_POWER, 41, None)], true, &p, &[], None, &Context::default()).unwrap();
        assert!(pct(&d, Suspect::Heat) >= 25, "{d:#?}");
        assert!(d.causes.iter().find(|c| c.suspect == Suspect::Heat).unwrap().advice.contains("93 °C"));
    }

    #[test]
    fn tdr_from_a_long_kernel_on_a_stock_card() {
        let events = vec![ev(-10, "Display", 4101, None), ev(-9, "nvlddmkm", 153, None), ev(-8, LIVE_KERNEL, 0x141, None)];
        let ctx = Context { oc: Some(stock()), ram: Some(ram(5600, 5600)), ..Default::default() };
        let d = diagnose(Cause::AppClosed, &events, true, &peaks(100, 70, 300.0), &[], None, &ctx).unwrap();
        assert_eq!(d.causes[0].suspect, Suspect::DriverTimeout, "{d:#?}");
        assert!(d.causes[0].evidence.iter().any(|e| e.contains("2-second timeout")));
    }

    #[test]
    fn application_crash_of_the_main_gpu_process() {
        let events = vec![ev(-5, APP_ERROR, 1000, Some("python.exe")), ev(-4, APP_ERROR, 1000, Some("tachyon.exe"))];
        let d = diagnose(Cause::AppClosed, &events, true, &peaks(90, 70, 250.0), &[], Some(&python()), &Context::default()).unwrap();
        assert_eq!(d.causes[0].suspect, Suspect::Application);
        assert!(d.causes[0].advice.starts_with("The crash came from python.exe"));
        assert!(!d.causes[0].evidence.iter().any(|e| e.contains("tachyon.exe")));
    }

    #[test]
    fn thin_or_no_evidence_is_said_so() {
        // Tachyon was simply closed: nothing to diagnose
        assert!(diagnose(Cause::AppClosed, &[], true, &peaks(10, 50, 60.0), &[], None, &Context::default()).is_none());
        // an unknown event alone is not enough
        let d = diagnose(Cause::AppClosed, &[ev(0, "nvlddmkm", 999, None)], true, &peaks(10, 50, 60.0), &[], None, &Context::default())
            .unwrap();
        assert!(d.causes.is_empty() && d.confidence == "unclear", "{d:#?}");
        // a restart with an unreadable log keeps more in reserve: no "likely"
        let d = diagnose(Cause::Restart, &[], false, &peaks(50, 60, 150.0), &[], None, &Context::default()).unwrap();
        assert_ne!(d.confidence, "likely", "{d:#?}");
        assert!(d.normal.iter().any(|n| n.contains("could not be read")));
    }

    #[test]
    fn never_certain() {
        // everything pointing at one cause still stays at or under the cap
        let mut events = vec![];
        for i in 0..20 {
            events.push(ev(i, WHEA, 47, None));
        }
        events.push(ev(30, KERNEL_POWER, 41, Some("0x0000001A")));
        let ctx = Context { ram: Some(ram(4800, 6400)), ..Default::default() };
        let d = diagnose(Cause::Restart, &events, true, &peaks(50, 60, 150.0), &[], None, &ctx).unwrap();
        assert!(d.causes[0].percent <= TOP_CAP);
        for c in &d.causes {
            assert!(!c.label.contains("certain") && !d.sentence.contains("certain"));
        }
    }
}
