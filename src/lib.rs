//! Tachyon library: config, log parsing, log discovery, NVML sampling, and the host-independent
//! parts of the UI (Aurora colour field, software glass for layered windows).
//! Everything here is read-only with respect to the render pipeline.
pub mod agent;
pub mod aurora;
pub mod config;
pub mod crashlog;
pub mod diagnose;
pub mod events;
pub mod explain;
pub mod fast;
pub mod forecast;
#[doc(hidden)]
pub mod fuzz;
pub mod glass;
pub mod hex;
pub mod history;
pub mod insight;
pub mod markers;
pub mod mcp;
pub mod monitor;
pub mod nvml;
pub mod parse;
pub mod pcie;
pub mod png;
pub mod preflight;
pub mod procinfo;
pub mod procvram;
pub mod recorder;
pub mod report;
pub mod scan;
pub mod settings;
#[cfg(windows)]
pub mod startup;
pub mod sysinfo;
pub mod telemetry;
pub mod theme;
pub mod voice;
#[cfg(windows)]
pub mod winsec;
