use anyhow::{Context, Result, bail, ensure};

use super::vmt::{self, Material};
use super::vtf;
use crate::s2::vpk::Fs;

pub const SUFFIXES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];
const SKY_MAX: usize = 1024;
const RGBS_SCALE: f32 = 16.0;
const WHITE_PCT: f64 = 0.99;
const GAMMA: f32 = 2.2;

enum Face {
    Ldr(image::RgbaImage),
    Hdr(u32, u32, Vec<[f32; 3]>),
}

fn material(fs: &Fs, name: &str, suffix: &str) -> Result<Material> {
    vmt::load(fs, &format!("skybox/{name}{suffix}"))
}

fn half(v: u16) -> f32 {
    crate::s2::buf::half(v)
}

fn hdr(d: &[u8], compressed: bool) -> Result<Face> {
    let inf = vtf::info(d)?;
    let m = inf.pick(SKY_MAX);
    let (w, h) = inf.dims(m);
    let raw = inf.face(d, m, 0, 0)?;
    let n = w * h;
    let px: Vec<[f32; 3]> = match inf.format {
        vtf::RGBA16161616F => {
            ensure!(raw.len() >= n * 8, "sky mip short");
            raw.chunks_exact(8).take(n).map(|s| std::array::from_fn(|k| half(u16::from_le_bytes([s[k * 2], s[k * 2 + 1]])).max(0.0))).collect()
        }
        f @ (vtf::BGRA8888 | vtf::RGBA8888) if compressed => {
            ensure!(raw.len() >= n * 4, "sky mip short");
            let order = if f == vtf::BGRA8888 { [2, 1, 0] } else { [0, 1, 2] };
            raw.chunks_exact(4).take(n).map(|s| order.map(|k| s[k] as f32 / 255.0 * s[3] as f32 / 255.0 * RGBS_SCALE)).collect()
        }
        f => bail!("HDR sky format {} not supported", vtf::format_name(f)),
    };
    Ok(Face::Hdr(w as u32, h as u32, px))
}

fn face(fs: &Fs, name: &str, suffix: &str) -> Result<Face> {
    let m = material(fs, name, suffix)?;
    for (k, compressed) in [("$hdrcompressedtexture", true), ("$hdrbasetexture", false)] {
        if let Some(Ok(f)) = m.texture(k).filter(|t| fs.has(t)).map(|t| fs.read(&t).and_then(|d| hdr(&d, compressed))) {
            return Ok(f);
        }
    }
    let t = m.texture("$basetexture").filter(|t| fs.has(t)).with_context(|| format!("sky material {} has no texture", m.name))?;
    let im = vtf::load(fs, &t, SKY_MAX)?;
    Ok(Face::Ldr(image::RgbaImage::from_raw(im.w as u32, im.h as u32, im.rgba).context("sky face size")?))
}

fn expose(faces: Vec<Face>) -> Vec<image::RgbaImage> {
    let mut lum: Vec<f32> = faces
        .iter()
        .flat_map(|f| match f {
            Face::Hdr(_, _, px) => px.iter().map(|c| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]).collect(),
            Face::Ldr(_) => Vec::new(),
        })
        .collect();
    let k = if lum.is_empty() {
        1.0
    } else {
        let i = ((lum.len() - 1) as f64 * WHITE_PCT) as usize;
        let (_, v, _) = lum.select_nth_unstable_by(i, f32::total_cmp);
        1.0 / v.max(1e-6)
    };
    faces
        .into_iter()
        .map(|f| match f {
            Face::Ldr(im) => im,
            Face::Hdr(w, h, px) => {
                let raw = px.iter().flat_map(|c| {
                    let g = c.map(|v| ((v * k).clamp(0.0, 1.0).powf(1.0 / GAMMA) * 255.0 + 0.5) as u8);
                    [g[0], g[1], g[2], 255]
                });
                image::RgbaImage::from_raw(w, h, raw.collect()).unwrap_or_default()
            }
        })
        .collect()
}

pub fn load(fs: &Fs, name: &str) -> Result<(u32, Vec<u8>)> {
    let faces: Vec<Face> = SUFFIXES.iter().map(|s| face(fs, name, s)).collect::<Result<_>>()?;
    let imgs = expose(faces);
    let size = imgs.iter().map(|im| im.width().max(im.height())).max().unwrap_or(1).max(1);
    let mut out = Vec::with_capacity((size * size * 4 * 6) as usize);
    for (k, im) in imgs.into_iter().enumerate() {
        let (w, h) = im.dimensions();
        ensure!(w > 0 && h > 0, "empty sky face");
        let rows = if k < 4 && h < w { (size * h / w).max(1) } else { size };
        let im = if (w, h) != (size, rows) { image::imageops::resize(&im, size, rows, image::imageops::FilterType::CatmullRom) } else { im };
        for y in 0..size {
            for x in 0..size {
                let p = im.get_pixel(x, y.min(rows - 1));
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
    }
    Ok((size, out))
}
