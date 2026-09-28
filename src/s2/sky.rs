use anyhow::{Context, Result};
use glam::Vec3;
use rayon::prelude::*;

use super::kv3::Value;
use super::resource::Res;
use super::texture;
use super::vpk::Fs;
use crate::light::LightParams;

const CUBE_MAX: usize = 1024;
const SIZE: usize = 512;

fn param<'a>(d: &'a Value, list: &str, key: &str, name: &str) -> Option<&'a Value> {
    d.get(list).arr().iter().find(|p| p.get("m_name").s() == name).map(|p| p.get(key))
}

pub fn resolve(fs: &Fs, name: &str) -> Option<String> {
    let n = name.trim().to_lowercase().replace('\\', "/");
    if n.is_empty() {
        return None;
    }
    let base = n.trim_end_matches(".vmat");
    [format!("{base}.vmat"), format!("materials/skybox/{base}.vmat")].into_iter().find(|c| fs.has(&format!("{c}_c")))
}

fn fetch(faces: &[Vec<f32>], n: usize, c: Vec3) -> [f32; 3] {
    let a = c.abs();
    let (f, sc, tc, ma) = if a.x >= a.y && a.x >= a.z {
        if c.x > 0.0 { (0, -c.z, -c.y, a.x) } else { (1, c.z, -c.y, a.x) }
    } else if a.y >= a.z {
        if c.y > 0.0 { (2, c.x, c.z, a.y) } else { (3, c.x, -c.z, a.y) }
    } else if c.z > 0.0 {
        (4, c.x, -c.y, a.z)
    } else {
        (5, -c.x, -c.y, a.z)
    };
    let u = ((sc / ma + 1.0) * 0.5 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
    let v = ((tc / ma + 1.0) * 0.5 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
    let (x0, y0) = (u.floor() as usize, v.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(n - 1), (y0 + 1).min(n - 1));
    let (fx, fy) = (u - x0 as f32, v - y0 as f32);
    let px = |x: usize, y: usize, k: usize| faces[f][(y * n + x) * 3 + k];
    std::array::from_fn(|k| {
        let top = px(x0, y0, k) * (1.0 - fx) + px(x1, y0, k) * fx;
        let bot = px(x0, y1, k) * (1.0 - fx) + px(x1, y1, k) * fx;
        top * (1.0 - fy) + bot * fy
    })
}

fn layer_dir(layer: usize, s: f32, t: f32) -> Vec3 {
    match layer {
        0 => Vec3::new(1.0, -s, t),
        1 => Vec3::new(-1.0, s, t),
        2 => Vec3::new(s, 1.0, t),
        3 => Vec3::new(-s, -1.0, t),
        4 => Vec3::new(-t, -s, 1.0),
        _ => Vec3::new(t, -s, -1.0),
    }
    .normalize()
}

pub fn load(fs: &Fs, material: &str, light: &LightParams, exposure: f32) -> Result<(u32, Vec<u8>)> {
    let d = Res::new(fs.read(&format!("{material}_c"))?)?.kv("DATA")?;
    let tex = param(&d, "m_textureParams", "m_pValue", "g_tSkyTexture").map(|v| v.s().to_lowercase()).context("no g_tSkyTexture")?;
    let bias = param(&d, "m_floatParams", "m_flValue", "g_flBrightnessExposureBias").map_or(0.0, |v| v.f() as f32);
    let (n, faces) = texture::load_cube(fs, &format!("{tex}_c"), CUBE_MAX)?;
    let k = exposure * 2f32.powf(bias);
    let lut: Vec<u8> = (0..4096).map(|i| light.light_to_screen(i as f64 / 1024.0)).collect();
    let mut out = vec![255u8; SIZE * SIZE * 4 * 6];
    out.par_chunks_mut(SIZE * 4).enumerate().for_each(|(row, px)| {
        let layer = row / SIZE;
        let y = row % SIZE;
        let t = 1.0 - 2.0 * (y as f32 + 0.5) / SIZE as f32;
        for x in 0..SIZE {
            let s = 2.0 * (x as f32 + 0.5) / SIZE as f32 - 1.0;
            let c = fetch(&faces, n, layer_dir(layer, s, t));
            for i in 0..3 {
                px[x * 4 + i] = lut[((c[i].max(0.0) * k * 1024.0) as usize).min(4095)];
            }
        }
    });
    Ok((SIZE as u32, out))
}
