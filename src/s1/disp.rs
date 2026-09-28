use std::collections::HashMap;

use anyhow::{Result, ensure};
use glam::DVec3;

use super::bsp::{LUMP_DISP_MULTIBLEND, LUMP_DISP_VERTS, LUMP_DISPINFO, Map, World};

pub const INFO_SIZE: usize = 176;
pub const VERT_SIZE: usize = 20;
pub const MULTIBLEND_SIZE: usize = 80;
const WELD: f64 = 8.0;

#[derive(Clone, Copy, Debug)]
pub struct DispInfo {
    pub start: DVec3,
    pub vert_start: usize,
    pub power: u32,
    pub flags: i32,
}

impl DispInfo {
    pub fn side(&self) -> usize {
        (1 << self.power) + 1
    }

    pub fn multiblend(&self) -> bool {
        (self.flags as u32).wrapping_add(0x8000_0000) & 0x4000_0000 != 0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DispVert {
    pub vec: DVec3,
    pub dist: f64,
    pub alpha: f32,
}

pub struct Disps {
    pub info: Vec<DispInfo>,
    pub verts: Vec<DispVert>,
    pub multiblend: usize,
}

pub fn read(map: &Map) -> Result<Disps> {
    let info = map.read(LUMP_DISPINFO, INFO_SIZE, |r, o| DispInfo {
        start: DVec3::from(r.vec3(o).map(|v| v as f64)),
        vert_start: r.i32(o + 12).max(0) as usize,
        power: r.i32(o + 20).clamp(0, 4) as u32,
        flags: r.i32(o + 24),
    })?;
    let verts = map.read(LUMP_DISP_VERTS, VERT_SIZE, |r, o| DispVert {
        vec: DVec3::from(r.vec3(o).map(|v| v as f64)),
        dist: r.f32(o + 12) as f64,
        alpha: r.f32(o + 16),
    })?;
    let mb = info.iter().filter(|d| d.multiblend());
    let (multiblend, multiblend_verts) = mb.fold((0, 0), |(n, v), d| (n + 1, v + d.side() * d.side()));
    if map.has(LUMP_DISP_MULTIBLEND) {
        let len = map.lump(LUMP_DISP_MULTIBLEND)?.len();
        ensure!(len == multiblend_verts * MULTIBLEND_SIZE, "multiblend lump is {len} bytes, expected {} x {MULTIBLEND_SIZE}", multiblend_verts);
    }
    Ok(Disps { info, verts, multiblend })
}

pub struct Grid {
    pub pos: Vec<DVec3>,
    pub flat: Vec<DVec3>,
    pub alpha: Vec<f32>,
    pub normal: Vec<DVec3>,
    pub frac: Vec<[f64; 2]>,
    pub idx: Vec<u32>,
}

pub fn grid(w: &World, d: &Disps, di: usize, fi: usize) -> Option<Grid> {
    let info = d.info.get(di)?;
    let pts = w.face_points(fi);
    if pts.len() != 4 {
        return None;
    }
    let s = (0..4).min_by(|&a, &b| pts[a].distance_squared(info.start).total_cmp(&pts[b].distance_squared(info.start)))?;
    let c: [DVec3; 4] = std::array::from_fn(|k| pts[(s + k) % 4]);
    let n = info.side();
    let src = d.verts.get(info.vert_start..info.vert_start + n * n)?;
    let step = 1.0 / (n - 1) as f64;
    let mut g = Grid { pos: Vec::with_capacity(n * n), flat: Vec::with_capacity(n * n), alpha: Vec::with_capacity(n * n), normal: vec![DVec3::ZERO; n * n], frac: Vec::with_capacity(n * n), idx: Vec::with_capacity((n - 1) * (n - 1) * 6) };
    for i in 0..n {
        let t = i as f64 * step;
        let (a, b) = (c[0].lerp(c[1], t), c[3].lerp(c[2], t));
        for j in 0..n {
            let u = j as f64 * step;
            let f = a.lerp(b, u);
            let v = &src[i * n + j];
            g.flat.push(f);
            g.pos.push(f + v.vec * v.dist);
            g.alpha.push(v.alpha);
            g.frac.push([u, t]);
        }
    }
    let flip = (c[1] - c[0]).cross(c[3] - c[0]).dot(w.face_normal(fi)) < 0.0;
    let mut tri = |a: usize, b: usize, cc: usize| {
        let (b, cc) = if flip { (cc, b) } else { (b, cc) };
        g.idx.extend([a as u32, b as u32, cc as u32]);
    };
    for i in 0..n - 1 {
        for j in 0..n - 1 {
            let a = i * n + j;
            if a % 2 == 1 {
                tri(a, a + n, a + 1);
                tri(a + 1, a + n, a + n + 1);
            } else {
                tri(a, a + n, a + n + 1);
                tri(a, a + n + 1, a + 1);
            }
        }
    }
    for t in g.idx.chunks_exact(3) {
        let (a, b, cc) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let nn = (g.pos[b] - g.pos[a]).cross(g.pos[cc] - g.pos[a]);
        for k in [a, b, cc] {
            g.normal[k] += nn;
        }
    }
    let base = w.face_normal(fi);
    for nn in &mut g.normal {
        *nn = nn.try_normalize().unwrap_or(base);
    }
    Some(g)
}

fn key(p: DVec3) -> [i64; 3] {
    [(p.x * WELD).round() as i64, (p.y * WELD).round() as i64, (p.z * WELD).round() as i64]
}

pub fn weld(grids: &mut [&mut Grid]) {
    let mut acc: HashMap<[i64; 3], (DVec3, u32)> = HashMap::new();
    for g in grids.iter() {
        for (p, n) in g.pos.iter().zip(&g.normal) {
            let e = acc.entry(key(*p)).or_insert((DVec3::ZERO, 0));
            e.0 += *n;
            e.1 += 1;
        }
    }
    for g in grids.iter_mut() {
        for (p, n) in g.pos.iter().zip(g.normal.iter_mut()) {
            if let Some((s, c)) = acc.get(&key(*p))
                && *c > 1
            {
                *n = s.try_normalize().unwrap_or(*n);
            }
        }
    }
}
