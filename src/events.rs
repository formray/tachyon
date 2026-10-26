//! Windows Event Log, read-only: the System log entries that explain a crash or an unexpected
//! restart (Kernel-Power 41, BugCheck 1001, EventLog 6008, WHEA-Logger hardware errors,
//! nvlddmkm driver errors and Display 4101 timeouts). wevtapi.dll is loaded at run time and only
//! queried; if it is missing or access is denied the report says so and goes on without events.
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

const KERNEL_POWER: &str = "Microsoft-Windows-Kernel-Power";
const WER_SYSTEM: &str = "Microsoft-Windows-WER-SystemErrorReporting";
const WHEA: &str = "Microsoft-Windows-WHEA-Logger";

impl SysEvent {
    /// Plain words for the report.
    pub fn describe(&self) -> String {
        let code = self.code.as_ref().map(|c| format!(", code {c}")).unwrap_or_default();
        match (self.provider.as_str(), self.id) {
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
            (p, id) => format!("{p} event {id}."),
        }
    }

    /// True for events that mean the PC restarted unexpectedly.
    pub fn is_restart(&self) -> bool {
        matches!((self.provider.as_str(), self.id), (KERNEL_POWER, 41) | (WER_SYSTEM | "BugCheck", 1001) | ("EventLog", 6008))
    }

    /// True for GPU driver trouble (TDR, nvlddmkm).
    pub fn is_gpu(&self) -> bool {
        self.provider == "nvlddmkm" || (self.provider == "Display" && self.id == 4101)
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

/// Parse one event as rendered by EvtRender(EvtRenderEventXml).
pub fn parse_event_xml(xml: &str) -> Option<SysEvent> {
    let provider = attr(xml, "Provider", "Name")?;
    let id = text(xml, "EventID")?.parse().ok()?;
    let level = text(xml, "Level").and_then(|l| l.parse().ok()).unwrap_or(4);
    let ts = attr(xml, "TimeCreated", "SystemTime")?;
    let t_ms = chrono::DateTime::parse_from_rfc3339(&ts).ok()?.timestamp_millis();
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

/// Query the System log. Errors are plain words (for the report).
#[cfg(windows)]
pub fn query(from_ms: i64, to_ms: i64, max: usize) -> Result<Vec<SysEvent>, String> {
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
        let path: Vec<u16> = "System\0".encode_utf16().collect();
        let xp: Vec<u16> = xpath(from_ms, to_ms).encode_utf16().chain([0]).collect();
        let rs = q(std::ptr::null_mut(), path.as_ptr(), xp.as_ptr(), EVT_QUERY_CHANNEL_PATH | EVT_QUERY_REVERSE);
        if rs.is_null() {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(match code {
                ERROR_ACCESS_DENIED => "Windows did not allow reading the System event log".into(),
                // 0: no error set (wine), 50/120: not supported or not implemented
                0 | 50 | 120 => "Windows did not answer the event log query".into(),
                c => format!("the System event log could not be read (error {c})"),
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
pub fn query(_from_ms: i64, _to_ms: i64, _max: usize) -> Result<Vec<SysEvent>, String> {
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
        assert!(e.describe().starts_with("Windows restarted without shutting down cleanly (Kernel-Power 41, code 0x00000116)"));
        let n = parse_event_xml(NV153).unwrap();
        assert_eq!((n.provider.as_str(), n.id, n.code.as_deref()), ("nvlddmkm", 153, None));
        assert!(n.is_gpu());
        assert!(n.describe().contains("reset the GPU"));
        let b = parse_event_xml(BC).unwrap();
        assert_eq!(b.code.as_deref(), Some("0x00000116"));
        assert!(b.describe().contains("blue screen"));
        assert!(parse_event_xml("<Event><System></System></Event>").is_none());
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
