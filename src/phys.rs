use glam::{DAffine3, DVec3, Vec3};

pub const CELL: f64 = 32.0;
pub const SHEET: f32 = 8.0;
const TIE: f32 = 0.1;
const MAX_CELLS: f64 = 4_194_304.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Solid,
    Clip,
    Decor,
}

impl Class {
    fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Mask {
    pub solid: bool,
    pub clip: bool,
    pub decor: bool,
}

impl Mask {
    pub const SOLID: Mask = Mask { solid: true, clip: false, decor: false };
    #[allow(dead_code)]
    pub const PLAYER: Mask = Mask { solid: true, clip: true, decor: false };
    #[allow(dead_code)]
    pub const ALL: Mask = Mask { solid: true, clip: true, decor: true };

    pub fn has(self, c: Class) -> bool {
        match c {
            Class::Solid => self.solid,
            Class::Clip => self.clip,
            Class::Decor => self.decor,
        }
    }
}

pub struct Hull {
    pub planes: Vec<[f32; 4]>,
    pub lo: Vec3,
    pub hi: Vec3,
    pub class: Class,
}

pub struct Tri {
    pub p: [Vec3; 3],
    pub up: bool,
    pub class: Class,
    pub group: u32,
}

#[derive(Default, Clone, Debug)]
pub struct PhysStats {
    pub hulls: [usize; 3],
    pub tris: [usize; 3],
    pub skipped_hulls: usize,
    pub skipped_tris: usize,
    pub planes: usize,
    pub max_planes: usize,
    pub up: usize,
    pub down: usize,
    pub capsules: usize,
    pub spheres: usize,
    pub attrs: usize,
    pub parts: usize,
    pub groups: usize,
    pub ents: usize,
    pub cells: usize,
    pub refs: usize,
    pub secs: f64,
}

impl PhysStats {
    pub fn player_hulls(&self) -> usize {
        self.hulls[0] + self.hulls[1]
    }
    pub fn player_tris(&self) -> usize {
        self.tris[0] + self.tris[1]
    }
}

#[derive(Default)]
pub struct Builder {
    pub hulls: Vec<Hull>,
    pub tris: Vec<Tri>,
    pub stats: PhysStats,
    group: u32,
}

fn v3(p: DVec3) -> Vec3 {
    p.as_vec3()
}

impl Builder {
    pub fn hull(&mut self, planes: &[[f32; 4]], verts: &[Vec3], xf: &DAffine3, class: Class) {
        self.stats.hulls[class.idx()] += 1;
        self.stats.planes += planes.len();
        self.stats.max_planes = self.stats.max_planes.max(planes.len());
        if planes.is_empty() || verts.is_empty() {
            return;
        }
        let it = xf.matrix3.inverse().transpose();
        let planes = planes
            .iter()
            .filter_map(|p| {
                let n = it * DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
                let l = n.length();
                (l > 1e-12).then(|| {
                    let d = (p[3] as f64 + n.dot(xf.translation)) / l;
                    let n = n / l;
                    [n.x as f32, n.y as f32, n.z as f32, d as f32]
                })
            })
            .collect();
        let (mut lo, mut hi) = (Vec3::INFINITY, Vec3::NEG_INFINITY);
        for v in verts {
            let q = v3(xf.transform_point3(v.as_dvec3()));
            lo = lo.min(q);
            hi = hi.max(q);
        }
        self.hulls.push(Hull { planes, lo, hi, class });
    }

    pub fn aabb(&mut self, lo: DVec3, hi: DVec3, xf: &DAffine3, class: Class) {
        let planes = [
            [1.0, 0.0, 0.0, hi.x as f32],
            [-1.0, 0.0, 0.0, -lo.x as f32],
            [0.0, 1.0, 0.0, hi.y as f32],
            [0.0, -1.0, 0.0, -lo.y as f32],
            [0.0, 0.0, 1.0, hi.z as f32],
            [0.0, 0.0, -1.0, -lo.z as f32],
        ];
        let verts: Vec<Vec3> = (0..8)
            .map(|i| v3(DVec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z })))
            .collect();
        self.hull(&planes, &verts, xf, class);
    }

    pub fn mesh(&mut self, verts: &[Vec3], tris: &[[u32; 3]], xf: &DAffine3, class: Class) {
        self.stats.tris[class.idx()] += tris.len();
        let flip = xf.matrix3.determinant() < 0.0;
        let pts: Vec<Vec3> = verts.iter().map(|v| v3(xf.transform_point3(v.as_dvec3()))).collect();
        let group = self.group;
        self.group += 1;
        self.stats.groups += 1;
        let player = class != Class::Decor;
        for t in tris {
            let Some(p) = t.iter().map(|&i| pts.get(i as usize).copied()).collect::<Option<Vec<Vec3>>>() else { continue };
            let p = [p[0], p[1], p[2]];
            let (a, b, c) = (p[0].as_dvec3(), p[1].as_dvec3(), p[2].as_dvec3());
            let nz = (b - a).cross(c - a).z;
            if player {
                if nz > 0.0 {
                    self.stats.up += 1;
                } else if nz < 0.0 {
                    self.stats.down += 1;
                }
            }
            if nz == 0.0 {
                continue;
            }
            self.tris.push(Tri { p, up: (nz > 0.0) != flip, class, group });
        }
    }

    pub fn finish(self) -> Phys {
        Phys::index(self.hulls, self.tris, self.stats)
    }
}

