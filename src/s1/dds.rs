#![allow(dead_code)]

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;

use crate::s2::buf::{slice, u32_};
use crate::s2::texture::Image;

const FOURCC: u32 = 0x4;
const ALPHA: u32 = 0x1;
const LUMINANCE: u32 = 0x20000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Masks { bits: u32, masks: [u32; 4] },
}

impl Format {
    fn block(self) -> Option<usize> {
        match self {
            Format::Bc1 | Format::Bc4 => Some(8),
            Format::Masks { .. } => None,
            _ => Some(16),
        }
    }

    pub fn size(self, w: usize, h: usize) -> usize {
        match (self, self.block()) {
            (_, Some(b)) => w.div_ceil(4) * h.div_ceil(4) * b,
            (Format::Masks { bits, .. }, None) => (w * bits as usize).div_ceil(8) * h,
            _ => 0,
        }
    }

    pub fn name(self) -> String {
        match self {
            Format::Bc1 => "DXT1".into(),
            Format::Bc2 => "DXT3".into(),
            Format::Bc3 => "DXT5".into(),
            Format::Bc4 => "BC4".into(),
            Format::Bc5 => "BC5".into(),
            Format::Bc7 => "BC7".into(),
            Format::Masks { bits, masks } => {
                let ch: String = masks.iter().zip("RGBA".chars()).filter(|(m, _)| **m != 0).map(|(_, c)| c).collect();
                format!("{ch}{bits}")
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Info {
    pub w: usize,
    pub h: usize,
    pub mips: usize,
    pub format: Format,
    pub data: usize,
}

impl Info {
    pub fn dims(&self, m: usize) -> (usize, usize) {
        ((self.w >> m).max(1), (self.h >> m).max(1))
    }

    pub fn offset(&self, m: usize) -> usize {
        self.data + (0..m).map(|k| self.dims(k)).map(|(w, h)| self.format.size(w, h)).sum::<usize>()
    }

    pub fn pick(&self, max_dim: usize) -> usize {
        (0..self.mips).find(|&m| self.dims(m).0.max(self.dims(m).1) <= max_dim).unwrap_or(self.mips - 1)
    }
}

fn dxgi(f: u32) -> Result<Format> {
    Ok(match f {
        70..=72 => Format::Bc1,
        73..=75 => Format::Bc2,
        76..=78 => Format::Bc3,
        79..=81 => Format::Bc4,
        82..=84 => Format::Bc5,
        97..=99 => Format::Bc7,
        27..=29 => Format::Masks { bits: 32, masks: [0xff, 0xff00, 0xff0000, 0xff000000] },
        87 | 90 | 91 => Format::Masks { bits: 32, masks: [0xff0000, 0xff00, 0xff, 0xff000000] },
        88 | 92 | 93 => Format::Masks { bits: 32, masks: [0xff0000, 0xff00, 0xff, 0] },
        _ => bail!("DDS DXGI format {f} not supported"),
    })
}

pub fn info(b: &[u8]) -> Result<Info> {
    ensure!(slice(b, 0, 4).ok() == Some(b"DDS "), "not a DDS file");
    ensure!(b.len() >= 128, "DDS header short ({} bytes)", b.len());
    let h = u32_(b, 12)? as usize;
    let w = u32_(b, 16)? as usize;
    let mips = (u32_(b, 28)? as usize).max(1);
    ensure!(w > 0 && h > 0 && w <= 1 << 16 && h <= 1 << 16, "DDS size {w}x{h}");
    ensure!(mips <= 17, "DDS mip count {mips}");
    let pf = u32_(b, 80)?;
    let four = slice(b, 84, 4)?;
    let mut data = 128;
    let format = if pf & FOURCC != 0 {
        match four {
            b"DXT1" => Format::Bc1,
            b"DXT2" | b"DXT3" => Format::Bc2,
            b"DXT4" | b"DXT5" => Format::Bc3,
            b"ATI1" | b"BC4U" => Format::Bc4,
            b"ATI2" | b"BC5U" => Format::Bc5,
            b"DX10" => {
                data = 148;
                dxgi(u32_(b, 128)?)?
            }
            _ => bail!("DDS format {:?} not supported", String::from_utf8_lossy(four)),
        }
    } else {
        let bits = u32_(b, 88)?;
        ensure!(matches!(bits, 8 | 16 | 24 | 32), "DDS {bits}-bit pixels not supported");
        let mut masks = [u32_(b, 92)?, u32_(b, 96)?, u32_(b, 100)?, u32_(b, 104)?];
        if pf & LUMINANCE != 0 {
            masks[1] = masks[0];
            masks[2] = masks[0];
        }
        if pf & ALPHA == 0 && pf & 0x2 == 0 {
            masks[3] = 0;
        }
        ensure!(masks[..3].iter().any(|&m| m != 0) || masks[3] != 0, "DDS pixel format has no channel masks");
        Format::Masks { bits, masks }
    };
    let inf = Info { w, h, mips, format, data };
    let top = format.size(w, h);
    ensure!(b.len() >= data + top, "DDS data short: {} bytes for {w}x{h} {} ({} needed)", b.len() - data.min(b.len()), format.name(), top);
    let mut have = mips;
    while have > 1 && inf.offset(have) > b.len() {
        have -= 1;
    }
    Ok(Info { mips: have, ..inf })
}

fn channel(px: u32, m: u32) -> u8 {
    if m == 0 {
        return 0;
    }
    let v = (px & m) >> m.trailing_zeros();
    let max = m >> m.trailing_zeros();
    if max == 255 { v as u8 } else { ((v as u64 * 255 + max as u64 / 2) / max as u64) as u8 }
}

fn unpack(bits: u32, masks: [u32; 4], w: usize, raw: &[u8], out: &mut [u8]) {
    let bpp = bits as usize / 8;
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let src = &raw[y * w * bpp..(y + 1) * w * bpp];
        for (d, s) in row.chunks_exact_mut(4).zip(src.chunks_exact(bpp)) {
            let mut px = [0u8; 4];
            px[..bpp].copy_from_slice(s);
            let v = u32::from_le_bytes(px);
            d[0] = channel(v, masks[0]);
            d[1] = channel(v, masks[1]);
            d[2] = channel(v, masks[2]);
            d[3] = if masks[3] == 0 { 255 } else { channel(v, masks[3]) };
        }
    });
}

fn blocks(f: Format, w: usize, raw: &[u8], out: &mut [u8]) {
    let bb = f.block().unwrap_or(16);
    let bw = w.div_ceil(4);
    out.par_chunks_mut(w * 4 * 4).enumerate().for_each(|(by, stripe)| {
        let rows = stripe.len() / (w * 4);
        let mut px = [0u8; 64];
        for bx in 0..bw {
            let o = (by * bw + bx) * bb;
            let b = &raw[o..o + bb];
            match f {
                Format::Bc1 => bcdec_rs::bc1(b, &mut px, 16),
                Format::Bc2 => bcdec_rs::bc2(b, &mut px, 16),
                Format::Bc3 => bcdec_rs::bc3(b, &mut px, 16),
                Format::Bc7 => bcdec_rs::bc7(b, &mut px, 16),
                Format::Bc4 => {
                    let mut o = [0u8; 16];
                    bcdec_rs::bc4(b, &mut o, 4, false);
                    for i in 0..16 {
                        px[i * 4..i * 4 + 4].copy_from_slice(&[o[i], o[i], o[i], 255]);
                    }
                }
                Format::Bc5 => {
                    let mut o = [0u8; 32];
                    bcdec_rs::bc5(b, &mut o, 8, false);
                    for i in 0..16 {
                        px[i * 4..i * 4 + 4].copy_from_slice(&[o[i * 2], o[i * 2 + 1], 0, 255]);
                    }
                }
                Format::Masks { .. } => {}
            }
            let cols = 4.min(w - bx * 4);
            for y in 0..rows {
                let d = (y * w + bx * 4) * 4;
                stripe[d..d + cols * 4].copy_from_slice(&px[y * 16..y * 16 + cols * 4]);
            }
        }
    });
}

pub fn decode(b: &[u8], max_dim: usize) -> Result<Image> {
    let inf = info(b)?;
    let m = inf.pick(max_dim);
    let (w, h) = inf.dims(m);
    let raw = slice(b, inf.offset(m), inf.format.size(w, h))?;
    let mut rgba = vec![0u8; w * h * 4];
    match inf.format {
        Format::Masks { bits, masks } => unpack(bits, masks, w, raw, &mut rgba),
        f => blocks(f, w, raw, &mut rgba),
    }
    Ok(Image { w, h, rgba })
}

pub fn read(path: &Path, max_dim: usize) -> Result<Image> {
    let b = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    decode(&b, max_dim).with_context(|| format!("decoding {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(w: u32, h: u32, mips: u32, pf: u32, four: &[u8; 4], bits: u32, masks: [u32; 4]) -> Vec<u8> {
        let mut b = b"DDS ".to_vec();
        let u = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        for v in [124, 0x1007, h, w, 0, 0, mips] {
            u(&mut b, v);
        }
        b.resize(76, 0);
        u(&mut b, 32);
        u(&mut b, pf);
        b.extend_from_slice(four);
        u(&mut b, bits);
        for m in masks {
            u(&mut b, m);
        }
        b.resize(128, 0);
        b
    }

    #[test]
    fn bgr_and_rgba_masks() {
        let mut b = header(3, 2, 0, 0x40, &[0; 4], 24, [0xff0000, 0xff00, 0xff, 0]);
        for i in 0..6u8 {
            b.extend_from_slice(&[i, 10 + i, 20 + i]);
        }
        let im = decode(&b, usize::MAX).unwrap();
        assert_eq!((im.w, im.h), (3, 2));
        assert_eq!(&im.rgba[4..8], &[21, 11, 1, 255]);
        let mut b = header(1, 1, 1, 0x41, &[0; 4], 32, [0xff, 0xff00, 0xff0000, 0xff000000]);
        b.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(decode(&b, usize::MAX).unwrap().rgba, vec![1, 2, 3, 4]);
    }

    #[test]
    fn dxt1_mips_and_odd_sizes() {
        let mut b = header(8, 8, 4, 0x4, b"DXT1", 0, [0; 4]);
        let white = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];
        let black = [0u8; 8];
        for _ in 0..4 {
            b.extend_from_slice(&white);
        }
        b.extend_from_slice(&black);
        b.extend_from_slice(&black);
        let inf = info(&b).unwrap();
        assert_eq!((inf.mips, inf.format), (3, Format::Bc1));
        let im = decode(&b, usize::MAX).unwrap();
        assert_eq!((im.w, im.h), (8, 8));
        assert!(im.rgba.chunks(4).all(|p| p == [255, 255, 255, 255]));
        let im = decode(&b, 4).unwrap();
        assert_eq!((im.w, im.h, im.rgba[0]), (4, 4, 0));
        let mut b = header(5, 3, 1, 0x4, b"DXT1", 0, [0; 4]);
        b.extend_from_slice(&white);
        b.extend_from_slice(&white);
        assert_eq!(decode(&b, usize::MAX).unwrap().rgba.len(), 5 * 3 * 4);
        b.truncate(b.len() - 1);
        assert!(decode(&b, usize::MAX).is_err());
    }

    #[test]
    #[ignore]
    fn real_radars() {
        let dir = Path::new(r"E:\steam\steamapps\common\Counter-Strike Global Offensive\csgo\resource\overviews");
        let out = std::env::var("DDS_PNG_OUT").ok();
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "dds")).collect();
        names.sort();
        let (mut ok, mut bad) = (0, 0);
        let mut kinds = std::collections::BTreeMap::new();
        for p in &names {
            let b = std::fs::read(p).unwrap();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            match (info(&b), decode(&b, usize::MAX)) {
                (Ok(inf), Ok(im)) => {
                    ok += 1;
                    *kinds.entry(format!("{} {}x{} mips {}", inf.format.name(), inf.w, inf.h, inf.mips)).or_insert(0) += 1;
                    let mean: u64 = im.rgba.chunks(4).map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64).sum::<u64>() / (im.w * im.h * 3) as u64;
                    let alpha = im.rgba.chunks(4).filter(|p| p[3] > 0).count() * 100 / (im.w * im.h);
                    println!("{name}: {} {}x{} mips {} mean {mean} alpha>0 {alpha}%", inf.format.name(), im.w, im.h, inf.mips);
                    if let Some(o) = &out {
                        image::RgbaImage::from_raw(im.w as u32, im.h as u32, im.rgba).unwrap().save(Path::new(o).join(name.replace(".dds", ".png"))).unwrap();
                    }
                }
                (a, b) => {
                    bad += 1;
                    println!("{name}: ERR {:?} {:?}", a.err(), b.err());
                }
            }
        }
        for (k, n) in &kinds {
            println!("{n:4} {k}");
        }
        println!("{ok} decoded, {bad} failed");
        assert_eq!(bad, 0);
    }
}
