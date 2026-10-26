//! Parser tests on real log lines in the formats of real render logs (anonymised).
use std::path::PathBuf;
use tachyon::parse::*;

fn sample(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples").join(name);
    decode(&std::fs::read(p).unwrap())
}

#[test]
fn ffedit_frame_lines() {
    let t = sample("job.log");
    let p = ffmpeg_progress(current_section(&t)).unwrap();
    assert_eq!(p.frame, 942);
    assert_eq!(p.fps, Some(84.0));
    assert_eq!(p.speed.as_deref(), Some("3.5x"));
    assert_eq!(p.total, None);
    assert_eq!(current_stage(&t).as_deref(), Some("ffedit.exe"));
}

#[test]
fn ffmpeg_carriage_return_stats() {
    let t = "frame=  100 fps= 11 q=-0.0 size=  1KiB time=00:00:04.00 bitrate=1.0kbits/s speed=0.46x\rframe=  140 fps= 12 q=-0.0 size=  2KiB time=00:00:05.00 bitrate=1.0kbits/s speed=0.47x\r";
    let p = ffmpeg_progress(t).unwrap();
    assert_eq!((p.frame, p.fps), (140, Some(12.0)));
}

#[test]
fn ffmpeg_progress_kv_blocks() {
    let t = "frame=10\nfps=5.0\nspeed=0.2x\nprogress=continue\nframe=20\nfps=6.5\nout_time=00:00:01\nspeed=0.3x\nprogress=continue\n";
    let p = ffmpeg_progress(t).unwrap();
    assert_eq!((p.frame, p.fps, p.speed.as_deref()), (20, Some(6.5), Some("0.3x")));
}

#[test]
fn stale_frame_lines_from_finished_stage_are_ignored() {
    let t = sample("job_done.log");
    // whole tail has frame= 1428, but the running stage is the ffmpeg concat after it
    assert_eq!(ffmpeg_progress(&t).unwrap().frame, 1428);
    assert!(ffmpeg_progress(current_section(&t)).is_none());
    assert_eq!(current_stage(&t).as_deref(), Some("ffmpeg"));
    // effect.py's "frames 1428 I 60 P 1368" and the QA dict are not totals
    assert_eq!(total_frames(&t), None);
}

#[test]
fn python_script_stage_names() {
    let t = "> C:\\tools\\venv\\Scripts\\python.exe C:\\work\\projects\\demo4k/run/stage2.py\nkeyframes [0, 44]\n";
    assert_eq!(current_stage(t).as_deref(), Some("stage2.py"));
}

#[test]
fn batch_log() {
    let b = batch_state(&sample("batch.log")).unwrap();
    assert_eq!(b.running.as_deref(), Some("02x 169 try 1"));
    assert_eq!(b.nf, Some(1420));
    assert_eq!(b.started, Some((None, "22:07:54".into())));
    // last ok job: 01x 169, 21:45:30 -> 22:07:54 = 1344 s for 1428 frames
    assert!((b.last_ok_spf.unwrap() - 1344.0 / 1428.0).abs() < 1e-9);
    assert_eq!(total_frames(&sample("batch.log")), Some(1420));
}

#[test]
fn batch_log_finished() {
    let t = sample("batch.log") + "22:30:10 end 02x 169 try 1 exit 0 ok=True\n";
    let b = batch_state(&t).unwrap();
    assert_eq!(b.running, None);
    assert!((b.last_ok_spf.unwrap() - (22 * 60 + 16) as f64 / 1420.0).abs() < 1e-9);
}

#[test]
fn night_queue_log() {
    let b = batch_state(&sample("queue.log")).unwrap();
    assert_eq!(b.running.as_deref(), Some("clipb_cut_60s_r6"));
    assert_eq!(b.started, Some((Some("2026-10-23".into()), "13:34:35".into())));
    let done = sample("queue.log") + "2026-10-23 13:36:48 DONE clipb_cut_60s_r6\n";
    assert_eq!(batch_state(&done).unwrap().running, None);
}

#[test]
fn elapsed_handles_midnight() {
    use chrono::NaiveDate;
    let d = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
    let now = d.and_hms_opt(0, 10, 0).unwrap();
    // stamp 23:50:00 with a mtime date of the 25th must mean the 24th
    let e = elapsed_since(&(None, "23:50:00".into()), now, d).unwrap();
    assert_eq!(e, 1200.0);
    let e = elapsed_since(&(Some("2026-10-24".into()), "23:00:00".into()), now, d).unwrap();
    assert_eq!(e, 4200.0);
}

#[test]
fn frame_n_of_m_lines_with_fps() {
    let p = slash_progress(&sample("mezz_4k_r6.log")).unwrap();
    assert_eq!((p.frame, p.total, p.fps), (451, Some(7432), Some(10.5)));
}

#[test]
fn tqdm_lines() {
    let p = slash_progress(" 50%|#####     | 1/2 [00:00<00:00,  1.40it/s]").unwrap();
    assert_eq!((p.frame, p.total, p.fps), (1, Some(2), Some(1.40)));
}

#[test]
fn dates_and_framecheck_are_not_progress_or_totals() {
    let t = sample("framecheck.log");
    assert_eq!(slash_progress(&t), None);
    assert_eq!(total_frames(&t), None);
    assert_eq!(slash_progress("2026/10/24 frame dump at C:/a/b/1/2"), None);
}