pub struct Phys {
    hulls: Vec<Hull>,
    tris: Vec<Tri>,
    lo: [f64; 2],
    cell: f64,
    nx: usize,
    ny: usize,
    start: Vec<u32>,
    refs: Vec<u32>,
    stats: PhysStats,
}

struct Hit {
    group: u32,
    z: f32,
    up: bool,
}

fn tri_z(t: &Tri, x: f64, y: f64) -> Option<f32> {
    let [a, b, c] = t.p.map(|p| p.as_dvec3());
    let d = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
    if d.abs() < 1e-9 {
        return None;
    }
    let w0 = ((b.y - c.y) * (x - c.x) + (c.x - b.x) * (y - c.y)) / d;
    let w1 = ((c.y - a.y) * (x - c.x) + (a.x - c.x) * (y - c.y)) / d;
    let w2 = 1.0 - w0 - w1;
    let e = -1e-6;
    (w0 >= e && w1 >= e && w2 >= e).then_some((w0 * a.z + w1 * b.z + w2 * c.z) as f32)
}

fn hull_z(h: &Hull, x: f64, y: f64) -> Option<(f32, f32)> {
    let (mut a, mut b) = (f64::NEG_INFINITY, f64::INFINITY);
    for p in &h.planes {
        let (nx, ny, nz, d) = (p[0] as f64, p[1] as f64, p[2] as f64, p[3] as f64);
        let r = d - nx * x - ny * y;
        if nz.abs() < 1e-6 {
            if r < 0.0 {
                return None;
            }
        } else if nz > 0.0 {
            b = b.min(r / nz);
        } else {
            a = a.max(r / nz);
        }
        if a > b {
            return None;
        }
    }
    Some((a as f32, b as f32))
}

fn scan(hits: &mut [Hit], out: &mut Vec<(f32, f32)>) {
    hits.sort_unstable_by(|p, q| p.group.cmp(&q.group).then(q.z.total_cmp(&p.z)));
    let mut k = 0;
    while k < hits.len() {
        let g = hits[k].group;
        let mut entry: Option<f32> = None;
        while k < hits.len() && hits[k].group == g {
            let z = hits[k].z;
            let (mut up, mut down) = (false, false);
            while k < hits.len() && hits[k].group == g && z - hits[k].z < TIE {
                up |= hits[k].up;
                down |= !hits[k].up;
                k += 1;
            }
            match (up, down, entry) {
                (true, false, None) => entry = Some(z),
                (false, true, Some(e)) => {
                    out.push((z, e));
                    entry = None;
                }
                (false, true, None) => out.push((z - SHEET, z)),
                _ => {}
            }
        }
        if let Some(e) = entry {
            out.push((f32::NEG_INFINITY, e));
        }
    }
}

fn merge(out: &mut Vec<(f32, f32)>) {
    if out.len() < 2 {
        return;
    }
    out.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut w = 0;
    for r in 1..out.len() {
        if out[r].0 <= out[w].1 {
            out[w].1 = out[w].1.max(out[r].1);
        } else {
            w += 1;
            out[w] = out[r];
        }
    }
    out.truncate(w + 1);
}

