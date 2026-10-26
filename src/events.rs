//! Windows Event Log, read-only: the System log entries that explain a crash or an unexpected
//! restart (Kernel-Power 41, BugCheck 1001, EventLog 6008, WHEA-Logger hardware errors,
//! nvlddmkm driver errors and Display 4101 timeouts), and from the Application log the GPU
//! timeout reports (Windows Error Reporting "LiveKernelEvent" 141/117) and application crashes
//! (Application Error 1000). wevtapi.dll is loaded at run time and only queried; if it is missing
//! or access is denied the report says so and goes on without events.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SysEvent {
    pub t_ms: i64,
    pub provider: String,
    pub id: u32,
    /// 1 critical, 2 error, 3 warning, 4 information.
    pub level: u8,
    /// A code from the event data when it says something (bug check code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

pub const KERNEL_POWER: &str = "Microsoft-Windows-Kernel-Power";
pub const WER_SYSTEM: &str = "Microsoft-Windows-WER-SystemErrorReporting";
pub const WHEA: &str = "Microsoft-Windows-WHEA-Logger";
/// A Windows Error Reporting kernel report (Application log); `id` is the report code (0x141...).
pub const LIVE_KERNEL: &str = "LiveKernelEvent";
/// An application crash (Application log, event 1000); `code` is the executable name.
pub const APP_ERROR: &str = "Application Error";
const WER_APP: &str = "Windows Error Reporting";

impl SysEvent {
    /// Plain words for the report.
    pub fn describe(&self) -> String {
        let code = self.code.as_ref().map(|c| format!(", code {c}")).unwrap_or_default();
        match (self.provider.as_str(), self.id) {
            (KERNEL_POWER, 41) if self.bugcheck().is_some() => format!(
                "Windows restarted after a blue screen (Kernel-Power 41 with bug check {}): a system crash, not a power loss.",
                self.code.as_deref().unwrap_or_default()
            ),
            (KERNEL_POWER, 41) => format!(
                "Windows restarted without shutting down cleanly (Kernel-Power 41{code}): power loss, a hard freeze or a forced restart."
            ),
            (WER_SYSTEM | "BugCheck", 1001) => format!("Windows stopped with a blue screen and restarted (bug check{code})."),
            ("EventLog", 6008) => "Windows noted that the previous shutdown was unexpected (EventLog 6008).".into(),
            (WHEA, 18) => "The hardware reported a fatal error (WHEA-Logger 18): often the CPU, memory or an unstable overclock.".into(),
            (WHEA, 17) => "The PCIe link reported a corrected error (WHEA-Logger 17).".into(),
            (WHEA, 19) => "The CPU reported a corrected error (WHEA-Logger 19).".into(),
            (WHEA, 47) => "The memory reported a corrected error (WHEA-Logger 47).".into(),
            (WHEA, id) => format!("The hardware reported an error (WHEA-Logger {id})."),
            ("nvlddmkm", 153) => "The NVIDIA driver reset the GPU after it stopped responding (nvlddmkm 153).".into(),
            ("nvlddmkm", 13) => "The NVIDIA driver reported a graphics engine error (nvlddmkm 13).".into(),
            ("nvlddmkm", 14) => "The NVIDIA driver reported a graphics error (nvlddmkm 14).".into(),
            ("nvlddmkm", id) => format!("The NVIDIA driver logged an error (nvlddmkm {id})."),
            ("Display", 4101) => "The display driver stopped responding and recovered (Display 4101, a GPU timeout).".into(),
            (LIVE_KERNEL, 0x141) => {
                "Windows saved a report of a GPU timeout (LiveKernelEvent 141): the GPU hung and the driver recovered.".into()
            }
            (LIVE_KERNEL, 0x117) => "Windows detected a GPU timeout and reset the driver (LiveKernelEvent 117).".into(),
            (LIVE_KERNEL, id) => format!("Windows saved a kernel problem report (LiveKernelEvent {id:X})."),
            (APP_ERROR, _) => format!("{} crashed (Application Error 1000).", self.code.as_deref().unwrap_or("An application")),
            (p, id) => format!("{p} event {id}."),
        }
    }

    /// True for events that mean the PC restarted unexpectedly.
    pub fn is_restart(&self) -> bool {
        matches!((self.provider.as_str(), self.id), (KERNEL_POWER, 41) | (WER_SYSTEM | "BugCheck", 1001) | ("EventLog", 6008))
    }