#[test]
fn total_frame_keys() {
    assert_eq!(total_frames("start x NF=7155"), Some(7155));
    assert_eq!(total_frames("nf: 900\nfoo"), Some(900));
    assert_eq!(total_frames("total_frames = 9010"), Some(9010));
    assert_eq!(total_frames("frame= 12 fps=3"), None);
}

#[test]
fn render_chunks_stage_a() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples/chunks_r");
    let mut ch: Vec<Chunk> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| {
            let (pre, s) = chunk_name(&e.file_name().to_string_lossy()).unwrap();
            assert_eq!(pre, "r");
            parse_chunk(s, &std::fs::read_to_string(e.path()).unwrap())
        })
        .collect();
    let (done, tot, fin, n) = chunk_progress(&mut ch, Some(1420)).unwrap();
    // r_0 DONE 0..350, r_350 DONE 350..733, r_733 at 816 (84 frames), r_1070 at 1128 (59)
    assert_eq!((fin, n, tot), (2, 4, 1420));
    assert_eq!(done, 350 + 383 + 84 + 59);
}

#[test]
fn post_chunks_stage_c() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples/chunks_p");
    let mut ch: Vec<Chunk> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| {
            let (_, s) = chunk_name(&e.file_name().to_string_lossy()).unwrap();
            parse_chunk(s, &std::fs::read_to_string(e.path()).unwrap())
        })
        .collect();
    let (done, tot, fin, n) = chunk_progress(&mut ch, Some(1428)).unwrap();
    assert_eq!((fin, n, tot), (1, 4, 1428));
    assert_eq!(done, 354 + (433 - 354) + (721 - 716));
    assert_eq!(chunk_name("demo4k_02x_169_20261024_220754.log"), None);
    assert_eq!(chunk_name("mezz_4k_r6.log"), None);
}

#[test]
fn utf16_logs_decode() {
    let s = "frame=  77 fps= 9 q=1\r\n";
    let mut b = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(ffmpeg_progress(&decode(&b)).unwrap().frame, 77);
}

#[test]
fn progress_json_and_eta() {
    let j = progress_json(r#"{"job":"datamirage 4K","frame":1200,"total":7155,"fps":9.5,"started":"2026-10-24T23:10:00"}"#).unwrap();
    assert_eq!((j.frame, j.total), (1200, Some(7155)));
    assert_eq!(eta_secs(1200, 7155, 9.5).unwrap().round(), 627.0);
    assert_eq!(fmt_dur(3725.0), "1h 02m");
    assert_eq!(eta_secs(10, 5, 1.0), None);
}

/// The real flight recording of 26 Oct 2026 (1.0 format, the last 5 minutes before the PC lost
/// power at idle): every record reads back, the process names are clean (1.0 wrote the third
/// name over the checksum) and the diagnosis sees an idle GPU.
#[test]
fn real_recording_before_the_26_oct_power_loss() {
    use tachyon::diagnose::{Context, Suspect, diagnose, idle_before};
    use tachyon::events::{KERNEL_POWER, SysEvent};
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/blackbox_2026-10-26_idle_power_loss.bin");
    let s = tachyon::recorder::read_ring(&p);
    assert_eq!(s.len(), 300);
    assert!(s.windows(2).all(|w| w[0].t_ms < w[1].t_ms));
    assert_eq!(s.last().unwrap().t_ms - s[0].t_ms, 299_234);
    let mut names: Vec<&str> = s.iter().flat_map(|x| x.procs.iter().map(|p| p.name.as_str())).collect();
    names.sort_unstable();
    names.dedup();
    assert!(names.contains(&"CrossDeviceResume.ex") && names.contains(&"dwm.exe"), "{names:?}");
    assert!(names.iter().all(|n| n.chars().all(|c| !c.is_control() && c != '\u{fffd}') && n.len() <= 24), "{names:?}");
    let last = s.last().unwrap().t_ms;
    let lm: Vec<_> = s.iter().filter(|x| x.t_ms >= last - 60_000).cloned().collect();
    let peaks = tachyon::report::peaks(&lm);
    assert_eq!(idle_before(&peaks, &lm), Some((3, 22.9, Some(360.0), Some(33))));
    let kp41 = SysEvent { t_ms: last + 3_816_000, provider: KERNEL_POWER.into(), id: 41, level: 1, code: None, record: 0 };
    let d = diagnose(tachyon::report::Cause::Restart, &[kp41], true, &peaks, &lm, None, &Context::default()).unwrap();
    assert_eq!(d.causes[0].suspect, Suspect::PowerCut, "{d:#?}");
    assert!(d.causes.iter().find(|c| c.suspect == Suspect::Power).is_none_or(|c| c.percent <= 20), "{d:#?}");
    // the 1.1 format keeps the whole name
    let enc = tachyon::recorder::encode(&s[0], 1);
    let mut full = s[0].clone();
    full.procs[2].name = "CrossDeviceResume.exe".into();
    let back = tachyon::recorder::decode(&tachyon::recorder::encode(&full, 2)).unwrap().1;
    assert_eq!(back.procs[2].name, "CrossDeviceResume.exe");
    assert_eq!(tachyon::recorder::decode(&enc).unwrap().1, s[0]);
}
