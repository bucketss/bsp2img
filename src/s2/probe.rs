use anyhow::{Context, Result};
use glam::{DMat3, DVec3};

use super::entities::Ent;
use super::texture::{self, Volume};
use super::vpk::Fs;
use crate::mesh::unpack_rgb9e5;

const FACES: usize = 6;

struct Vol {
    inv: DMat3,
    t: DVec3,
    mins: DVec3,
    ext: DVec3,
    n: [usize; 3],
    off: [usize; 3],
    hand: i64,
    size: f64,
}

pub struct Probes {
    vols: Vec<Vol>,
    atlas: Volume<u32>,
    shd: Option<Volume<u8>>,
    slab: usize,
}

fn path(s: &str) -> String {
    s.to_lowercase().replace('\\', "/")
}

fn nums(e: &Ent, k: &str) -> Vec<f64> {
    e.kv.get(k).map(|v| v.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default()
}

fn num(e: &Ent, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| nums(e, k).first().copied())
}

fn v3(e: &Ent, k: &str) -> Option<DVec3> {
    let v = nums(e, k);
    (v.len() >= 3).then(|| DVec3::new(v[0], v[1], v[2]))
}

pub struct Sample {
    pub light: [f32; 3],
    pub mask: [u8; 4],
}

impl Probes {
    pub fn load(fs: &Fs, ents: &[Ent]) -> Result<Option<Probes>> {
        let list: Vec<&Ent> = ents
            .iter()
            .filter(|e| e.kv.get("classname").is_some_and(|c| c == "env_combined_light_probe_volume" || c == "env_light_probe_volume"))
            .filter(|e| e.kv.get("lightprobetexture").is_some_and(|t| !t.is_empty()))
            .collect();
        let Some(first) = list.first() else { return Ok(None) };
        let tex = path(&first.kv["lightprobetexture"]);
        let shd_name = first.kv.get("lightprobetexture_dlshd").filter(|t| !t.is_empty()).map(|t| path(t));
        let (atlas, shd) = rayon::join(
            || texture::load_volume_hdr(fs, &format!("{tex}_c")),
            || shd_name.map(|n| texture::load_volume(fs, &format!("{n}_c"))),
        );
        let atlas = atlas.context("light probe atlas")?;
        let shd = shd.transpose().ok().flatten();
        if atlas.d < FACES {
            return Ok(None);
        }
        let slab = atlas.d / FACES;
        let shd = shd.filter(|s| s.w == atlas.w && s.h == atlas.h && s.d >= slab);
        let mut vols = Vec::new();
        for e in list {
            if path(&e.kv["lightprobetexture"]) != tex {
                continue;
            }
            let (Some(mins), Some(maxs)) = (v3(e, "box_mins"), v3(e, "box_maxs")) else { continue };
            let size = |a: &str| num(e, &[a]).map(|v| v.max(1.0) as usize);
            let (Some(nx), Some(ny), Some(nz)) = (size("light_probe_size_x"), size("light_probe_size_y"), size("light_probe_size_z")) else {
                continue;
            };
            let off = [
                num(e, &["light_probe_atlas_x", "light_probe_atlas_X"]).unwrap_or(0.0) as usize,
                num(e, &["light_probe_atlas_y", "light_probe_atlas_Y"]).unwrap_or(0.0) as usize,
                num(e, &["light_probe_atlas_z", "light_probe_atlas_Z"]).unwrap_or(0.0) as usize,
            ];
            if off[0] + nx > atlas.w || off[1] + ny > atlas.h || off[2] + nz > slab {
                continue;
            }
            let ext = (maxs - mins).max(DVec3::splat(1e-3));
            vols.push(Vol {
                inv: e.xf.r.inverse(),
                t: e.xf.t,
                mins,
                ext,
                n: [nx, ny, nz],
                off,
                hand: num(e, &["handshake"]).unwrap_or(-1.0) as i64,
                size: ext.x * ext.y * ext.z,
            });
        }
        if vols.is_empty() {
            return Ok(None);
        }
        Ok(Some(Probes { vols, atlas, shd, slab }))
    }

    pub fn count(&self) -> usize {
        self.vols.len()
    }

    fn local(&self, v: &Vol, p: DVec3) -> DVec3 {
        (v.inv * (p - v.t) - v.mins) / v.ext
    }

    fn pick(&self, p: DVec3, hand: i64) -> Option<(&Vol, DVec3)> {
        if let Some(v) = self.vols.iter().find(|v| hand > 0 && v.hand == hand) {
            let l = self.local(v, p);
            if l.min_element() > -0.25 && l.max_element() < 1.25 {
                return Some((v, l));
            }
        }
        self.vols
            .iter()
            .map(|v| (v, self.local(v, p)))
            .filter(|(_, l)| l.min_element() >= 0.0 && l.max_element() <= 1.0)
            .min_by(|a, b| a.0.size.total_cmp(&b.0.size))
    }

    pub fn sample(&self, p: DVec3, n: DVec3, hand: i64) -> Option<Sample> {
        let (v, l) = self.pick(p, hand)?;
        let l = l.clamp(DVec3::ZERO, DVec3::ONE);
        let mut i0 = [0usize; 3];
        let mut f = [0.0f32; 3];
        for k in 0..3 {
            let x = (l[k] * v.n[k] as f64 - 0.5).clamp(0.0, (v.n[k] - 1) as f64);
            i0[k] = x.floor() as usize;
            f[k] = (x - x.floor()) as f32;
        }
        let a = &self.atlas;
        let mut taps = [(0usize, 0.0f32); 8];
        for (c, tap) in taps.iter_mut().enumerate() {
            let d = [c & 1, (c >> 1) & 1, (c >> 2) & 1];
            let mut idx = [0usize; 3];
            let mut w = 1.0;
            for k in 0..3 {
                idx[k] = v.off[k] + (i0[k] + d[k]).min(v.n[k] - 1);
                w *= if d[k] == 1 { f[k] } else { 1.0 - f[k] };
            }
            *tap = ((idx[2] * a.h + idx[1]) * a.w + idx[0], w);
        }
        let face = |s: usize| -> [f32; 3] {
            let base = s * self.slab * a.h * a.w;
            let mut c = [0.0f32; 3];
            for &(i, w) in &taps {
                let t = unpack_rgb9e5(a.data[base + i]);
                for k in 0..3 {
                    c[k] += t[k] * w;
                }
            }
            c
        };
        let wn = v.inv * n;
        let wn = wn.normalize_or(DVec3::Z);
        let sq = [wn.x * wn.x, wn.y * wn.y, wn.z * wn.z];
        let pick = [if wn.x >= 0.0 { 0 } else { 3 }, if wn.y >= 0.0 { 1 } else { 4 }, if wn.z >= 0.0 { 2 } else { 5 }];
        let mut light = [0.0f32; 3];
        for ax in 0..3 {
            if sq[ax] < 1e-6 {
                continue;
            }
            let c = face(pick[ax]);
            for k in 0..3 {
                light[k] += c[k] * sq[ax] as f32;
            }
        }
        let mask = match &self.shd {
            Some(s) => {
                let mut m = [0.0f32; 4];
                for &(i, w) in &taps {
                    for (k, c) in m.iter_mut().enumerate() {
                        *c += s.data[i * 4 + k] as f32 * w;
                    }
                }
                m.map(|x| x.round().clamp(0.0, 255.0) as u8)
            }
            None => [255; 4],
        };
        Some(Sample { light: light.map(|c| c.max(0.0)), mask })
    }
}
