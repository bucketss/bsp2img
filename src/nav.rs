use std::cmp::Reverse;
use std::collections::BinaryHeap;

use anyhow::{Result, bail};
use glam::DVec3;
use rayon::prelude::*;

use crate::bsp::{Bsp, CONTENTS_SKY, CONTENTS_SOLID};

pub const STEP: f32 = 18.0;
const JUMP: f32 = 63.0;
pub const JUMP_COST: f64 = 0.2;
const DUCK_SPEED: f64 = 0.333;
pub const LADDER_SPEED: f64 = 200.0;
const CROUCH_OFS: f64 = 18.0;
pub const STAND_OFS: f64 = 36.0;
const DZ: f64 = 8.0;
pub const NONE: u32 = u32::MAX;
pub const BLOCKERS: &[&str] = &["func_wall", "func_wall_toggle", "func_pushable"];

#[derive(Clone, Copy)]
pub struct Node {
    pub z: f32,
    pub top: f32,
    pub stand: bool,
}

struct Blocker {
    model: usize,
    origin: DVec3,
    lo: DVec3,
    hi: DVec3,
}

fn blockers(bsp: &Bsp) -> Vec<Blocker> {
    let pad = DVec3::new(16.0, 16.0, 36.0);
    bsp.entities
        .iter()
        .filter(|e| {
            let flags: i64 = e.get("spawnflags").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
            BLOCKERS.contains(&e.class()) || (e.class() == "func_breakable" && flags & 1 != 0)
        })
        .filter_map(|e| {
            let mi = e.model().filter(|&m| m > 0 && m < bsp.models.len())?;
            let m = &bsp.models[mi];
            let origin = e.origin().unwrap_or(DVec3::ZERO);
            Some(Blocker { model: mi, origin, lo: m.mins + origin - pad, hi: m.maxs + origin + pad })
        })
        .collect()
}

fn open(bsp: &Bsp, bl: &[Blocker], p: DVec3, hull: usize) -> bool {
    let c = bsp.hull_contents(p, hull);
    if c == CONTENTS_SOLID || c == CONTENTS_SKY {
        return false;
    }
    !bl.iter().any(|b| {
        p.cmpge(b.lo).all() && p.cmple(b.hi).all() && bsp.model_contents(b.model, p - b.origin, hull) == CONTENTS_SOLID
    })
}

fn boundary(op: &impl Fn(f64) -> bool, mut a: f64, mut b: f64) -> (f64, f64) {
    let oa = op(a);
    for _ in 0..4 {
        let m = (a + b) / 2.0;
        if op(m) == oa {
            a = m;
        } else {
            b = m;
        }
    }
    (a, b)
}

pub struct Zone {
    pub label: String,
    pub lo: DVec3,
    pub hi: DVec3,
    pub color: [u8; 3],
    pub boxed: bool,
}

pub fn zones(bsp: &Bsp) -> Vec<Zone> {
    let kinds: [(&[&str], &str, [u8; 3]); 4] = [
        (&["func_bomb_target", "info_bomb_target"], "bombsite", [255, 140, 0]),
        (&["func_hostage_rescue", "info_hostage_rescue"], "rescue", [0, 220, 220]),
        (&["hostage_entity"], "hostage", [255, 220, 0]),
        (&["func_vip_safetyzone"], "escape", [0, 220, 220]),
    ];
    let mut out = Vec::new();
    for (classes, name, color) in kinds {
        let mut n = 0;
        for e in bsp.entities.iter().filter(|e| classes.contains(&e.class())) {
            let o = e.origin().unwrap_or(DVec3::ZERO);
            let (lo, hi, boxed) = match e.model().and_then(|m| bsp.models.get(m)).filter(|_| e.model() != Some(0)) {
                Some(m) => (m.mins + o, m.maxs + o, true),
                None if e.origin().is_some() => {
                    let r = if name == "bombsite" { 128.0 } else { 48.0 };
                    (o - DVec3::new(r, r, 0.0), o + DVec3::new(r, r, 0.0), false)
                }
                None => continue,
            };
            n += 1;
            out.push(Zone { label: format!("{name} {n}"), lo, hi, color, boxed });
        }
    }
    out
}

