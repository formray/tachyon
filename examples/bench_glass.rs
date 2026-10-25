//! Times the software glass at panel size (build box helper).
use tachyon::aurora::target;
use tachyon::glass::*;
fn main() {
    let (w, h, pad) = (540usize, 790usize, 30.0);
    let shape = Shape { x: pad, y: pad, w: 480.0, h: 730.0, r: 30.0 };
    let sh = ShadowSpec { key_dy: 12.0, key_blur: 24.0, key_a: 0.34, near_dy: 2.0, near_blur: 6.0, near_a: 0.2 };
    let t = std::time::Instant::now();
    let g = Geometry::new(w, h, shape, Some(sh));
    println!("geometry {:?}", t.elapsed());
    let f = target(Some(84.0), Some(74.0), 80.0, 83.0);
    let l = FieldLayout {
        x: -18.0,
        y: -16.0,
        w: 576.0,
        h: 276.0,
        blobs: [(0.22, 0.7, 0.33, 0.55), (0.55, 0.45, 0.42, 0.65), (0.88, 0.72, 0.28, 0.5)],
        peak: 0.52,
        gain: [1.0; 3],
    };
    let b = Body { tint: (14, 16, 30), tint_a: 0.72, field: Some((f, l)), level: None };
    let mut out = vec![0u8; w * h * 4];
    let t = std::time::Instant::now();
    for _ in 0..50 {
        compose_body(&g, &b, &mut out);
    }
    println!("compose {:?}", t.elapsed() / 50);
    let b2 = Body { field: None, ..b };
    let t = std::time::Instant::now();
    for _ in 0..50 {
        compose_body(&g, &b2, &mut out);
    }
    println!("compose no field {:?}", t.elapsed() / 50);
    let src = out.clone();
    let t = std::time::Instant::now();
    for _ in 0..50 {
        over(&mut out, &src);
    }
    println!("over {:?}", t.elapsed() / 50);
}
#[allow(dead_code)]
fn unused() {}
