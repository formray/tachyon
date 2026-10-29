//! "Hex", the Tachy companion: a hexagon with shape-driven eyes in two built-in skins (Velvet,
//! the default, and Aurora flat). Host-independent parts: the embedded atlases (8-bit masks,
//! decoded for one DPI scale only), the tint rules, the trigger rules (with hysteresis), the frame
//! scheduler (states, transitions, blinks; one-shot holds instead of a constant 12 fps timer)
//! and a compositor that matches the design kit's reference renderer pixel for pixel (within one
//! level), including the live rendering edge. All data comes from
//! `res/hex/<skin>/atlas.min.json` (the atlas description, minified).
use crate::config::HexSkin;
use serde_json::Value;
use std::collections::HashMap;

const VELVET_JSON: &str = include_str!("../res/hex/velvet/atlas.min.json");
const VELVET_PNG: [&[u8]; 3] = [
    include_bytes!("../res/hex/velvet/hex_atlas_1x.png"),
    include_bytes!("../res/hex/velvet/hex_atlas_1_5x.png"),
    include_bytes!("../res/hex/velvet/hex_atlas_2x.png"),
];
const FLAT_JSON: &str = include_str!("../res/hex/flat/atlas.min.json");
const FLAT_PNG: [&[u8]; 3] = [
    include_bytes!("../res/hex/flat/hex_atlas_1x.png"),
    include_bytes!("../res/hex/flat/hex_atlas_1_5x.png"),
    include_bytes!("../res/hex/flat/hex_atlas_2x.png"),
];

/// The atlas description of a skin.
pub fn atlas_json(skin: HexSkin) -> &'static str {
    match skin {
        HexSkin::Velvet => VELVET_JSON,
        HexSkin::Flat => FLAT_JSON,
    }
}
fn atlas_png(skin: HexSkin, key: &str) -> &'static [u8] {
    let set = match skin {
        HexSkin::Velvet => &VELVET_PNG,
        HexSkin::Flat => &FLAT_PNG,
    };
    match key {
        "2" => set[2],
        "1.5" => set[1],
        _ => set[0],
    }
}
/// One animation tick (12 fps).
pub const TICK_MS: f64 = 1000.0 / 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Idle,
    Render,
    Hot,
    Sleepy,
    /// VRAM nearly full (>= 92 % for ~3 s; leaves below 85 %)
    Vram,
    /// something is wrong: a render crash, a GPU / driver error, or the red temperature
    Alarm,
}

impl State {
    pub fn key(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Render => "render",
            State::Hot => "hot",
            State::Sleepy => "sleepy",
            State::Vram => "vram",
            State::Alarm => "alarm",
        }
    }
    pub fn from_key(k: &str) -> Option<State> {
        STATES.into_iter().find(|s| s.key().eq_ignore_ascii_case(k.trim()))
    }
}

/// Atlas scale for a DPI factor: only this one PNG is decoded.
pub fn scale_key(dpi_scale: f32) -> &'static str {
    if dpi_scale >= 1.75 {
        "2"
    } else if dpi_scale >= 1.25 {
        "1.5"
    } else {
        "1"
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub ox: i32,
    pub oy: i32,
}

/// One animation step from atlas.json (a loop frame, a blink overlay or a transition frame).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub hold: u32,
    pub halo: u8,
    pub eyes: String,
    pub sparkle: Option<u8>,
    pub steam: Option<u8>,
    pub z: Option<u8>,
    /// alarm '!' and alert ticks
    pub mark: Option<u8>,
    /// VRAM sweat drop
    pub drop: Option<u8>,
    pub dy: i32,
    /// small horizontal jolt (alarm)
    pub dx: i32,
    pub rot: f32,
    pub t: f32,
}

fn step(v: &Value) -> Step {
    let u = |k: &str| v.get(k).and_then(Value::as_u64);
    Step {
        hold: u("hold").unwrap_or(1).max(1) as u32,
        halo: u("halo").unwrap_or(0) as u8,
        eyes: v.get("eyes").and_then(Value::as_str).unwrap_or("eyes_open").to_string(),
        sparkle: u("sparkle").map(|x| x as u8),
        steam: u("steam").map(|x| x as u8),
        z: u("z").map(|x| x as u8),
        mark: u("mark").map(|x| x as u8),
        drop: u("drop").map(|x| x as u8),
        dy: v.get("dy").and_then(Value::as_i64).unwrap_or(0) as i32,
        dx: v.get("dx").and_then(Value::as_i64).unwrap_or(0) as i32,
        rot: v.get("rot").and_then(Value::as_f64).unwrap_or(0.0) as f32,
        t: v.get("t").and_then(Value::as_f64).unwrap_or(1.0) as f32,
    }
}

/// Layer tint: a colour (fixed, per state, or field.A / field.B) and an alpha (fixed or per state).
#[derive(Clone, Debug)]
struct LayerStyle {
    color: HashMap<&'static str, Tint>,
    alpha: HashMap<&'static str, f32>,
    /// follows the breathing offset (dy) and the jolt (dx); ambient effects that float in place do not
    moves: bool,
}
#[derive(Clone, Copy, Debug)]
enum Tint {
    Rgb([f32; 3]),
    FieldA,
    FieldB,
}

