//! Tachyon library: config, log parsing, log discovery, NVML sampling, and the host-independent
//! parts of the UI (Aurora colour field, software glass for layered windows).
//! Everything here is read-only with respect to the render pipeline.
pub mod aurora;
pub mod config;
pub mod explain;
pub mod forecast;
pub mod glass;
pub mod hex;
pub mod monitor;
pub mod nvml;
pub mod parse;
pub mod png;
pub mod procinfo;
pub mod scan;
pub mod sysinfo;
pub mod telemetry;
pub mod theme;
pub mod voice;
