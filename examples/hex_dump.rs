//! Dump composed Hex frames as raw premultiplied BGRA for comparison with the design kit's
//! reference renderer
//! (`cargo run --example hex_dump -- <outdir>`): both skins, every state's loop steps, the blink
//! frames and every transition step, at all three scales. Writes `<outdir>/<skin>/manifest.txt`
//! (one line per frame: name from to t step-json) and times compose and the edge.
use std::fmt::Write as _;
use std::time::Instant;
use tachyon::config::HexSkin;
use tachyon::hex::*;

fn step_json(s: &Step) -> String {
    serde_json::json!({
        "hold": s.hold, "halo": s.halo, "eyes": s.eyes, "sparkle": s.sparkle, "steam": s.steam,
        "z": s.z, "mark": s.mark, "drop": s.drop, "dy": s.dy, "dx": s.dx, "rot": s.rot,
    })
    .to_string()
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/hexdump".into());
    for skin in HexSkin::ALL {
        let dir = format!("{out}/{}", skin.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        let sc = Script::parse(atlas_json(skin)).unwrap();
        let mut cases: Vec<(String, Pose)> = vec![];
        for st in STATES {
            for (i, s) in sc.loops[&st].iter().enumerate() {
                cases.push((format!("{}{i}", st.key()), Pose { from: st, to: st, t: 1.0, step: s.clone() }));
            }
        }
        for (i, b) in sc.blink.iter().enumerate() {
            let mut s = sc.loops[&State::Idle][0].clone();
            s.eyes = b.eyes.clone();
            cases.push((format!("blink{i}"), Pose { from: State::Idle, to: State::Idle, t: 1.0, step: s }));
        }
        let mut tr: Vec<_> = sc.transitions.iter().collect();
        tr.sort_by_key(|((a, b), _)| (a.key(), b.key()));
        for ((a, b), list) in tr {
            for (i, s) in list.iter().enumerate() {
                cases.push((format!("{}-{}{i}", a.key(), b.key()), Pose { from: *a, to: *b, t: s.t, step: s.clone() }));
            }
        }
        let mut man = String::new();
        for (name, p) in &cases {
            writeln!(man, "{name}\t{}\t{}\t{}\t{}", p.from.key(), p.to.key(), p.t, step_json(&p.step)).unwrap();
        }
        std::fs::write(format!("{dir}/manifest.txt"), man).unwrap();
        for key in ["1", "1.5", "2"] {
            let at = Atlas::load(skin, key).unwrap();
            let t0 = Instant::now();
            let eg = EdgeGeo::new(&sc, &at);
            let t_geo = t0.elapsed();
            let n = at.size();
            let mut buf = vec![0u8; n * n * 4];
            let mut scratch = vec![];
            let t1 = Instant::now();
            let e = eg.alpha(0.668);
            let t_edge = t1.elapsed();
            let mut worst = std::time::Duration::ZERO;
            for (name, pose) in &cases {
                let inp = Inputs { load: 84.0, temp: 81.0, temp_red: 83.0, progress: 0.668, vram: 0.95 };
                let t2 = Instant::now();
                compose(&sc, &at, pose, &inp, Some(&e), &mut scratch, &mut buf);
                worst = worst.max(t2.elapsed());
                std::fs::write(format!("{dir}/{name}_{key}.bgra"), &buf).unwrap();
            }
            println!(
                "{} scale {key}: size {n}, {} frames, edge geo {t_geo:?} edge alpha {t_edge:?} worst compose {worst:?}",
                skin.as_str(),
                cases.len()
            );
        }
    }
}