pub fn hexc(s: &str) -> [f32; 3] {
    let s = s.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(s.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f32 / 255.0;
    [p(0), p(2), p(4)]
}
fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

pub const STATES: [State; 6] = [State::Idle, State::Render, State::Hot, State::Sleepy, State::Vram, State::Alarm];

/// Everything in atlas.json except the pixels (shared by all scales).
#[derive(Clone, Debug)]
pub struct Script {
    pub loops: HashMap<State, Vec<Step>>,
    pub blink: Vec<Step>,
    pub blink_every: (u32, u32),
    pub transitions: HashMap<(State, State), Vec<Step>>,
    layers: HashMap<String, LayerStyle>,
    /// edge polyline in design units with cumulative lengths
    edge: Vec<(f32, f32)>,
    pub edge_width_u: f32,
    pub edge_sigma_u: f32,
    pub perimeter_u: f32,
}

impl Script {
    pub fn parse(json: &str) -> Option<Script> {
        let v: Value = serde_json::from_str(json).ok()?;
        let st = v.get("states")?;
        let mut loops = HashMap::new();
        for s in STATES {
            loops.insert(s, st.get(s.key())?.get("loop")?.as_array()?.iter().map(step).collect::<Vec<_>>());
        }
        let bl = st.get("idle")?.get("blink")?;
        let blink = bl.get("frames")?.as_array()?.iter().map(step).collect();
        let ev = bl.get("every_ticks")?.as_array()?;
        let blink_every = (ev.first()?.as_u64()? as u32, ev.get(1)?.as_u64()? as u32);
        let mut transitions = HashMap::new();
        for (k, list) in v.get("transitions")?.as_object()? {
            let Some((a, b)) = k.split_once('>') else { continue };
            let find = |n: &str| STATES.into_iter().find(|s| s.key() == n);
            if let (Some(a), Some(b), Some(list)) = (find(a), find(b), list.as_array()) {
                transitions.insert((a, b), list.iter().map(step).collect());
            }
        }
        let tint = |c: &Value| -> Option<Tint> {
            let c = c.as_str()?;
            Some(match c {
                "field.A" => Tint::FieldA,
                "field.B" => Tint::FieldB,
                _ => Tint::Rgb(hexc(c)),
            })
        };
        let mut layers = HashMap::new();
        for (name, l) in v.get("layers")?.as_object()? {
            let mut color = HashMap::new();
            let mut alpha = HashMap::new();
            // per-state values fall back to the idle one (like the design kit)
            let per = |v: &'_ Value, s: State| -> Option<Value> {
                if v.is_object() { v.get(s.key()).or_else(|| v.get("idle")).cloned() } else { Some(v.clone()) }
            };
            for s in STATES {
                color.insert(s.key(), tint(&per(l.get("color")?, s)?)?);
                alpha.insert(s.key(), per(l.get("alpha")?, s)?.as_f64()? as f32);
            }
            let moves = l.get("moves_with_dy").and_then(Value::as_bool).unwrap_or(true);
            layers.insert(name.clone(), LayerStyle { color, alpha, moves });
        }
        let e = v.get("edge")?;
        let mut edge = Vec::new();
        for sg in e.get("segments")?.as_array()? {
            let p = |k: &str| -> Option<(f32, f32)> {
                let a = sg.get(k)?.as_array()?;
                Some((a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32))
            };
            let (p0, p1) = (p("p0")?, p("p1")?);
            if sg.get("type")?.as_str()? == "quad" {
                let c = p("ctrl")?;
                for i in 0..24 {
                    let t = i as f32 / 24.0;
                    let (a, b, cc) = ((1.0 - t) * (1.0 - t), 2.0 * (1.0 - t) * t, t * t);
                    edge.push((a * p0.0 + b * c.0 + cc * p1.0, a * p0.1 + b * c.1 + cc * p1.1));
                }
            } else {
                edge.push(p0);
            }
        }
        edge.push(*edge.first()?);
        let stroke = e.get("stroke")?;
        Some(Script {
            loops,
            blink,
            blink_every,
            transitions,
            layers,
            edge,
            edge_width_u: stroke.get("width_u")?.as_f64()? as f32,
            edge_sigma_u: 1.6,
            perimeter_u: e.get("perimeter_u")?.as_f64()? as f32,
        })
    }

    /// Frames from `a` to `b`; pairs without their own list go through idle without holding it.
    pub fn transition(&self, a: State, b: State) -> Vec<(State, State, Step)> {
        if a == b {
            return vec![];
        }
        if let Some(l) = self.transitions.get(&(a, b)) {
            return l.iter().map(|s| (a, b, s.clone())).collect();
        }
        let mut v = self.transition(a, State::Idle);
        if a == State::Idle || b == State::Idle {
            return v;
        }
        v.extend(self.transition(State::Idle, b));
        v
    }
}

/// The decoded atlas for one scale.
pub struct Atlas {
    pub scale: f32,
    pub cell: usize,
    pub anchor: (i32, i32),
    w: usize,
    px: Vec<u8>,
    frames: HashMap<String, FrameRect>,
}

impl Atlas {
    pub fn load(skin: HexSkin, key: &str) -> Option<Atlas> {
        let v: Value = serde_json::from_str(atlas_json(skin)).ok()?;
        let m = v.get("scales")?.get(key)?;
        let png = atlas_png(skin, key);
        let (w, h, px) = crate::png::decode_gray8(png)?;
        let mut frames = HashMap::new();
        for (name, f) in m.get("frames")?.as_object()? {
            let r = f.get("rect")?.as_array()?;
            let o = f.get("offset")?.as_array()?;
            let n = |a: &Vec<Value>, i: usize| a.get(i).and_then(Value::as_f64).unwrap_or(0.0) as i64;
            let fr = FrameRect {
                x: n(r, 0) as usize,
                y: n(r, 1) as usize,
                w: n(r, 2) as usize,
                h: n(r, 3) as usize,
                ox: n(o, 0) as i32,
                oy: n(o, 1) as i32,
            };
            if fr.x + fr.w > w || fr.y + fr.h > h {
                return None;
            }
            frames.insert(name.clone(), fr);
        }
        let an = m.get("anchor")?.as_array()?;
        Some(Atlas {
            scale: key.parse().ok()?,
            cell: m.get("cell")?.as_u64()? as usize,
            anchor: (an.first()?.as_f64()? as i32, an.get(1)?.as_f64()? as i32),
            w,
            px,
            frames,
        })
    }
    pub fn frame(&self, name: &str) -> Option<&FrameRect> {
        self.frames.get(name)
    }
    /// Shadow padding around the cell (preview.py: int(24 * scale)).
    pub fn pad(&self) -> usize {
        (24.0 * self.scale) as usize
    }
    /// Side of the square sprite surface.
    pub fn size(&self) -> usize {
        self.cell + self.pad()
    }
}

