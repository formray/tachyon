//! Robustness: every reader of outside data gets random bytes and mutated valid inputs and must
//! neither panic nor hang. Deterministic (fixed seeds), so a failure always reproduces.
use std::time::Instant;
use tachyon::fuzz::Rng;
use tachyon::{config, events, history, parse, png, recorder, report, scan, telemetry::Sample};

/// Rounds per reader; `TACHYON_FUZZ_ROUNDS=20000 cargo test --release --test robustness_test` for a long run.
fn rounds() -> usize {
    std::env::var("TACHYON_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(600)
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("tachyon_rb_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn inputs(rng: &mut Rng, seeds: &[&[u8]]) -> Vec<u8> {
    if rng.below(4) == 0 {
        rng.bytes(4096)
    } else {
        let k = rng.below(seeds.len());
        rng.mutate(seeds[k])
    }
}

#[test]
fn log_parsers_take_anything() {
    let seeds: [&[u8]; 6] = [
        b"frame=  120 fps= 24 q=28.0 size=1024kB time=00:00:05.00 bitrate=1677.7kbits/s speed=1.0x\n",
        b"12:01:02 start 02x 169 try 1 NF=1420\n> python prepare.py\nframe 10/1420\n",
        b"  Duration: 00:01:00.00, start: 0.000000, bitrate: 1000 kb/s\n    Stream #0:0: Video: h264, 24 fps\n",
        b"{\"frame\": 10, \"total\": 100, \"fps\": 2.5, \"stage\": \"render\"}",
        b"\xff\xfe1\x002\x00/\x001\x000\x00\n\x00",
        b"744 1.0\n747 2.0\n",
    ];
    let mut rng = Rng::new(11);
    let t = Instant::now();
    for _ in 0..rounds() * 4 {
        let b = inputs(&mut rng, &seeds);
        let text = parse::decode(&b);
        let _ = parse::ffmpeg_progress(&text);
        let _ = parse::slash_progress(&text);
        let _ = parse::total_frames(&text);
        let _ = parse::ffmpeg_header_total(&text);
        let _ = parse::progress_json(&text);
        let _ = parse::current_section(&text);
        let _ = parse::current_stage(&text);
        let _ = parse::batch_state(&text);
        let _ = parse::chunk_name(&text);
        let mut c = vec![parse::parse_chunk(rng.next_u64() % 10_000, &text)];
        let _ = parse::chunk_progress(&mut c, Some(rng.next_u64()));
        let _ = parse::lines(&text).count();
        let _ = parse::eta_secs(rng.next_u64(), rng.next_u64(), f64::from_bits(rng.next_u64()));
        let _ = parse::fmt_dur(f64::from_bits(rng.next_u64()));
    }
    assert!(t.elapsed().as_secs() < 30);
}

#[test]
fn flight_recorder_files_take_anything() {
    let d = scratch("rec");
    let good = recorder::encode(&Sample { t_ms: 1_700_000_000_000, util: Some(50), temp: Some(60), ..Default::default() }, 7);
    let mut rng = Rng::new(22);
    for _ in 0..rounds() * 4 {
        let b = rng.mutate(&good);
        let _ = recorder::decode(&b);
        let _ = recorder::decode(&rng.bytes(400));
    }
    // whole ring files: our header + damaged records, random files, and session.json pointing elsewhere
    let mut file = b"TACHYBB1".to_vec();
    file.resize(64, 0);
    for i in 0..20 {
        file.extend_from_slice(&recorder::encode(&Sample { t_ms: 1_700_000_000_000 + i * 1000, ..Default::default() }, i as u32));
    }
    for round in 0..200 {
        let body = if round % 2 == 0 { rng.mutate(&file) } else { rng.bytes(8192) };
        std::fs::write(d.join("blackbox_a.bin"), &body).unwrap();
        let _ = recorder::read_ring(&d.join("blackbox_a.bin"));
        let s = rng.mutate(br#"{"version":"1.0.0","pid":1,"file":"a","started_ms":1,"clean":false}"#);
        std::fs::write(d.join("session.json"), s).unwrap();
        let _ = recorder::read_current(&d);
    }
    std::fs::write(d.join("session.json"), br#"{"version":"1","pid":1,"file":"../../etc/passwd","started_ms":1,"clean":true}"#).unwrap();
    assert!(recorder::read_session(&d).is_none(), "session.json can only name our two ring files");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn job_history_skips_damaged_lines_and_keeps_the_rest() {
    let d = scratch("hist");
    let line = br#"{"kind":"render","name":"demo","key":"render:demo","start_ms":1,"end_ms":61000,"duration_s":60.0,"avg_gpu":90.0,"peak_gpu":99,"avg_fps":null,"energy_wh":1.5,"energy_source":"counter","peak_vram_mb":null,"peak_temp":70,"power_throttled_s":0.0,"thermal_throttled_s":0.0,"bottleneck":null}"#;
    // a damaged middle: bad UTF-8, garbage, an over-long line; jobs after it still count
    let mut body = Vec::new();
    body.extend_from_slice(line);
    body.extend_from_slice(b"\n\xff\xfe\xfd\n{{{\n");
    body.extend(std::iter::repeat_n(b'x', 200_000));
    body.push(b'\n');
    body.extend_from_slice(line);
    body.push(b'\n');
    std::fs::write(d.join("jobs.jsonl"), &body).unwrap();
    assert_eq!(history::recent(&d, 10).len(), 2);
    let mut rng = Rng::new(33);
    for _ in 0..rounds() / 2 {
        let mut b = Vec::new();
        for _ in 0..rng.below(6) {
            b.extend(rng.mutate(line));
            b.push(b'\n');
        }
        std::fs::write(d.join("jobs.jsonl"), &b).unwrap();
        let v = history::recent(&d, 50);
        if let [.., a, b] = v.as_slice() {
            let _ = history::compare(a, b);
        }
        let _ = history::previous(&d, "render:demo", i64::MAX);
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn crash_report_and_event_xml_take_anything() {
    let d = scratch("rep");
    let mut rng = Rng::new(44);
    let xml: &[u8] = br#"<Event><System><Provider Name='Microsoft-Windows-Kernel-Power'/><EventID>41</EventID><TimeCreated SystemTime='2026-10-26T10:00:00.000Z'/></System><EventData><Data Name='BugcheckCode'>0</Data></EventData></Event>"#;
    for _ in 0..rounds() * 2 {
        let b = inputs(&mut rng, &[xml]);
        let _ = events::parse_event_xml(&String::from_utf8_lossy(&b));
        std::fs::write(d.join("last_report.json"), rng.bytes(2048)).unwrap();
        let _ = report::load(&d);
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn settings_text_takes_anything() {
    let seed = config::HEADER.to_string() + &toml::to_string_pretty(&config::Config::default()).unwrap();
    let mut rng = Rng::new(55);
    let d = scratch("cfg");
    let p = d.join("tachyon.toml");
    for round in 0..rounds() {
        let b = inputs(&mut rng, &[seed.as_bytes()]);
        if let Ok(t) = std::str::from_utf8(&b) {
            if let Ok(mut c) = toml::from_str::<config::Config>(t) {
                tachyon::settings::sanitise(&mut c);
                let _ = c.poll_interval();
                let _ = tachyon::settings::with_settings(t, &c);
            }
            let _ = config::with_window(t, &config::WindowCfg::default());
        }
        if round % 10 == 0 {
            std::fs::write(&p, &b).unwrap();
            let l = config::load(&p);
            assert!(config::load(&p).warn.is_none(), "after load() the file always loads: {:?}", l.warn);
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn log_tail_is_capped() {
    let d = scratch("tail");
    let p = d.join("render.log");
    let f = std::fs::File::create(&p).unwrap();
    f.set_len(scan::MAX_TAIL * 3).unwrap(); // sparse
    let t = scan::tail(&p, u64::MAX).unwrap();
    assert!(t.len() as u64 <= scan::MAX_TAIL + 2);
    let h = scan::head(&p, u64::MAX).unwrap();
    assert!(h.len() as u64 <= scan::MAX_TAIL);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn embedded_png_decoder_takes_anything() {
    let mut rng = Rng::new(66);
    let z = png::zlib(b"hello hello hello hello").unwrap_or_default();
    for _ in 0..rounds() * 2 {
        let b = inputs(&mut rng, &[&z]);
        let _ = png::inflate(&b);
        let _ = png::decode_gray8(&rng.bytes(512));
    }
}
