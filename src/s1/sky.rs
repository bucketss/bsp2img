use anyhow::{Context, Result};

use super::{vmt, vtf};
use crate::s2::vpk::Fs;

pub const SUFFIXES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];
const SKY_MAX: usize = 1024;

pub fn face_texture(fs: &Fs, name: &str, suffix: &str) -> Result<String> {
    let m = vmt::load(fs, &format!("skybox/{name}{suffix}"))?;
    ["$basetexture", "$hdrcompressedtexture", "$hdrbasetexture"]
        .iter()
        .find_map(|k| m.texture(k).filter(|t| fs.has(t)))
        .with_context(|| format!("sky material {} has no texture", m.name))
}

pub fn load(fs: &Fs, name: &str) -> Result<(u32, Vec<u8>)> {
    let imgs: Vec<image::RgbaImage> = SUFFIXES
        .iter()
        .map(|s| {
            let t = face_texture(fs, name, s)?;
            let im = vtf::load(fs, &t, SKY_MAX)?;
            image::RgbaImage::from_raw(im.w as u32, im.h as u32, im.rgba).context("sky face size")
        })
        .collect::<Result<_>>()?;
    let size = imgs.iter().map(|im| im.width().max(im.height())).max().unwrap_or(1);
    let mut out = Vec::with_capacity((size * size * 4 * 6) as usize);
    for (k, im) in imgs.into_iter().enumerate() {
        let (w, h) = im.dimensions();
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