/// Inputs to the tint rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inputs {
    pub load: f32,
    pub temp: f32,
    pub temp_red: f32,
    /// render progress 0..1 (edge)
    pub progress: f32,
    /// VRAM used / total, 0..1 (the VRAM state's colour)
    pub vram: f32,
}

/// field.A / field.B for a state (atlas.json tint_rules).
pub fn field(state: State, inp: &Inputs) -> ([f32; 3], [f32; 3]) {
    let s = (inp.load / 100.0).clamp(0.0, 1.0);
    let mut a = mix3(hexc("#4FE3FF"), hexc("#6F8BFF"), s);
    let mut b = mix3(hexc("#9A7BFF"), hexc("#B46BFF"), s);
    match state {
        State::Render => {
            a = hexc("#5CFFD8");
            b = mix3(hexc("#7F8CFF"), hexc("#B46BFF"), s * 0.5);
        }
        State::Hot => {
            let h = ((inp.temp - 75.0) / 5.0).clamp(0.0, 1.0);
            a = mix3(a, hexc("#FFB257"), h);
            b = mix3(b, hexc("#FF4F6A"), h);
            if inp.temp >= inp.temp_red {
                a = hexc("#FF7A45");
                b = hexc("#FF3355");
            }
        }
        State::Sleepy => {
            a = hexc("#7A88DD");
            b = hexc("#3D3F95");
        }
        State::Vram => {
            let v = ((inp.vram - 0.85) / 0.1).clamp(0.0, 1.0);
            a = mix3(hexc("#4FE3FF"), hexc("#FFC24D"), 0.5 + 0.5 * v);
            b = mix3(hexc("#9A7BFF"), hexc("#FF8A3D"), 0.4 + 0.6 * v);
        }
        State::Alarm => {
            a = hexc("#FF5A6E");
            b = hexc("#B0204A");
        }
        State::Idle => {}
    }
    (a, b)
}

/// What to draw: the frame step plus the colour mix between two states.
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    pub from: State,
    pub to: State,
    pub t: f32,
    pub step: Step,
}

impl Pose {
    pub fn edge(&self) -> bool {
        self.from == State::Render && self.to == State::Render
    }
}

/// What the triggers read each poll.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Signals {
    /// a render job is active
    pub render: bool,
    /// GPU load %, temperature °C, VRAM used / total (None = no reading)
    pub load: Option<f32>,
    pub temp: Option<f32>,
    pub vram: Option<f32>,
    /// the GPU or its driver is in trouble now (NVML lost / driver reset / GPU not answering)
    pub gpu_error: bool,
    /// an unannounced crash report exists (a render or the PC stopped unexpectedly)
    pub crash: bool,
}

/// Trigger thresholds.
pub const VRAM_ON: f32 = 0.92;
pub const VRAM_OFF: f32 = 0.85;
pub const VRAM_ON_MS: u64 = 3_000;
pub const ALARM_MIN_MS: u64 = 10_000;
pub const SLEEPY_MS: u64 = 10 * 60 * 1000;

/// Trigger rules, priority alarm > hot > VRAM > render > sleepy > idle.
/// - alarm: the red temperature (until 2 °C below it), a GPU / driver error, or a new crash
///   report. Shown for at least 10 s; one started by a crash stays until Hex is clicked or the
///   next render starts (whichever comes first, but not before the 10 s).
/// - hot: from temp_amber_c, until 2 °C below it.
/// - VRAM: used >= 92 % for 3 s; leaves below 85 %.
/// - sleepy: 10 min below 5 % load with no render.
#[derive(Clone, Debug, Default)]
pub struct Triggers {
    hot: bool,
    red: bool,
    quiet_since: Option<u64>,
    vram_on: bool,
    vram_since: Option<u64>,
    /// a level cause (red temp, GPU error) was last active at this time
    alarm_level_at: Option<u64>,
    /// a crash alarm started at this time and is not acknowledged yet
    alarm_event: Option<u64>,
    acked: bool,
    last_crash: bool,
    last_render: bool,
}

impl Triggers {
    pub fn desired(&mut self, sig: &Signals, amber: f32, red: f32, now_ms: u64) -> State {
        if let Some(t) = sig.temp {
            self.hot = if self.hot { t >= amber - 2.0 } else { t >= amber };
            self.red = if self.red { t >= red - 2.0 } else { t >= red };
        }
        if sig.render || sig.load.is_none_or(|l| l >= 5.0) {
            self.quiet_since = None;
        } else if self.quiet_since.is_none() {
            self.quiet_since = Some(now_ms);
        }
        match sig.vram {
            Some(v) if v >= VRAM_ON => {
                let t0 = *self.vram_since.get_or_insert(now_ms);
                if now_ms.saturating_sub(t0) >= VRAM_ON_MS {
                    self.vram_on = true;
                }
            }
            Some(v) if v < VRAM_OFF => {
                self.vram_on = false;
                self.vram_since = None;
            }
            Some(_) => self.vram_since = None,
            None => {}
        }
        // alarm: level causes, then the crash event (rising edge of `crash`)
        if self.red || sig.gpu_error {
            self.alarm_level_at = Some(now_ms);
        }
        if sig.crash && !self.last_crash {
            self.alarm_event = Some(now_ms);
            self.acked = false;
        }
        self.last_crash = sig.crash;
        if sig.render && !self.last_render {
            self.acked = true; // the next render clears a crash alarm
        }
        self.last_render = sig.render;
        if let Some(t0) = self.alarm_event {
            if self.acked && now_ms.saturating_sub(t0) >= ALARM_MIN_MS {
                self.alarm_event = None;
            }
        }
        let alarm = self.alarm_event.is_some() || self.alarm_level_at.is_some_and(|t| now_ms.saturating_sub(t) < ALARM_MIN_MS);
        if alarm {
            State::Alarm
        } else if self.hot {
            State::Hot
        } else if self.vram_on {
            State::Vram
        } else if sig.render {
            State::Render
        } else if self.quiet_since.is_some_and(|t0| now_ms.saturating_sub(t0) >= SLEEPY_MS) {
            State::Sleepy
        } else {
            State::Idle
        }
    }

