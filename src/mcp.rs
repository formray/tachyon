//! `tachyon mcp`: a Model Context Protocol server on stdin/stdout (JSON-RPC 2.0, one message per
//! line) so local AI agents can ask what the GPU is doing and why. Every tool is read-only. It
//! reads the status the running Tachyon publishes (with `agent_api = true`), the flight recorder
//! files and the job history; if Tachyon is not running it takes its own short reading. No
//! network: the client starts this process and talks to it through pipes.
use crate::config::Config;
use crate::history::{self, JobRecord};
use crate::recorder;
use crate::report;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Where the server gets its data. The real one reads shared memory and files; tests fake it.
pub trait Source {
    /// The agent API switch from tachyon.toml.
    fn enabled(&self) -> bool;
    /// Live status: from the running Tachyon, or a short reading of its own.
    fn status(&mut self) -> Value;
    /// A fresh bottleneck verdict (standalone: a few quick readings).
    fn bottleneck(&mut self) -> Value {
        self.status()["bottleneck"].clone()
    }
    fn blackbox(&mut self) -> Vec<crate::telemetry::Sample>;
    fn report(&mut self) -> Option<report::Report>;
    fn history(&mut self, n: usize) -> Vec<JobRecord>;
    /// Windows Event Log entries in a window (System and Application logs).
    fn events(&mut self, from_ms: i64, to_ms: i64) -> Result<Vec<crate::events::SysEvent>, String> {
        crate::events::query(from_ms, to_ms, 400)
    }
    /// What the diagnosis knows about this PC now: RAM speed, GPU clock offsets.
    fn context(&mut self) -> crate::diagnose::Context {
        crate::diagnose::Context { ram: crate::sysinfo::ram_speed(), ..Default::default() }
    }
}

/// A local date ("2026-10-22", the whole day) or date and time ("2026-10-22 18:40", an hour
/// either side) as a window in ms.
fn parse_around(s: &str) -> Option<(i64, i64)> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let s = s.trim().replace('T', " ");
    let local = |n: NaiveDateTime| Local.from_local_datetime(&n).earliest().map(|d| d.timestamp_millis());
    if let Ok(n) = NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M") {
        let t = local(n)?;
        return Some((t - 3_600_000, t + 3_600_000));
    }
    let d = NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()?;
    let t = local(d.and_hms_opt(0, 0, 0)?)?;
    Some((t, t + 86_400_000))
}

/// `diagnose_crash` without a crash report (or for a date): the latest unexpected restart in the
/// Event Log, diagnosed from the events around it and what Tachyon can read about the PC now.
fn diagnose_from_event_log(args: &Value, src: &mut dyn Source, now: i64) -> Result<Value, String> {
    let (from, to, what) = match args["around"].as_str() {
        Some(a) => {
            let (f, t) = parse_around(a).ok_or("`around` must be a local date (2026-10-22) or date and time (2026-10-22 18:40).")?;
            (f, t, format!("around {}", a.trim()))
        }
        None => {
            let days = args["days"].as_u64().unwrap_or(14).clamp(1, 60) as i64;
            (now - days * 86_400_000, now, format!("in the last {days} days"))
        }
    };
    let found = src.events(from, to)?;
    let gpu_resets = found.iter().filter(|e| e.is_gpu()).count();
    let Some(anchor) = found.iter().filter(|e| e.is_restart()).map(|e| e.t_ms).max() else {
        return Ok(json!({
            "diagnosis": null,
            "source": "event_log",
            "gpu_driver_events": gpu_resets,
            "note": format!("No unexpected restart in the Windows Event Log {what}{}.", if gpu_resets > 0 { format!(", but {gpu_resets} GPU driver event(s)") } else { String::new() }),
        }));
    };
    // the crash itself comes before the restart's reports (Kernel-Power 41, then the bug check);
    // GPU driver resets in the 12 hours before count as signs of the same instability
    let first = found.iter().filter(|e| e.is_restart() && e.t_ms >= anchor - 15 * 60_000).map(|e| e.t_ms).min().unwrap_or(anchor);
    let ev = src.events(first - 12 * 3_600_000, anchor + 10 * 60_000)?;
    let ctx = src.context();
    let d = crate::diagnose::diagnose(report::Cause::Restart, &ev, true, &report::Peaks::default(), &[], None, &ctx);
    let at = chrono::DateTime::from_timestamp_millis(first).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string());
    Ok(json!({
        "source": "event_log",
        "restart_at": at,
        "diagnosis": d,
        "events": ev.iter().map(|e| format!("{}  {}", chrono::DateTime::from_timestamp_millis(e.t_ms).map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string()).unwrap_or_default(), e.describe())).collect::<Vec<_>>(),
        "note": "From the Windows Event Log only (the 12 hours before the restart): Tachyon has no readings from before this restart, and the GPU clock offsets and RAM speed are as they are now, not necessarily as they were then.",
    }))
}

fn tool(name: &str, title: &str, description: &str, props: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
    })
}

