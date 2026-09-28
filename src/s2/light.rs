use anyhow::{Context, Result};
use rayon::prelude::*;

use super::entities::Ent;
use super::mesh::Lightmap;
use super::texture;
use super::vpk::Fs;
use super::world::World;
use crate::light::LightParams;
use crate::mesh::{HdrLight, Image, PointLight, pack_rgb9e5};

pub const EXPOSURE: f32 = 0.5;
const PATCH: usize = 4;

pub struct Sun {
    pub color: [f32; 3],
    pub channel: Option<usize>,
    pub k: f32,
}

pub struct Baked {
    pub lightmap: Lightmap,
    pub hdr: HdrLight,
    pub info: String,
    pub size: (u32, u32),
}

fn nums(s: Option<&String>) -> Vec<f32> {
    s.map(|v| v.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default()
}

fn class(e: &Ent) -> &str {
    e.kv.get("classname").map_or("", |c| c.as_str())
}

fn channel(world: &World, e: &Ent) -> Option<usize> {
    let idx = nums(e.kv.get("bakedshadowindex")).first().map(|v| *v as usize)?;
    world.lighting.shadows.get(idx).map(|s| s.1 as usize).filter(|&c| c < 4)
}

pub fn kelvin(t: f32) -> [f32; 3] {
    let t = t.clamp(1000.0, 40000.0) / 100.0;
    let r = if t <= 66.0 { 255.0 } else { 329.699 * (t - 60.0).powf(-0.133_204_76) };
    let g = if t <= 66.0 { 99.470_8 * t.ln() - 161.119_57 } else { 288.122_16 * (t - 60.0).powf(-0.075_514_85) };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_73 * (t - 10.0).ln() - 305.044_8
    };
    [r, g, b].map(|v| v.clamp(0.0, 255.0) / 255.0)
}

fn light_color(e: &Ent) -> [f32; 3] {
    if nums(e.kv.get("colormode")).first() == Some(&1.0) {
        return kelvin(nums(e.kv.get("colortemperature")).first().copied().unwrap_or(6600.0));
    }
    let c = nums(e.kv.get("color"));
    if c.len() >= 3 { [c[0], c[1], c[2]].map(|v| v / 255.0) } else { [1.0; 3] }
}

pub fn sun(world: &World, ents: &[Ent]) -> Option<Sun> {
    let e = ents.iter().find(|e| class(e) == "light_environment")?;
    let b = nums(e.kv.get("brightness")).first().copied().unwrap_or(1.0);
    let s = nums(e.kv.get("brightnessscale")).first().copied().unwrap_or(1.0);
    let fwd = e.xf.r.x_axis;
    Some(Sun { color: light_color(e).map(|v| v * b * s), channel: channel(world, e), k: (-fwd.z as f32).max(0.0) })
}

pub fn lights(world: &World, ents: &[Ent]) -> Vec<PointLight> {
    let mut out = Vec::new();
    for e in ents {
        let c = class(e);
        if !c.starts_with("light_") || c == "light_environment" {
            continue;
        }
        if e.kv.get("enabled").is_some_and(|v| v == "0") {
            continue;
        }
        let Some(ch) = channel(world, e) else { continue };
        let b = nums(e.kv.get("brightness")).first().copied().unwrap_or(0.0);
        let s = nums(e.kv.get("brightnessscale")).first().copied().unwrap_or(1.0);
        let range = nums(e.kv.get("range")).first().copied().unwrap_or(512.0).max(1.0);
        let fwd = e.xf.r.x_axis;
        let outer = nums(e.kv.get("outerconeangle")).first().copied();
        let inner = nums(e.kv.get("innerconeangle")).first().copied();
        let (kind, cone) = match (c, outer) {
            ("light_omni2" | "light_omni", _) => (0, [0.0; 2]),
            (_, Some(o)) => {
                let i = inner.unwrap_or(o * 0.5).min(o);
                (2, [o.to_radians().cos(), i.to_radians().cos()])
            }
            _ => (1, [0.0; 2]),
        };
        let t = e.xf.t;
        out.push(PointLight {
            pos: [t.x as f32, t.y as f32, t.z as f32],
            range,
            color: light_color(e).map(|v| v * 2f32.powf(b) * s),
            channel: ch as u32,
            dir: [fwd.x as f32, fwd.y as f32, fwd.z as f32],
            kind,
            cone,
        });
    }
    out
}

fn find<'a>(world: &'a World, suffix: &str) -> Option<&'a str> {
    world.lighting.maps.iter().map(|s| s.as_str()).find(|s| s.to_lowercase().ends_with(suffix))
}