    /// Hex was clicked: a crash alarm may end (after its 10 s).
    pub fn acknowledge(&mut self) {
        self.acked = true;
    }
    pub fn alarm_pending(&self) -> bool {
        self.alarm_event.is_some()
    }
}

/// Frame scheduler. `next()` returns the pose to draw and how many ticks to hold it, so the
/// window arms one one-shot timer per drawn frame (12 fps only for blinks and transitions).
pub struct Animator {
    pub state: State,
    trans: Vec<(State, State, Step)>,
    li: usize,
    rem: u32,
    blink: Option<usize>,
    ttb: u32,
    rng: u32,
}

impl Animator {
    pub fn new(sc: &Script, state: State) -> Animator {
        let mut a = Animator { state, trans: vec![], li: 0, rem: 0, blink: None, ttb: 0, rng: 0x9e37_79b9 };
        a.reset(sc);
        a.ttb = a.rand(sc.blink_every);
        a
    }
    fn rand(&mut self, (lo, hi): (u32, u32)) -> u32 {
        // xorshift32: deterministic, good enough for blink spacing
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        lo + self.rng % (hi.saturating_sub(lo) + 1)
    }
    /// Back to frame 0 of the current loop (after a pause).
    pub fn reset(&mut self, sc: &Script) {
        self.li = 0;
        self.blink = None;
        self.rem = sc.loops[&self.state].first().map(|s| s.hold).unwrap_or(1);
    }
    pub fn in_transition(&self) -> bool {
        !self.trans.is_empty()
    }

    /// Start moving to `target` (ignored while a transition is still playing; call again after).
    /// Returns true when a transition was started.
    pub fn go(&mut self, sc: &Script, target: State) -> bool {
        if target == self.state || self.in_transition() {
            return false;
        }
        self.trans = sc.transition(self.state, target);
        self.trans.reverse(); // popped from the back
        self.state = target;
        self.reset(sc);
        true
    }

    pub fn next(&mut self, sc: &Script) -> (Pose, u32) {
        if let Some((a, b, s)) = self.trans.pop() {
            let hold = s.hold;
            return (Pose { from: a, to: b, t: s.t, step: s }, hold);
        }
        let lp = &sc.loops[&self.state];
        let base = lp[self.li % lp.len()].clone();
        let mut step = base.clone();
        let hold;
        if let Some(bi) = self.blink {
            let b = &sc.blink[bi];
            step.eyes = b.eyes.clone();
            hold = b.hold.min(self.rem).max(1);
            self.blink = if bi + 1 < sc.blink.len() { Some(bi + 1) } else { None };
        } else if self.state == State::Idle && self.ttb < self.rem {
            if self.ttb == 0 {
                self.blink = Some(0);
                self.ttb = self.rand(sc.blink_every);
                return self.next(sc);
            }
            hold = self.ttb;
        } else {
            hold = self.rem.max(1);
        }
        self.rem = self.rem.saturating_sub(hold);
        self.ttb = self.ttb.saturating_sub(hold);
        if self.rem == 0 {
            self.li = (self.li + 1) % lp.len();
            self.rem = lp[self.li].hold;
        }
        (Pose { from: self.state, to: self.state, t: 1.0, step }, hold)
    }
}

/// Precomputed geometry of the edge path for one scale: for each pixel near the path, its
/// distance to the path and the arc length of the nearest point.
pub struct EdgeGeo {
    c: usize,
    px: Vec<(u32, f32, f32)>,
    total: f32,
    p0: (f32, f32),
    pts: Vec<(f32, f32)>,
    cum: Vec<f32>,
    w: f32,
    sigma: f32,
    /// exact perimeter in px (progress is measured on it, like preview.py)
    perim: f32,
}

fn erf(x: f32) -> f32 {
    // Abramowitz & Stegun 7.1.26
    let s = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0 - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_7) * t - 0.284_496_74) * t + 0.254_829_6) * t * (-x * x).exp();
    s * y
}

impl EdgeGeo {
    pub fn new(sc: &Script, at: &Atlas) -> EdgeGeo {
        let k = at.scale;
        let pts: Vec<(f32, f32)> = sc.edge.iter().map(|p| ((p.0 + 8.0) * k, (p.1 + 8.0) * k)).collect();
        let mut cum = vec![0.0f32];
        for w in pts.windows(2) {
            let d = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
            cum.push(cum.last().copied().unwrap_or(0.0) + d);
        }
        let total = *cum.last().unwrap_or(&1.0);
        let (w, sigma) = (sc.edge_width_u * k, sc.edge_sigma_u * k);
        let band = w / 2.0 + 3.0 * sigma + 1.0;
        let c = at.cell;
        let (minx, maxx) = pts.iter().fold((f32::MAX, f32::MIN), |m, p| (m.0.min(p.0), m.1.max(p.0)));
        let (miny, maxy) = pts.iter().fold((f32::MAX, f32::MIN), |m, p| (m.0.min(p.1), m.1.max(p.1)));
        let mut px = Vec::new();
        let y0 = ((miny - band).floor().max(0.0)) as usize;
        let y1 = ((maxy + band).ceil() as usize).min(c);
        let x0 = ((minx - band).floor().max(0.0)) as usize;
        let x1 = ((maxx + band).ceil() as usize).min(c);
        for y in y0..y1 {
            for x in x0..x1 {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut best = (f32::MAX, 0.0f32);
                for (i, s) in pts.windows(2).enumerate() {
                    let (ax, ay, bx, by) = (s[0].0, s[0].1, s[1].0, s[1].1);
                    let (dx, dy) = (bx - ax, by - ay);
                    let l2 = (dx * dx + dy * dy).max(1e-6);
                    let t = (((fx - ax) * dx + (fy - ay) * dy) / l2).clamp(0.0, 1.0);
                    let (qx, qy) = (ax + dx * t - fx, ay + dy * t - fy);
                    let d2 = qx * qx + qy * qy;
                    if d2 < best.0 {
                        best = (d2, cum[i] + t * l2.sqrt());
                    }
                }
                let d = best.0.sqrt();
                if d <= band {
                    px.push(((y * c + x) as u32, d, best.1));
                }
            }
        }
        EdgeGeo { c, px, total, p0: pts[0], pts, cum, w, sigma, perim: sc.perimeter_u * k }
    }

