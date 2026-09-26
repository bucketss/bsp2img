use anyhow::{Context, Result};
use rayon::prelude::*;

use super::entities::Ent;
use super::mesh::Lightmap;
use super::texture;
use super::vpk::Fs;
use super::world::World;
use crate::light::LightParams;
use crate::mesh::Image;

const EXPOSURE: f32 = 0.5;
const PATCH: usize = 4;

pub struct Sun {
    pub color: [f32; 3],
    pub channel: Option<usize>,
    pub k: f32,
}

fn nums(s: Option<&String>) -> Vec<f32> {
    s.map(|v| v.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default()
}

pub fn sun(world: &World, ents: &[Ent]) -> Option<Sun> {
    let e = ents.iter().find(|e| e.kv.get("classname").is_some_and(|c| c == "light_environment"))?;
    let c = nums(e.kv.get("color"));
    let b = nums(e.kv.get("brightness")).first().copied().unwrap_or(1.0);
    let pitch = nums(e.kv.get("pitch")).first().copied().unwrap_or(-60.0);
    let idx = nums(e.kv.get("bakedshadowindex")).first().map(|v| *v as usize);
    let channel = idx.and_then(|i| world.lighting.shadows.get(i)).map(|s| s.1 as usize).filter(|&c| c < 4);
    let col = if c.len() >= 3 { [c[0], c[1], c[2]] } else { [255.0; 3] };
    Some(Sun { color: col.map(|v| v / 255.0 * b), channel, k: (-pitch).to_radians().sin().max(0.0) })
}

fn find<'a>(world: &'a World, suffix: &str) -> Option<&'a str> {
    world.lighting.maps.iter().map(|s| s.as_str()).find(|s| s.to_lowercase().ends_with(suffix))
}

pub fn bake(fs: &Fs, world: &World, ents: &[Ent], light: &LightParams, cap: usize) -> Result<Option<(Lightmap, String)>> {
    if !world.lighting.baked {
        return Ok(None);
    }
    let Some(irr_name) = find(world, "irradiance.vtex") else { return Ok(None) };
    let sun = sun(world, ents);
    let shadow_name = sun.as_ref().and_then(|s| s.channel).and(find(world, "direct_light_shadows.vtex"));
    let (irr, mask) = rayon::join(
        || texture::load_hdr(fs, &format!("{irr_name}_c"), cap),
        || shadow_name.map(|n| texture::load(fs, &format!("{n}_c"), cap)).transpose(),
    );
    let irr = irr.context("irradiance lightmap")?;
    let mask = mask.context("baked shadow lightmap")?;
    let (w, h) = (irr.w, irr.h);
    let sun_add = match (&sun, &mask) {
        (Some(s), Some(_)) => s.color.map(|c| c * s.k),
        _ => [0.0; 3],
    };
    let ch = sun.as_ref().and_then(|s| s.channel).unwrap_or(0);
    let lut: Vec<u8> = (0..4096).map(|i| light.light_to_screen(i as f64 / 1024.0)).collect();
    let to_byte = |v: f32| lut[((v * EXPOSURE * 1024.0) as usize).min(4095)];
    let mut rgba = vec![255u8; w * (h + PATCH) * 4];
    let sums: Vec<([f64; 3], usize)> = rgba[..w * h * 4]
        .par_chunks_mut(w * 4)
        .enumerate()
        .map(|(y, row)| {
            let mut acc = [0.0f64; 3];
            let mut n = 0;
            for x in 0..w {
                let s = match &mask {
                    Some(m) => {
                        let mx = (x * m.w / w).min(m.w - 1);
                        let my = (y * m.h / h).min(m.h - 1);
                        m.rgba[(my * m.w + mx) * 4 + ch] as f32 / 255.0
                    }
                    None => 0.0,
                };
                let i = (y * w + x) * 3;
                let v: [f32; 3] = std::array::from_fn(|k| irr.rgb[i + k].max(0.0) + sun_add[k] * s);
                if v.iter().any(|&c| c > 0.0) {
                    for k in 0..3 {
                        acc[k] += v[k] as f64;
                    }
                    n += 1;
                }
                for k in 0..3 {
                    row[x * 4 + k] = to_byte(v[k]);
                }
            }
            (acc, n)
        })
        .collect();
    let (acc, n) = sums.iter().fold(([0.0; 3], 0), |(a, n), (b, m)| ([a[0] + b[0], a[1] + b[1], a[2] + b[2]], n + m));
    let avg: [f32; 3] = std::array::from_fn(|k| (acc[k] / n.max(1) as f64) as f32);
    for px in rgba[w * h * 4..].chunks_exact_mut(4) {
        for k in 0..3 {
            px[k] = to_byte(avg[k]);
        }
    }
    let th = (h + PATCH) as f32;
    let s = world.lighting.uv_scale;
    let info = format!(
        "baked lighting {w}x{h}, sun {}",
        match (&sun, &mask) {
            (Some(s), Some(_)) => format!("shadow channel {}", s.channel.unwrap_or(0)),
            _ => "baked into irradiance".into(),
        }
    );
    Ok(Some((
        Lightmap {
            atlas: Image { w: w as u32, h: (h + PATCH) as u32, rgba },
            rect: [0.0, 0.0, s[0], s[1] * h as f32 / th],
            flat: [0.5, (h as f32 + PATCH as f32 / 2.0) / th],
        },
        info,
    )))
}