pub fn curve(light: &LightParams) -> [f32; 3] {
    let b = light.brightness;
    let g3 = if b <= 0.0 {
        0.125
    } else if b > 1.0 {
        0.05
    } else {
        0.125 - b * b * 0.075
    };
    let k = EXPOSURE as f64 * light.scale * if b > 1.0 { b } else { 1.0 };
    [k as f32, g3 as f32, (1.0 / light.gamma.min(3.0)) as f32]
}

pub fn bake(fs: &Fs, world: &World, ents: &[Ent], light: &LightParams, cap: usize) -> Result<Option<Baked>> {
    if !world.lighting.baked {
        return Ok(None);
    }
    let Some(irr_name) = find(world, "irradiance.vtex") else { return Ok(None) };
    let sun = sun(world, ents);
    let lamps = lights(world, ents);
    let shadow_name = find(world, "direct_light_shadows.vtex");
    let (irr, mask) = rayon::join(
        || texture::load_hdr(fs, &format!("{irr_name}_c"), cap),
        || shadow_name.map(|n| texture::load(fs, &format!("{n}_c"), cap)).transpose(),
    );
    let irr = irr.context("irradiance lightmap")?;
    let mask = mask.context("baked shadow lightmap")?;
    let (w, h) = (irr.w, irr.h);
    let sun_ch = sun.as_ref().and_then(|s| s.channel);
    let sun_add = match (&sun, &mask, sun_ch) {
        (Some(s), Some(_), Some(_)) => s.color.map(|c| c * s.k),
        _ => [0.0; 3],
    };
    let ch = sun_ch.unwrap_or(0);
    let lut: Vec<u8> = (0..4096).map(|i| light.light_to_screen(i as f64 / 1024.0)).collect();
    let to_byte = |v: f32| lut[((v * EXPOSURE * 1024.0) as usize).min(4095)];
    let th = h + PATCH;
    let mut rgba = vec![255u8; w * th * 4];
    let mut packed = vec![0u32; w * th];
    let sums: Vec<([f64; 3], usize)> = rgba[..w * h * 4]
        .par_chunks_mut(w * 4)
        .zip(packed[..w * h].par_chunks_mut(w))
        .enumerate()
        .map(|(y, (row, prow))| {
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
                let e: [f32; 3] = std::array::from_fn(|k| irr.rgb[i + k].max(0.0));
                prow[x] = pack_rgb9e5(e);
                let v: [f32; 3] = std::array::from_fn(|k| e[k] + sun_add[k] * s);
                if e.iter().any(|&c| c > 0.0) {
                    for k in 0..3 {
                        acc[k] += e[k] as f64;
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
    let avg_lit: [f32; 3] = std::array::from_fn(|k| avg[k] + sun_add[k] * 0.5);
    for px in rgba[w * h * 4..].chunks_exact_mut(4) {
        for k in 0..3 {
            px[k] = to_byte(avg_lit[k]);
        }
    }
    packed[w * h..].fill(pack_rgb9e5(avg));
    let shadows = mask.map(|m| {
        let mut px = m.rgba;
        px.resize(m.w * m.h * 4, 0);
        let pad = m.h * PATCH / h;
        px.extend(std::iter::repeat_n(0u8, m.w * pad.max(1) * 4));
        Image { w: m.w as u32, h: (m.h + pad.max(1)) as u32, rgba: px }
    });
    let s = world.lighting.uv_scale;
    let tht = th as f32;
    let info = format!(
        "baked lighting {w}x{h}, live sun {}, {} shadowed lights{}",
        match &sun {
            Some(s) if s.channel.is_some() => format!("{:.2} {:.2} {:.2}", s.color[0], s.color[1], s.color[2]),
            Some(_) => "baked".into(),
            None => "none".into(),
        },
        lamps.len(),
        if shadows.is_none() { ", no shadow masks" } else { "" }
    );
    Ok(Some(Baked {
        lightmap: Lightmap {
            atlas: Image { w: w as u32, h: th as u32, rgba },
            rect: [0.0, 0.0, s[0], s[1] * h as f32 / tht],
            flat: [0.5, (h as f32 + PATCH as f32 / 2.0) / tht],
        },
        hdr: HdrLight {
            w: w as u32,
            h: th as u32,
            irradiance: packed,
            shadows,
            sun: sun.as_ref().filter(|s| s.channel.is_some()).map_or([0.0; 3], |s| s.color),
            lights: lamps,
            curve: curve(light),
        },
        info,
        size: (w as u32, h as u32),
    }))
}
