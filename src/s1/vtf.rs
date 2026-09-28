#![allow(dead_code)]

use anyhow::{Context, Result, bail, ensure};

use crate::bsp::Rd;
use crate::s2::texture::{Compressed, Image, Tex};
use crate::s2::vpk::Fs;

pub const RGBA8888: i32 = 0;
pub const ABGR8888: i32 = 1;
pub const RGB888: i32 = 2;
pub const BGR888: i32 = 3;
pub const RGB565: i32 = 4;
pub const I8: i32 = 5;
pub const IA88: i32 = 6;
pub const A8: i32 = 8;
pub const RGB888_BLUESCREEN: i32 = 9;
pub const BGR888_BLUESCREEN: i32 = 10;
pub const ARGB8888: i32 = 11;
pub const BGRA8888: i32 = 12;
pub const DXT1: i32 = 13;
pub const DXT3: i32 = 14;
pub const DXT5: i32 = 15;
pub const BGRX8888: i32 = 16;
pub const BGR565: i32 = 17;
pub const BGRX5551: i32 = 18;
pub const BGRA4444: i32 = 19;
pub const DXT1_ONEBITALPHA: i32 = 20;
pub const BGRA5551: i32 = 21;
pub const UV88: i32 = 22;
pub const UVWQ8888: i32 = 23;
pub const RGBA16161616F: i32 = 24;
pub const RGBA16161616: i32 = 25;
pub const UVLX8888: i32 = 26;

pub const FLAG_ENVMAP: u32 = 0x4000;

#[derive(Clone, Debug)]
pub struct Info {
    pub version: (u32, u32),
    pub w: usize,
    pub h: usize,
    pub flags: u32,
    pub frames: usize,
    pub first_frame: u16,
    pub reflectivity: [f32; 3],
    pub format: i32,
    pub mips: usize,
    pub depth: usize,
    pub faces: usize,
    pub data_off: usize,
}

fn block(format: i32) -> Option<usize> {
    match format {
        DXT1 | DXT1_ONEBITALPHA => Some(8),
        DXT3 | DXT5 => Some(16),
        _ => None,
    }
}

fn pixel(format: i32) -> Option<usize> {
    Some(match format {
        I8 | A8 | 7 => 1,
        IA88 | RGB565 | BGR565 | BGRX5551 | BGRA4444 | BGRA5551 | UV88 => 2,
        RGB888 | BGR888 | RGB888_BLUESCREEN | BGR888_BLUESCREEN => 3,
        RGBA8888 | ABGR8888 | ARGB8888 | BGRA8888 | BGRX8888 | UVWQ8888 | UVLX8888 => 4,
        RGBA16161616F | RGBA16161616 => 8,
        _ => return None,
    })
}

pub fn format_name(f: i32) -> String {
    let n = match f {
        RGBA8888 => "RGBA8888",
        ABGR8888 => "ABGR8888",
        RGB888 => "RGB888",
        BGR888 => "BGR888",
        RGB565 => "RGB565",
        I8 => "I8",
        IA88 => "IA88",
        A8 => "A8",
        ARGB8888 => "ARGB8888",
        BGRA8888 => "BGRA8888",
        DXT1 => "DXT1",
        DXT3 => "DXT3",
        DXT5 => "DXT5",
        BGRX8888 => "BGRX8888",
        DXT1_ONEBITALPHA => "DXT1_ONEBITALPHA",
        UV88 => "UV88",
        RGBA16161616F => "RGBA16161616F",
        RGBA16161616 => "RGBA16161616",
        _ => return format!("format {f}"),
    };
    n.to_string()
}