    fn point_at(&self, l: f32) -> (f32, f32) {
        let i = self.cum.partition_point(|&c| c <= l).clamp(1, self.pts.len() - 1);
        let (a, b) = (self.pts[i - 1], self.pts[i]);
        let seg = (self.cum[i] - self.cum[i - 1]).max(1e-6);
        let t = ((l - self.cum[i - 1]) / seg).clamp(0.0, 1.0);
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }

    /// Edge alpha per cell pixel for `progress` (0..1): glow (alpha = min(1, 2*blur + sharp)).
    pub fn alpha(&self, progress: f32) -> Vec<f32> {
        let mut out = vec![0.0f32; self.c * self.c];
        let p = progress.clamp(0.0, 1.0);
        if p <= 0.0 {
            return out;
        }
        let l = (self.perim * p).min(self.total);
        let pe = self.point_at(l);
        let s2 = self.sigma * std::f32::consts::SQRT_2;
        for &(i, d, s) in &self.px {
            let d = if s <= l || p >= 1.0 {
                d
            } else {
                let (x, y) = ((i as usize % self.c) as f32 + 0.5, (i as usize / self.c) as f32 + 0.5);
                let a = ((x - self.p0.0).powi(2) + (y - self.p0.1).powi(2)).sqrt();
                let b = ((x - pe.0).powi(2) + (y - pe.1).powi(2)).sqrt();
                a.min(b)
            };
            let sharp = (self.w / 2.0 - d + 0.5).clamp(0.0, 1.0);
            let blur = 0.5 * (erf((d + self.w / 2.0) / s2) - erf((d - self.w / 2.0) / s2));
            out[i as usize] = (2.0 * blur + sharp).min(1.0);
        }
        out
    }
}

/// Premultiplied float accumulation exactly like preview.py's `over`.
fn over(dst: &mut [[f32; 4]], dw: usize, at: &Atlas, f: &FrameRect, x0: i32, y0: i32, col: [f32; 3], alpha: f32) {
    let dh = dst.len() / dw;
    for r in 0..f.h {
        let y = y0 + f.oy + r as i32;
        if y < 0 || y as usize >= dh {
            continue;
        }
        let src = &at.px[(f.y + r) * at.w + f.x..(f.y + r) * at.w + f.x + f.w];
        for (c, &m) in src.iter().enumerate() {
            if m == 0 {
                continue;
            }
            let x = x0 + f.ox + c as i32;
            if x < 0 || x as usize >= dw {
                continue;
            }
            let a = m as f32 / 255.0 * alpha;
            let d = &mut dst[y as usize * dw + x as usize];
            d[0] = col[0] * a + d[0] * (1.0 - a);
            d[1] = col[1] * a + d[1] * (1.0 - a);
            d[2] = col[2] * a + d[2] * (1.0 - a);
            d[3] = a + d[3] * (1.0 - a);
        }
    }
}

