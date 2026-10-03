use anyhow::{Context, Result, bail, ensure};

use super::buf::{half, slice, u16_, u32_};
use super::vpk::Fs;
use rayon::prelude::*;

pub const HEAD: usize = 1 << 16;
const CUBE: u16 = 0x10;

pub struct Info {
    pub w: usize,
    pub h: usize,
    pub depth: usize,
    pub format: u8,
    pub mips: usize,
    pub flags: u16,
    compressed: Option<Vec<usize>>,
    data_off: usize,
}

pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

#[derive(Clone)]
pub struct Compressed {
    pub format: u8,
    pub w: usize,
    pub h: usize,
    pub levels: Vec<Vec<u8>>,
}

impl Compressed {
    pub fn decode(&self) -> Result<Image> {
        Ok(Image { w: self.w, h: self.h, rgba: decode(self.format, self.w, self.h, &self.levels[0])? })
    }
}

pub enum Tex {
    Plain(Image),
    Bc(Compressed),
}

pub struct Hdr {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<f32>,
}

fn data_block(d: &[u8]) -> Result<(usize, usize)> {
    let boff = u32_(d, 8)? as usize;
    let n = u32_(d, 12)? as usize;
    for i in 0..n {
        let p = 8 + boff + i * 12;
        if slice(d, p, 4)? == b"DATA" {
            return Ok((p + 4 + u32_(d, p + 4)? as usize, u32_(d, p + 8)? as usize));
        }
    }
    bail!("texture has no DATA block")
}

pub fn info(head: &[u8]) -> Result<Info> {
    let (o, s) = data_block(head)?;
    let d = slice(head, o, s)?;
    let v = u16_(d, 0)?;
    ensure!(v == 1, "vtex version {v}");
    let flags = u16_(d, 2)?;
    let (w, h, depth) = (u16_(d, 20)? as usize, u16_(d, 22)? as usize, u16_(d, 24)? as usize);
    let format = d[26];
    let mips = (d[27] as usize).max(1);
    let ex_off = u32_(d, 32)? as usize;
    let ex_n = u32_(d, 36)? as usize;
    let mut compressed = None;
    let mut p = 32 + ex_off;
    for _ in 0..ex_n {
        let t = u32_(d, p)?;
        let eo = p + 4 + u32_(d, p + 4)? as usize;
        if t == 4 && u32_(d, eo)? == 1 {
            let moff = eo + 4 + u32_(d, eo + 4)? as usize;
            let n = u32_(d, eo + 8)? as usize;
            compressed = Some((0..n).map(|i| u32_(d, moff + i * 4).map(|v| v as usize)).collect::<Result<Vec<_>>>()?);
        }
        p += 12;
    }
    ensure!(w > 0 && h > 0, "empty texture");
    Ok(Info { w, h, depth: depth.max(1), format, mips, flags, compressed, data_off: o + s })
}

fn block_bytes(f: u8) -> Option<usize> {
    match f {
        1 | 27 => Some(8),
        2 | 19 | 20 | 21 => Some(16),
        _ => None,
    }
}

fn pixel_bytes(f: u8) -> Option<usize> {
    match f {
        3 => Some(1),
        4 | 28 => Some(4),
        10 => Some(8),
        13 => Some(12),
        _ => None,
    }
}

impl Info {
    fn dims(&self, m: usize) -> (usize, usize) {
        ((self.w >> m).max(1), (self.h >> m).max(1))
    }

    fn layers(&self) -> usize {
        self.depth * if self.flags & CUBE != 0 { 6 } else { 1 }
    }

    fn mip_size(&self, m: usize) -> Result<usize> {
        let (w, h) = self.dims(m);
        Ok(self.layers()
            * match (block_bytes(self.format), pixel_bytes(self.format)) {
                (Some(b), _) => w.div_ceil(4) * h.div_ceil(4) * b,
                (_, Some(b)) => w * h * b,
                _ => bail!("texture format {} not supported", self.format),
            })
    }

    fn stored(&self, m: usize) -> Result<usize> {
        let full = self.mip_size(m)?;
        Ok(self.compressed.as_ref().and_then(|c| c.get(m).copied()).map_or(full, |c| c.min(full)))
    }

    pub fn pick(&self, max_dim: usize) -> usize {
        let mut m = 0;
        while m + 1 < self.mips && self.dims(m).0.max(self.dims(m).1) > max_dim {
            m += 1;
        }
        m
    }

    fn range(&self, m: usize) -> Result<(usize, usize)> {
        let mut p = self.data_off;
        for k in (m + 1..self.mips).rev() {
            p += self.stored(k)?;
        }
        Ok((p, p + self.stored(m)?))
    }
}

