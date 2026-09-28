use std::collections::HashMap;

use glam::DVec2;

pub struct Area {
    pub poly: Vec<[f32; 3]>,
    pub crouch: bool,
    pub conns: Vec<u32>,
}

pub struct Ladder {
    pub top: [f32; 3],
    pub bottom: [f32; 3],
    pub width: f32,
    pub top_areas: Vec<u32>,
    pub bottom_areas: Vec<u32>,
}

pub struct NavMesh {
    pub areas: Vec<Area>,
    pub ladders: Vec<Ladder>,
}

impl NavMesh {
    pub fn from_ids(ids: &[u32], mut areas: Vec<Area>, mut ladders: Vec<Ladder>) -> NavMesh {
        let index: HashMap<u32, u32> = ids.iter().enumerate().map(|(k, &id)| (id, k as u32)).collect();
        let map = |v: &[u32], own: Option<u32>| {
            let mut out: Vec<u32> = v.iter().filter_map(|id| index.get(id).copied()).filter(|&k| Some(k) != own).collect();
            out.sort_unstable();
            out.dedup();
            out
        };
        for (k, a) in areas.iter_mut().enumerate() {
            a.conns = map(&a.conns, Some(k as u32));
        }
        for l in &mut ladders {
            l.top_areas = map(&l.top_areas, None);
            l.bottom_areas = map(&l.bottom_areas, None);
        }
        NavMesh { areas, ladders }
    }

    pub fn floor_below(&self, x: f32, y: f32, zmax: f32) -> Option<f32> {
        let mut best: Option<f32> = None;
        for a in &self.areas {
            let p = &a.poly;
            for i in 1..p.len().saturating_sub(1) {
                let (a, b, c) = (p[0], p[i], p[i + 1]);
                let d = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
                if d.abs() < 1e-6 {
                    continue;
                }
                let w0 = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / d;
                let w1 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / d;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                if z <= zmax && best.is_none_or(|b| z > b) {
                    best = Some(z);
                }
            }
        }
        best
    }
}

const BUCKET: f64 = 64.0;
const BUFFER: f64 = 0.75;
const ZPAD: f32 = 18.0;

pub struct Links<'a> {
    mesh: &'a NavMesh,
    zr: Vec<[f32; 2]>,
    i0: i64,
    j0: i64,
    bx: usize,
    by: usize,
    buckets: Vec<Vec<u32>>,
}