    /// True for GPU driver trouble (TDR, nvlddmkm).
    pub fn is_gpu(&self) -> bool {
        self.provider == "nvlddmkm"
            || (self.provider == "Display" && self.id == 4101)
            || (self.provider == LIVE_KERNEL && matches!(self.id, 0x141 | 0x117))
    }

    /// The bug check (blue screen) code, for Kernel-Power 41 and BugCheck 1001.
    pub fn bugcheck(&self) -> Option<u32> {
        if !matches!((self.provider.as_str(), self.id), (KERNEL_POWER, 41) | (WER_SYSTEM | "BugCheck", 1001)) {
            return None;
        }
        let c = self.code.as_deref()?.trim();
        let c = c.strip_prefix("0x").or_else(|| c.strip_prefix("0X")).unwrap_or(c);
        u32::from_str_radix(c, 16).ok().or_else(|| u64::from_str_radix(c, 16).ok().map(|v| v as u32)).filter(|v| *v != 0)
    }

    pub fn local(&self) -> chrono::DateTime<chrono::Local> {
        chrono::DateTime::from_timestamp_millis(self.t_ms).unwrap_or_default().with_timezone(&chrono::Local)
    }
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// The structured query for the System log between two Unix times (milliseconds).
pub fn xpath(from_ms: i64, to_ms: i64) -> String {
    format!(
        "*[System[((Provider[@Name='{KERNEL_POWER}'] and EventID=41) or (Provider[@Name='{WER_SYSTEM}'] and EventID=1001) or \
         (Provider[@Name='EventLog'] and EventID=6008) or Provider[@Name='{WHEA}'] or Provider[@Name='nvlddmkm'] or \
         (Provider[@Name='Display'] and EventID=4101)) and TimeCreated[@SystemTime>='{}' and @SystemTime<='{}']]]",
        iso(from_ms),
        iso(to_ms)
    )
}

/// The query for the Application log: kernel reports (LiveKernelEvent) and application crashes.
pub fn xpath_app(from_ms: i64, to_ms: i64) -> String {
    format!(
        "*[System[((Provider[@Name='{WER_APP}'] and EventID=1001) or (Provider[@Name='{APP_ERROR}'] and EventID=1000)) and \
         TimeCreated[@SystemTime>='{}' and @SystemTime<='{}']]]",
        iso(from_ms),
        iso(to_ms)
    )
}

/// Value of attribute `attr` in the first `<tag ...>` element.
fn attr(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag} ")).or_else(|| xml.find(&format!("<{tag}\t")))?;
    let el = &xml[start..start + xml[start..].find('>')?];
    let k = el.find(&format!("{attr}="))? + attr.len() + 1;
    let q = el.get(k..)?.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let rest = &el[k + 1..];
    Some(rest[..rest.find(q)?].to_string())
}

/// Text of the first `<tag>...</tag>` (attributes allowed).
fn text<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = xml.find(&format!("<{tag}>")).or_else(|| xml.find(&format!("<{tag} ")))?;
    let body = open + xml[open..].find('>')? + 1;
    let end = body + xml[body..].find(&format!("</{tag}>"))?;
    Some(xml[body..end].trim())
}

/// `<Data Name='name'>value</Data>` from EventData.
fn data(xml: &str, name: &str) -> Option<String> {
    for q in ['\'', '"'] {
        if let Some(i) = xml.find(&format!("<Data Name={q}{name}{q}>")) {
            let body = i + xml[i..].find('>')? + 1;
            let end = body + xml[body..].find("</Data>")?;
            return Some(xml[body..end].trim().to_string());
        }
    }
    None
}

