//! Minimal PNG reader for the embedded Hex atlas: zlib inflate (RFC 1950/1951) plus PNG
//! filtering for 8-bit greyscale, non-interlaced images. Nothing else is supported, which is
//! all the atlas needs; anything unexpected returns None instead of panicking.

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Option<u32> {
        while self.nbits < n {
            let b = *self.d.get(self.pos)? as u32;
            self.pos += 1;
            self.bit |= b << self.nbits;
            self.nbits += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Some(v)
    }
}

/// Canonical Huffman table (puff-style): counts per length and symbols sorted by code.
struct Huff {
    count: [u16; 16],
    sym: Vec<u16>,
}

impl Huff {
    fn new(lens: &[u8]) -> Option<Huff> {
        let mut count = [0u16; 16];
        for &l in lens {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + count[i - 1];
        }
        let mut sym = vec![0u16; lens.len()];
        for (s, &l) in lens.iter().enumerate() {
            if l != 0 {
                sym[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Some(Huff { count, sym })
    }
    fn decode(&self, b: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.need(1)? as i32;
            let c = self.count[len] as i32;
            if code - c < first {
                return self.sym.get((index + (code - first)) as usize).copied();
            }
            index += c;
            first += c;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LBASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385,
    24577,
];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn codes(b: &mut Bits, out: &mut Vec<u8>, lit: &Huff, dist: &Huff) -> Option<()> {
    loop {
        let s = lit.decode(b)?;
        if s < 256 {
            out.push(s as u8);
        } else if s == 256 {
            return Some(());
        } else {
            let i = (s - 257) as usize;
            let len = *LBASE.get(i)? as usize + b.need(*LEXT.get(i)? as u32)? as usize;
            let di = dist.decode(b)? as usize;
            let d = *DBASE.get(di)? as usize + b.need(*DEXT.get(di)? as u32)? as usize;
            if d > out.len() {
                return None;
            }
            let start = out.len() - d;
            for k in 0..len {
                let v = out[start + k];
                out.push(v);
            }
        }
    }
}

/// Raw DEFLATE stream.
pub fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut b = Bits { d: data, pos: 0, bit: 0, nbits: 0 };
    let mut out = Vec::with_capacity(data.len() * 4);
    loop {
        let last = b.need(1)?;
        match b.need(2)? {
            0 => {
                b.bit = 0;
                b.nbits = 0;
                let h = b.d.get(b.pos..b.pos + 4)?;
                let len = u16::from_le_bytes([h[0], h[1]]) as usize;
                if len != !u16::from_le_bytes([h[2], h[3]]) as usize {
                    return None;
                }
                b.pos += 4;
                out.extend_from_slice(b.d.get(b.pos..b.pos + len)?);
                b.pos += len;
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                codes(&mut b, &mut out, &Huff::new(&l)?, &Huff::new(&[5u8; 30])?)?;
            }
            2 => {
                let nlen = b.need(5)? as usize + 257;
                let ndist = b.need(5)? as usize + 1;
                let ncode = b.need(4)? as usize + 4;
                const ORD: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in &ORD[..ncode] {
                    cl[o] = b.need(3)? as u8;
                }
                let ch = Huff::new(&cl)?;
                let mut lens = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let s = ch.decode(&mut b)?;
                    let (v, rep) = match s {
                        0..=15 => (s as u8, 1),
                        16 => (*lens.get(i.checked_sub(1)?)?, 3 + b.need(2)? as usize),
                        17 => (0, 3 + b.need(3)? as usize),
                        _ => (0, 11 + b.need(7)? as usize),
                    };
                    if i + rep > lens.len() {
                        return None;
                    }
                    lens[i..i + rep].fill(v);
                    i += rep;
                }
                codes(&mut b, &mut out, &Huff::new(&lens[..nlen])?, &Huff::new(&lens[nlen..])?)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some(out);
        }
    }
}

/// zlib wrapper (2-byte header, no preset dictionary; the Adler-32 trailer is not checked).
pub fn zlib(data: &[u8]) -> Option<Vec<u8>> {
    let (cmf, flg) = (*data.first()?, *data.get(1)?);
    if cmf & 0x0f != 8 || !((cmf as u16) << 8 | flg as u16).is_multiple_of(31) || flg & 0x20 != 0 {
        return None;
    }
    inflate(&data[2..])
}

/// 8-bit greyscale, non-interlaced PNG -> (width, height, pixels).
pub fn decode_gray8(png: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if png.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let (mut pos, mut w, mut h, mut idat) = (8usize, 0usize, 0usize, Vec::new());
    while pos + 8 <= png.len() {
        let n = u32::from_be_bytes(png[pos..pos + 4].try_into().ok()?) as usize;
        let ty = &png[pos + 4..pos + 8];
        let body = png.get(pos + 8..pos + 8 + n)?;
        match ty {
            b"IHDR" => {
                w = u32::from_be_bytes(body.get(0..4)?.try_into().ok()?) as usize;
                h = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?) as usize;
                // bit depth 8, colour type 0 (grey), compression 0, filter 0, no interlace
                if body.get(8..13)? != [8, 0, 0, 0, 0] || w == 0 || h == 0 || w > 8192 || h > 8192 {
                    return None;
                }
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + n;
    }
    let raw = zlib(&idat)?;
    if raw.len() < (w + 1) * h {
        return None;
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let f = raw[y * (w + 1)];
        let src = &raw[y * (w + 1) + 1..(y + 1) * (w + 1)];
        let (prev, cur) = out.split_at_mut(y * w);
        let up = if y > 0 { &prev[(y - 1) * w..] } else { &[][..] };
        let cur = &mut cur[..w];
        for x in 0..w {
            let a = if x > 0 { cur[x - 1] as i16 } else { 0 };
            let b = if y > 0 { up[x] as i16 } else { 0 };
            let c = if x > 0 && y > 0 { up[x - 1] as i16 } else { 0 };
            let pred = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => return None,
            };
            cur[x] = (src[x] as i16 + pred) as u8;
        }
    }
    Some((w, h, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_and_fixed_blocks() {
        // python: zlib.compress(b"hello hello hello", 9) (fixed Huffman with a back-reference)
        let z = [120, 218, 203, 72, 205, 201, 201, 87, 200, 64, 144, 0, 58, 46, 6, 125];
        assert_eq!(zlib(&z).unwrap(), b"hello hello hello");
        // a stored block: BFINAL=1, BTYPE=00, LEN=3
        let s = [1, 3, 0, 252, 255, b'a', b'b', b'c'];
        assert_eq!(inflate(&s).unwrap(), b"abc");
    }

    #[test]
    fn garbage_is_rejected_not_a_panic() {
        assert!(zlib(&[1, 2, 3]).is_none());
        assert!(decode_gray8(b"not a png").is_none());
        assert!(inflate(&[0xff; 16]).is_none());
    }

    #[test]
    fn decodes_the_embedded_atlas() {
        // sizes and pixel sums computed with PIL for the same files
        let cases: [(&[u8], usize, usize, u64); 6] = [
            (include_bytes!("../res/hex/velvet/hex_atlas_1x.png"), 511, 375, 10_416_198),
            (include_bytes!("../res/hex/velvet/hex_atlas_1_5x.png"), 757, 562, 23_393_821),
            (include_bytes!("../res/hex/velvet/hex_atlas_2x.png"), 999, 747, 41_562_452),
            (include_bytes!("../res/hex/flat/hex_atlas_1x.png"), 508, 356, 10_078_770),
            (include_bytes!("../res/hex/flat/hex_atlas_1_5x.png"), 754, 528, 22_679_836),
            (include_bytes!("../res/hex/flat/hex_atlas_2x.png"), 983, 702, 40_303_102),
        ];
        for (png, w0, h0, sum0) in cases {
            let (w, h, px) = decode_gray8(png).unwrap();
            assert_eq!((w, h), (w0, h0));
            assert_eq!(px.iter().map(|&v| v as u64).sum::<u64>(), sum0);
        }
    }
}
