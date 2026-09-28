use anyhow::Result;
use glam::DVec3;

use super::bsp::{LUMP_DISP_VERTS, LUMP_DISPINFO, Map, World};

pub const INFO_SIZE: usize = 176;
pub const VERT_SIZE: usize = 20;
pub const NO_PHYSICS: i32 = 0x2;
pub const NO_HULL: i32 = 0x4;
pub const NO_RAY: i32 = 0x8;

pub struct Disp {
    pub index: usize,
    pub face: usize,
    pub power: u32,
    pub flags: i32,
    pub contents: i32,
    pub pos: Vec<DVec3>,
    pub alpha: Vec<f32>,
    pub tris: Vec<[u32; 3]>,
}

struct Info {
    start: DVec3,
    vert: usize,
    power: i32,
    flags: i32,
    contents: i32,
    face: usize,
}

pub fn grid(power: u32) -> usize {
    (1 << power) + 1
}

pub fn triangles(n: usize) -> Vec<[u32; 3]> {
    let mut out = Vec::with_capacity((n - 1) * (n - 1) * 2);
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let a = (y * n + x) as u32;
            let (b, c, d) = (a + 1, a + n as u32, a + n as u32 + 1);
            if (x + y) % 2 == 0 {
                out.extend([[a, c, b], [b, c, d]]);
            } else {
                out.extend([[a, c, d], [a, d, b]]);
            }
        }
    }
    out
}

pub fn build(map: &Map, w: &World) -> Result<Vec<Disp>> {
    if !map.has(LUMP_DISPINFO) {
        return Ok(Vec::new());
    }
    let infos = map.read(LUMP_DISPINFO, INFO_SIZE, |r, o| Info {
        start: DVec3::from(r.vec3(o).map(|v| v as f64)),
        vert: r.i32(o + 12).max(0) as usize,
        power: r.i32(o + 20),
        flags: r.i32(o + 24),
        contents: r.i32(o + 32),
        face: r.u16(o + 36) as usize,
    })?;
    let verts = map.read(LUMP_DISP_VERTS, VERT_SIZE, |r, o| (DVec3::from(r.vec3(o).map(|v| v as f64)), r.f32(o + 12) as f64, r.f32(o + 16)))?;
    let mut out = Vec::with_capacity(infos.len());
    for (index, d) in infos.iter().enumerate() {
        if !(1..=4).contains(&d.power) || d.face >= w.faces.len() {
            continue;
        }
        let power = d.power as u32;
        let n = grid(power);
        let pts = w.face_points(d.face);
        let Some(vs) = verts.get(d.vert..d.vert + n * n).filter(|_| pts.len() == 4) else { continue };
        let s = (0..4).min_by(|&a, &b| pts[a].distance_squared(d.start).total_cmp(&pts[b].distance_squared(d.start))).unwrap_or(0);
        let p: [DVec3; 4] = std::array::from_fn(|i| pts[(s + i) % 4]);
        let mut pos = Vec::with_capacity(n * n);
        let mut alpha = Vec::with_capacity(n * n);
        let k = (n - 1) as f64;
        for y in 0..n {
            let t = y as f64 / k;
            let (l, r) = (p[0].lerp(p[1], t), p[3].lerp(p[2], t));
            for x in 0..n {
                let v = vs[y * n + x];
                pos.push(l.lerp(r, x as f64 / k) + v.0 * v.1);
                alpha.push(v.2);
            }
        }
        let mut tris = triangles(n);
        if (p[1] - p[0]).cross(p[3] - p[0]).dot(w.face_normal(d.face)) < 0.0 {
            for t in &mut tris {
                t.swap(1, 2);
            }
        }
        out.push(Disp { index, face: d.face, power, flags: d.flags, contents: d.contents, pos, alpha, tris });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winding_is_consistent() {
        let n = grid(2);
        let p: Vec<DVec3> = (0..n * n).map(|i| DVec3::new((i % n) as f64, (i / n) as f64, 0.0)).collect();
        let tris = triangles(n);
        assert_eq!(tris.len(), 32);
        let z: Vec<f64> = tris.iter().map(|t| (p[t[1] as usize] - p[t[0] as usize]).cross(p[t[2] as usize] - p[t[0] as usize]).z).collect();
        assert!(z.iter().all(|&v| v < 0.0));
    }
}