/// Every `<Data>` value in order (named or not), at most 32.
fn data_list(xml: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = xml;
    while out.len() < 32 {
        let Some(i) = rest.find("<Data") else { break };
        let after = &rest[i + 5..];
        // `<Data>` or `<Data Name=...>`, not `<DataItem>` and the like
        if !after.starts_with('>') && !after.starts_with(' ') {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        if after[..gt].ends_with('/') {
            out.push(String::new());
            rest = &after[gt + 1..];
            continue;
        }
        let body = &after[gt + 1..];
        let Some(end) = body.find("</Data>") else { break };
        out.push(body[..end].trim().to_string());
        rest = &body[end..];
    }
    out
}

/// Executable name only (never a path), at most 64 characters.
fn exe_only(s: &str) -> String {
    let n = s.rsplit(['\\', '/']).next().unwrap_or(s).trim();
    n.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// The report code of a WER LiveKernelEvent: P1 (three values after the event name), or the
/// `LKD_0x141_...` fault bucket.
fn live_kernel_code(d: &[String]) -> Option<u32> {
    let k = d.iter().position(|v| v == LIVE_KERNEL)?;
    let hex = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok().filter(|v| *v != 0);
    if let Some(v) = d.get(k + 3).and_then(|p| hex(p)) {
        return Some(v);
    }
    d.iter().find_map(|v| {
        let i = v.find("LKD_0x")? + 6;
        let end = v[i..].find(|c: char| !c.is_ascii_hexdigit()).map_or(v.len(), |e| i + e);
        hex(&v[i..end])
    })
}

/// Parse one event as rendered by EvtRender(EvtRenderEventXml).
pub fn parse_event_xml(xml: &str) -> Option<SysEvent> {
    let mut provider = attr(xml, "Provider", "Name")?;
    let mut id: u32 = text(xml, "EventID")?.parse().ok()?;
    let level = text(xml, "Level").and_then(|l| l.parse().ok()).unwrap_or(4);
    let ts = attr(xml, "TimeCreated", "SystemTime")?;
    let t_ms = chrono::DateTime::parse_from_rfc3339(&ts).ok()?.timestamp_millis();
    match (provider.as_str(), id) {
        // only the kernel reports; WER 1001 also covers every application crash report
        (WER_APP, 1001) => {
            id = live_kernel_code(&data_list(xml))?;
            provider = LIVE_KERNEL.into();
            return Some(SysEvent { t_ms, provider, id, level, code: None });
        }
        (APP_ERROR, 1000) => {
            let app = data(xml, "AppName").or_else(|| data_list(xml).into_iter().next()).map(|a| exe_only(&a)).filter(|a| !a.is_empty());
            return Some(SysEvent { t_ms, provider, id, level, code: app });
        }
        _ => {}
    }
    let code = match (provider.as_str(), id) {
        (KERNEL_POWER, 41) => data(xml, "BugcheckCode").filter(|c| c != "0").map(|c| match c.parse::<u64>() {
            Ok(n) => format!("0x{n:08X}"),
            Err(_) => c,
        }),
        (WER_SYSTEM | "BugCheck", 1001) => data(xml, "param1").map(|p| p.split_whitespace().next().unwrap_or("").to_string()),
        _ => None,
    };
    Some(SysEvent { t_ms, provider, id, level, code })
}

/// Query the System log, then the Application log (GPU timeout reports, application crashes).
/// Errors are plain words (for the report); the Application log is optional.
pub fn query(from_ms: i64, to_ms: i64, max: usize) -> Result<Vec<SysEvent>, String> {
    let mut out = query_channel("System", &xpath(from_ms, to_ms), max)?;
    if let Ok(app) = query_channel("Application", &xpath_app(from_ms, to_ms), max) {
        out.extend(app);
        out.sort_by_key(|e| e.t_ms);
        // keep the newest when both logs together go over the cap
        let skip = out.len().saturating_sub(max);
        out.drain(..skip);
    }
    Ok(out)
}

#[cfg(windows)]
fn query_channel(channel: &str, xpath: &str, max: usize) -> Result<Vec<SysEvent>, String> {
    use std::ffi::c_void;
    type H = *mut c_void;
    type FQuery = unsafe extern "system" fn(H, *const u16, *const u16, u32) -> H;
    type FNext = unsafe extern "system" fn(H, u32, *mut H, u32, u32, *mut u32) -> i32;
    type FRender = unsafe extern "system" fn(H, H, u32, u32, *mut c_void, *mut u32, *mut u32) -> i32;
    type FClose = unsafe extern "system" fn(H) -> i32;
    const EVT_QUERY_CHANNEL_PATH: u32 = 0x1;
    const EVT_QUERY_REVERSE: u32 = 0x200;
    const EVT_RENDER_XML: u32 = 1;
    const ERROR_ACCESS_DENIED: i32 = 5;
    unsafe {
        let lib = libloading::Library::new("wevtapi.dll").map_err(|_| "the Windows Event Log API is not available".to_string())?;
        let q: libloading::Symbol<FQuery> = lib.get(b"EvtQuery\0").map_err(|_| "EvtQuery missing")?;
        let next: libloading::Symbol<FNext> = lib.get(b"EvtNext\0").map_err(|_| "EvtNext missing")?;
        let render: libloading::Symbol<FRender> = lib.get(b"EvtRender\0").map_err(|_| "EvtRender missing")?;
        let close: libloading::Symbol<FClose> = lib.get(b"EvtClose\0").map_err(|_| "EvtClose missing")?;
        let path: Vec<u16> = channel.encode_utf16().chain([0]).collect();
        let xp: Vec<u16> = xpath.encode_utf16().chain([0]).collect();
        let rs = q(std::ptr::null_mut(), path.as_ptr(), xp.as_ptr(), EVT_QUERY_CHANNEL_PATH | EVT_QUERY_REVERSE);
        if rs.is_null() {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(match code {
                ERROR_ACCESS_DENIED => format!("Windows did not allow reading the {channel} event log"),
                // 0: no error set (wine), 50/120: not supported or not implemented
                0 | 50 | 120 => "Windows did not answer the event log query".into(),
                c => format!("the {channel} event log could not be read (error {c})"),
            });
        }
        let mut out = vec![];
        let mut buf: Vec<u16> = vec![0; 8192];
        'outer: while out.len() < max {
            let mut hs: [H; 16] = [std::ptr::null_mut(); 16];
            let mut got = 0u32;
            if next(rs, 16, hs.as_mut_ptr(), 1000, 0, &mut got) == 0 {
                break;
            }
            for h in hs.iter().take(got as usize) {
                let (mut used, mut props) = (0u32, 0u32);
                let mut ok = render(
                    std::ptr::null_mut(),
                    *h,
                    EVT_RENDER_XML,
                    (buf.len() * 2) as u32,
                    buf.as_mut_ptr().cast(),
                    &mut used,
                    &mut props,
                );
                if ok == 0 && used as usize > buf.len() * 2 {
                    buf = vec![0; used as usize / 2 + 16];
                    ok = render(
                        std::ptr::null_mut(),
                        *h,
                        EVT_RENDER_XML,
                        (buf.len() * 2) as u32,
                        buf.as_mut_ptr().cast(),
                        &mut used,
                        &mut props,
                    );
                }
                if ok != 0 {
                    let n = (used as usize / 2).min(buf.len());
                    let xml = String::from_utf16_lossy(&buf[..n]);
                    if let Some(e) = parse_event_xml(xml.trim_end_matches('\0')) {
                        out.push(e);
                    }
                }
                close(*h);
                if out.len() >= max {
                    break 'outer;
                }
            }
        }
        close(rs);
        out.sort_by_key(|e| e.t_ms);
        Ok(out)
    }
}

#[cfg(not(windows))]
fn query_channel(_channel: &str, _xpath: &str, _max: usize) -> Result<Vec<SysEvent>, String> {
    Err("the Windows Event Log is only available on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KP41: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Microsoft-Windows-Kernel-Power' Guid='{331c3b3a-2005-44c2-ac5e-77220c37d6b4}'/><EventID>41</EventID><Version>8</Version><Level>1</Level><Task>63</Task><TimeCreated SystemTime='2026-10-25T21:43:12.5000000Z'/><Channel>System</Channel></System><EventData><Data Name='BugcheckCode'>278</Data><Data Name='PowerButtonTimestamp'>0</Data></EventData></Event>"#;
    const NV153: &str = r#"<Event><System><Provider Name="nvlddmkm"/><EventID Qualifiers="49322">153</EventID><Level>2</Level><TimeCreated SystemTime="2026-10-25T21:41:02.25Z"/></System><EventData><Data>\Device\Video3</Data></EventData></Event>"#;
    const BC: &str = r#"<Event><System><Provider Name='Microsoft-Windows-WER-SystemErrorReporting' EventSourceName='BugCheck'/><EventID Qualifiers='16384'>1001</EventID><Level>2</Level><TimeCreated SystemTime='2026-10-25T21:44:00.000Z'/></System><EventData><Data Name='param1'>0x00000116 (0xffffd00f, 0x0, 0x0, 0x0)</Data></EventData></Event>"#;

    #[test]
    fn parses_rendered_events() {
        let e = parse_event_xml(KP41).unwrap();
        assert_eq!((e.provider.as_str(), e.id, e.level), (KERNEL_POWER, 41, 1));
        assert_eq!(e.code.as_deref(), Some("0x00000116"));
        assert_eq!(e.t_ms, chrono::DateTime::parse_from_rfc3339("2026-10-25T21:43:12.5Z").unwrap().timestamp_millis());
        assert!(e.is_restart() && !e.is_gpu());
        assert!(e.describe().starts_with(
            "Windows restarted after a blue screen (Kernel-Power 41 with bug check 0x00000116): a system crash, not a power loss."
        ));
        let n = parse_event_xml(NV153).unwrap();
        assert_eq!((n.provider.as_str(), n.id, n.code.as_deref()), ("nvlddmkm", 153, None));
        assert!(n.is_gpu());
        assert!(n.describe().contains("reset the GPU"));
        let b = parse_event_xml(BC).unwrap();
        assert_eq!(b.code.as_deref(), Some("0x00000116"));
        assert!(b.describe().contains("blue screen"));
        assert!(parse_event_xml("<Event><System></System></Event>").is_none());
        assert_eq!(b.bugcheck(), Some(0x116));
        assert_eq!(e.bugcheck(), Some(0x116));
        assert_eq!(n.bugcheck(), None);
    }

    const LKE: &str = r#"<Event><System><Provider Name='Windows Error Reporting'/><EventID Qualifiers='0'>1001</EventID><Level>4</Level><TimeCreated SystemTime='2026-10-25T21:41:30.000Z'/><Channel>Application</Channel></System><EventData><Data>1834566321145</Data><Data>5</Data><Data>LiveKernelEvent</Data><Data>Not available</Data><Data>0</Data><Data>141</Data><Data>ffffb08a</Data><Data/><Data>10_0_26100</Data></EventData></Event>"#;
    const APPCRASH_WER: &str = r#"<Event><System><Provider Name='Windows Error Reporting'/><EventID>1001</EventID><Level>4</Level><TimeCreated SystemTime='2026-10-25T21:41:30.000Z'/></System><EventData><Data>0</Data><Data>4</Data><Data>APPCRASH</Data><Data>Not available</Data><Data>0</Data><Data>python.exe</Data></EventData></Event>"#;
    const APPERR: &str = r#"<Event><System><Provider Name='Application Error' Guid='{a0e9b465-b939-57d7-b27d-95d8e925ff57}'/><EventID>1000</EventID><Level>2</Level><TimeCreated SystemTime='2026-10-25T21:40:00.000Z'/></System><EventData><Data Name='AppName'>python.exe</Data><Data Name='AppVersion'>3.11.9150.1013</Data><Data Name='ModuleName'>c10_cuda.dll</Data></EventData></Event>"#;
    const APPERR_OLD: &str = r#"<Event><System><Provider Name="Application Error"/><EventID>1000</EventID><Level>2</Level><TimeCreated SystemTime="2026-10-25T21:40:00.000Z"/></System><EventData><Data>C:\Users\ann\envs\py\python.exe</Data><Data>3.11</Data></EventData></Event>"#;

    #[test]
    fn parses_application_log_reports() {
        let l = parse_event_xml(LKE).unwrap();
        assert_eq!((l.provider.as_str(), l.id), (LIVE_KERNEL, 0x141));
        assert!(l.is_gpu() && !l.is_restart());
        assert!(l.describe().contains("LiveKernelEvent 141"), "{}", l.describe());
        assert!(parse_event_xml(APPCRASH_WER).is_none(), "only kernel reports from WER");
        let bucket = LKE.replace("<Data>141</Data>", "<Data></Data>").replace("1834566321145", "LKD_0x117_Tdr:6_IMAGE_nvlddmkm.sys");
        assert_eq!(parse_event_xml(&bucket).unwrap().id, 0x117, "from the fault bucket");
        let a = parse_event_xml(APPERR).unwrap();
        assert_eq!((a.provider.as_str(), a.code.as_deref()), (APP_ERROR, Some("python.exe")));
        assert_eq!(a.describe(), "python.exe crashed (Application Error 1000).");
        assert_eq!(parse_event_xml(APPERR_OLD).unwrap().code.as_deref(), Some("python.exe"), "never a path");
        let q = xpath_app(0, 1);
        assert!(q.contains("@Name='Windows Error Reporting'") && q.contains("@Name='Application Error'"));
        assert_eq!(q.matches('[').count(), q.matches(']').count());
    }

    #[test]
    fn query_text_covers_the_providers_and_window() {
        let q = xpath(0, 60_000);
        for p in [KERNEL_POWER, WER_SYSTEM, WHEA, "nvlddmkm", "Display", "EventLog"] {
            assert!(q.contains(&format!("@Name='{p}'")), "{p}");
        }
        assert!(q.contains("@SystemTime>='1970-01-01T00:00:00.000Z' and @SystemTime<='1970-01-01T00:01:00.000Z'"));
        assert_eq!(q.matches('[').count(), q.matches(']').count());
        assert_eq!(q.matches('(').count(), q.matches(')').count());
    }
}
