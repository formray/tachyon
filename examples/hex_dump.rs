//! Dump composed Hex frames as raw premultiplied BGRA for comparison with preview.py
//! (`cargo run --example hex_dump -- <outdir>`). Also times compose and the edge.
use std::time::Instant;
use tachyon::hex::*;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/hexdump".into());
    std::fs::create_dir_all(&out).unwrap();
    let sc = Script::parse(ATLAS_JSON).unwrap();
    for key in ["1", "1.5", "2"] {
        let at = Atlas::load(key).unwrap();
        let t0 = Instant::now();
        let eg = EdgeGeo::new(&sc, &at);
        let t_geo = t0.elapsed();
        let n = at.size();
        let mut buf = vec![0u8; n * n * 4];
        let mut scratch = vec![];
        let t1 = Instant::now();
        let e = eg.alpha(0.668);
        let t_edge = t1.elapsed();
        let cases: Vec<(&str, Pose, f32)> = vec![
            ("idle0", Pose { from: State::Idle, to: State::Idle, t: 1.0, step: sc.loops[&State::Idle][0].clone() }, 74.0),
            ("render1", Pose { from: State::Render, to: State::Render, t: 1.0, step: sc.loops[&State::Render][1].clone() }, 81.0),
            ("hot1", Pose { from: State::Hot, to: State::Hot, t: 1.0, step: sc.loops[&State::Hot][1].clone() }, 81.0),
            ("sleepy1", Pose { from: State::Sleepy, to: State::Sleepy, t: 1.0, step: sc.loops[&State::Sleepy][1].clone() }, 81.0),
            (
                "idle_render0",
                {
                    let s = sc.transitions[&(State::Idle, State::Render)][0].clone();
                    Pose { from: State::Idle, to: State::Render, t: s.t, step: s }
                },
                81.0,
            ),
        ];
        for (name, pose, temp) in cases {
            let inp = Inputs { load: 84.0, temp, temp_red: 83.0, progress: 0.668 };
            let t2 = Instant::now();
            compose(&sc, &at, &pose, &inp, Some(&e), &mut scratch, &mut buf);
            let tc = t2.elapsed();
            std::fs::write(format!("{out}/{name}_{key}.bgra"), &buf).unwrap();
            if name == "render1" {
                println!("scale {key}: size {n} edge geo {:?} edge alpha {:?} compose {:?}", t_geo, t_edge, tc);
            }
        }
    }
}