pub fn info(d: &[u8]) -> Result<Info> {
    ensure!(d.len() >= 64 && d.starts_with(b"VTF\0"), "not a VTF");
    let r = Rd { data: d };
    let version = (r.u32(4), r.u32(8));
    ensure!(version.0 == 7 && version.1 <= 5, "VTF version {}.{}", version.0, version.1);
    let header = r.u32(12) as usize;
    let (w, h) = (r.u16(16) as usize, r.u16(18) as usize);
    let flags = r.u32(20);
    let frames = (r.u16(24) as usize).max(1);
    let first_frame = r.u16(26);
    let reflectivity = [r.f32(32), r.f32(36), r.f32(40)];
    let format = r.i32(52);
    let mips = (r.u8(56) as usize).max(1);
    let low_format = r.i32(57);
    let (lw, lh) = (r.u8(61) as usize, r.u8(62) as usize);
    let depth = if version.1 >= 2 && d.len() >= 65 { (r.u16(63) as usize).max(1) } else { 1 };
    ensure!(w > 0 && h > 0, "empty texture");
    let faces = if flags & FLAG_ENVMAP != 0 { if version.1 < 5 && first_frame != 0xffff { 7 } else { 6 } } else { 1 };
    let data_off = if version.1 >= 3 {
        ensure!(d.len() >= 80, "VTF header truncated");
        let n = r.u32(68) as usize;
        let mut found = None;
        for k in 0..n {
            let o = 80 + k * 8;
            let e = r.bytes(o, 8).context("VTF resource table truncated")?;
            if e[..3] == [0x30, 0, 0] {
                found = Some(r.u32(o + 4) as usize);
            }
        }
        found.context("VTF has no image data resource")?
    } else {
        let low = match (block(low_format), pixel(low_format)) {
            _ if low_format < 0 || lw == 0 || lh == 0 => 0,
            (Some(b), _) => lw.div_ceil(4) * lh.div_ceil(4) * b,
            (_, Some(b)) => lw * lh * b,
            _ => bail!("VTF low-res {} not supported", format_name(low_format)),
        };
        header + low
    };
    Ok(Info { version, w, h, flags, frames, first_frame, reflectivity, format, mips, depth, faces, data_off })
}

impl Info {
    pub fn dims(&self, m: usize) -> (usize, usize) {
        ((self.w >> m).max(1), (self.h >> m).max(1))
    }

    pub fn image_size(&self, m: usize) -> Result<usize> {
        let (w, h) = self.dims(m);
        let d = (self.depth >> m).max(1);
        Ok(d * match (block(self.format), pixel(self.format)) {
            (Some(b), _) => w.div_ceil(4) * h.div_ceil(4) * b,
            (_, Some(b)) => w * h * b,
            _ => bail!("VTF {} not supported", format_name(self.format)),
        })
    }

    pub fn mip_size(&self, m: usize) -> Result<usize> {
        Ok(self.image_size(m)? * self.frames * self.faces)
    }

    pub fn offset(&self, m: usize) -> Result<usize> {
        let mut p = self.data_off;
        for k in (m + 1..self.mips).rev() {
            p += self.mip_size(k)?;
        }
        Ok(p)
    }

    pub fn pick(&self, max_dim: usize) -> usize {
        let mut m = 0;
        while m + 1 < self.mips && self.dims(m).0.max(self.dims(m).1) > max_dim {
            m += 1;
        }
        m
    }

    pub fn end(&self) -> Result<usize> {
        Ok(self.offset(0)? + self.mip_size(0)?)
    }

    pub fn face<'a>(&self, d: &'a [u8], m: usize, frame: usize, face: usize) -> Result<&'a [u8]> {
        let each = self.image_size(m)?;
        let o = self.offset(m)? + (frame.min(self.frames - 1) * self.faces + face.min(self.faces - 1)) * each;
        d.get(o..o + each).with_context(|| format!("VTF mip {m} truncated ({} of {} bytes)", d.len(), o + each))
    }
}

fn half(v: u16) -> f32 {
    crate::s2::buf::half(v)
}

