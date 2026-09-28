use anyhow::Result;
use glam::DVec3;

use super::bsp::{LUMP_LEAF_AMBIENT, LUMP_LEAF_AMBIENT_HDR, LUMP_LEAFS, LUMP_NODES, Map, Plane};

pub const LUMP_LEAF_AMBIENT_INDEX_HDR: usize = 51;
pub const LUMP_LEAF_AMBIENT_INDEX: usize = 52;
const NUDGE: [f64; 5] = [0.0, 16.0, 32.0, 64.0, -16.0];

pub type Cube = [[f32; 3]; 6];

struct Node {
    plane: usize,
    children: [i32; 2],
}

struct Leaf {
    mins: DVec3,
    maxs: DVec3,
    count: u16,
    first: u16,
}

struct Sample {
    cube: Cube,
    pos: [u8; 3],
}

pub struct Cubes {
    planes: Vec<Plane>,
    nodes: Vec<Node>,
    leaves: Vec<Leaf>,
    samples: Vec<Sample>,
    pub hdr: bool,
    pub mean: Cube,
    v0: bool,
}

pub fn rgbe(b: &[u8]) -> [f32; 3] {
    let e = 2f32.powi(b[3] as i8 as i32);
    [b[0] as f32 * e, b[1] as f32 * e, b[2] as f32 * e]
}

fn cube_at(b: &[u8]) -> Cube {
    std::array::from_fn(|f| rgbe(&b[f * 4..f * 4 + 4]))
}

pub fn apply(c: &Cube, n: DVec3) -> [f32; 3] {
    let l = n.length_squared();
    if l < 1e-12 {
        return std::array::from_fn(|k| c.iter().map(|f| f[k]).sum::<f32>() / 6.0);
    }
    let n = n / l.sqrt();
    let mut out = [0.0f32; 3];
    for ax in 0..3 {
        let w = (n[ax] * n[ax]) as f32;
        let f = &c[ax * 2 + (n[ax] < 0.0) as usize];
        for k in 0..3 {
            out[k] += f[k] * w;
        }
    }
    out
}

impl Cubes {
    pub fn load(map: &Map) -> Result<Option<Cubes>> {
        let planes = map.planes()?;
        let nodes = map.read(LUMP_NODES, 32, |r, o| Node { plane: r.i32(o).max(0) as usize, children: [r.i32(o + 4), r.i32(o + 8)] })?;
        let v0 = map.lumps[LUMP_LEAFS].version == 0 && map.raw(LUMP_LEAFS).len().is_multiple_of(56);
        let size = if v0 { 56 } else { 32 };
        let bounds = |r: &crate::bsp::Rd, o: usize| {
            (
                DVec3::new(r.i16(o + 8) as f64, r.i16(o + 10) as f64, r.i16(o + 12) as f64),
                DVec3::new(r.i16(o + 14) as f64, r.i16(o + 16) as f64, r.i16(o + 18) as f64),
            )
        };
        let (index, lighting, hdr) = if map.has(LUMP_LEAF_AMBIENT_INDEX_HDR) && map.has(LUMP_LEAF_AMBIENT_HDR) {
            (LUMP_LEAF_AMBIENT_INDEX_HDR, LUMP_LEAF_AMBIENT_HDR, true)
        } else {
            (LUMP_LEAF_AMBIENT_INDEX, LUMP_LEAF_AMBIENT, false)
        };
        let (leaves, samples) = if map.has(index) && map.has(lighting) {
            let idx = map.read(index, 4, |r, o| (r.u16(o), r.u16(o + 2)))?;
            let samples = map.read(lighting, 28, |r, o| Sample { cube: cube_at(&r.data[o..o + 24]), pos: [r.u8(o + 24), r.u8(o + 25), r.u8(o + 26)] })?;
            let leaves = map.read(LUMP_LEAFS, size, |r, o| {
                let (mins, maxs) = bounds(r, o);
                Leaf { mins, maxs, count: 0, first: 0 }
            })?;
            let leaves = leaves.into_iter().zip(idx.iter().chain(std::iter::repeat(&(0, 0)))).map(|(l, &(count, first))| Leaf { count, first, ..l }).collect();
            (leaves, samples)
        } else if v0 {
            let mut samples = Vec::new();
            let leaves = map.read(LUMP_LEAFS, 56, |r, o| {
                let (mins, maxs) = bounds(r, o);
                (Leaf { mins, maxs, count: 1, first: 0 }, cube_at(&r.data[o + 32..o + 56]))
            })?;
            let leaves = leaves
                .into_iter()
                .map(|(l, cube)| {
                    samples.push(Sample { cube, pos: [128; 3] });
                    Leaf { first: 0, ..l }
                })
                .collect::<Vec<_>>();
            (leaves, samples)
        } else {
            return Ok(None);
        };
        if leaves.is_empty() || samples.is_empty() || nodes.is_empty() {
            return Ok(None);
        }
        let mut mean = [[0.0f32; 3]; 6];
        let mut n = 0.0f32;
        for s in &samples {
            if s.cube.iter().flatten().any(|&v| v > 0.0) {
                for (m, c) in mean.iter_mut().zip(&s.cube) {
                    for k in 0..3 {
                        m[k] += c[k];
                    }
                }
                n += 1.0;
            }
        }
        if n > 0.0 {
            mean = mean.map(|m| m.map(|v| v / n));
        }
        let v0 = v0 && !(map.has(index) && map.has(lighting));
        Ok(Some(Cubes { planes, nodes, leaves, samples, hdr: hdr && !v0, mean, v0 }))
    }

