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
                    "instructions": "Read-only view of this PC's NVIDIA GPU from Tachyon. Use get_bottleneck to explain slowness, get_vram_forecast before loading large models, get_blackbox after a crash, get_history and compare_last_run for render jobs.",
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

    struct Fake {
        on: bool,
        jobs: Vec<JobRecord>,
    }

    impl Source for Fake {
        fn enabled(&self) -> bool {
            self.on
        }
        fn status(&mut self) -> Value {
            json!({ "source": "tachyon", "bottleneck": { "kind": "gpu_bound", "sentence": "The GPU is the limit." }, "vram_forecast": { "warning": "At this rate VRAM will be full in ~3 min." }, "reading": { "procs": [{ "name": "python.exe" }] } })
        }
        fn blackbox(&mut self) -> Vec<Sample> {
            let now = crate::telemetry::now_ms();
            (0..600).map(|i| Sample { t_ms: now - (600 - i) * 1000, util: Some(90), ..Default::default() }).collect()
        }
        fn report(&mut self) -> Option<report::Report> {
            None
        }
        fn history(&mut self, n: usize) -> Vec<JobRecord> {
            let k = self.jobs.len().saturating_sub(n);
            self.jobs[k..].to_vec()
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
        let mut f = Fake { on: true, jobs: vec![] };
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
            ["get_status", "get_bottleneck", "get_vram_forecast", "get_processes", "get_blackbox", "get_history", "compare_last_run"]
        );
        assert!(r["result"]["tools"].as_array().unwrap().iter().all(|t| t["annotations"]["readOnlyHint"] == true));
        assert_eq!(rpc(&mut f, 3, "ping", json!({}))["result"], json!({}));
        assert_eq!(rpc(&mut f, 4, "resources/list", json!({}))["error"]["code"], -32601);
    }

    #[test]
    fn tools_answer_and_respect_the_switch() {
        let mut f = Fake {
            on: true,
            jobs: vec![job("Shot 12 try 1", 0, 1000.0), job("Other", 2_000_000, 50.0), job("Shot 12 try 2", 3_000_000, 1140.0)],
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

    fn serve_bytes(input: &[u8]) -> Vec<Value> {
        let mut f = Fake { on: true, jobs: vec![job("a", 1, 60.0)] };
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