pub fn tools() -> Value {
    json!([
        tool(
            "get_status",
            "GPU status",
            "What the NVIDIA GPU is doing right now: load, temperature, power, VRAM, clocks, CPU and RAM, the main bottleneck, the VRAM forecast, the running render job and the last crash report headline.",
            json!({})
        ),
        tool(
            "get_bottleneck",
            "Why is it slow?",
            "The current bottleneck in one plain sentence with a confidence (sure, likely, possibly), the process involved and the readings behind it.",
            json!({})
        ),
        tool(
            "get_vram_forecast",
            "VRAM forecast",
            "VRAM use and trend: when it will be full at the current rate (only when the trend is steady), the fastest-growing process, and background apps holding VRAM without using the GPU.",
            json!({})
        ),
        tool("get_processes", "GPU processes", "Processes using the GPU with their category, VRAM and engine use.", json!({})),
        tool(
            "get_blackbox",
            "Flight recorder",
            "The last minutes from Tachyon's flight recorder: peaks, the process holding the most VRAM, a per-minute timeline and, if asked, the samples. Also the last crash report, if any.",
            json!({
                "minutes": { "type": "number", "description": "How many minutes back (1-60, default 5).", "minimum": 1, "maximum": 60 },
                "include_samples": { "type": "boolean", "description": "Also return the samples (at most 300, evenly spread). Default false." },
            })
        ),
        tool(
            "get_history",
            "Job history",
            "The most recent finished render jobs and GPU-busy sessions: duration, average and peak GPU load, fps, energy in Wh, peak VRAM and temperature, time spent throttled, main bottleneck and the comparison with the previous run.",
            json!({ "n": { "type": "integer", "description": "How many jobs (1-100, default 10), newest last.", "minimum": 1, "maximum": 100 } })
        ),
        tool(
            "diagnose_crash",
            "Why did it crash?",
            "The probable causes of the last unexpected restart or GPU driver failure, ranked with a rough percentage: unstable RAM or EXPO/XMP, GPU overclock, power or PSU, overheating, driver timeout (TDR) or an application bug. Each with the evidence (bug check codes, NVIDIA driver resets, GPU timeout reports, WHEA errors, temperatures, power, clock offsets, memory speed) and one piece of advice. Uses Tachyon's crash report; without one (or with `around`) it diagnoses the latest unexpected restart in the Windows Event Log. Estimates, never certain; Tachyon changes no settings.",
            json!({
                "around": { "type": "string", "description": "Optional local date (2026-10-22) or date and time (2026-10-22 18:40) of a past crash to diagnose from the Event Log." },
                "days": { "type": "integer", "description": "Without a crash report: how many days back to look (1-60, default 14).", "minimum": 1, "maximum": 60 },
            })
        ),
        tool(
            "preflight",
            "Will this job fit?",
            "Pre-render check: predicts a render job's peak VRAM and duration from similar past runs (same job, same project, or a similar name, at the same resolution) and compares it with the VRAM free right now, including memory held by idle background apps. Returns fits / tight / wont_fit / unknown with a confidence, one plain sentence and the evidence. Without arguments it checks the render job that is running. Thin history gives `unknown`, never a guess.",
            json!({
                "name": { "type": "string", "description": "Job name, e.g. \"demo4k try 3\" (default: the running render job)." },
                "project": { "type": "string", "description": "Optional project folder name." },
                "resolution": { "type": "string", "description": "Optional resolution: 4K, 1440p, 1080p, 720p, 8K or WxH such as 3840x2160." },
                "frames": { "type": "integer", "description": "Optional frame count, for the duration.", "minimum": 1 },
            })
        ),
        tool(
            "compare_last_run",
            "Compare with last time",
            "Compares the latest job (or the latest job whose name contains `name`) with the previous run of the same job.",
            json!({ "name": { "type": "string", "description": "Optional part of the job name." } })
        ),
    ])
}

const OFF: &str = "The agent API is off. Turn on Agent API in Tachyon's Settings (right-click menu, AI agents), or set agent_api = true in tachyon.toml next to tachyon.exe.";

fn minute_lines(w: &[crate::telemetry::Sample]) -> Vec<Value> {
    let mut out = vec![];
    for chunk in w.chunk_by(|a, b| a.t_ms.div_euclid(60_000) == b.t_ms.div_euclid(60_000)) {
        let p = report::peaks(chunk);
        let avg = |f: &dyn Fn(&crate::telemetry::Sample) -> Option<f32>| {
            let v: Vec<f32> = chunk.iter().filter_map(f).collect();
            (!v.is_empty()).then(|| (v.iter().sum::<f32>() / v.len() as f32 * 10.0).round() / 10.0)
        };
        out.push(json!({
            "t_ms": chunk[0].t_ms,
            "time": chunk[0].local().format("%H:%M").to_string(),
            "avg_gpu": avg(&|s| s.util.map(f32::from)),
            "max_temp": p.temp,
            "avg_power_w": avg(&|s| s.power_w),
            "max_vram_mb": p.vram_mb,
            "avg_cpu": avg(&|s| s.cpu),
            "power_throttled_s": p.power_throttled_s,
            "thermal_throttled_s": p.thermal_throttled_s,
        }));
    }
    out
}

