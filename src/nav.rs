use std::cmp::Reverse;
use std::collections::BinaryHeap;

use anyhow::{Result, bail};
use glam::{DVec2, DVec3};
use rayon::prelude::*;

use crate::bsp::{Bsp, CONTENTS_SKY, CONTENTS_SOLID};
use crate::navmesh::{Links, NavMesh};
use crate::phys::{Mask, Phys};

pub const STEP: f32 = 18.0;
const JUMP: f32 = 63.0;
pub const MAX_DROP: f32 = 655.0;
const HEAD: f32 = 72.0;
const THIN: f64 = 4.0;
const SAMPLE: f64 = 2.0;
const REACH: f64 = 32.0;
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

pub fn root(up: &mut [u32], mut a: u32) -> u32 {
    while up[a as usize] != a {
        up[a as usize] = up[up[a as usize] as usize];
        a = up[a as usize];
    }
    a
}

pub fn unite(up: &mut [u32], a: u32, b: u32) {
    let (a, b) = (root(up, a), root(up, b));
    if a != b {
        up[a.max(b) as usize] = a.min(b);
    }
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
    node_areas: Vec<u32>,
    area_start: Vec<u32>,
    pub removed: Option<Vec<(u32, u32)>>,
    pub drops: Option<Vec<(u32, u32)>>,
    pub warn: Option<String>,
    pub ladders: usize,
    pub blockers: usize,
}