pub struct Nav {
    pub lo: DVec3,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    start: Vec<u32>,
    pub nodes: Vec<Node>,
    pub col: Vec<u32>,
    orth: Vec<[u32; 4]>,
    extra: Vec<Vec<(u32, f32)>>,
    pub ladders: usize,
    pub blockers: usize,
}

const DIRS: [(i64, i64); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

impl Nav {
    pub fn build(bsp: &Bsp, cell: f64, speed: f64) -> Result<Nav> {
        if bsp.clipnodes.is_empty() {
            bail!("map has no collision hulls");
        }
        let bl = blockers(bsp);
        let (lo, hi) = bsp.world_bounds();
        let nx = ((hi.x - lo.x) / cell).ceil().max(1.0) as usize;
        let ny = ((hi.y - lo.y) / cell).ceil().max(1.0) as usize;
        let nz = ((hi.z - lo.z) / DZ).ceil().max(1.0) as usize + 1;
        let cols: Vec<Vec<Node>> = (0..nx * ny)
            .into_par_iter()
            .map(|c| {
                let x = lo.x + ((c / ny) as f64 + 0.5) * cell;
                let y = lo.y + ((c % ny) as f64 + 0.5) * cell;
                let op = |z: f64| open(bsp, &bl, DVec3::new(x, y, z), 3);
                let mut out = Vec::new();
                let mut prev = false;
                let mut floor = 0.0;
                let mut push = |floor: f64, top: f64| {
                    let feet = floor - CROUCH_OFS;
                    let stand = open(bsp, &bl, DVec3::new(x, y, feet + STAND_OFS + 0.5), 1);
                    out.push(Node { z: feet as f32, top: (top - CROUCH_OFS) as f32, stand });
                };
                for k in 0..nz {
                    let z = lo.z + k as f64 * DZ;
                    let o = op(z);
                    if o && !prev {
                        floor = if k == 0 { z } else { boundary(&op, z - DZ, z).1 };
                    }
                    if !o && prev {
                        push(floor, boundary(&op, z - DZ, z).0);
                    }
                    prev = o;
                }
                if prev {
                    push(floor, lo.z + (nz - 1) as f64 * DZ);
                }
                out
            })
            .collect();

        let mut start = Vec::with_capacity(nx * ny + 1);
        let mut nodes = Vec::new();
        let mut col = Vec::new();
        for (c, v) in cols.into_iter().enumerate() {
            start.push(nodes.len() as u32);
            col.extend(std::iter::repeat_n(c as u32, v.len()));
            nodes.extend(v);
        }
        start.push(nodes.len() as u32);
        let blockers = bl.len();
        let mut nav =
            Nav { lo, cell, nx, ny, start, nodes, col, orth: Vec::new(), extra: Vec::new(), ladders: 0, blockers };
        nav.orth = (0..nav.nodes.len())
            .into_par_iter()
            .map(|u| {
                let c = nav.col[u] as usize;
                let (i, j) = ((c / ny) as i64, (c % ny) as i64);
                DIRS.map(|(dx, dy)| {
                    let (a, b) = (i + dx, j + dy);
                    if a < 0 || b < 0 || a >= nx as i64 || b >= ny as i64 {
                        return NONE;
                    }
                    nav.link(u, a as usize * ny + b as usize).map_or(NONE, |v| v as u32)
                })
            })
            .collect();
        nav.extra = vec![Vec::new(); nav.nodes.len()];
        nav.add_ladders(bsp, speed);
        Ok(nav)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn column(&self, c: usize) -> std::ops::Range<usize> {
        self.start[c] as usize..self.start[c + 1] as usize
    }

    fn link(&self, u: usize, c: usize) -> Option<usize> {
        let a = self.nodes[u];
        self.column(c)
            .filter(|&v| {
                let b = self.nodes[v];
                let h = a.z.max(b.z);
                b.z - a.z <= JUMP && h <= a.top + 0.5 && h <= b.top + 0.5
            })
            .min_by(|&v, &w| (self.nodes[v].z - a.z).abs().total_cmp(&(self.nodes[w].z - a.z).abs()))
    }

    pub fn cell_xy(&self, c: usize) -> (f64, f64) {
        (self.lo.x + ((c / self.ny) as f64 + 0.5) * self.cell, self.lo.y + ((c % self.ny) as f64 + 0.5) * self.cell)
    }

    pub fn cols_in(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> impl Iterator<Item = usize> + '_ {
        let ci = |v: f64, n: usize| (((v) / self.cell).floor().max(0.0) as usize).min(n - 1);
        let (i0, i1) = (ci(x0 - self.lo.x, self.nx), ci(x1 - self.lo.x, self.nx));
        let (j0, j1) = (ci(y0 - self.lo.y, self.ny), ci(y1 - self.lo.y, self.ny));
        (i0..=i1).flat_map(move |i| (j0..=j1).map(move |j| i * self.ny + j))
    }

    pub fn nodes_in(&self, lo: DVec3, hi: DVec3) -> Vec<usize> {
        self.cols_in(lo.x, lo.y, hi.x, hi.y)
            .flat_map(|c| self.column(c))
            .filter(|&n| (self.nodes[n].z as f64) >= lo.z && (self.nodes[n].z as f64) <= hi.z)
            .collect()
    }

    fn add_ladders(&mut self, bsp: &Bsp, speed: f64) {
        for e in bsp.entities.iter().filter(|e| e.class() == "func_ladder") {
            let Some(m) = e.model().and_then(|m| bsp.models.get(m)) else { continue };
            let o = e.origin().unwrap_or(DVec3::ZERO);
            let pad = DVec3::new(24.0, 24.0, 40.0);
            let near = self.nodes_in(m.mins + o - pad, m.maxs + o + pad);
            if near.len() > 400 {
                continue;
            }
            let before: usize = near.iter().map(|&a| self.extra[a].len()).sum();
            for &a in &near {
                for &b in &near {
                    let dz = (self.nodes[b].z - self.nodes[a].z).abs();
                    if dz <= STEP {
                        continue;
                    }
                    let (ax, ay) = self.cell_xy(self.col[a] as usize);
                    let (bx, by) = self.cell_xy(self.col[b] as usize);
                    let t = dz as f64 / LADDER_SPEED + (bx - ax).hypot(by - ay) / speed;
                    self.extra[a].push((b as u32, t as f32));
                }
            }
            if near.iter().map(|&a| self.extra[a].len()).sum::<usize>() > before {
                self.ladders += 1;
            }
        }
    }

    pub fn links(&self, u: usize) -> [u32; 4] {
        self.orth[u]
    }

    fn hop(&self, u: usize, d: usize) -> Option<(usize, bool, bool)> {
        let v = self.orth[u][d];
        if v == NONE {
            return None;
        }
        let v = v as usize;
        let (a, b) = (self.nodes[u], self.nodes[v]);
        Some((v, b.z - a.z > STEP, !a.stand || !b.stand))
    }

    fn chain(&self, u: usize, ds: &[usize]) -> Option<(usize, bool, bool)> {
        let (mut n, mut j, mut k) = (u, false, false);
        for &d in ds {
            let (v, jj, kk) = self.hop(n, d)?;
            (n, j, k) = (v, j || jj, k || kk);
        }
        Some((n, j, k))
    }

    fn diag(&self, u: usize, dx: usize, dy: usize) -> Option<(usize, bool, bool)> {
        let a = self.chain(u, &[dx, dy])?;
        let b = self.chain(u, &[dy, dx])?;
        (a.0 == b.0).then_some((a.0, a.1 || b.1, a.2 || b.2))
    }

    fn knight(&self, u: usize, major: usize, dx: usize, dy: usize) -> Option<(usize, bool, bool)> {
        let (m1, j1, k1) = self.hop(u, major)?;
        let a = self.diag(m1, dx, dy)?;
        let (m2, j2, k2) = self.diag(u, dx, dy)?;
        let b = self.hop(m2, major)?;
        (a.0 == b.0).then_some((a.0, j1 || a.1 || j2 || b.1, k1 || a.2 || k2 || b.2))
    }

    pub fn flood(&self, seeds: &[usize], speed: f64) -> Vec<f32> {
        let mut dist = vec![f32::INFINITY; self.nodes.len()];
        let mut heap = BinaryHeap::new();
        for &s in seeds {
            dist[s] = 0.0;
            heap.push(Reverse((0u32, s as u32)));
        }
        let s2 = std::f64::consts::SQRT_2;
        let s5 = 5f64.sqrt();
        while let Some(Reverse((bits, u))) = heap.pop() {
            let u = u as usize;
            let du = f32::from_bits(bits);
            if du > dist[u] {
                continue;
            }
            let mut relax = |v: usize, t: f64| {
                let nd = du + t as f32;
                if nd < dist[v] {
                    dist[v] = nd;
                    heap.push(Reverse((nd.to_bits(), v as u32)));
                }
            };
            let cost = |len: f64, jump: bool, duck: bool| {
                len * self.cell / (speed * if duck { DUCK_SPEED } else { 1.0 }) + if jump { JUMP_COST } else { 0.0 }
            };
            for d in 0..4 {
                if let Some((v, j, k)) = self.hop(u, d) {
                    relax(v, cost(1.0, j, k));
                }
            }
            for dx in 0..2 {
                for dy in 2..4 {
                    if let Some((v, j, k)) = self.diag(u, dx, dy) {
                        relax(v, cost(s2, j, k));
                    }
                    for major in [dx, dy] {
                        if let Some((v, j, k)) = self.knight(u, major, dx, dy) {
                            relax(v, cost(s5, j, k));
                        }
                    }
                }
            }
            for &(v, t) in &self.extra[u] {
                relax(v as usize, t as f64);
            }
        }
        dist
    }

    pub fn nearest(&self, p: DVec3, feet_ofs: f64) -> Option<usize> {
        let target = p.z - feet_ofs;
        let r = 2.0 * self.cell;
        self.cols_in(p.x - r, p.y - r, p.x + r, p.y + r)
            .flat_map(|c| self.column(c))
            .filter(|&n| {
                let dz = self.nodes[n].z as f64 - target;
                (-128.0..=24.0).contains(&dz)
            })
            .min_by(|&a, &b| {
                let score = |n: usize| {
                    let (x, y) = self.cell_xy(self.col[n] as usize);
                    (x - p.x).hypot(y - p.y) + (self.nodes[n].z as f64 - target).abs() * 0.25
                };
                score(a).total_cmp(&score(b))
            })
    }

    #[allow(dead_code)]
    pub fn floor_at(&self, x: f64, y: f64, zmax: f64) -> Option<usize> {
        let i = ((x - self.lo.x) / self.cell).floor();
        let j = ((y - self.lo.y) / self.cell).floor();
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 {
            return None;
        }
        self.column(i as usize * self.ny + j as usize).rev().find(|&n| self.nodes[n].z as f64 <= zmax)
    }

    pub fn clearance(&self) -> Vec<f32> {
        let mut dist = vec![f32::INFINITY; self.nodes.len()];
        let mut heap = BinaryHeap::new();
        let edge = (self.cell / 2.0) as f32;
        for u in 0..self.nodes.len() {
            if self.orth[u].contains(&NONE) {
                dist[u] = edge;
                heap.push(Reverse((edge.to_bits(), u as u32)));
            }
        }
        let (c1, c2) = (self.cell as f32, (self.cell * std::f64::consts::SQRT_2) as f32);
        while let Some(Reverse((bits, u))) = heap.pop() {
            let u = u as usize;
            let du = f32::from_bits(bits);
            if du > dist[u] {
                continue;
            }
            let mut relax = |v: usize, t: f32| {
                let nd = du + t;
                if nd < dist[v] {
                    dist[v] = nd;
                    heap.push(Reverse((nd.to_bits(), v as u32)));
                }
            };
            for &v in &self.orth[u] {
                if v != NONE {
                    relax(v as usize, c1);
                }
            }
            for dx in 0..2 {
                for dy in 2..4 {
                    if let Some((v, _, _)) = self.diag(u, dx, dy) {
                        relax(v, c2);
                    }
                }
            }
        }
        dist
    }
}