pub fn decode(format: i32, w: usize, h: usize, raw: &[u8]) -> Result<Vec<u8>> {
    let n = w * h;
    let mut out = vec![0u8; n * 4];
    if let Some(bb) = block(format) {
        let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
        ensure!(raw.len() >= bw * bh * bb, "mip data short");
        let mut px = [0u8; 64];
        for by in 0..bh {
            for bx in 0..bw {
                let b = &raw[(by * bw + bx) * bb..(by * bw + bx + 1) * bb];
                match format {
                    DXT1 | DXT1_ONEBITALPHA => bcdec_rs::bc1(b, &mut px, 16),
                    DXT3 => bcdec_rs::bc2(b, &mut px, 16),
                    _ => bcdec_rs::bc3(b, &mut px, 16),
                }
                for y in 0..4.min(h - by * 4) {
                    for x in 0..4.min(w - bx * 4) {
                        let d = ((by * 4 + y) * w + bx * 4 + x) * 4;
                        out[d..d + 4].copy_from_slice(&px[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4]);
                    }
                }
            }
        }
        return Ok(out);
    }
    let bpp = pixel(format).with_context(|| format!("VTF {} not supported", format_name(format)))?;
    ensure!(raw.len() >= n * bpp, "mip data short");
    let tone = |v: f32| ((v.clamp(0.0, 1.0)).powf(1.0 / 2.2) * 255.0 + 0.5) as u8;
    for (i, d) in out.chunks_exact_mut(4).enumerate() {
        let s = &raw[i * bpp..(i + 1) * bpp];
        let u16_ = |k: usize| u16::from_le_bytes([s[k * 2], s[k * 2 + 1]]);
        let c5 = |v: u16| ((v as u32 * 255 + 15) / 31) as u8;
        let c6 = |v: u16| ((v as u32 * 255 + 31) / 63) as u8;
        let c4 = |v: u16| (v as u8 & 15) * 17;
        let px: [u8; 4] = match format {
            RGBA8888 | UVWQ8888 | UVLX8888 => [s[0], s[1], s[2], s[3]],
            ABGR8888 => [s[3], s[2], s[1], s[0]],
            ARGB8888 => [s[1], s[2], s[3], s[0]],
            BGRA8888 => [s[2], s[1], s[0], s[3]],
            BGRX8888 => [s[2], s[1], s[0], 255],
            RGB888 | RGB888_BLUESCREEN => [s[0], s[1], s[2], 255],
            BGR888 | BGR888_BLUESCREEN => [s[2], s[1], s[0], 255],
            I8 | 7 => [s[0], s[0], s[0], 255],
            A8 => [255, 255, 255, s[0]],
            IA88 => [s[0], s[0], s[0], s[1]],
            UV88 => [s[0], s[1], 0, 255],
            RGB565 => {
                let v = u16_(0);
                [c5(v & 31), c6((v >> 5) & 63), c5(v >> 11), 255]
            }
            BGR565 => {
                let v = u16_(0);
                [c5(v >> 11), c6((v >> 5) & 63), c5(v & 31), 255]
            }
            BGRX5551 | BGRA5551 => {
                let v = u16_(0);
                let a = if format == BGRA5551 && v & 0x8000 == 0 { 0 } else { 255 };
                [c5((v >> 10) & 31), c5((v >> 5) & 31), c5(v & 31), a]
            }
            BGRA4444 => {
                let v = u16_(0);
                [c4(v >> 8), c4(v >> 4), c4(v), c4(v >> 12)]
            }
            RGBA16161616F => [tone(half(u16_(0))), tone(half(u16_(1))), tone(half(u16_(2))), (half(u16_(3)).clamp(0.0, 1.0) * 255.0) as u8],
            RGBA16161616 => [(u16_(0) >> 8) as u8, (u16_(1) >> 8) as u8, (u16_(2) >> 8) as u8, (u16_(3) >> 8) as u8],
            f => bail!("VTF {} not supported", format_name(f)),
        };
        d.copy_from_slice(&px);
    }
    Ok(out)
}

pub fn texture(d: &[u8], max_dim: usize) -> Result<Tex> {
    let inf = info(d)?;
    let m = inf.pick(max_dim);
    let (w, h) = inf.dims(m);
    let bc = match inf.format {
        DXT1 | DXT1_ONEBITALPHA => Some(1),
        DXT5 => Some(2),
        _ => None,
    };
    if let Some(format) = bc.filter(|_| w % 4 == 0 && h % 4 == 0 && inf.depth == 1) {
        let levels = (m..inf.mips).map(|k| inf.face(d, k, 0, 0).map(<[u8]>::to_vec)).collect::<Result<Vec<_>>>()?;
        return Ok(Tex::Bc(Compressed { format, w, h, levels }));
    }
    Ok(Tex::Plain(Image { w, h, rgba: decode(inf.format, w, h, inf.face(d, m, 0, 0)?)? }))
}

pub fn image(d: &[u8], max_dim: usize) -> Result<Image> {
    let inf = info(d)?;
    let m = inf.pick(max_dim);
    let (w, h) = inf.dims(m);
    Ok(Image { w, h, rgba: decode(inf.format, w, h, inf.face(d, m, 0, 0)?)? })
}

pub fn load_tex(fs: &Fs, name: &str, max_dim: usize) -> Result<Tex> {
    texture(&fs.read(name)?, max_dim).with_context(|| name.to_string())
}

pub fn load(fs: &Fs, name: &str, max_dim: usize) -> Result<Image> {
    image(&fs.read(name)?, max_dim).with_context(|| name.to_string())
}

pub fn path(tex: &str) -> String {
    let t = super::fs::norm(tex);
    let t = t.strip_suffix(".vtf").unwrap_or(&t);
    let t = t.strip_prefix("materials/").unwrap_or(t);
    format!("materials/{t}.vtf")
}
