//! "Who is using the GPU": launcher chain and category for each GPU process.
//! Read-only: a Toolhelp32 process snapshot (names and parent PIDs only) and, optionally,
//! the command line via a PROCESS_QUERY_LIMITED_INFORMATION handle. Nothing is ever changed.
use crate::config::ProcessRule;
use std::collections::HashMap;

/// pid -> (parent pid, exe file name)
pub type ProcTable = HashMap<u32, (u32, String)>;

/// Case-insensitive file-name glob with `*` wildcards.
pub fn glob_match(pat: &str, name: &str) -> bool {
    let p = pat.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == n;
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !n.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            return n.len() >= pos + part.len() && n[pos..].ends_with(part);
        } else {
            match n[pos..].find(part) {
                Some(j) => pos += j + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// Launcher chain (nearest parent first), at most `max` entries. Stops at the shell/service roots,
/// at missing parents, and on PID cycles.
pub fn chain(pid: u32, table: &ProcTable, max: usize) -> Vec<String> {
    const ROOTS: [&str; 5] = ["explorer.exe", "services.exe", "wininit.exe", "svchost.exe", "userinit.exe"];
    let mut out = Vec::new();
    let mut seen = vec![pid];
    let mut cur = pid;
    while out.len() < max {
        let Some(&(ppid, _)) = table.get(&cur) else { break };
        if ppid == 0 || ppid == 4 || seen.contains(&ppid) {
            break;
        }
        let Some((_, name)) = table.get(&ppid) else { break };
        out.push(name.clone());
        if ROOTS.iter().any(|r| r.eq_ignore_ascii_case(name)) {
            break;
        }
        seen.push(ppid);
        cur = ppid;
    }
    out
}

/// First rule whose `names` match the exe or whose `parents` match any ancestor.
pub fn categorize<'a>(rules: &'a [ProcessRule], exe: &str, chain: &[String]) -> Option<&'a ProcessRule> {
    rules.iter().find(|r| r.names.iter().any(|p| glob_match(p, exe)) || r.parents.iter().any(|p| chain.iter().any(|c| glob_match(p, c))))
}

/// "#RRGGBB" -> (r, g, b); falls back to grey.
pub fn parse_color(s: &str) -> (u8, u8, u8) {
    let h = s.trim().trim_start_matches('#');
    if h.len() == 6 {
        if let Ok(v) = u32::from_str_radix(h, 16) {
            return ((v >> 16) as u8, (v >> 8) as u8, v as u8);
        }
    }
    (124, 139, 161)
}