fn call(name: &str, args: &Value, src: &mut dyn Source) -> Result<Value, String> {
    if !src.enabled() {
        return Err(OFF.into());
    }
    let now = crate::telemetry::now_ms();
    match name {
        "get_status" => Ok(src.status()),
        "get_bottleneck" => {
            let b = src.bottleneck();
            Ok(if b.is_null() { json!({ "bottleneck": null, "note": "No GPU reading yet." }) } else { json!({ "bottleneck": b }) })
        }
        "get_vram_forecast" => {
            let s = src.status();
            Ok(json!({ "forecast": s["vram_forecast"], "idle_vram_note": s["idle_vram_note"], "source": s["source"] }))
        }
        "get_processes" => {
            let s = src.status();
            Ok(json!({ "processes": s["reading"]["procs"], "source": s["source"] }))
        }
        "get_blackbox" => {
            let minutes = args["minutes"].as_f64().unwrap_or(5.0).clamp(1.0, 60.0);
            let all = src.blackbox();
            let from = now - (minutes * 60_000.0) as i64;
            let w: Vec<_> = all.into_iter().filter(|s| s.t_ms >= from).collect();
            let mut v = json!({
                "minutes": minutes,
                "samples_in_window": w.len(),
                "from_ms": w.first().map(|s| s.t_ms),
                "to_ms": w.last().map(|s| s.t_ms),
                "peaks": report::peaks(&w),
                "most_vram": report::vram_holder(&w),
                "timeline": minute_lines(&w),
                "last_crash_report": src.report(),
            });
            if w.is_empty() {
                v["note"] = json!("Nothing recorded in this window. The flight recorder runs while Tachyon is open (blackbox = true).");
            }
            if args["include_samples"].as_bool().unwrap_or(false) {
                let step = w.len().div_ceil(300).max(1);
                v["samples"] = json!(w.iter().step_by(step).collect::<Vec<_>>());
            }
            Ok(v)
        }
        "get_history" => {
            let n = args["n"].as_u64().unwrap_or(10).clamp(1, 100) as usize;
            let jobs = src.history(n);
            Ok(json!({ "jobs": jobs, "count": jobs.len() }))
        }
        "diagnose_crash" => {
            let report = if args["around"].is_string() { None } else { src.report() };
            let Some(r) = report else {
                return diagnose_from_event_log(args, src, now);
            };
            let (d, recomputed) = match r.diagnosis.clone() {
                Some(d) => (Some(d), false),
                None => {
                    // a report from 1.0: work it out from what the report kept
                    let ctx = crate::diagnose::Context { ram: crate::sysinfo::ram_speed(), ..Default::default() };
                    let ev: Vec<_> = r.events.iter().map(|e| e.event.clone()).collect();
                    (
                        crate::diagnose::diagnose(
                            r.cause,
                            &ev,
                            r.events_note.is_none(),
                            &r.peaks_last_minute,
                            &[],
                            r.vram_holder.as_ref(),
                            &ctx,
                        ),
                        true,
                    )
                }
            };
            let mut v = json!({
                "headline": r.headline,
                "stopped_ms": r.stopped_ms,
                "cause": r.cause,
                "diagnosis": d,
                "events": r.events.iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
                "just_before": r.just_before,
            });
            if d.is_none() {
                v["note"] = json!("Tachyon stopped without closing, but Windows logged nothing that points to a system problem.");
            } else if recomputed {
                v["note"] = json!("Worked out now from an older report, without the GPU overclock reading from before the crash.");
            }
            Ok(v)
        }
        "preflight" => preflight(args, src),
        "compare_last_run" => {
            let filter = args["name"].as_str().map(str::to_lowercase);
            let jobs = src.history(2000);
            let Some(cur) = jobs.iter().rev().find(|j| filter.as_ref().is_none_or(|f| j.name.to_lowercase().contains(f))) else {
                return Ok(json!({ "comparison": null, "note": "No finished job matches yet." }));
            };
            let prev = jobs.iter().rev().find(|j| j.key == cur.key && j.end_ms <= cur.start_ms);
            Ok(match prev {
                Some(p) => json!({ "comparison": history::compare(p, cur), "latest": cur, "previous": p }),
                None => json!({ "comparison": null, "latest": cur, "note": "This is the first recorded run of this job." }),
            })
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

/// The `preflight` tool: the running job's published check, or a check for the job asked about.
fn preflight(args: &Value, src: &mut dyn Source) -> Result<Value, String> {
    use crate::preflight::{LiveVram, Query, predict};
    let s = src.status();
    let arg = |k: &str| args[k].as_str().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let resolution = arg("resolution").map(|r| crate::parse::resolution_hint(&r).map(crate::parse::resolution_label).unwrap_or(r));
    let running = s["render"]["job"].as_str().map(str::to_string);
    let Some(name) = arg("name").or_else(|| running.clone()) else {
        return Ok(
            json!({ "preflight": null, "note": "No render job is running. Pass `name` (and `resolution`) to check a job before starting it." }),
        );
    };
    let is_running = running
        .as_ref()
        .is_some_and(|r| history::job_key(history::JobKind::Render, r) == history::job_key(history::JobKind::Render, &name));
    // the running Tachyon's own check knows what the job already holds
    if is_running
        && !s["preflight"].is_null()
        && arg("project").is_none()
        && args["frames"].is_null()
        && resolution.as_ref().is_none_or(|r| s["preflight"]["resolution"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(r)))
    {
        return Ok(json!({ "preflight": s["preflight"], "source": s["source"] }));
    }
    let from_render = |k: &str| if is_running { s["render"][k].as_str().map(str::to_string) } else { None };
    let q = Query {
        name,
        project: arg("project").or_else(|| from_render("project")),
        resolution: resolution.or_else(|| from_render("resolution")),
        frames: args["frames"].as_u64().or_else(|| if is_running { s["render"]["total_frames"].as_u64() } else { None }),
    };
    let r = &s["reading"];
    let live = LiveVram {
        total_mb: r["vram_total_mb"].as_f64().map(|v| v as f32),
        used_mb: r["vram_mb"].as_f64().map(|v| v as f32),
        job_now_mb: 0.0,
        hogs: serde_json::from_value(s["vram_forecast"]["hogs"].clone()).unwrap_or_default(),
    };
    let p = predict(&q, &src.history(500), &live);
    let mut v = json!({ "preflight": p, "source": s["source"] });
    if is_running {
        v["note"] = json!("This job is already running: what it holds now counts as used here.");
    } else if live.used_mb.is_none() {
        v["note"] = json!("No VRAM reading right now, so only the history is shown.");
    }
    Ok(v)
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Handle one JSON-RPC message. Returns the reply, or None for notifications.
pub fn handle(msg: &Value, src: &mut dyn Source) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let id = id?; // none: a notification (initialized, cancelled, ...)
    if msg["jsonrpc"] != "2.0" || method.is_empty() {
        return Some(err(&id, -32600, "Invalid request"));
    }
    Some(match method {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOLS[0]);
            let v = if PROTOCOLS.contains(&asked) { asked } else { PROTOCOLS[0] };
            ok(
                &id,
                json!({
                    "protocolVersion": v,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "tachyon", "title": "Tachyon GPU monitor", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Read-only view of this PC's NVIDIA GPU from Tachyon. Use get_bottleneck to explain slowness, get_vram_forecast before loading large models, get_blackbox and diagnose_crash after a crash, preflight before starting a render (will it fit?), get_history and compare_last_run for render jobs.",

                }),
            )
        }
        "ping" => ok(&id, json!({})),
        "tools/list" => ok(&id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args, src).map(|mut v| {
                crate::agent::tidy(&mut v);
                v
            }) {
                Ok(v) => ok(
                    &id,
                    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }], "structuredContent": v, "isError": false }),
                ),
                Err(e) => ok(&id, json!({ "content": [{ "type": "text", "text": e }], "isError": true })),
            }
        }
        _ => err(&id, -32601, &format!("Method not found: {method}")),
    })
}