    pub fn leaf(&self, p: DVec3) -> Option<usize> {
        let mut node = 0i32;
        for _ in 0..self.nodes.len() + 1 {
            if node < 0 {
                return Some((-1 - node) as usize).filter(|&l| l < self.leaves.len());
            }
            let nd = self.nodes.get(node as usize)?;
            let pl = self.planes.get(nd.plane)?;
            node = nd.children[(pl.normal.dot(p) - pl.dist < 0.0) as usize];
        }
        None
    }

    fn in_leaf(&self, li: usize, p: DVec3) -> Option<Cube> {
        let mut leaf = self.leaves.get(li)?;
        if self.v0 {
            return self.samples.get(li).map(|s| s.cube);
        }
        if leaf.count == 0 && leaf.first != 0 {
            leaf = self.leaves.get(leaf.first as usize)?;
        }
        if leaf.count == 0 {
            return None;
        }
        let a = leaf.first as usize;
        let list = self.samples.get(a..a + leaf.count as usize)?;
        let mut out = [[0.0f32; 3]; 6];
        let mut total = 0.0f64;
        for s in list {
            let q = DVec3::new(s.pos[0] as f64, s.pos[1] as f64, s.pos[2] as f64) / 255.0;
            let sp = leaf.mins + (leaf.maxs - leaf.mins) * q;
            let w = 1.0 / (sp.distance_squared(p) + 1.0);
            total += w;
            for (o, c) in out.iter_mut().zip(&s.cube) {
                for k in 0..3 {
                    o[k] += c[k] * w as f32;
                }
            }
        }
        Some(out.map(|o| o.map(|v| v / total as f32)))
    }

    pub fn sample(&self, p: DVec3) -> Option<Cube> {
        NUDGE.iter().find_map(|&dz| {
            let q = p + DVec3::Z * dz;
            self.leaf(q).and_then(|li| self.in_leaf(li, q)).filter(|c| c.iter().flatten().any(|&v| v > 0.0))
        })
    }

    pub fn at(&self, p: DVec3) -> Cube {
        self.sample(p).unwrap_or(self.mean)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn probe() {
        let Ok(p) = std::env::var("S1_MAP") else { return };
        let map = Map::open(std::path::Path::new(&p)).unwrap();
        let c = Cubes::load(&map).unwrap().unwrap();
        println!("leaves {} samples {} hdr {} v0 {} mean {:?}", c.leaves.len(), c.samples.len(), c.hdr, c.v0, c.mean);
        for q in [[0.0, 0.0, 0.0], [-400.0, 2100.0, -100.0], [300.0, 2400.0, 0.0], [-1500.0, 1500.0, 100.0]] {
            let q = DVec3::from(q);
            let li = c.leaf(q);
            let l = li.map(|i| (&c.leaves[i].mins, &c.leaves[i].maxs, c.leaves[i].count, c.leaves[i].first));
            println!("{q:?}: leaf {li:?} {l:?} -> {:?}", c.sample(q));
        }
    }
}
