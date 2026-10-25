//! Minimal DirectWrite text rasteriser (raw COM vtable calls; windows-sys has no COM interfaces).
//! Text is turned into glyph runs (with per-character font fallback), rasterised once with
//! IDWriteGlyphRunAnalysis into a ClearType 3x1 coverage texture, cached, and blended into the
//! premultiplied BGRA back buffer: ClearType on the solid theme, grayscale over glass.
//! Small sizes use GDI-classic hinting with whole-pixel advances for crisp stems.
#![allow(clippy::missing_safety_doc)]
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::rc::Rc;

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);
const IID_IDWRITE_FACTORY: Guid = Guid(0xb859ee5a, 0xd838, 0x4b5b, [0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48]);

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FontMetrics {
    upem: u16,
    ascent: u16,
    descent: u16,
    line_gap: i16,
    cap: u16,
    x_height: u16,
    ul_pos: i16,
    ul_thick: u16,
    st_pos: i16,
    st_thick: u16,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct GlyphMetrics {
    lsb: i32,
    adv_w: u32,
    rsb: i32,
    tsb: i32,
    adv_h: u32,
    bsb: i32,
    vorg: i32,
}
#[repr(C)]
struct GlyphRun {
    face: *mut c_void,
    em: f32,
    count: u32,
    indices: *const u16,
    advances: *const f32,
    offsets: *const c_void,
    sideways: i32,
    bidi: u32,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

unsafe fn vfn(obj: *mut c_void, idx: usize) -> *const c_void {
    unsafe { *(*(obj as *const *const *const c_void)).add(idx) }
}
macro_rules! com {
    ($obj:expr, $idx:expr, ($($t:ty),*) -> $r:ty; $($a:expr),*) => {{
        let f: unsafe extern "system" fn(*mut c_void $(, $t)*) -> $r = std::mem::transmute(vfn($obj, $idx));
        f($obj $(, $a)*)
    }};
}
unsafe fn release(o: *mut c_void) {
    if !o.is_null() {
        unsafe {
            com!(o, 2, () -> u32;);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    Sans,
    SansSemi,
    Mono,
    MonoSemi,
    /// 700: island numerals, job names
    SansBold,
    /// 200-300: the big hero numeral
    SansLight,
}

#[derive(Clone, Copy)]
struct FaceInfo {
    face: *mut c_void,
    m: FontMetrics,
}

/// One rasterised piece (a run of glyphs from one font) relative to the pen origin/baseline.
struct Piece {
    left: i32,
    top: i32,
    w: i32,
    h: i32,
    cov: Vec<u8>,
}

/// A laid-out, rasterised string in device pixels.
pub struct Text {
    pub adv: f32,
    pub asc: f32,
    pub desc: f32,
    pub cap: f32,
    pieces: Vec<Piece>,
}

pub struct Dw {
    factory: *mut c_void,
    chains: HashMap<Face, Vec<FaceInfo>>,
    cache: HashMap<(String, Face, u32, i32), Rc<Text>>,
    lut: [u8; 256],
    pub rasterised: u64,
}

impl Dw {
    pub fn new() -> Option<Dw> {
        unsafe {
            let lib = libloading::Library::new("dwrite.dll").ok()?;
            type Create = unsafe extern "system" fn(u32, *const Guid, *mut *mut c_void) -> i32;
            let create: Create = *lib.get::<Create>(b"DWriteCreateFactory\0").ok()?;
            std::mem::forget(lib); // kept for the life of the process
            let mut f = null_mut();
            if create(0, &IID_IDWRITE_FACTORY, &mut f) < 0 || f.is_null() {
                return None;
            }
            let mut coll = null_mut();
            com!(f, 3, (*mut *mut c_void, i32) -> i32; &mut coll, 0);
            if coll.is_null() {
                return None;
            }
            let find = |names: &[&str], weight: u32| -> Option<FaceInfo> {
                for n in names {
                    let w: Vec<u16> = n.encode_utf16().chain(Some(0)).collect();
                    let (mut idx, mut exists) = (0u32, 0i32);
                    if com!(coll, 5, (*const u16, *mut u32, *mut i32) -> i32; w.as_ptr(), &mut idx, &mut exists) < 0 || exists == 0 {
                        continue;
                    }
                    let mut fam = null_mut();
                    com!(coll, 4, (u32, *mut *mut c_void) -> i32; idx, &mut fam);
                    if fam.is_null() {
                        continue;
                    }
                    let mut font = null_mut();
                    com!(fam, 7, (u32, u32, u32, *mut *mut c_void) -> i32; weight, 5, 0, &mut font);
                    // Never take a slanted or simulated face (Wine's matcher can return a synthetic
                    // oblique): pick the upright, non-simulated font with the closest weight instead.
                    let upright = |f: *mut c_void| -> bool { com!(f, 6, () -> u32;) == 0 && com!(f, 10, () -> u32;) == 0 };
                    if !font.is_null() && !upright(font) {
                        release(font);
                        let n = com!(fam, 4, () -> u32;);
                        let mut best: (u32, *mut c_void) = (u32::MAX, null_mut());
                        for i in 0..n {
                            let mut f = null_mut();
                            if com!(fam, 5, (u32, *mut *mut c_void) -> i32; i, &mut f) < 0 || f.is_null() {
                                continue;
                            }
                            let d = (com!(f, 4, () -> u32;) as i32 - weight as i32).unsigned_abs();
                            if upright(f) && d < best.0 {
                                if !best.1.is_null() {
                                    release(best.1);
                                }
                                best = (d, f);
                            } else {
                                release(f);
                            }
                        }
                        font = best.1;
                    }
                    release(fam);
                    if font.is_null() {
                        continue;
                    }
                    let mut face = null_mut();
                    com!(font, 13, (*mut *mut c_void) -> i32; &mut face);
                    release(font);
                    if face.is_null() {
                        continue;
                    }
                    let mut m = FontMetrics::default();
                    com!(face, 8, (*mut FontMetrics) -> (); &mut m);
                    if m.upem == 0 {
                        release(face);
                        continue;
                    }
                    if m.cap == 0 {
                        m.cap = (m.ascent as f32 * 0.7) as u16;
                    }
                    return Some(FaceInfo { face, m });
                }
                None
            };
            const SANS: &[&str] = &["Segoe UI", "Inter", "Tahoma", "Arial", "DejaVu Sans"];
            const MONO: &[&str] = &["Cascadia Mono", "Cascadia Code", "Consolas", "Courier New", "DejaVu Sans Mono"];
            const SYM: &[&str] = &["Segoe UI Symbol", "Arial Unicode MS"];
            let mut chains = HashMap::new();
            for (face, primary, other, weight) in [
                (Face::Sans, SANS, MONO, 400),
                (Face::SansSemi, SANS, MONO, 600),
                (Face::Mono, MONO, SANS, 400),
                (Face::MonoSemi, MONO, SANS, 600),
                (Face::SansBold, SANS, MONO, 700),
                (Face::SansLight, SANS, MONO, 200),
            ] {
                let v: Vec<FaceInfo> = [find(primary, weight), find(other, weight), find(SYM, 400)].into_iter().flatten().collect();
                if v.is_empty() {
                    return None;
                }
                chains.insert(face, v);
            }
            release(coll);
            // coverage boost for light-on-dark text (naive sRGB blending otherwise looks thin)
            let mut lut = [0u8; 256];
            for (i, v) in lut.iter_mut().enumerate() {
                *v = ((i as f32 / 255.0).powf(0.80) * 255.0).round() as u8;
            }
            Some(Dw { factory: f, chains, cache: HashMap::new(), lut, rasterised: 0 })
        }
    }

    /// Drop cached rasters (e.g. while minimised).
    pub fn trim(&mut self) {
        self.cache.clear();
        self.cache.shrink_to_fit();
    }

    /// Lay out and rasterise `s` at `px` pixels (cached).
    pub fn get(&mut self, s: &str, face: Face, px: f32) -> Rc<Text> {
        self.get_t(s, face, px, 0.0)
    }

    /// As `get`, with `track` extra pixels between characters (letter-spacing; may be negative).
    pub fn get_t(&mut self, s: &str, face: Face, px: f32, track: f32) -> Rc<Text> {
        let key = (s.to_string(), face, (px * 64.0) as u32, (track * 64.0) as i32);
        if let Some(t) = self.cache.get(&key) {
            return t.clone();
        }
        if self.cache.len() > 1500 {
            self.cache.clear();
        }
        let t = Rc::new(unsafe { self.layout(s, face, px, track) });
        self.cache.insert(key, t.clone());
        t
    }

    unsafe fn layout(&mut self, s: &str, face: Face, px: f32, track: f32) -> Text {
        unsafe {
            let chain = self.chains[&face].clone();
            let m0 = chain[0].m;
            let k0 = px / m0.upem as f32;
            let (asc, desc, cap) = (m0.ascent as f32 * k0, m0.descent as f32 * k0, m0.cap as f32 * k0);
            let small = px <= 18.0;
            // glyph indices with per-character fallback, grouped into runs
            let mut runs: Vec<(usize, Vec<u16>)> = vec![];
            for ch in s.chars() {
                let cp = ch as u32;
                let mut pick = (0usize, 0u16);
                for (fi, f) in chain.iter().enumerate() {
                    let mut g = 0u16;
                    com!(f.face, 11, (*const u32, u32, *mut u16) -> i32; &cp, 1, &mut g);
                    if g != 0 {
                        pick = (fi, g);
                        break;
                    }
                }
                match runs.last_mut() {
                    Some((fi, v)) if *fi == pick.0 => v.push(pick.1),
                    _ => runs.push((pick.0, vec![pick.1])),
                }
            }
            let mut pen = 0.0f32;
            let mut pieces = vec![];
            for (fi, glyphs) in runs {
                let f = &chain[fi];
                let k = px / f.m.upem as f32;
                let mut gm = vec![GlyphMetrics::default(); glyphs.len()];
                let mut ok = false;
                if small {
                    // GDI-compatible (hinted) advances
                    ok = com!(f.face, 17, (f32, f32, *const c_void, i32, *const u16, u32, *mut GlyphMetrics, i32) -> i32;
                              px, 1.0, null(), 0, glyphs.as_ptr(), glyphs.len() as u32, gm.as_mut_ptr(), 0)
                        >= 0;
                }
                if !ok {
                    com!(f.face, 10, (*const u16, u32, *mut GlyphMetrics, i32) -> i32; glyphs.as_ptr(), glyphs.len() as u32, gm.as_mut_ptr(), 0);
                }
                let advs: Vec<f32> = gm
                    .iter()
                    .map(|g| {
                        let a = g.adv_w as f32 * k + track;
                        if small { a.round() } else { a }
                    })
                    .collect();
                let origin = if small { pen.round() } else { pen };
                if let Some(p) = self.raster(f.face, &glyphs, &advs, px, origin, small) {
                    pieces.push(p);
                }
                pen = origin + advs.iter().sum::<f32>();
            }
            let adv = if track != 0.0 && !s.is_empty() { pen - track } else { pen };
            Text { adv, asc, desc, cap, pieces }
        }
    }

    unsafe fn raster(&mut self, face: *mut c_void, glyphs: &[u16], advs: &[f32], px: f32, x: f32, small: bool) -> Option<Piece> {
        unsafe {
            let run = GlyphRun {
                face,
                em: px,
                count: glyphs.len() as u32,
                indices: glyphs.as_ptr(),
                advances: advs.as_ptr(),
                offsets: null(),
                sideways: 0,
                bidi: 0,
            };
            // rendering/measuring modes: GDI_CLASSIC(2)/GDI_CLASSIC(1) when small, else NATURAL_SYMMETRIC(5)/NATURAL(0)
            let tries: &[(u32, u32)] = if small { &[(2, 1), (5, 0), (4, 0)] } else { &[(5, 0), (4, 0)] };
            let mut ana = null_mut();
            // explicit identity transform (Wine shears glyphs when this is NULL)
            let ident: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            for (rm, mm) in tries {
                let hr = com!(self.factory, 23, (*const GlyphRun, f32, *const c_void, u32, u32, f32, f32, *mut *mut c_void) -> i32;
                              &run, 1.0, ident.as_ptr() as *const c_void, *rm, *mm, x, 0.0, &mut ana);
                if hr >= 0 && !ana.is_null() {
                    break;
                }
                ana = null_mut();
            }
            if ana.is_null() {
                return None;
            }
            let mut r = Rect::default();
            com!(ana, 3, (u32, *mut Rect) -> i32; 1, &mut r);
            let (w, h) = (r.r - r.l, r.b - r.t);
            if w <= 0 || h <= 0 {
                release(ana);
                return None;
            }
            let mut cov = vec![0u8; (w * h * 3) as usize];
            let hr = com!(ana, 4, (u32, *const Rect, *mut u8, u32) -> i32; 1, &r, cov.as_mut_ptr(), cov.len() as u32);
            release(ana);
            if hr < 0 {
                return None;
            }
            for v in cov.iter_mut() {
                *v = self.lut[*v as usize];
            }
            self.rasterised += 1;
            Some(Piece { left: r.l, top: r.t, w, h, cov })
        }
    }
}

/// Blend `t` with its pen origin at (x, baseline) into a premultiplied BGRA buffer.
/// `cleartype`: per-channel coverage (opaque backgrounds only); otherwise grayscale.
pub fn blend(
    t: &Text,
    buf: &mut [u8],
    bw: i32,
    bh: i32,
    x: i32,
    baseline: i32,
    c: (u8, u8, u8),
    alpha: f32,
    cleartype: bool,
    clip_top: i32,
) {
    let ca = (alpha.clamp(0.0, 1.0) * 255.0) as u32;
    for p in &t.pieces {
        for row in 0..p.h {
            let y = baseline + p.top + row;
            if y < clip_top.max(0) || y >= bh {
                continue;
            }
            for col in 0..p.w {
                let xx = x + p.left + col;
                if xx < 0 || xx >= bw {
                    continue;
                }
                let ci = ((row * p.w + col) * 3) as usize;
                let (cr, cg, cb) = (p.cov[ci] as u32, p.cov[ci + 1] as u32, p.cov[ci + 2] as u32);
                if cr | cg | cb == 0 {
                    continue;
                }
                let di = ((y * bw + xx) * 4) as usize;
                let d = &mut buf[di..di + 4];
                if cleartype {
                    // d = [B, G, R, A]; channel coverage from the 3x1 texture (R, G, B order)
                    for (i, (cov, col)) in [(cb, c.2), (cg, c.1), (cr, c.0)].into_iter().enumerate() {
                        let a = cov * ca / 255;
                        d[i] = ((col as u32 * a + d[i] as u32 * (255 - a)) / 255) as u8;
                    }
                    let a = cr.max(cg).max(cb) * ca / 255;
                    d[3] = (a + d[3] as u32 * (255 - a) / 255) as u8;
                } else {
                    let a = (cr + cg + cb) / 3 * ca / 255;
                    for (i, col) in [c.2, c.1, c.0].into_iter().enumerate() {
                        d[i] = ((col as u32 * a + d[i] as u32 * (255 - a)) / 255) as u8;
                    }
                    d[3] = (a + d[3] as u32 * (255 - a) / 255) as u8;
                }
            }
        }
    }
}

/// Debug aid: coverage of the first piece as a grayscale PGM.
pub fn debug_pgm(t: &Text) -> Vec<u8> {
    let Some(p) = t.pieces.first() else { return vec![] };
    let mut v = format!("P5 {} {} 255\n", p.w * 3, p.h).into_bytes();
    v.extend_from_slice(&p.cov);
    v
}