impl Phys {
    fn index(hulls: Vec<Hull>, tris: Vec<Tri>, mut stats: PhysStats) -> Phys {
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        let mut grow = |a: Vec3, b: Vec3| {
            lo = [lo[0].min(a.x as f64), lo[1].min(a.y as f64)];
            hi = [hi[0].max(b.x as f64), hi[1].max(b.y as f64)];
        };
        for h in &hulls {
            grow(h.lo, h.hi);
        }
        let tri_box = |t: &Tri| (t.p[0].min(t.p[1]).min(t.p[2]), t.p[0].max(t.p[1]).max(t.p[2]));
        for t in &tris {
            let (a, b) = tri_box(t);
            grow(a, b);
        }
        if lo[0] > hi[0] {
            lo = [0.0; 2];
            hi = [0.0; 2];
        }
        let area = (hi[0] - lo[0] + CELL) * (hi[1] - lo[1] + CELL);
        let cell = CELL.max((area / MAX_CELLS).sqrt());
        let nx = ((hi[0] - lo[0]) / cell).floor() as usize + 1;
        let ny = ((hi[1] - lo[1]) / cell).floor() as usize + 1;
        let span = |a: Vec3, b: Vec3| {
            let c = |v: f32, o: f64, n: usize| (((v as f64 - o) / cell).floor().max(0.0) as usize).min(n - 1);
            (c(a.x, lo[0], nx), c(a.y, lo[1], ny), c(b.x, lo[0], nx), c(b.y, lo[1], ny))
        };
        let boxes: Vec<(Vec3, Vec3)> = tris.iter().map(tri_box).chain(hulls.iter().map(|h| (h.lo, h.hi))).collect();
        let mut start = vec![0u32; nx * ny + 1];
        for &(a, b) in &boxes {
            let (i0, j0, i1, j1) = span(a, b);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    start[i + nx * j + 1] += 1;
                }
            }
        }
        for k in 0..nx * ny {
            start[k + 1] += start[k];
        }
        let mut fill = start.clone();
        let mut refs = vec![0u32; start[nx * ny] as usize];
        for (r, &(a, b)) in boxes.iter().enumerate() {
            let (i0, j0, i1, j1) = span(a, b);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let c = &mut fill[i + nx * j];
                    refs[*c as usize] = r as u32;
                    *c += 1;
                }
            }
        }
        stats.cells = nx * ny;
        stats.refs = refs.len();
        Phys { hulls, tris, lo, cell, nx, ny, start, refs, stats }
    }

    pub fn stats(&self) -> &PhysStats {
        &self.stats
    }

    fn cell_of(&self, x: f64, y: f64) -> Option<usize> {
        let i = ((x - self.lo[0]) / self.cell).floor();
        let j = ((y - self.lo[1]) / self.cell).floor();
        (i >= 0.0 && j >= 0.0 && (i as usize) < self.nx && (j as usize) < self.ny).then(|| i as usize + self.nx * j as usize)
    }

    fn cell_refs(&self, c: usize) -> &[u32] {
        &self.refs[self.start[c] as usize..self.start[c + 1] as usize]
    }

    fn tri_hits(&self, c: usize, x: f64, y: f64, want: Mask, hits: &mut Vec<Hit>) {
        for &r in self.cell_refs(c) {
            let Some(t) = self.tris.get(r as usize) else { continue };
            if !want.has(t.class) {
                continue;
            }
            if let Some(z) = tri_z(t, x, y) {
                hits.push(Hit { group: t.group, z, up: t.up });
            }
        }
    }

    pub fn column(&self, x: f64, y: f64, want: Mask, out: &mut Vec<(f32, f32)>) {
        out.clear();
        let Some(c) = self.cell_of(x, y) else { return };
        let n = self.tris.len();
        for &r in self.cell_refs(c) {
            let Some(h) = (r as usize).checked_sub(n).map(|k| &self.hulls[k]) else { continue };
            if !want.has(h.class) || (x as f32) < h.lo.x || (x as f32) > h.hi.x || (y as f32) < h.lo.y || (y as f32) > h.hi.y {
                continue;
            }
            out.extend(hull_z(h, x, y));
        }
        let mut hits = Vec::new();
        self.tri_hits(c, x, y, want, &mut hits);
        scan(&mut hits, out);
        merge(out);
    }

    #[allow(dead_code)]
    pub fn box_solid(&self, lo: DVec3, hi: DVec3, want: Mask) -> bool {
        let n = self.tris.len();
        let (c, e) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
        let (bl, bh) = (v3(lo), v3(hi));
        let clamp = |v: f64, o: f64, m: usize| (((v - o) / self.cell).floor().max(0.0) as usize).min(m - 1);
        let (i0, i1) = (clamp(lo.x, self.lo[0], self.nx), clamp(hi.x, self.lo[0], self.nx));
        let (j0, j1) = (clamp(lo.y, self.lo[1], self.ny), clamp(hi.y, self.lo[1], self.ny));
        let hit = |h: &Hull| {
            h.lo.cmple(bh).all()
                && h.hi.cmpge(bl).all()
                && h.planes.iter().all(|p| {
                    let (pn, d) = (DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64), p[3] as f64);
                    pn.dot(c) - pn.abs().dot(e) < d
                })
        };
        for j in j0..=j1 {
            for i in i0..=i1 {
                for &r in self.cell_refs(i + self.nx * j) {
                    if let Some(k) = (r as usize).checked_sub(n) {
                        let h = &self.hulls[k];
                        if want.has(h.class) && hit(h) {
                            return true;
                        }
                    }
                }
            }
        }
        let mut hits = Vec::new();
        let mut iv = Vec::new();
        for (x, y) in [(lo.x, lo.y), (hi.x, lo.y), (lo.x, hi.y), (hi.x, hi.y), (c.x, c.y)] {
            let Some(cell) = self.cell_of(x, y) else { continue };
            hits.clear();
            iv.clear();
            self.tri_hits(cell, x, y, want, &mut hits);
            scan(&mut hits, &mut iv);
            if iv.iter().any(|&(a, b)| (a as f64) < hi.z && (b as f64) > lo.z) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(b: &mut Builder, lo: Vec3, hi: Vec3) {
        let v: Vec<Vec3> = (0..8)
            .map(|i| Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }))
            .collect();
        let t = [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        b.mesh(&v, &t, &DAffine3::IDENTITY, Class::Solid);
    }

    #[test]
    fn hull_column() {
        let mut b = Builder::default();
        b.aabb(DVec3::new(0.0, 0.0, 10.0), DVec3::new(64.0, 64.0, 50.0), &DAffine3::IDENTITY, Class::Solid);
        let p = b.finish();
        let mut out = Vec::new();
        p.column(20.0, 30.0, Mask::SOLID, &mut out);
        assert_eq!(out, vec![(10.0, 50.0)]);
        p.column(70.0, 30.0, Mask::SOLID, &mut out);
        assert!(out.is_empty());
        p.column(20.0, 30.0, Mask { solid: false, clip: true, decor: true }, &mut out);
        assert!(out.is_empty());
        assert!(p.box_solid(DVec3::new(-16.0, -16.0, 0.0), DVec3::new(16.0, 16.0, 72.0), Mask::SOLID));
        assert!(!p.box_solid(DVec3::new(-16.0, -16.0, 51.0), DVec3::new(16.0, 16.0, 123.0), Mask::SOLID));
    }

    #[test]
    fn rotated_hull() {
        let mut b = Builder::default();
        let xf = DAffine3::from_rotation_z(90f64.to_radians()) * DAffine3::from_translation(DVec3::new(100.0, 0.0, 0.0));
        b.aabb(DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 20.0, 30.0), &xf, Class::Solid);
        let p = b.finish();
        let mut out = Vec::new();
        p.column(-10.0, 105.0, Mask::SOLID, &mut out);
        assert_eq!(out.len(), 1);
        assert!((out[0].0 - 0.0).abs() < 1e-3 && (out[0].1 - 30.0).abs() < 1e-3);
        p.column(10.0, 105.0, Mask::SOLID, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn open_quad() {
        let mut b = Builder::default();
        let v = [Vec3::new(0.0, 0.0, 5.0), Vec3::new(100.0, 0.0, 5.0), Vec3::new(100.0, 100.0, 5.0), Vec3::new(0.0, 100.0, 5.0)];
        b.mesh(&v, &[[0, 1, 2], [0, 2, 3]], &DAffine3::IDENTITY, Class::Solid);
        let p = b.finish();
        let mut out = Vec::new();
        p.column(50.0, 50.0, Mask::SOLID, &mut out);
        assert_eq!(out, vec![(f32::NEG_INFINITY, 5.0)]);
        p.column(30.0, 70.0, Mask::SOLID, &mut out);
        assert_eq!(out, vec![(f32::NEG_INFINITY, 5.0)]);
    }

    #[test]
    fn ceiling_sheet() {
        let mut b = Builder::default();
        let v = [Vec3::new(0.0, 0.0, 5.0), Vec3::new(100.0, 0.0, 5.0), Vec3::new(100.0, 100.0, 5.0)];
        b.mesh(&v, &[[0, 2, 1]], &DAffine3::IDENTITY, Class::Solid);
        let p = b.finish();
        let mut out = Vec::new();
        p.column(60.0, 20.0, Mask::SOLID, &mut out);
        assert_eq!(out, vec![(5.0 - SHEET, 5.0)]);
    }

    #[test]
    fn closed_box() {
        let mut b = Builder::default();
        cube(&mut b, Vec3::new(0.0, 0.0, 10.0), Vec3::new(64.0, 64.0, 40.0));
        cube(&mut b, Vec3::new(0.0, 0.0, 40.0), Vec3::new(64.0, 64.0, 60.0));
        let p = b.finish();
        let mut out = Vec::new();
        for (x, y) in [(20.0, 30.0), (32.0, 32.0), (0.0, 0.0), (64.0, 10.0), (10.0, 54.0)] {
            p.column(x, y, Mask::SOLID, &mut out);
            assert_eq!(out, vec![(10.0, 60.0)], "at {x} {y}");
        }
        p.column(80.0, 30.0, Mask::SOLID, &mut out);
        assert!(out.is_empty());
        assert!(p.box_solid(DVec3::new(50.0, 50.0, 0.0), DVec3::new(80.0, 80.0, 20.0), Mask::SOLID));
        assert!(!p.box_solid(DVec3::new(50.0, 50.0, 61.0), DVec3::new(80.0, 80.0, 90.0), Mask::SOLID));
    }
}