fn mip_bytes(fs: &Fs, name: &str, max_dim: usize) -> Result<(Info, usize, Vec<u8>)> {
    let head = fs.read_limit(name, HEAD)?;
    let inf = info(&head)?;
    let m = inf.pick(max_dim);
    let (_, b) = inf.range(m)?;
    let full = if b <= head.len() { head } else { fs.read_limit(name, b)? };
    let raw = mip(&inf, &full, m)?;
    Ok((inf, m, raw))
}

fn mip(inf: &Info, full: &[u8], m: usize) -> Result<Vec<u8>> {
    let (a, b) = inf.range(m)?;
    let src = slice(full, a, b - a).context("mip data truncated")?;
    let size = inf.mip_size(m)?;
    Ok(if src.len() < size {
        lz4_flex::block::decompress(src, size).map_err(|e| anyhow::anyhow!("lz4 mip: {e}"))?
    } else {
        src.to_vec()
    })
}

pub fn load_tex(fs: &Fs, name: &str, max_dim: usize) -> Result<Tex> {
    let head = fs.read_limit(name, HEAD)?;
    let inf = info(&head)?;
    let m = inf.pick(max_dim);
    let (_, b) = inf.range(m)?;
    let full = if b <= head.len() { head } else { fs.read_limit(name, b)? };
    let (w, h) = inf.dims(m);
    if matches!(inf.format, 1 | 2 | 20) && inf.layers() == 1 && w % 4 == 0 && h % 4 == 0 {
        let levels = (m..inf.mips).map(|k| mip(&inf, &full, k)).collect::<Result<Vec<_>>>()?;
        return Ok(Tex::Bc(Compressed { format: inf.format, w, h, levels }));
    }
    Ok(Tex::Plain(Image { w, h, rgba: decode(inf.format, w, h, &mip(&inf, &full, m)?)? }))
}

fn blocks<T: Copy + Default + Send>(
    w: usize,
    h: usize,
    bb: usize,
    ch: usize,
    raw: &[u8],
    out: &mut [T],
    dec: impl Fn(&[u8], &mut [T]) + Sync,
) -> Result<()> {
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    ensure!(raw.len() >= bw * bh * bb, "mip data short");
    out.par_chunks_mut(w * ch * 4).enumerate().for_each(|(by, stripe)| {
        let rows = stripe.len() / (w * ch);
        let mut px = vec![T::default(); 16 * ch];
        for bx in 0..bw {
            let i = by * bw + bx;
            dec(&raw[i * bb..(i + 1) * bb], &mut px);
            for yy in 0..rows {
                for xx in 0..4.min(w - bx * 4) {
                    let (s, d) = ((yy * 4 + xx) * ch, (yy * w + bx * 4 + xx) * ch);
                    stripe[d..d + ch].copy_from_slice(&px[s..s + ch]);
                }
            }
        }
    });
    Ok(())
}

pub fn decode_bytes(full: &[u8], max_dim: usize) -> Result<Image> {
    let inf = info(full)?;
    let m = inf.pick(max_dim);
    let (w, h) = inf.dims(m);
    Ok(Image { w, h, rgba: decode(inf.format, w, h, &mip(&inf, full, m)?)? })
}

pub fn load(fs: &Fs, name: &str, max_dim: usize) -> Result<Image> {
    let (inf, m, raw) = mip_bytes(fs, name, max_dim)?;
    let (w, h) = inf.dims(m);
    Ok(Image { w, h, rgba: decode(inf.format, w, h, &raw)? })
}

pub struct Volume<T> {
    pub w: usize,
    pub h: usize,
    pub d: usize,
    pub data: Vec<T>,
}

fn slices(fs: &Fs, name: &str) -> Result<(Info, usize, usize, Vec<Vec<u8>>)> {
    let (inf, m, raw) = mip_bytes(fs, name, usize::MAX)?;
    let (w, h) = inf.dims(m);
    let n = inf.layers();
    let each = inf.mip_size(m)? / n;
    ensure!(raw.len() >= each * n, "volume data short");
    Ok((inf, w, h, raw.chunks_exact(each).take(n).map(|c| c.to_vec()).collect()))
}

pub fn load_volume(fs: &Fs, name: &str) -> Result<Volume<u8>> {
    let (inf, w, h, sl) = slices(fs, name)?;
    let d = sl.len();
    let mut data = Vec::with_capacity(w * h * d * 4);
    for s in &sl {
        data.extend(decode(inf.format, w, h, s)?);
    }
    Ok(Volume { w, h, d, data })
}

