//! A panic writes a short local crash log and ends the process cleanly (exit code 70) instead
//! of a silent abort. Nothing is sent anywhere; the next start mentions the log once.
use std::path::{Path, PathBuf};

const MARKER: &str = "last_panic.txt";

pub fn logs_dir() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join("logs"))
}

/// The log text (pure, for the tests).
pub fn render(msg: &str, location: Option<String>, thread: Option<&str>, mode: &str) -> String {
    format!(
        "Tachyon {} crashed ({mode})\ntime: {}\nthread: {}\nwhere: {}\nwhat: {}\nos: {} {}\n\nThis file stays on this PC. If you report the problem, you can attach it: it holds no paths from your projects and no readings.\n",
        env!("CARGO_PKG_VERSION"),
        chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        thread.unwrap_or("?"),
        location.unwrap_or_else(|| "?".into()),
        msg,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Write the log into `dir` and leave a marker for the next start. Returns the log path.
pub fn write(dir: &Path, text: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let p = dir.join(format!("panic-{}.txt", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    std::fs::write(&p, text).ok()?;
    let _ = std::fs::write(dir.join(MARKER), p.to_string_lossy().as_bytes());
    Some(p)
}

/// The log of a crash since the last start (once: the marker is renamed to `.seen`).
pub fn take_last(dir: &Path) -> Option<PathBuf> {
    let m = dir.join(MARKER);
    let p = PathBuf::from(std::fs::read_to_string(&m).ok()?.trim());
    let _ = std::fs::rename(&m, dir.join(format!("{MARKER}.seen")));
    p.exists().then_some(p)
}

/// Install the hook. `on_crash` runs after the log is written (the GUI shows a message box).
pub fn install(mode: &'static str, on_crash: fn(Option<&Path>)) {
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        // the message itself is ours; trim it so a long value cannot flood the file
        let msg: String = msg.chars().take(2000).collect();
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
        let text = render(&msg, loc, std::thread::current().name(), mode);
        let path = logs_dir().and_then(|d| write(&d, &text));
        eprintln!("{text}");
        on_crash(path.as_deref());
        std::process::exit(70);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_is_written_once_and_announced_once() {
        let d = std::env::temp_dir().join(format!("tachyon_crash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t = render("index out of bounds", Some("src/x.rs:1".into()), Some("main"), "gui");
        assert!(t.contains(env!("CARGO_PKG_VERSION")) && t.contains("src/x.rs:1") && t.contains("index out of bounds"));
        let p = write(&d, &t).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), t);
        assert_eq!(take_last(&d), Some(p));
        assert_eq!(take_last(&d), None, "only the next start mentions it");
        let _ = std::fs::remove_dir_all(&d);
    }
}
