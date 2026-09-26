use anyhow::{Context, Result, bail, ensure};

use super::buf::{half, slice, u16_, u32_};
use super::vpk::Fs;

const HEAD: usize = 1 << 16;
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
    let (a, b) = inf.range(m)?;
    let full = if b <= head.len() { head } else { fs.read_limit(name, b)? };
    let src = slice(&full, a, b - a).context("mip data truncated")?;
    let size = inf.mip_size(m)?;
    let raw = if src.len() < size {
        lz4_flex::block::decompress(src, size).map_err(|e| anyhow::anyhow!("lz4 mip: {e}"))?
    } else {
        src.to_vec()
    };
    Ok((inf, m, raw))
}

fn blocks(w: usize, h: usize, bb: usize, raw: &[u8], mut put: impl FnMut(usize, usize, &[u8]) -> Result<()>) -> Result<()> {
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    ensure!(raw.len() >= bw * bh * bb, "mip data short");
    for by in 0..bh {
        for bx in 0..bw {
            let i = by * bw + bx;
            put(bx, by, &raw[i * bb..(i + 1) * bb])?;
        }
    }
    Ok(())
}

pub fn load(fs: &Fs, name: &str, max_dim: usize) -> Result<Image> {
    let (inf, m, raw) = mip_bytes(fs, name, max_dim)?;
    let (w, h) = inf.dims(m);
    let mut rgba = vec![0u8; w * h * 4];
    let mut store = |bx: usize, by: usize, px: &[u8; 64]| {
        for yy in 0..4 {
            for xx in 0..4 {
                let (x, y) = (bx * 4 + xx, by * 4 + yy);
                if x < w && y < h {
                    let s = (yy * 4 + xx) * 4;
                    rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&px[s..s + 4]);
                }
            }
        }
    };
    match inf.format {
        1 | 2 | 20 | 21 | 27 => blocks(w, h, block_bytes(inf.format).unwrap(), &raw, |bx, by, b| {
            let mut out = [0u8; 64];
            match inf.format {
                1 => bcdec_rs::bc1(b, &mut out, 16),
                2 => bcdec_rs::bc3(b, &mut out, 16),
                20 => bcdec_rs::bc7(b, &mut out, 16),
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
            store(bx, by, &out);
            Ok(())
        })?,
        19 => {
            let hdr = hdr_from(&inf, m, &raw)?;
            for (i, c) in hdr.rgb.chunks_exact(3).enumerate() {
                for k in 0..3 {
                    let v = c[k].max(0.0);
                    rgba[i * 4 + k] = ((v / (1.0 + v)).powf(1.0 / 2.2) * 255.0) as u8;
                }
                rgba[i * 4 + 3] = 255;
            }
        }
        4 => rgba.copy_from_slice(slice(&raw, 0, w * h * 4)?),
        28 => {
            for (d, s) in rgba.chunks_exact_mut(4).zip(slice(&raw, 0, w * h * 4)?.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        3 => {
            for (d, &s) in rgba.chunks_exact_mut(4).zip(slice(&raw, 0, w * h)?) {
                d.copy_from_slice(&[s, s, s, 255]);
            }
        }
        f => bail!("texture format {f} not supported"),
    }
    Ok(Image { w, h, rgba })
}

fn hdr_from(inf: &Info, m: usize, raw: &[u8]) -> Result<Hdr> {
    let (w, h) = inf.dims(m);
    let mut rgb = vec![0f32; w * h * 3];
    match inf.format {
        19 => blocks(w, h, 16, raw, |bx, by, b| {
            let mut fo = [0f32; 48];
            bcdec_rs::bc6h_float(b, &mut fo, 12, false);
            for yy in 0..4 {
                for xx in 0..4 {
                    let (x, y) = (bx * 4 + xx, by * 4 + yy);
                    if x < w && y < h {
                        let s = (yy * 4 + xx) * 3;
                        rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&fo[s..s + 3]);
                    }
                }
            }
            Ok(())
        })?,
        10 => {
            let src = slice(raw, 0, w * h * 8)?;
            for i in 0..w * h {
                for k in 0..3 {
                    rgb[i * 3 + k] = half(u16::from_le_bytes([src[i * 8 + k * 2], src[i * 8 + k * 2 + 1]]));
                }
            }
        }
        f => bail!("HDR texture format {f} not supported"),
    }
    Ok(Hdr { w, h, rgb })
}

pub fn load_hdr(fs: &Fs, name: &str, max_dim: usize) -> Result<Hdr> {
    let (inf, m, raw) = mip_bytes(fs, name, max_dim)?;
    hdr_from(&inf, m, &raw)
}