/// The real source.
pub struct Live {
    cfg: Config,
    dir: Option<PathBuf>,
    standalone: Option<Standalone>,
}

struct Standalone {
    nvml: Option<crate::nvml::Nvml>,
    sys: crate::sysinfo::SysMeter,
    explainer: crate::explain::Explainer,
}

impl Live {
    pub fn new(cfg: Config) -> Self {
        Live { cfg, dir: crate::config::data_dir(), standalone: None }
    }

    /// One reading of our own (Tachyon not running or agent API off in the running copy).
    fn own_reading(&mut self) -> crate::monitor::Snapshot {
        let st = self.standalone.get_or_insert_with(|| Standalone {
            nvml: crate::nvml::Nvml::open().ok(),
            sys: Default::default(),
            explainer: Default::default(),
        });
        let mut snap = crate::monitor::Snapshot::default();
        let (cpu, mem) = st.sys.sample(1);
        let mut g = None;
        if let Some(n) = st.nvml.as_mut() {
            let sel = self.cfg.gpu_index.min(n.names.len().saturating_sub(1) as u32);
            snap.gpus = n.names.clone();
            snap.driver = n.driver.clone();
            snap.selected = sel;
            g = Some(n.sample(sel));
        } else {
            snap.error = Some("NVML unavailable".into());
        }
        let procs = g.as_ref().and_then(|g| g.procs.as_ref()).map(|list| {
            #[cfg(windows)]
            let table = crate::procinfo::win::snapshot();
            #[cfg(not(windows))]
            let table = crate::procinfo::ProcTable::default();
            crate::monitor::attribute(list, &table, &self.cfg.process_rules, &Default::default())
        });
        let s = crate::telemetry::Sample::from_tick(
            crate::telemetry::now_ms(),
            g.as_ref(),
            procs.as_deref(),
            cpu,
            mem,
            crate::insight::KEEP_PROCS,
        );
        snap.insights.bottleneck = st.explainer.push(&s).cloned();
        snap.insights.latest = Some(s);
        snap.insights.data_dir = self.dir.clone();
        snap.insights.report = self.dir.as_ref().and_then(|d| report::load(&d.join("blackbox"))).map(std::sync::Arc::new);
        snap.sample = g;
        snap.procs = procs;
        snap
    }
}