pub fn load_volume_hdr(fs: &Fs, name: &str) -> Result<Volume<u32>> {
    let (inf, w, h, sl) = slices(fs, name)?;
    let d = sl.len();
    let parts: Vec<Result<Vec<u32>>> = {
        use rayon::prelude::*;
        sl.par_iter()
            .map(|s| hdr_decode(&inf, w, h, s).map(|x| x.rgb.chunks_exact(3).map(|c| crate::mesh::pack_rgb9e5([c[0], c[1], c[2]])).collect()))
            .collect()
    };
    let mut data = Vec::with_capacity(w * h * d);
    for p in parts {
        data.extend(p?);
    }
    Ok(Volume { w, h, d, data })
}

pub fn load_cube(fs: &Fs, name: &str, max_dim: usize) -> Result<(usize, Vec<Vec<f32>>)> {
    let (inf, m, raw) = mip_bytes(fs, name, max_dim)?;
    ensure!(inf.flags & CUBE != 0, "not a cube map");
    let (w, h) = inf.dims(m);
    let each = inf.mip_size(m)? / inf.layers();
    ensure!(raw.len() >= each * 6, "cube data short");
    let faces = raw
        .chunks_exact(each)
        .take(6)
        .map(|c| match inf.format {
            19 | 10 => hdr_decode(&inf, w, h, c).map(|x| x.rgb),
            _ => decode(inf.format, w, h, c).map(|px| {
                px.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]].map(|v| (v as f32 / 255.0).powf(2.2))).collect()
            }),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((w.min(h), faces))
}

fn decode(format: u8, w: usize, h: usize, raw: &[u8]) -> Result<Vec<u8>> {
    let mut rgba = vec![0u8; w * h * 4];
    match format {
        1 | 2 | 20 | 21 | 27 => blocks(w, h, block_bytes(format).unwrap(), 4, raw, &mut rgba, |b, out| {
            match format {
                1 => bcdec_rs::bc1(b, out, 16),
                2 => bcdec_rs::bc3(b, out, 16),
                20 => bcdec_rs::bc7(b, out, 16),
                21 => {
                    let mut o = [0u8; 32];
                    bcdec_rs::bc5(b, &mut o, 8, false);
                    for i in 0..16 {
                        out[i * 4..i * 4 + 4].copy_from_slice(&[o[i * 2], o[i * 2 + 1], 0, 255]);
                    }
                }
                _ => {
                    let mut o = [0u8; 16];
                    bcdec_rs::bc4(b, &mut o, 4, false);
                    for i in 0..16 {
                        out[i * 4..i * 4 + 4].copy_from_slice(&[o[i], o[i], o[i], 255]);
                    }
                }
            }
        })?,
        19 => {
            let hdr = hdr_bc6(w, h, raw)?;
            for (i, c) in hdr.rgb.chunks_exact(3).enumerate() {
                for k in 0..3 {
                    let v = c[k].max(0.0);
                    rgba[i * 4 + k] = ((v / (1.0 + v)).powf(1.0 / 2.2) * 255.0) as u8;
                }
                rgba[i * 4 + 3] = 255;
            }
        }
        4 => rgba.copy_from_slice(slice(raw, 0, w * h * 4)?),
        28 => {
            for (d, s) in rgba.chunks_exact_mut(4).zip(slice(raw, 0, w * h * 4)?.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        3 => {
            for (d, &s) in rgba.chunks_exact_mut(4).zip(slice(raw, 0, w * h)?) {
                d.copy_from_slice(&[s, s, s, 255]);
            }
        }
        f => bail!("texture format {f} not supported"),
    }
    Ok(rgba)
}

fn hdr_from(inf: &Info, m: usize, raw: &[u8]) -> Result<Hdr> {
    let (w, h) = inf.dims(m);
    hdr_decode(inf, w, h, raw)
}

fn hdr_bc6(w: usize, h: usize, raw: &[u8]) -> Result<Hdr> {
    let mut rgb = vec![0f32; w * h * 3];
    blocks(w, h, 16, 3, raw, &mut rgb, |b, out| bcdec_rs::bc6h_float(b, out, 12, false))?;
    Ok(Hdr { w, h, rgb })
}

fn hdr_decode(inf: &Info, w: usize, h: usize, raw: &[u8]) -> Result<Hdr> {
    match inf.format {
        19 => hdr_bc6(w, h, raw),
        10 => {
            let src = slice(raw, 0, w * h * 8)?;
            let mut rgb = vec![0f32; w * h * 3];
            for i in 0..w * h {
                for k in 0..3 {
                    rgb[i * 3 + k] = half(u16::from_le_bytes([src[i * 8 + k * 2], src[i * 8 + k * 2 + 1]]));
                }
            }
            Ok(Hdr { w, h, rgb })
        }
        f => bail!("HDR texture format {f} not supported"),
    }
}

pub fn load_hdr(fs: &Fs, name: &str, max_dim: usize) -> Result<Hdr> {
    let (inf, m, raw) = mip_bytes(fs, name, max_dim)?;
    hdr_from(&inf, m, &raw)
}
