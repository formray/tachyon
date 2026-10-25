#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Tachyon: lightweight, read-only NVIDIA GPU monitor with an optional render-progress panel.

#[cfg(windows)]
mod dw;
#[cfg(windows)]
mod gui;
#[cfg(windows)]
mod present;

/// `tachyon mcp`: the read-only MCP server for local AI agents (stdin/stdout, no window).
fn mcp_requested() -> bool {
    std::env::args().nth(1).is_some_and(|a| a.eq_ignore_ascii_case("mcp"))
}

fn run_mcp() {
    let path = tachyon::config::config_path();
    // read the settings without creating a file
    let cfg = std::fs::read_to_string(&path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
    tachyon::mcp::serve(cfg);
}

#[cfg(windows)]
fn main() {
    if mcp_requested() {
        return run_mcp();
    }
    gui::run();
}

/// On non-Windows hosts: print one snapshot (used for testing on the build box).
#[cfg(not(windows))]
fn main() {
    use tachyon::{config, nvml, scan};
    if mcp_requested() {
        return run_mcp();
    }
    let mut cfg = config::Config::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        cfg.render_roots = args;
    }
    let c = scan::discover(&cfg.render_roots, cfg.scan_depth);
    println!("{:#?}", scan::current_job(&c, &cfg));
    match nvml::Nvml::open() {
        Ok(mut n) => {
            let s = n.sample(0);
            println!("{} driver {}\n{s:#?}", n.names.join(", "), n.driver)
        }
        Err(e) => println!("NVML unavailable: {e}"),
    }
}