/// Compose one sprite frame into `out` (premultiplied BGRA, `at.size()` square).
/// `edge`: per-cell-pixel edge alpha for the render state (see `EdgeGeo::alpha`).
///
/// The approved previews were saved by preview.py's `to_img`, which writes the accumulated
/// colour as straight RGB; the final premultiply below reproduces that look exactly.
pub fn compose(sc: &Script, at: &Atlas, pose: &Pose, inp: &Inputs, edge: Option<&[f32]>, scratch: &mut Vec<[f32; 4]>, out: &mut [u8]) {
    let n = at.size();
    scratch.clear();
    scratch.resize(n * n, [0.0; 4]);
    let (s0, s1, t) = (pose.from, pose.to, pose.t);
    let hot_inp = |s: State| if s == State::Hot { *inp } else { Inputs { temp: 74.0, ..*inp } };
    let (a0, b0) = field(s0, &hot_inp(s0));
    let (a1, b1) = field(s1, &hot_inp(s1));
    let (fa, fb) = (mix3(a0, a1, t), mix3(b0, b1, t));
    let style = |layer: &str| -> ([f32; 3], f32, bool) {
        let Some(l) = sc.layers.get(layer) else { return ([1.0; 3], 0.0, true) };
        let c = |s: State| match l.color[s.key()] {
            Tint::Rgb(c) => c,
            Tint::FieldA => mix3(a0, a1, if s == s0 { 0.0 } else { 1.0 }),
            Tint::FieldB => mix3(b0, b1, if s == s0 { 0.0 } else { 1.0 }),
        };
        let col = match l.color[s0.key()] {
            Tint::FieldA => fa,
            Tint::FieldB => fb,
            Tint::Rgb(_) => mix3(c(s0), c(s1), t),
        };
        let a = l.alpha[s0.key()] + (l.alpha[s1.key()] - l.alpha[s0.key()]) * t;
        (col, a, l.moves)
    };
    let p = (at.pad() / 2) as i32;
    let dyp = pose.step.dy * (at.scale.round() as i32).max(1);
    let dxp = pose.step.dx * (at.scale.round() as i32).max(1);
    let rot = match pose.step.rot {
        r if r >= 6.0 => "_r9",
        r if r >= 2.0 => "_r45",
        _ => "_r0",
    };
    // `moves`: Some(fixed) for the body layers (always move), None = the layer's moves_with_dy
    let draw = |scratch: &mut Vec<[f32; 4]>, layer: &str, name: &str, moves: Option<bool>| {
        if let Some(f) = at.frame(name) {
            let (col, a, lm) = style(layer);
            let m = moves.unwrap_or(lm);
            over(scratch, n, at, f, p + if m { dxp } else { 0 }, p + if m { dyp } else { 0 }, col, a);
        }
    };
    draw(scratch, "shadow", "shadow", Some(false));
    draw(scratch, "halo", &format!("halo_{}", pose.step.halo), Some(true));
    for ly in ["base", "tintA", "tintB", "glass"] {
        draw(scratch, ly, &format!("{ly}{rot}"), Some(true));
    }
    if let (true, Some(e)) = (pose.edge(), edge) {
        let c = at.cell;
        let lime = hexc("#D8FF6A");
        let gx = ((23.63 + 8.0) * at.scale, (96.37 + 8.0) * at.scale);
        for y in 0..c {
            let dy = p + dyp + y as i32;
            if dy < 0 || dy as usize >= n {
                continue;
            }
            for x in 0..c {
                let a = e[y * c + x];
                if a <= 0.0 {
                    continue;
                }
                let tx = ((x as f32 - gx.0) / (gx.1 - gx.0)).clamp(0.0, 1.0);
                let col = mix3(fa, lime, tx);
                let d = &mut scratch[dy as usize * n + p as usize + x];
                for k in 0..3 {
                    d[k] = col[k] * a + d[k] * (1.0 - a);
                }
                d[3] = a + d[3] * (1.0 - a);
            }
        }
    }
    draw(scratch, "led", &format!("led{rot}"), Some(true));
    draw(scratch, "eyes", &pose.step.eyes, Some(true));
    for (layer, i) in
        [("sparkle", pose.step.sparkle), ("steam", pose.step.steam), ("z", pose.step.z), ("mark", pose.step.mark), ("drop", pose.step.drop)]
    {
        if let Some(i) = i {
            draw(scratch, layer, &format!("{layer}_{i}"), None);
        }
    }
    for (o, d) in out.as_chunks_mut::<4>().0.iter_mut().zip(scratch.iter()) {
        let a = d[3].clamp(0.0, 1.0);
        let q = |v: f32| (v.clamp(0.0, 1.0) * a * 255.0 + 0.5) as u8;
        o[0] = q(d[2]);
        o[1] = q(d[1]);
        o[2] = q(d[0]);
        o[3] = (a * 255.0 + 0.5) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(skin: HexSkin) -> Script {
        Script::parse(atlas_json(skin)).expect("atlas.json parses")
    }
    fn inp() -> Inputs {
        Inputs { load: 84.0, temp: 74.0, temp_red: 83.0, progress: 0.668, vram: 0.95 }
    }

    #[test]
    fn both_skins_load_with_all_six_states_and_every_frame_they_use() {
        for skin in HexSkin::ALL {
            let s = sc(skin);
            for st in STATES {
                assert!(!s.loops[&st].is_empty(), "{skin:?} {st:?}");
            }
            assert_eq!(s.loops[&State::Render].len(), 4);
            assert_eq!(s.blink.len(), 3);
            assert_eq!(s.blink_every, (48, 84));
            // render > hot has no list of its own: render>idle then idle>hot
            let t = s.transition(State::Render, State::Hot);
            assert_eq!(t.len(), 4);
            assert_eq!((t[0].0, t[0].1, t[3].0, t[3].1), (State::Render, State::Idle, State::Idle, State::Hot));
            assert!(!s.transition(State::Idle, State::Alarm).is_empty() && !s.transition(State::Vram, State::Idle).is_empty());
            // every frame any step names exists, at every scale
            let mut steps: Vec<&Step> = s.loops.values().flatten().collect();
            steps.extend(s.blink.iter());
            steps.extend(s.transitions.values().flatten());
            for key in ["1", "1.5", "2"] {
                let at = Atlas::load(skin, key).unwrap();
                assert_eq!(at.size(), at.cell + at.pad());
                for name in ["shadow", "halo_0", "halo_1", "base_r9", "tintB_r45", "glass_r0", "led_r9"] {
                    assert!(at.frame(name).is_some(), "{skin:?} {key} {name}");
                }
                for st in &steps {
                    let mut names = vec![st.eyes.clone(), format!("halo_{}", st.halo)];
                    for (l, i) in [("sparkle", st.sparkle), ("steam", st.steam), ("z", st.z), ("mark", st.mark), ("drop", st.drop)] {
                        names.extend(i.map(|i| format!("{l}_{i}")));
                    }
                    for n in names {
                        assert!(at.frame(&n).is_some(), "{skin:?} {key} {n}");
                    }
                }
            }
        }
    }

    #[test]
    fn each_state_has_its_own_ambient_motion() {
        let s = sc(HexSkin::Velvet);
        let any = |st: State, f: fn(&Step) -> bool| s.loops[&st].iter().any(f);
        assert!(any(State::Render, |x| x.sparkle.is_some()));
        assert!(any(State::Hot, |x| x.steam.is_some()));
        assert!(any(State::Sleepy, |x| x.z.is_some()));
        assert!(any(State::Vram, |x| x.drop.is_some()));
        assert!(any(State::Alarm, |x| x.mark.is_some()) && any(State::Alarm, |x| x.dx != 0), "alarm: ! and a jolt");
        assert!(any(State::Alarm, |x| x.eyes == "eyes_alarm_a") && any(State::Alarm, |x| x.eyes == "eyes_alarm_b"), "trembling eyes");
        assert!(any(State::Idle, |x| x.eyes == "eyes_happy"), "the occasional happy blink");
        // the alarm pulse is faster than the idle breath
        let max_hold = |st: State| s.loops[&st].iter().map(|x| x.hold).max().unwrap();
        assert!(max_hold(State::Alarm) * 3 < max_hold(State::Idle));
    }

    #[test]
    fn scheduler_holds_frames_and_blinks_only_when_idle() {
        let s = sc(HexSkin::Velvet);
        let mut a = Animator::new(&s, State::Idle);
        let (mut ticks, mut blinks, mut frames, mut happy) = (0u32, 0, 0, 0);
        while ticks < 12 * 60 {
            let (p, h) = a.next(&s);
            assert!((1..=18).contains(&h), "hold {h}");
            if p.step.eyes.starts_with("eyes_blink") {
                blinks += 1;
                assert_eq!(h, 1);
            }
            happy += (p.step.eyes == "eyes_happy") as u32;
            ticks += h;
            frames += 1;
        }
        // one minute idle: a blink every 4-7 s (3 frames each), breath frames every 1.5 s
        assert!((20..=48).contains(&blinks), "blink frames {blinks}");
        assert!(happy >= 3, "a happy blink about every 12 s ({happy})");
        assert!(frames < 140, "{frames} frames in 60 s is too many");
        let mut r = Animator::new(&s, State::Render);
        for _ in 0..20 {
            let (p, h) = r.next(&s);
            assert_eq!(h, 6);
            assert!(p.edge() && !p.step.eyes.contains("blink"));
        }
    }

    fn sig(render: bool, load: f32, temp: f32, vram: f32) -> Signals {
        Signals { render, load: Some(load), temp: Some(temp), vram: Some(vram), gpu_error: false, crash: false }
    }

    #[test]
    fn priority_alarm_hot_vram_render_sleepy_idle() {
        let mut t = Triggers::default();
        let (amber, red) = (80.0, 83.0);
        assert_eq!(t.desired(&sig(false, 40.0, 60.0, 0.5), amber, red, 0), State::Idle);
        assert_eq!(t.desired(&sig(true, 90.0, 60.0, 0.5), amber, red, 1_000), State::Render);
        // VRAM beats render (after 3 s)
        t.desired(&sig(true, 90.0, 60.0, 0.95), amber, red, 2_000);
        assert_eq!(t.desired(&sig(true, 90.0, 60.0, 0.95), amber, red, 5_000), State::Vram);
        // hot beats VRAM
        assert_eq!(t.desired(&sig(true, 90.0, 81.0, 0.95), amber, red, 6_000), State::Hot);
        // alarm (red temperature) beats hot
        assert_eq!(t.desired(&sig(true, 90.0, 84.0, 0.95), amber, red, 7_000), State::Alarm);
        // sleepy only after 10 min of quiet, and below everything else
        let mut q = Triggers::default();
        assert_eq!(q.desired(&sig(false, 2.0, 40.0, 0.2), amber, red, 1_000), State::Idle);
        assert_eq!(q.desired(&sig(false, 2.0, 40.0, 0.2), amber, red, 1_000 + SLEEPY_MS), State::Sleepy);
        assert_eq!(q.desired(&sig(false, 2.0, 40.0, 0.96), amber, red, 2_000 + SLEEPY_MS), State::Sleepy, "VRAM needs 3 s");
        assert_eq!(q.desired(&sig(false, 2.0, 40.0, 0.96), amber, red, 5_000 + SLEEPY_MS), State::Vram);
    }

    #[test]
    fn thresholds_have_hysteresis() {
        let (amber, red) = (80.0, 83.0);
        // hot: on at amber, off below amber - 2
        let mut t = Triggers::default();
        assert_eq!(t.desired(&sig(false, 40.0, 79.9, 0.1), amber, red, 0), State::Idle);
        assert_eq!(t.desired(&sig(false, 40.0, 80.0, 0.1), amber, red, 1), State::Hot);
        assert_eq!(t.desired(&sig(true, 40.0, 78.5, 0.1), amber, red, 2), State::Hot, "stays hot until amber - 2");
        assert_eq!(t.desired(&sig(true, 40.0, 77.9, 0.1), amber, red, 3), State::Render);
        // VRAM: >= 92 % for 3 s on, below 85 % off, a dip under 92 % before 3 s restarts the wait
        let mut v = Triggers::default();
        assert_eq!(v.desired(&sig(false, 40.0, 60.0, 0.92), amber, red, 0), State::Idle);
        assert_eq!(v.desired(&sig(false, 40.0, 60.0, 0.93), amber, red, 2_000), State::Idle);
        assert_eq!(v.desired(&sig(false, 40.0, 60.0, 0.90), amber, red, 2_500), State::Idle);
        assert_eq!(v.desired(&sig(false, 40.0, 60.0, 0.93), amber, red, 3_500), State::Idle, "the wait restarted");
        assert_eq!(v.desired(&sig(false, 40.0, 60.0, 0.93), amber, red, 6_500), State::Vram);
        assert_eq!(v.desired(&sig(false, 40.0, 60.0, 0.86), amber, red, 7_000), State::Vram, "stays until below 85 %");
        assert_eq!(v.desired(&sig(false, 40.0, 60.0, 0.849), amber, red, 8_000), State::Idle);
        // red temperature: alarm, held 10 s after it drops below red - 2
        let mut r = Triggers::default();
        assert_eq!(r.desired(&sig(false, 40.0, 83.0, 0.1), amber, red, 0), State::Alarm);
        assert_eq!(r.desired(&sig(false, 40.0, 81.5, 0.1), amber, red, 1_000), State::Alarm, "until red - 2");
        assert_eq!(r.desired(&sig(false, 40.0, 70.0, 0.1), amber, red, 2_000), State::Alarm, "held 10 s");
        assert_eq!(r.desired(&sig(false, 40.0, 70.0, 0.1), amber, red, 10_999), State::Alarm);
        assert_eq!(r.desired(&sig(false, 40.0, 70.0, 0.1), amber, red, 11_000), State::Idle);
    }

    #[test]
    fn crash_and_gpu_error_alarms() {
        let (amber, red) = (80.0, 83.0);
        let base = sig(false, 40.0, 60.0, 0.1);
        // crash: stays until clicked, never shorter than 10 s
        let mut t = Triggers::default();
        assert_eq!(t.desired(&Signals { crash: true, ..base }, amber, red, 0), State::Alarm);
        assert_eq!(t.desired(&Signals { crash: false, ..base }, amber, red, 60_000), State::Alarm, "not acknowledged yet");
        t.acknowledge();
        assert_eq!(t.desired(&base, amber, red, 60_001), State::Idle);
        // an early click ends it at 10 s
        let mut t = Triggers::default();
        t.desired(&Signals { crash: true, ..base }, amber, red, 0);
        t.acknowledge();
        assert_eq!(t.desired(&base, amber, red, 4_000), State::Alarm);
        assert_eq!(t.desired(&base, amber, red, 10_000), State::Idle);
        // the next render clears it too
        let mut t = Triggers::default();
        t.desired(&Signals { crash: true, ..base }, amber, red, 0);
        assert_eq!(t.desired(&Signals { render: true, ..base }, amber, red, 12_000), State::Render);
        // the crash flag staying true does not re-arm it
        let mut t = Triggers::default();
        t.desired(&Signals { crash: true, ..base }, amber, red, 0);
        t.acknowledge();
        assert_eq!(t.desired(&Signals { crash: true, ..base }, amber, red, 11_000), State::Idle);
        // GPU / driver error: while it lasts, plus 10 s
        let mut g = Triggers::default();
        assert_eq!(g.desired(&Signals { gpu_error: true, temp: None, load: None, vram: None, ..base }, amber, red, 0), State::Alarm);
        assert_eq!(g.desired(&base, amber, red, 9_000), State::Alarm);
        assert_eq!(g.desired(&base, amber, red, 10_000), State::Idle);
    }

    #[test]
    fn transitions_mix_colours() {
        let s = sc(HexSkin::Velvet);
        let mut a = Animator::new(&s, State::Idle);
        assert!(a.go(&s, State::Hot));
        assert!(!a.go(&s, State::Render), "no new transition while one plays");
        let (p1, _) = a.next(&s);
        let (p2, _) = a.next(&s);
        assert_eq!((p1.from, p1.to), (State::Idle, State::Hot));
        assert!(p1.t < p2.t);
        let (p3, _) = a.next(&s);
        assert_eq!((p3.from, p3.to), (State::Hot, State::Hot));
        assert!(a.go(&s, State::Alarm));
        let (p4, _) = a.next(&s);
        assert_eq!((p4.from, p4.to), (State::Hot, State::Idle), "hot > alarm goes through idle");
    }

    #[test]
    fn field_rules() {
        let i = Inputs { load: 0.0, temp: 60.0, temp_red: 83.0, progress: 0.0, vram: 0.95 };
        let (a, _) = field(State::Idle, &i);
        assert_eq!(a, hexc("#4FE3FF"));
        let (a, b) = field(State::Hot, &Inputs { temp: 84.0, ..i });
        assert_eq!((a, b), (hexc("#FF7A45"), hexc("#FF3355")));
        let (a, _) = field(State::Render, &i);
        assert_eq!(a, hexc("#5CFFD8"));
        let near = |x: [f32; 3], y: [f32; 3]| x.iter().zip(y).all(|(p, q)| (p - q).abs() < 1e-5);
        let (a, b) = field(State::Vram, &i);
        assert!(near(a, hexc("#FFC24D")) && near(b, hexc("#FF8A3D")), "full VRAM is amber/orange");
        let (a, _) = field(State::Vram, &Inputs { vram: 0.80, ..i });
        assert!(near(a, mix3(hexc("#4FE3FF"), hexc("#FFC24D"), 0.5)), "below 85 % it is half way");
        let (a, b) = field(State::Alarm, &i);
        assert_eq!((a, b), (hexc("#FF5A6E"), hexc("#B0204A")));
    }

    #[test]
    fn composed_sprite_is_premultiplied_with_a_clear_border() {
        for skin in HexSkin::ALL {
            let s = sc(skin);
            let at = Atlas::load(skin, "1").unwrap();
            let eg = EdgeGeo::new(&s, &at);
            let n = at.size();
            let mut out = vec![0u8; n * n * 4];
            let mut scratch = vec![];
            let e = eg.alpha(0.668);
            for st in STATES {
                let pose = Animator::new(&s, st).next(&s).0;
                compose(&s, &at, &pose, &inp(), Some(&e), &mut scratch, &mut out);
                for px in out.as_chunks::<4>().0 {
                    assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]);
                }
                for (x, y) in [(0, 0), (n - 1, 0), (0, n - 1), (n - 1, n - 1)] {
                    assert_eq!(&out[(y * n + x) * 4..(y * n + x) * 4 + 4], &[0, 0, 0, 0], "{skin:?} {st:?}");
                }
            }
            // the edge is drawn: more opaque pixels with the edge than without
            let pose = Animator::new(&s, State::Render).next(&s).0;
            compose(&s, &at, &pose, &inp(), Some(&e), &mut scratch, &mut out);
            let mut no_edge = vec![0u8; n * n * 4];
            compose(&s, &at, &pose, &inp(), None, &mut scratch, &mut no_edge);
            let opaque = |v: &[u8]| v.as_chunks::<4>().0.iter().filter(|p| p[3] > 200).count();
            assert!(opaque(&out) > opaque(&no_edge) + 50);
            assert!(eg.alpha(0.0).iter().all(|&a| a == 0.0));
            assert!(eg.alpha(1.0).iter().filter(|&&a| a > 0.9).count() > eg.alpha(0.5).iter().filter(|&&a| a > 0.9).count());
        }
    }
}