#[cfg(windows)]
pub mod win {
    use super::ProcTable;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };

    /// One Toolhelp32 snapshot of all processes (names and parent PIDs only).
    pub fn snapshot() -> ProcTable {
        let mut t = ProcTable::new();
        unsafe {
            let h = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if h == INVALID_HANDLE_VALUE {
                return t;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(h, &mut e) != 0;
            while ok {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                t.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
                ok = Process32NextW(h, &mut e) != 0;
            }
            CloseHandle(h);
        }
        t
    }

    /// Command line via NtQueryInformationProcess(ProcessCommandLineInformation) with a
    /// query-limited handle. Returns None when access is denied (elevated/protected processes).
    pub fn cmdline(pid: u32) -> Option<String> {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        type NtQip = unsafe extern "system" fn(isize, u32, *mut u8, u32, *mut u32) -> i32;
        static NTQ: std::sync::OnceLock<Option<NtQip>> = std::sync::OnceLock::new();
        let f = (*NTQ.get_or_init(|| unsafe {
            let lib = libloading::Library::new("ntdll.dll").ok()?;
            let f: NtQip = *lib.get::<NtQip>(b"NtQueryInformationProcess\0").ok()?;
            std::mem::forget(lib); // ntdll is never unloaded
            Some(f)
        }))?;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = vec![0u8; 8192];
            let mut ret = 0u32;
            let st = f(h as isize, 60, buf.as_mut_ptr(), buf.len() as u32, &mut ret);
            CloseHandle(h);
            if st < 0 {
                return None;
            }
            // UNICODE_STRING { u16 Length; u16 MaxLength; (pad) ; *u16 Buffer }
            let len = u16::from_le_bytes([buf[0], buf[1]]) as usize / 2;
            let ptr = usize::from_le_bytes(buf[8..16].try_into().ok()?) as *const u16;
            let base = buf.as_ptr() as usize;
            let off = (ptr as usize).checked_sub(base)?;
            if off + len * 2 > buf.len() {
                return None;
            }
            let s = std::slice::from_raw_parts(buf.as_ptr().add(off) as *const u16, len);
            Some(String::from_utf16_lossy(s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_process_rules;

    fn table(rows: &[(u32, u32, &str)]) -> ProcTable {
        rows.iter().map(|(p, pp, n)| (*p, (*pp, n.to_string()))).collect()
    }

    #[test]
    fn globbing() {
        assert!(glob_match("python*.exe", "Python3.11.exe"));
        assert!(glob_match("*-win64-shipping.exe", "Game-Win64-Shipping.exe"));
        assert!(glob_match("*webhelper*.exe", "steamwebhelper.exe"));
        assert!(glob_match("dwm.exe", "DWM.EXE"));
        assert!(!glob_match("python*.exe", "pythonista.dll"));
        assert!(!glob_match("chrome.exe", "chrome_proxy.exe"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn launcher_chain_and_cycles() {
        let t = table(&[
            (1, 0, "System Idle"),
            (500, 1, "explorer.exe"),
            (600, 500, "cmd.exe"),
            (700, 600, "python.exe"),
            (800, 700, "python.exe"),
            (900, 901, "a.exe"),
            (901, 900, "b.exe"),
        ]);
        assert_eq!(chain(800, &t, 4), vec!["python.exe", "cmd.exe", "explorer.exe"]);
        assert_eq!(chain(800, &t, 2), vec!["python.exe", "cmd.exe"]);
        assert_eq!(chain(900, &t, 4), vec!["b.exe"]); // cycle stops
        assert!(chain(12345, &t, 4).is_empty());
    }

    #[test]
    fn default_categories() {
        let r = default_process_rules();
        let cat = |exe: &str, ch: &[&str]| {
            let ch: Vec<String> = ch.iter().map(|s| s.to_string()).collect();
            categorize(&r, exe, &ch).map(|x| x.category.clone()).unwrap_or("Other".into())
        };
        assert_eq!(cat("dwm.exe", &[]), "System");
        assert_eq!(cat("chrome.exe", &["explorer.exe"]), "Browser");
        assert_eq!(cat("steamwebhelper.exe", &["steam.exe"]), "Browser");
        assert_eq!(cat("Cyberpunk2077.exe", &["steam.exe", "explorer.exe"]), "Game");
        assert_eq!(cat("python.exe", &["ComfyUI.exe", "explorer.exe"]), "AI/agent");
        assert_eq!(cat("python.exe", &["cmd.exe", "explorer.exe"]), "Script/Python");
        assert_eq!(cat("ollama_llama_server.exe", &["ollama.exe"]), "AI/agent");
        assert_eq!(cat("Resolve.exe", &["explorer.exe"]), "Creative app");
        assert_eq!(cat("ffmpeg.exe", &["python.exe"]), "Creative app");
        assert_eq!(cat("obs64.exe", &[]), "Creative app");
        assert_eq!(cat("mystery.exe", &["explorer.exe"]), "Other");
        assert_eq!(parse_color("#FF5C7A"), (255, 92, 122));
        assert_eq!(parse_color("nope"), (124, 139, 161));
    }

    #[test]
    fn rules_from_toml_replace_defaults() {
        let c: crate::config::Config =
            toml::from_str("[[process_rules]]\ncategory = \"Render\"\ncolor = \"#00FF00\"\nnames = [\"blender*.exe\"]\n").unwrap();
        assert_eq!(c.process_rules.len(), 1);
        assert_eq!(categorize(&c.process_rules, "blender.exe", &[]).unwrap().category, "Render");
        // default config round-trips with the rules table
        let s = toml::to_string_pretty(&crate::config::Config::default()).unwrap();
        let back: crate::config::Config = toml::from_str(&s).unwrap();
        assert_eq!(back.process_rules, default_process_rules());
    }
}