impl Source for Live {
    fn enabled(&self) -> bool {
        self.cfg.agent_api
    }

    fn status(&mut self) -> Value {
        if let Some(v) =
            crate::agent::read_published().filter(|v| v["t_ms"].as_i64().is_some_and(|t| crate::telemetry::now_ms() - t < 10_000))
        {
            return v;
        }
        // CPU load needs two readings; take them a moment apart.
        if self.standalone.is_none() {
            self.own_reading();
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let snap = self.own_reading();
        let mut v = crate::agent::status(&snap, "standalone");
        v["note"] =
            json!("Tachyon is not running (or runs without agent_api = true), so this is a single reading: no VRAM trend or job yet.");
        v
    }

    fn bottleneck(&mut self) -> Value {
        let v = self.status();
        if v["source"] == "tachyon" {
            return v["bottleneck"].clone();
        }
        // standalone: a few quick readings for the rules to work on
        let mut last = None;
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            last = self.own_reading().insights.bottleneck;
        }
        serde_json::to_value(last).unwrap_or(Value::Null)
    }

    fn blackbox(&mut self) -> Vec<crate::telemetry::Sample> {
        self.dir.as_ref().map(|d| recorder::read_current(&d.join("blackbox"))).unwrap_or_default()
    }

    fn report(&mut self) -> Option<report::Report> {
        report::load(&self.dir.as_ref()?.join("blackbox"))
    }

    fn history(&mut self, n: usize) -> Vec<JobRecord> {
        self.dir.as_ref().map(|d| history::recent(&d.join("history"), n)).unwrap_or_default()
    }

    fn context(&mut self) -> crate::diagnose::Context {
        // the clock offsets now (read-only NVML queries); they may differ from the day of the crash
        let oc = crate::nvml::Nvml::open().ok().map(|mut n| {
            let sel = self.cfg.gpu_index.min(n.names.len().saturating_sub(1) as u32);
            n.oc_info(sel)
        });
        crate::diagnose::Context { oc: oc.filter(|o| !o.is_empty()), oc_after: true, ram: crate::sysinfo::ram_speed() }
    }
}

/// Run the server until stdin closes.
pub fn serve(cfg: Config) {
    let mut src = Live::new(cfg);
    let stdin = std::io::stdin();
    serve_io(stdin.lock(), std::io::stdout().lock(), &mut src);
}

/// Longest request line read; longer lines are answered with an error and skipped.
pub const MAX_LINE: usize = 1 << 20;
/// Most requests answered from one JSON-RPC batch.
pub const MAX_BATCH: usize = 64;

/// Read one line of at most `MAX_LINE` bytes. `Ok(None)` = end of input; `Err(())` = too long
/// (the rest of the line has been skipped).
fn read_line_capped(input: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Option<Result<(), ()>>> {
    buf.clear();
    let mut too_long = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && !too_long { None } else { Some(if too_long { Err(()) } else { Ok(()) }) });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            if buf.len() + take > MAX_LINE + 1 {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        input.consume(take);
        if done {
            return Ok(Some(if too_long { Err(()) } else { Ok(()) }));
        }
    }
}