impl<'a> Links<'a> {
    pub fn new(mesh: &'a NavMesh) -> Links<'a> {
        let cell = |v: f32| (v as f64 / BUCKET).floor() as i64;
        let (inf, ninf) = (f32::INFINITY, f32::NEG_INFINITY);
        let bbs: Vec<[f32; 6]> = mesh
            .areas
            .iter()
            .map(|a| {
                a.poly.iter().fold([inf, inf, inf, ninf, ninf, ninf], |b, p| {
                    [b[0].min(p[0]), b[1].min(p[1]), b[2].min(p[2]), b[3].max(p[0]), b[4].max(p[1]), b[5].max(p[2])]
                })
            })
            .collect();
        let used = || bbs.iter().enumerate().filter(|(_, b)| b[0] <= b[3]);
        let i0 = used().map(|(_, b)| cell(b[0])).min().unwrap_or(0);
        let j0 = used().map(|(_, b)| cell(b[1])).min().unwrap_or(0);
        let i1 = used().map(|(_, b)| cell(b[3])).max().unwrap_or(0);
        let j1 = used().map(|(_, b)| cell(b[4])).max().unwrap_or(0);
        let (bx, by) = ((i1 - i0 + 1) as usize, (j1 - j0 + 1) as usize);
        let mut buckets = vec![Vec::new(); bx * by];
        for (k, b) in used() {
            for i in cell(b[0])..=cell(b[3]) {
                for j in cell(b[1])..=cell(b[4]) {
                    buckets[(i - i0) as usize * by + (j - j0) as usize].push(k as u32);
                }
            }
        }
        let zr = bbs.iter().map(|b| [b[2], b[5]]).collect();
        Links { mesh, zr, i0, j0, bx, by, buckets }
    }

    fn bucket(&self, p: DVec2) -> &[u32] {
        let i = (p.x / BUCKET).floor() as i64 - self.i0;
        let j = (p.y / BUCKET).floor() as i64 - self.j0;
        if i < 0 || j < 0 || i >= self.bx as i64 || j >= self.by as i64 {
            return &[];
        }
        &self.buckets[i as usize * self.by + j as usize]
    }

    fn crosses(&self, k: usize, p0: DVec2, p1: DVec2) -> bool {
        let q: Vec<DVec2> = self.mesh.areas[k].poly.iter().map(|p| DVec2::new(p[0] as f64, p[1] as f64)).collect();
        let d = p1 - p0;
        let edges = (0..q.len()).map(|i| (q[(i + 1) % q.len()] - q[i]).perp());
        std::iter::once(d.perp()).chain(edges).filter(|a| a.length_squared() > 1e-12).all(|a| {
            let a = a.normalize();
            let (s0, s1) = (p0.dot(a), p1.dot(a));
            let (lo, hi) = q.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.dot(a)), hi.max(p.dot(a))));
            hi >= s0.min(s1) - BUFFER && lo <= s0.max(s1) + BUFFER
        })
    }

    pub fn allowed(&self, from: &[u32], to: &[u32], p0: DVec2, p1: DVec2, z0: f32, z1: f32) -> bool {
        let conns = |a: u32| self.mesh.areas[a as usize].conns.as_slice();
        if from.iter().any(|&a| to.contains(&a) || conns(a).iter().any(|c| to.contains(c))) {
            return true;
        }
        let (zlo, zhi) = (z0.min(z1) - ZPAD, z0.max(z1) + ZPAD);
        let mut set: Vec<u32> = to.to_vec();
        for &k in self.bucket(p0).iter().chain(self.bucket(p1)) {
            let z = self.zr[k as usize];
            if z[1] < zlo || z[0] > zhi || set.contains(&k) || from.contains(&k) {
                continue;
            }
            if self.crosses(k as usize, p0, p1) {
                set.push(k);
            }
        }
        let mut seen: Vec<u32> = from.to_vec();
        let mut front = seen.clone();
        while !front.is_empty() {
            let mut next = Vec::new();
            for &a in &front {
                for &b in conns(a) {
                    if seen.contains(&b) || !set.contains(&b) {
                        continue;
                    }
                    if to.contains(&b) {
                        return true;
                    }
                    seen.push(b);
                    next.push(b);
                }
            }
            front = next;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(x0: f32, x1: f32, z: f32, conns: Vec<u32>) -> Area {
        Area { poly: vec![[x0, 0.0, z], [x1, 0.0, z], [x1, 32.0, z], [x0, 32.0, z]], crouch: false, conns }
    }

    #[test]
    fn gate_follows_connections() {
        let m = NavMesh {
            areas: vec![quad(0.0, 32.0, 0.0, vec![1]), quad(32.0, 40.0, -10.0, vec![2]), quad(40.0, 72.0, -20.0, vec![]), quad(72.0, 96.0, -200.0, vec![])],
            ladders: Vec::new(),
        };
        let g = Links::new(&m);
        let (p, q) = (DVec2::new(28.0, 16.0), DVec2::new(44.0, 16.0));
        assert!(g.allowed(&[0], &[1], p, q, 0.0, -10.0));
        assert!(g.allowed(&[0], &[2], p, q, 0.0, -20.0));
        assert!(!g.allowed(&[2], &[0], q, p, -20.0, 0.0));
        assert!(!g.allowed(&[2], &[3], DVec2::new(68.0, 16.0), DVec2::new(76.0, 16.0), -20.0, -200.0));
    }
}