const DIRS: [(i64, i64); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const BUCKET: f64 = 128.0;
const LADDER_PAD: DVec3 = DVec3::new(24.0, 24.0, 40.0);

struct NavTri {
    p: [DVec3; 3],
    stand: bool,
    area: u32,
}

impl NavTri {
    fn z_at(&self, x: f64, y: f64) -> Option<f64> {
        let [a, b, c] = self.p;
        let d = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
        if d.abs() < 1e-9 {
            return None;
        }
        let w0 = ((b.y - c.y) * (x - c.x) + (c.x - b.x) * (y - c.y)) / d;
        let w1 = ((c.y - a.y) * (x - c.x) + (a.x - c.x) * (y - c.y)) / d;
        let w2 = 1.0 - w0 - w1;
        let e = -1e-6;
        (w0 >= e && w1 >= e && w2 >= e).then(|| w0 * a.z + w1 * b.z + w2 * c.z)
    }
}

impl Nav {
    pub fn build(bsp: &Bsp, cell: f64, speed: f64) -> Result<Nav> {
        Nav::build_with(bsp, cell, speed, true)
    }

    pub fn build_with(bsp: &Bsp, cell: f64, speed: f64, collide: bool) -> Result<Nav> {
        if let Some(x) = &bsp.cs2 {
            let Some(m) = &x.nav else { bail!("map has no nav mesh") };
            let gate = std::env::var("BSP2IMG_NAV_LINKS").map_or(true, |v| v != "height");
            let phys = (gate && collide).then(|| x.phys());
            let mut nav = Nav::from_areas(m, cell, speed, gate, phys.as_ref().and_then(|p| p.as_ref().ok()).copied())?;
            if let Some(Err(e)) = phys {
                nav.warn = Some(format!("{e:#}; drops without a nav connection stay blocked"));
            }
            return Ok(nav);
        }
        if bsp.clipnodes.is_empty() {
            bail!("map has no collision hulls");
        }
        let bl = blockers(bsp);
        let (lo, hi) = bsp.world_bounds();
        let nx = ((hi.x - lo.x) / cell).ceil().max(1.0) as usize;
        let ny = ((hi.y - lo.y) / cell).ceil().max(1.0) as usize;
        let nz = ((hi.z - lo.z) / DZ).ceil().max(1.0) as usize + 1;
        let cols: Vec<Vec<(Node, Vec<u32>)>> = (0..nx * ny)
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
                    out.push((Node { z: feet as f32, top: (top - CROUCH_OFS) as f32, stand }, Vec::new()));
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

        let mut nav = Nav::assemble(lo, cell, nx, ny, cols, bl.len(), None);
        nav.add_ladders(bsp, speed);
        Ok(nav)
    }

    pub fn from_areas(m: &NavMesh, cell: f64, speed: f64, gate: bool, phys: Option<&Phys>) -> Result<Nav> {
        if m.areas.is_empty() {
            bail!("map has no nav mesh areas");
        }
        let v = |c: &[f32; 3]| DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64);
        let mut tris = Vec::new();
        for (k, a) in m.areas.iter().enumerate() {
            let p: Vec<DVec3> = a.poly.iter().map(v).collect();
            for i in 1..p.len().saturating_sub(1) {
                tris.push(NavTri { p: [p[0], p[i], p[i + 1]], stand: !a.crouch, area: k as u32 });
            }
        }
        let pts = tris.iter().flat_map(|t| t.p);
        let (mut lo, mut hi) = pts.fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(a, b), p| (a.min(p), b.max(p)));
        lo -= DVec3::new(cell, cell, 0.0);
        hi += DVec3::new(cell, cell, 0.0);
        let nx = ((hi.x - lo.x) / cell).ceil().max(1.0) as usize;
        let ny = ((hi.y - lo.y) / cell).ceil().max(1.0) as usize;
        let bx = ((hi.x - lo.x) / BUCKET).ceil().max(1.0) as usize;
        let by = ((hi.y - lo.y) / BUCKET).ceil().max(1.0) as usize;
        let bi = |x: f64, y: f64| {
            let i = (((x - lo.x) / BUCKET).floor().max(0.0) as usize).min(bx - 1);
            let j = (((y - lo.y) / BUCKET).floor().max(0.0) as usize).min(by - 1);
            (i, j)
        };
        let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); bx * by];
        for (k, t) in tris.iter().enumerate() {
            let (a0, b0) = bi(t.p.iter().map(|p| p.x).fold(f64::INFINITY, f64::min), t.p.iter().map(|p| p.y).fold(f64::INFINITY, f64::min));
            let (a1, b1) = bi(t.p.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max), t.p.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max));
            for i in a0..=a1 {
                for j in b0..=b1 {
                    buckets[i * by + j].push(k as u32);
                }
            }
        }
        let cols: Vec<Vec<(Node, Vec<u32>)>> = (0..nx * ny)
            .into_par_iter()
            .map(|c| {
                let x = lo.x + ((c / ny) as f64 + 0.5) * cell;
                let y = lo.y + ((c % ny) as f64 + 0.5) * cell;
                let (i, j) = bi(x, y);
                let mut hits: Vec<(f64, bool, u32)> = buckets[i * by + j]
                    .iter()
                    .filter_map(|&k| {
                        let t = &tris[k as usize];
                        t.z_at(x, y).map(|z| (z, t.stand, t.area))
                    })
                    .collect();
                hits.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut groups: Vec<(f64, bool, Vec<u32>)> = Vec::new();
                for (z, s, a) in hits {
                    match groups.last_mut() {
                        Some(g) if z - g.0 <= STEP as f64 => {
                            (g.0, g.1) = (z, g.1 || s);
                            if !g.2.contains(&a) {
                                g.2.push(a);
                            }
                        }
                        _ => groups.push((z, s, vec![a])),
                    }
                }
                let tops: Vec<f32> = groups.iter().skip(1).map(|g| g.0 as f32 - 1.0).chain([f32::INFINITY]).collect();
                groups.into_iter().zip(tops).map(|((z, stand, a), top)| (Node { z: z as f32, top, stand }, a)).collect()
            })
            .collect();
        let links = gate.then(|| Links::new(m));
        let mut nav = Nav::assemble(lo, cell, nx, ny, cols, 0, links.as_ref().map(|g| (g, phys)));
        for l in &m.ladders {
            let (t, b) = (v(&l.top), v(&l.bottom));
            let w = DVec3::new(l.width as f64 / 2.0, l.width as f64 / 2.0, 0.0);
            if nav.ladder(t.min(b) - w - LADDER_PAD, t.max(b) + w + LADDER_PAD, speed) {
                nav.ladders += 1;
            }
        }
        Ok(nav)
    }

    fn assemble(
        lo: DVec3,
        cell: f64,
        nx: usize,
        ny: usize,
        cols: Vec<Vec<(Node, Vec<u32>)>>,
        blockers: usize,
        gate: Option<(&Links, Option<&Phys>)>,
    ) -> Nav {
        let mut start = Vec::with_capacity(nx * ny + 1);
        let mut nodes = Vec::new();
        let mut col = Vec::new();
        let mut node_areas = Vec::new();
        let mut area_start = vec![0];
        for (c, v) in cols.into_iter().enumerate() {
            start.push(nodes.len() as u32);
            col.extend(std::iter::repeat_n(c as u32, v.len()));
            for (n, a) in v {
                nodes.push(n);
                node_areas.extend(a);
                area_start.push(node_areas.len() as u32);
            }
        }
        start.push(nodes.len() as u32);
        if node_areas.is_empty() {
            area_start.clear();
        }
        let mut nav = Nav {
            lo,
            cell,
            nx,
            ny,
            start,
            nodes,
            col,
            orth: Vec::new(),
            extra: Vec::new(),
            node_areas,
            area_start,
            removed: None,
            drops: None,
            warn: None,
            ladders: 0,
            blockers,
        };
        let res: Vec<_> = (0..nav.nodes.len())
            .into_par_iter()
            .map(|u| {
                let c = nav.col[u] as usize;
                let (i, j) = ((c / ny) as i64, (c % ny) as i64);
                let mut cut = Vec::new();
                let mut back = Vec::new();
                let o = DIRS.map(|(dx, dy)| {
                    let (a, b) = (i + dx, j + dy);
                    if a < 0 || b < 0 || a >= nx as i64 || b >= ny as i64 {
                        return NONE;
                    }
                    let c = a as usize * ny + b as usize;
                    let v = match (nav.link(u, c, |_| true), gate) {
                        (Some(v), Some((g, phys))) if !nav.allowed(g, u, v) => {
                            let drop = |w: usize| phys.is_some_and(|p| nav.drop_open(p, u, w));
                            if !drop(v) {
                                cut.push((u as u32, v as u32));
                            }
                            let w = nav.link(u, c, |w| nav.allowed(g, u, w) || drop(w));
                            if let Some(w) = w.filter(|&w| !nav.allowed(g, u, w)) {
                                back.push((u as u32, w as u32));
                            }
                            w
                        }
                        (v, _) => v,
                    };
                    v.map_or(NONE, |v| v as u32)
                });
                (o, cut, back)
            })
            .collect();
        nav.removed = gate.map(|_| res.iter().flat_map(|r| r.1.iter().copied()).collect());
        nav.drops = gate.and_then(|g| g.1).map(|_| res.iter().flat_map(|r| r.2.iter().copied()).collect());
        nav.orth = res.into_iter().map(|r| r.0).collect();
        nav.extra = vec![Vec::new(); nav.nodes.len()];
        nav
    }

    fn areas(&self, n: usize) -> &[u32] {
        match self.area_start.get(n..n + 2) {
            Some(r) => &self.node_areas[r[0] as usize..r[1] as usize],
            None => &[],
        }
    }

    fn allowed(&self, g: &Links, u: usize, v: usize) -> bool {
        let (x0, y0) = self.cell_xy(self.col[u] as usize);
        let (x1, y1) = self.cell_xy(self.col[v] as usize);
        g.allowed(self.areas(u), self.areas(v), DVec2::new(x0, y0), DVec2::new(x1, y1), self.nodes[u].z, self.nodes[v].z)
    }

    fn drop_open(&self, p: &Phys, u: usize, v: usize) -> bool {
        let (a, b) = (self.nodes[u], self.nodes[v]);
        let dz = a.z - b.z;
        if dz <= STEP || dz > MAX_DROP {
            return false;
        }
        let (x0, y0) = self.cell_xy(self.col[u] as usize);
        let (x1, y1) = self.cell_xy(self.col[v] as usize);
        let p0 = DVec2::new(x0, y0);
        let Some(d) = (DVec2::new(x1, y1) - p0).try_normalize() else { return false };
        let top = a.z + HEAD;
        let mut iv = Vec::new();
        let mut free = |q: DVec2, lo: f32| {
            p.column(q.x, q.y, Mask::PLAYER, &mut iv);
            !iv.iter().any(|&(z0, z1)| z0 < top && z1 > lo)
        };
        let side = [-THIN, 0.0, THIN];
        for k in 1..=(REACH / SAMPLE) as usize {
            let q = p0 + d * (k as f64 * SAMPLE);
            if !side.iter().all(|&s| free(q + d.perp() * s, a.z + STEP)) {
                return false;
            }
            if side.iter().all(|&s| side.iter().all(|&r| free(q + d * s + d.perp() * r, b.z + STEP))) {
                return true;
            }
        }
        false
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn column(&self, c: usize) -> std::ops::Range<usize> {
        self.start[c] as usize..self.start[c + 1] as usize
    }

    fn link(&self, u: usize, c: usize, ok: impl Fn(usize) -> bool) -> Option<usize> {
        let a = self.nodes[u];
        self.column(c)
            .filter(|&v| {
                let b = self.nodes[v];
                let h = a.z.max(b.z);
                b.z - a.z <= JUMP && h <= a.top + 0.5 && h <= b.top + 0.5 && ok(v)
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
            if self.ladder(m.mins + o - LADDER_PAD, m.maxs + o + LADDER_PAD, speed) {
                self.ladders += 1;
            }
        }
    }

    fn ladder(&mut self, lo: DVec3, hi: DVec3, speed: f64) -> bool {
        let near = self.nodes_in(lo, hi);
        if near.len() > 400 {
            return false;
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
        near.iter().map(|&a| self.extra[a].len()).sum::<usize>() > before
    }

    pub fn links(&self, u: usize) -> [u32; 4] {
        self.orth[u]
    }

    pub fn components(&self) -> Vec<u32> {
        let mut up: Vec<u32> = (0..self.nodes.len() as u32).collect();
        for u in 0..self.nodes.len() {
            let more = self.extra[u].iter().map(|e| e.0);
            for v in self.orth[u].iter().copied().filter(|&v| v != NONE).chain(more) {
                unite(&mut up, u as u32, v);
            }
        }
        (0..self.nodes.len() as u32).map(|u| root(&mut up, u)).collect()
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

#[cfg(test)]
mod tests {
    use glam::DAffine3;

    use super::*;
    use crate::navmesh::Area;
    use crate::phys::{Builder, Class};

    fn quad(x0: f32, x1: f32, z: f32) -> Area {
        Area { poly: vec![[x0, 0.0, z], [x1, 0.0, z], [x1, 32.0, z], [x0, 32.0, z]], crouch: false, conns: Vec::new() }
    }

    fn drops(low: f32, extra: &[([f64; 3], [f64; 3])]) -> (usize, usize) {
        let m = NavMesh { areas: vec![quad(0.0, 32.0, 0.0), quad(32.0, 96.0, low)], ladders: Vec::new() };
        let mut b = Builder::default();
        let v = |a: [f64; 3]| DVec3::from_array(a);
        b.aabb(v([-64.0, -64.0, -16.0]), v([32.0, 96.0, 0.0]), &DAffine3::IDENTITY, Class::Solid);
        b.aabb(v([-64.0, -64.0, low as f64 - 16.0]), v([160.0, 96.0, low as f64]), &DAffine3::IDENTITY, Class::Solid);
        for &(lo, hi) in extra {
            b.aabb(v(lo), v(hi), &DAffine3::IDENTITY, Class::Clip);
        }
        let p = b.finish();
        let nav = Nav::from_areas(&m, 8.0, 250.0, true, Some(&p)).unwrap();
        (nav.drops.unwrap().len(), nav.removed.unwrap().len())
    }

    #[test]
    fn drops_follow_collision() {
        assert_eq!(drops(-200.0, &[]), (4, 0));
        assert_eq!(drops(-200.0, &[([32.0, -64.0, -16.0], [34.0, 96.0, 40.0])]), (0, 4));
        assert_eq!(drops(-200.0, &[([32.0, -64.0, -110.0], [160.0, 96.0, -100.0])]), (0, 4));
        assert_eq!(drops(-700.0, &[]), (0, 4));
        assert_eq!(drops(-10.0, &[]), (0, 8));
    }
}