/// The JSON-RPC loop over any reader and writer: one request (or batch) per line.
/// Bad UTF-8, garbage and oversized lines get an error reply; the server keeps going.
pub fn serve_io(mut input: impl BufRead, mut out: impl Write, src: &mut dyn Source) {
    let mut buf = Vec::with_capacity(4096);
    loop {
        let reply = match read_line_capped(&mut input, &mut buf) {
            Err(_) | Ok(None) => break,
            Ok(Some(Err(()))) => Some(err(&Value::Null, -32600, "Request too large")),
            Ok(Some(Ok(()))) => {
                let Ok(line) = std::str::from_utf8(&buf) else {
                    let r = err(&Value::Null, -32700, "Parse error: not UTF-8");
                    let _ = writeln!(out, "{r}");
                    let _ = out.flush();
                    continue;
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(Value::Array(batch)) if batch.is_empty() => Some(err(&Value::Null, -32600, "Empty batch")),
                    Ok(Value::Array(batch)) if batch.len() > MAX_BATCH => Some(err(&Value::Null, -32600, "Batch too large")),
                    Ok(Value::Array(batch)) => {
                        let r: Vec<Value> = batch.iter().filter_map(|m| handle(m, src)).collect();
                        (!r.is_empty()).then_some(Value::Array(r))
                    }
                    Ok(m) => handle(&m, src),
                    Err(_) => Some(err(&Value::Null, -32700, "Parse error")),
                }
            }
        };
        if let Some(r) = reply {
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::Sample;

    #[derive(Default)]
    struct Fake {
        on: bool,
        jobs: Vec<JobRecord>,
        report: Option<report::Report>,
        status: Option<Value>,
        events: Vec<crate::events::SysEvent>,
        ctx: crate::diagnose::Context,
    }

    impl Source for Fake {
        fn enabled(&self) -> bool {
            self.on
        }
        fn status(&mut self) -> Value {
            if let Some(s) = &self.status {
                return s.clone();
            }
            json!({ "source": "tachyon", "bottleneck": { "kind": "gpu_bound", "sentence": "The GPU is the limit." }, "vram_forecast": { "warning": "At this rate VRAM will be full in ~3 min." }, "reading": { "procs": [{ "name": "python.exe" }] } })
        }
        fn blackbox(&mut self) -> Vec<Sample> {
            let now = crate::telemetry::now_ms();
            (0..600).map(|i| Sample { t_ms: now - (600 - i) * 1000, util: Some(90), ..Default::default() }).collect()
        }
        fn report(&mut self) -> Option<report::Report> {
            self.report.clone()
        }
        fn history(&mut self, n: usize) -> Vec<JobRecord> {
            let k = self.jobs.len().saturating_sub(n);
            self.jobs[k..].to_vec()
        }
        fn events(&mut self, from: i64, to: i64) -> Result<Vec<crate::events::SysEvent>, String> {
            Ok(self.events.iter().filter(|e| e.t_ms >= from && e.t_ms <= to).cloned().collect())
        }
        fn context(&mut self) -> crate::diagnose::Context {
            self.ctx.clone()
        }
    }

    fn job(name: &str, start: i64, dur: f64) -> JobRecord {
        serde_json::from_value(json!({
            "kind": "render", "name": name, "key": history::job_key(history::JobKind::Render, name), "start_ms": start, "end_ms": start + (dur * 1000.0) as i64,
            "duration_s": dur, "avg_gpu": 90.0, "peak_gpu": 99, "avg_fps": null, "energy_wh": null, "energy_source": null, "peak_vram_mb": null,
            "peak_temp": null, "power_throttled_s": 0.0, "thermal_throttled_s": 0.0, "bottleneck": null
        }))
        .unwrap()
    }

    fn rpc(src: &mut Fake, id: i64, method: &str, params: Value) -> Value {
        handle(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }), src).unwrap()
    }

    #[test]
    fn handshake_and_tool_list() {
        let mut f = Fake { on: true, ..Default::default() };
        let r = rpc(
            &mut f,
            1,
            "initialize",
            json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "tachyon");
        let r = rpc(&mut f, 1, "initialize", json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(r["result"]["protocolVersion"], PROTOCOLS[0]);
        assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), &mut f).is_none());
        let r = rpc(&mut f, 2, "tools/list", json!({}));
        let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "get_status",
                "get_bottleneck",
                "get_vram_forecast",
                "get_processes",
                "get_blackbox",
                "get_history",
                "diagnose_crash",
                "preflight",
                "compare_last_run"
            ]
        );
        assert!(r["result"]["tools"].as_array().unwrap().iter().all(|t| t["annotations"]["readOnlyHint"] == true));
        assert_eq!(rpc(&mut f, 3, "ping", json!({}))["result"], json!({}));
        assert_eq!(rpc(&mut f, 4, "resources/list", json!({}))["error"]["code"], -32601);
    }

    #[test]
    fn preflight_checks_a_named_job_or_the_running_one() {
        let mut jobs = vec![];
        for i in 0..3 {
            let mut j = job(&format!("demo4k try {i}"), i * 4_000_000, 1800.0);
            j.peak_vram_mb = Some(15000.0);
            j.profile.own_vram_mb = Some(15900.0 + i as f32 * 10.0);
            j.profile.hints.resolution = Some("4K".into());
            j.profile.hints.frames = Some(240);
            jobs.push(j);
        }
        let status = json!({
            "source": "tachyon",
            "reading": { "vram_mb": 1800.0, "vram_total_mb": 16376.0 },
            "vram_forecast": { "hogs": [{ "name": "chrome.exe", "category": "Browser", "vram_mb": 1230.0, "idle_secs": 300.0 }] },
            "render": null, "preflight": null,
        });
        let mut f = Fake { on: true, jobs, status: Some(status.clone()), ..Default::default() };
        let call = |f: &mut Fake, args: Value| rpc(f, 9, "tools/call", json!({ "name": "preflight", "arguments": args }))["result"].clone();
        // nothing running, no name
        let r = call(&mut f, json!({}));
        assert!(r["structuredContent"]["note"].as_str().unwrap().starts_with("No render job is running"));
        // before starting: named, resolution written any way
        let r = call(&mut f, json!({ "name": "demo4k try 3", "resolution": "3840x2160", "frames": 480 }))["structuredContent"].clone();
        let p = &r["preflight"];
        assert_eq!(p["fit"], "wont_fit", "{r:#}");
        assert_eq!(p["resolution"], "4K");
        assert_eq!(p["duration_s"], 3600.0);
        assert!(
            p["sentence"].as_str().unwrap().starts_with("At 4K this job needs ~15.5 GB of VRAM; 14.2 GB is free."),
            "{}",
            p["sentence"]
        );
        assert!(p["hog_note"].as_str().unwrap().starts_with("chrome.exe holds 1.2 GB"));
        // at 1080p there is no history
        let r = call(&mut f, json!({ "name": "demo4k", "resolution": "1080p" }))["structuredContent"].clone();
        assert_eq!(r["preflight"]["fit"], "unknown");
        // running: the published check is returned as is
        let mut s = status;
        s["render"] = json!({ "job": "demo4k try 3", "resolution": "4K" });
        s["preflight"] = json!({ "job": "demo4k try 3", "resolution": "4K", "fit": "tight", "sentence": "published" });
        f.status = Some(s);
        assert_eq!(call(&mut f, json!({}))["structuredContent"]["preflight"]["sentence"], "published");
        assert_eq!(call(&mut f, json!({ "name": "Demo4K try 3" }))["structuredContent"]["preflight"]["sentence"], "published");
        // asked about something else while it runs: computed, with the running job's hints when it is the same job
        let r = call(&mut f, json!({ "name": "demo4k", "frames": 240 }))["structuredContent"].clone();
        assert_eq!(r["preflight"]["resolution"], "4K");
        assert!(r["note"].as_str().unwrap().contains("already running"));
        // the switch
        f.on = false;
        assert_eq!(call(&mut f, json!({}))["isError"], true);
    }

    #[test]
    fn tools_answer_and_respect_the_switch() {
        let mut f = Fake {
            on: true,
            jobs: vec![job("Shot 12 try 1", 0, 1000.0), job("Other", 2_000_000, 50.0), job("Shot 12 try 2", 3_000_000, 1140.0)],
            ..Default::default()
        };
        let call =
            |f: &mut Fake, name: &str, args: Value| rpc(f, 9, "tools/call", json!({ "name": name, "arguments": args }))["result"].clone();
        let r = call(&mut f, "get_bottleneck", json!({}));
        assert_eq!(r["isError"], false);
        assert_eq!(r["structuredContent"]["bottleneck"]["kind"], "gpu_bound");
        assert!(r["content"][0]["text"].as_str().unwrap().contains("The GPU is the limit."));
        let r = call(&mut f, "get_vram_forecast", json!({}));
        assert_eq!(r["structuredContent"]["forecast"]["warning"], "At this rate VRAM will be full in ~3 min.");
        assert_eq!(call(&mut f, "get_processes", json!({}))["structuredContent"]["processes"][0]["name"], "python.exe");
        let r = call(&mut f, "get_blackbox", json!({ "minutes": 2, "include_samples": true }))["structuredContent"].clone();
        assert!((119..=121).contains(&r["samples_in_window"].as_u64().unwrap()), "{}", r["samples_in_window"]);
        assert_eq!(r["peaks"]["gpu_util"], 90);
        assert!(r["timeline"].as_array().unwrap().len() >= 2);
        assert_eq!(r["samples"].as_array().unwrap().len(), r["samples_in_window"].as_u64().unwrap() as usize);
        assert_eq!(call(&mut f, "get_history", json!({ "n": 2 }))["structuredContent"]["count"], 2);
        let r = call(&mut f, "compare_last_run", json!({ "name": "shot 12" }))["structuredContent"].clone();
        assert_eq!(r["comparison"], "14% slower than last time.");
        let r = call(&mut f, "compare_last_run", json!({}))["structuredContent"].clone();
        assert_eq!(r["comparison"], "14% slower than last time.", "latest job is Shot 12 try 2");
        assert_eq!(call(&mut f, "nope", json!({}))["isError"], true);
        f.on = false;
        let r = call(&mut f, "get_status", json!({}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("agent_api = true"));
    }

    #[test]
    fn diagnose_crash_from_the_event_log() {
        use crate::events::{KERNEL_POWER, SysEvent, WER_SYSTEM};
        let call = |f: &mut Fake, a: Value| rpc(f, 5, "tools/call", json!({ "name": "diagnose_crash", "arguments": a }))["result"].clone();
        let day = parse_around("2026-10-22").unwrap();
        assert_eq!(day.1 - day.0, 86_400_000);
        let at = parse_around("2026-10-22 18:40").unwrap();
        let t = at.0 + 3_600_000;
        let ev = |dt: i64, p: &str, id: u32, code: Option<&str>| SysEvent {
            t_ms: t + dt * 1000,
            provider: p.into(),
            id,
            level: 2,
            code: code.map(Into::into),
        };
        let mut f = Fake {
            on: true,
            events: vec![
                ev(-86_400, "nvlddmkm", 153, None),   // a day earlier: outside the crash window
                ev(-9 * 3600, "nvlddmkm", 153, None), // hours before: inside it
                ev(-900, "nvlddmkm", 153, None),
                ev(-60, "nvlddmkm", 153, None),
                ev(90, KERNEL_POWER, 41, Some("0x0000001A")),
                ev(150, WER_SYSTEM, 1001, Some("0x0000001a")),
            ],
            ctx: crate::diagnose::Context {
                ram: Some(crate::sysinfo::RamSpeed {
                    kind: "DDR5".into(),
                    rated_mts: Some(4800),
                    configured_mts: Some(6000),
                    modules: 2,
                    part: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        for args in [json!({ "around": "2026-10-22" }), json!({ "around": "2026-10-22 18:40" })] {
            let r = call(&mut f, args)["structuredContent"].clone();
            assert_eq!(r["source"], "event_log");
            assert_eq!(r["diagnosis"]["causes"][0]["suspect"], "ram", "{r:#}");
            assert_eq!(r["events"].as_array().unwrap().len(), 5, "{r:#}");
            assert!(r["note"].as_str().unwrap().contains("Event Log only"));
        }
        assert!(call(&mut f, json!({ "around": "22/10" }))["isError"] == true);
        assert!(
            call(&mut f, json!({ "around": "2026-10-21" }))["structuredContent"]["note"].as_str().unwrap().contains("1 GPU driver event")
        );
    }

    #[test]
    fn diagnose_crash_from_the_report() {
        use crate::events::SysEvent;
        use crate::recorder::{PrevSession, Session};
        let call = |f: &mut Fake| rpc(f, 5, "tools/call", json!({ "name": "diagnose_crash", "arguments": {} }))["result"].clone();
        let mut f = Fake { on: true, ..Default::default() };
        assert!(call(&mut f)["structuredContent"]["note"].as_str().unwrap().starts_with("No unexpected restart in the Windows Event Log"));
        let t0 = 1_790_000_040_000i64;
        let prev = PrevSession {
            session: Session { version: "1.0.1".into(), pid: 1, file: "a".into(), started_ms: t0, clean: false, ended_ms: None },
            samples: (0..120)
                .map(|i| Sample {
                    t_ms: t0 + i * 1000,
                    util: Some(97),
                    temp: Some(66),
                    power_w: Some(264.0),
                    power_limit_w: Some(360.0),
                    ..Default::default()
                })
                .collect(),
        };
        let stop = t0 + 119_000;
        let ev = |dt: i64, p: &str, id: u32, code: Option<&str>| SysEvent {
            t_ms: stop + dt,
            provider: p.into(),
            id,
            level: 2,
            code: code.map(str::to_string),
        };
        let events = vec![ev(-5000, "nvlddmkm", 153, None), ev(60_000, crate::events::KERNEL_POWER, 41, Some("0x0000001A"))];
        let mut r = report::build(&prev, Some(stop + 30_000), Ok(events), stop + 90_000).unwrap();
        assert!(r.diagnosis.is_some());
        f.report = Some(r.clone());
        let v = call(&mut f)["structuredContent"].clone();
        assert_eq!(v["diagnosis"]["causes"][0]["suspect"], "ram", "{v:#}");
        assert!(v["note"].is_null());
        // a 1.0 report without a diagnosis: worked out from the report
        r.diagnosis = None;
        f.report = Some(r);
        let v = call(&mut f)["structuredContent"].clone();
        assert_eq!(v["diagnosis"]["causes"][0]["suspect"], "ram");
        assert!(v["note"].as_str().unwrap().contains("older report"));
    }

    fn serve_bytes(input: &[u8]) -> Vec<Value> {
        let mut f = Fake { on: true, jobs: vec![job("a", 1, 60.0)], ..Default::default() };
        let mut out = Vec::new();
        serve_io(std::io::Cursor::new(input.to_vec()), &mut out, &mut f);
        String::from_utf8(out).expect("replies are UTF-8").lines().map(|l| serde_json::from_str(l).expect("each reply is JSON")).collect()
    }

    #[test]
    fn hostile_input_gets_errors_and_the_server_keeps_going() {
        let ping = br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
        let mut input = Vec::new();
        input.extend_from_slice(b"\xff\xfe not utf-8\n");
        input.extend_from_slice(b"garbage {{{\n");
        input.extend_from_slice(&vec![b'x'; MAX_LINE + 100]);
        input.push(b'\n');
        input.extend_from_slice(format!("[{}]\n", vec![std::str::from_utf8(ping).unwrap(); MAX_BATCH + 1].join(",")).as_bytes());
        input.extend_from_slice(b"[]\n");
        input.extend_from_slice(&[b'['; 10_000]);
        input.push(b'\n');
        input.extend_from_slice(b"42\n\"str\"\nnull\n{}\n");
        input.extend_from_slice(ping);
        input.push(b'\n');
        let r = serve_bytes(&input);
        let codes: Vec<i64> = r.iter().map(|v| v["error"]["code"].as_i64().unwrap_or(0)).collect();
        assert_eq!(&codes[..6], &[-32700, -32700, -32600, -32600, -32600, -32700], "{r:?}");
        assert_eq!(r.last().unwrap()["id"], 7, "still answering after all that");
        assert_eq!(r.last().unwrap()["result"], json!({}));
    }

    #[test]
    fn random_bytes_never_panic_or_hang() {
        let mut rng = crate::fuzz::Rng::new(0x7ac4);
        let seeds: [&[u8]; 4] = [
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_history","arguments":{"n":5}}}"#,
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_blackbox","arguments":{"minutes":1e308}}}"#,
            br#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":null}}"#,
            br#"[{"jsonrpc":"2.0","id":4,"method":"tools/list"},{"id":null}]"#,
        ];
        for round in 0..400 {
            let mut input = Vec::new();
            for _ in 0..4 {
                let k = rng.below(seeds.len());
                input.extend(rng.mutate(seeds[k]));
                input.push(b'\n');
            }
            let t = std::time::Instant::now();
            let _ = serve_bytes(&input);
            assert!(t.elapsed().as_secs() < 5, "round {round} took too long");
        }
    }
}
