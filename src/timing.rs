use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glam::DVec3;
use rayon::prelude::*;
use tiny_skia::{PathBuilder, Pixmap, Stroke, Transform};

use crate::bsp::{Bsp, CONTENTS_SKY, CONTENTS_SOLID};
use crate::camera::top_down;
use crate::grid::{CT, T, Text, grid_frame, line, outline, paint, rect};
use crate::paths::{Partial, free_name};
use crate::render::{Cuts, NO_CLIP, Renderer, View};
use crate::scene::Report;

const STEP: f32 = 18.0;
const JUMP: f32 = 63.0;
const JUMP_COST: f64 = 0.2;
const DUCK_SPEED: f64 = 0.333;
const LADDER_SPEED: f64 = 200.0;
const CROUCH_OFS: f64 = 18.0;
const STAND_OFS: f64 = 36.0;
const DZ: f64 = 8.0;
const NONE: u32 = u32::MAX;
const BLOCKERS: &[&str] = &["func_wall", "func_wall_toggle", "func_pushable"];

#[derive(Clone, Debug, PartialEq)]
pub struct TimingOpts {
    pub speed: f64,
    pub cell: f64,
    pub interval: f64,
    pub size: u32,
}

impl Default for TimingOpts {
    fn default() -> Self {
        TimingOpts { speed: 250.0, cell: 8.0, interval: 5.0, size: 1600 }
    }
}

#[derive(Clone, Copy)]
struct Node {
    z: f32,
    top: f32,
    stand: bool,
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

pub struct Nav {
    lo: DVec3,
    cell: f64,
    nx: usize,
    ny: usize,
    start: Vec<u32>,
    nodes: Vec<Node>,
    col: Vec<u32>,
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

    fn column(&self, c: usize) -> std::ops::Range<usize> {
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

    fn cell_xy(&self, c: usize) -> (f64, f64) {
        (self.lo.x + ((c / self.ny) as f64 + 0.5) * self.cell, self.lo.y + ((c % self.ny) as f64 + 0.5) * self.cell)
    }

    fn cols_in(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> impl Iterator<Item = usize> + '_ {
        let ci = |v: f64, n: usize| (((v) / self.cell).floor().max(0.0) as usize).min(n - 1);
        let (i0, i1) = (ci(x0 - self.lo.x, self.nx), ci(x1 - self.lo.x, self.nx));
        let (j0, j1) = (ci(y0 - self.lo.y, self.ny), ci(y1 - self.lo.y, self.ny));
        (i0..=i1).flat_map(move |i| (j0..=j1).map(move |j| i * self.ny + j))
    }

    fn nodes_in(&self, lo: DVec3, hi: DVec3) -> Vec<usize> {
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
}

struct Zone {
    label: String,
    lo: DVec3,
    hi: DVec3,
    color: [u8; 3],
    boxed: bool,
}

fn zones(bsp: &Bsp) -> Vec<Zone> {
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

fn zone_time(nav: &Nav, z: &Zone, d: &[f32]) -> f32 {
    if z.boxed {
        return nav.nodes_in(z.lo - DVec3::new(0.0, 0.0, 72.0), z.hi).into_iter().map(|n| d[n]).fold(f32::INFINITY, f32::min);
    }
    let top = z.hi.z as f32 + 16.0;
    nav.cols_in(z.lo.x, z.lo.y, z.hi.x, z.hi.y)
        .filter_map(|c| nav.column(c).rev().find(|&n| nav.nodes[n].z <= top && nav.nodes[n].z >= top - 272.0))
        .map(|n| d[n])
        .fold(f32::INFINITY, f32::min)
}

fn fmt_t(t: f32) -> String {
    if t.is_finite() { format!("{t:.1}") } else { "-".into() }
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t)
}

fn viridis(t: f64) -> [f64; 3] {
    const S: [[f64; 3]; 5] =
        [[253.0, 231.0, 37.0], [94.0, 201.0, 98.0], [33.0, 145.0, 140.0], [59.0, 82.0, 139.0], [68.0, 1.0, 84.0]];
    let t = t.clamp(0.0, 1.0) * 4.0;
    let i = (t.floor() as usize).min(3);
    lerp3(S[i], S[i + 1], t - i as f64)
}

fn team_color(team_t: bool, f: f64) -> [f64; 3] {
    if team_t {
        lerp3([255.0, 214.0, 140.0], [170.0, 30.0, 20.0], f)
    } else {
        lerp3([170.0, 230.0, 255.0], [20.0, 50.0, 170.0], f)
    }
}

struct Field {
    w: usize,
    h: usize,
    t: Vec<f32>,
    ct: Vec<f32>,
}

impl Field {
    fn first(&self, i: usize) -> f32 {
        self.t[i].min(self.ct[i])
    }
}

fn sample_field(nav: &Nav, dt: &[f32], dct: &[f32], zmax: f64, g: &crate::grid::GridFrame) -> Field {
    let sel: Vec<u32> = (0..nav.nx * nav.ny)
        .into_par_iter()
        .map(|c| {
            nav.column(c)
                .rev()
                .find(|&n| (nav.nodes[n].z as f64) <= zmax && (dt[n].is_finite() || dct[n].is_finite()))
                .map_or(NONE, |n| n as u32)
        })
        .collect();
    let (w, h) = (g.wpx as usize, g.hpx as usize);
    let at = |i: i64, j: i64| -> Option<usize> {
        if i < 0 || j < 0 || i >= nav.nx as i64 || j >= nav.ny as i64 {
            return None;
        }
        let n = sel[i as usize * nav.ny + j as usize];
        (n != NONE).then_some(n as usize)
    };
    let px: Vec<(f32, f32)> = (0..w * h)
        .into_par_iter()
        .map(|p| {
            let wx = g.x0 + ((p % w) as f64 + 0.5) * g.upp;
            let wy = g.y1 - ((p / w) as f64 + 0.5) * g.upp;
            let fx = (wx - nav.lo.x) / nav.cell - 0.5;
            let fy = (wy - nav.lo.y) / nav.cell - 0.5;
            let (i0, j0) = (fx.floor() as i64, fy.floor() as i64);
            let (tx, ty) = (fx - i0 as f64, fy - j0 as f64);
            let near = at(fx.round() as i64, fy.round() as i64);
            let Some(nn) = near else { return (f32::INFINITY, f32::INFINITY) };
            let quad = [at(i0, j0), at(i0 + 1, j0), at(i0, j0 + 1), at(i0 + 1, j0 + 1)];
            let z0 = nav.nodes[nn].z;
            let smooth = quad.iter().all(|q| q.is_some_and(|n| (nav.nodes[n].z - z0).abs() <= STEP));
            let get = |d: &[f32]| -> f32 {
                if smooth {
                    let v = quad.map(|q| d[q.unwrap()]);
                    if v.iter().all(|x| x.is_finite()) {
                        let a = v[0] as f64 * (1.0 - tx) + v[1] as f64 * tx;
                        let b = v[2] as f64 * (1.0 - tx) + v[3] as f64 * tx;
                        return (a * (1.0 - ty) + b * ty) as f32;
                    }
                }
                d[nn]
            };
            (get(dt), get(dct))
        })
        .collect();
    let (t, ct) = px.into_iter().unzip();
    Field { w, h, t, ct }
}

fn blend(px: &mut [u8], c: [f64; 3], a: f64) {
    for k in 0..3 {
        px[k] = (px[k] as f64 * (1.0 - a) + c[k] * a).round().clamp(0.0, 255.0) as u8;
    }
}

fn band(t: f32, iv: f64) -> i64 {
    if t.is_finite() { (t as f64 / iv).floor() as i64 } else { i64::MIN }
}

enum Kind {
    Both,
    Team(bool),
}

fn paint_heat(base: &[u8], f: &Field, kind: &Kind, tmax: f64, iv: f64) -> Vec<u8> {
    let mut out = base.to_vec();
    let val = |i: usize| match kind {
        Kind::Both => f.first(i),
        Kind::Team(true) => f.t[i],
        Kind::Team(false) => f.ct[i],
    };
    let side = |i: usize| f.t[i] <= f.ct[i];
    for y in 0..f.h {
        for x in 0..f.w {
            let i = y * f.w + x;
            let v = val(i);
            if !v.is_finite() {
                continue;
            }
            let p = &mut out[i * 4..i * 4 + 4];
            let fr = v as f64 / tmax;
            match kind {
                Kind::Both => blend(p, team_color(side(i), fr), 0.62),
                Kind::Team(_) => blend(p, viridis(fr), 0.7),
            }
            let b = band(v, iv);
            let mut edge = false;
            let mut contact = false;
            for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                if nx >= f.w || ny >= f.h {
                    continue;
                }
                let j = ny * f.w + nx;
                if val(j).is_finite() && band(val(j), iv) != b {
                    edge = true;
                }
                if matches!(kind, Kind::Both) && f.t[i].is_finite() && f.ct[i].is_finite() && f.t[j].is_finite() && f.ct[j].is_finite() && side(i) != side(j) {
                    contact = true;
                }
            }
            if contact {
                blend(p, [255.0, 255.0, 255.0], 1.0);
            } else if edge {
                blend(p, [0.0, 0.0, 0.0], 0.8);
            }
        }
    }
    out
}

fn percentile(mut v: Vec<f32>, q: f64) -> f64 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return 1.0;
    }
    v.sort_by(f32::total_cmp);
    (v[((v.len() - 1) as f64 * q) as usize] as f64).max(1.0)
}

pub fn export_timing(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &TimingOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<Vec<PathBuf>> {
    let t0 = std::time::Instant::now();
    rep.step(0.0)?;
    let nav = Nav::build(bsp, o.cell, o.speed)?;
    rep.step(0.35)?;
    let count = |cls: &str| bsp.entities.iter().filter(|e| e.class() == cls).count();
    let spawn = |cls: &str| -> Vec<usize> {
        bsp.entities
            .iter()
            .filter(|e| e.class() == cls)
            .filter_map(|e| e.origin())
            .filter_map(|p| nav.nearest(p, STAND_OFS))
            .collect()
    };
    let (st, sct) = (spawn("info_player_deathmatch"), spawn("info_player_start"));
    if st.is_empty() && sct.is_empty() {
        let n = bsp.entities.iter().filter(|e| e.class().starts_with("info_player_")).count();
        bail!("none of the {n} spawn points is on walkable ground");
    }
    let dt = nav.flood(&st, o.speed);
    let dct = nav.flood(&sct, o.speed);
    rep.step(0.5)?;
    let reach = (0..nav.len()).filter(|&n| dt[n].is_finite() || dct[n].is_finite()).count();
    rep.log(format!(
        "  nav: {} floor cells, {} reachable, {} ladders, {} blocking entities, spawns placed T {}/{} CT {}/{} ({:.1}s)",
        nav.len(),
        reach,
        nav.ladders,
        nav.blockers,
        st.len(),
        count("info_player_deathmatch"),
        sct.len(),
        count("info_player_start"),
        t0.elapsed().as_secs_f64()
    ));

    let zs = zones(bsp);
    let rows: Vec<(String, f32, f32)> =
        zs.iter().map(|z| (z.label.clone(), zone_time(&nav, z, &dt), zone_time(&nav, z, &dct))).collect();
    let mut txt = String::new();
    writeln!(txt, "{name} rush timings")?;
    writeln!(
        txt,
        "{} units/s on a {}-unit grid; steps up to 18, crouch-jumps up to 63 (+{JUMP_COST} s), crouch-only areas at 1/3 speed, ladders {LADDER_SPEED} units/s",
        o.speed, o.cell
    )?;
    writeln!(txt, "seconds after freeze time for the first player of each team to arrive")?;
    writeln!(txt)?;
    writeln!(txt, "{:<14}{:>8}{:>8}", "", "T", "CT")?;
    for (l, a, b) in &rows {
        writeln!(txt, "{:<14}{:>8}{:>8}", l, fmt_t(*a), fmt_t(*b))?;
    }
    for line in txt.lines().skip(4) {
        rep.log(format!("  {line}"));
    }

    let g = grid_frame(r, cuts, o.size);
    let view = View {
        basis: top_down(DVec3::X, DVec3::Y),
        cx: (g.x0 + g.x1) / 2.0,
        cy: (g.y0 + g.y1) / 2.0,
        w: g.x1 - g.x0,
        h: g.y1 - g.y0,
        sky_yaw: None,
    };
    let rc = Cuts { clip: NO_CLIP, use_mask: false, ..*cuts };
    let mut base = r.render_view(&view, g.wpx, g.hpx, 2, &rc, true, Some([0x1c, 0x1c, 0x1c]))?.into_raw();
    for p in base.chunks_exact_mut(4) {
        let l = 0.3 * p[0] as f64 + 0.59 * p[1] as f64 + 0.11 * p[2] as f64;
        for k in 0..3 {
            p[k] = ((p[k] as f64 * 0.4 + l * 0.6) * 0.75) as u8;
        }
    }
    let field = sample_field(&nav, &dt, &dct, cuts.zmax, &g);
    let firsts: Vec<f32> = (0..field.t.len()).map(|i| field.first(i)).collect();
    let tmax_both = percentile(firsts, 0.98);
    let tmax_t = percentile(field.t.clone(), 0.98);
    let tmax_ct = percentile(field.ct.clone(), 0.98);
    rep.step(0.7)?;

    let sx = |x: f64| ((x - g.x0) / g.upp) as f32;
    let sy = |y: f64| ((g.y1 - y) / g.upp) as f32;
    let text = Text::new(14.0);
    let small = Text::new(12.0);
    let spawns: Vec<(DVec3, bool)> = bsp
        .entities
        .iter()
        .filter_map(|e| match e.class() {
            "info_player_deathmatch" => e.origin().map(|p| (p, true)),
            "info_player_start" => e.origin().map(|p| (p, false)),
            _ => None,
        })
        .collect();

    let mut files = Partial::default();
    for (k, (suffix, kind, tmax)) in [
        ("", Kind::Both, tmax_both),
        ("_t", Kind::Team(true), tmax_t),
        ("_ct", Kind::Team(false), tmax_ct),
    ]
    .into_iter()
    .enumerate()
    {
        rep.step(0.7 + 0.1 * k as f32)?;
        let px = paint_heat(&base, &field, &kind, tmax, o.interval);
        let mut pm = Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(g.wpx, g.hpx).context("bad size")?)
            .context("pixmap")?;
        let (fw, fh) = (g.wpx as f32, g.hpx as f32);

        for (p, is_t) in &spawns {
            if let Some(c) = PathBuilder::from_circle(sx(p.x), sy(p.y), 3.5) {
                let col = if *is_t { T } else { CT };
                pm.fill_path(&c, &paint(col, 255), tiny_skia::FillRule::Winding, Transform::identity(), None);
                pm.stroke_path(&c, &paint([0, 0, 0], 255), &Stroke::default(), Transform::identity(), None);
            }
        }
        for (z, (label, a, b)) in zs.iter().zip(&rows) {
            let (x0, y0, x1, y1) = (sx(z.lo.x), sy(z.hi.y), sx(z.hi.x), sy(z.lo.y));
            if z.boxed {
                outline(&mut pm, x0, y0, x1, y1, z.color, 2.0);
            } else if let Some(c) = PathBuilder::from_circle((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0) {
                pm.stroke_path(&c, &paint(z.color, 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
            }
            let s = match kind {
                Kind::Both => format!("{label}  T {}  CT {}", fmt_t(*a), fmt_t(*b)),
                Kind::Team(true) => format!("{label}  {} s", fmt_t(*a)),
                Kind::Team(false) => format!("{label}  {} s", fmt_t(*b)),
            };
            let (tw, th) = small.size(&s);
            let (lx, ly) = (x0.clamp(4.0, (fw - tw - 8.0).max(4.0)), (y0 - th - 6.0).max(4.0));
            rect(&mut pm, lx - 3.0, ly - 1.0, lx + tw + 3.0, ly + th + 1.0, [0, 0, 0], 200);
            small.draw(&mut pm, lx, ly, &s, z.color);
        }

        let title = match kind {
            Kind::Both => format!("{name}  rush timings  {} u/s", o.speed),
            Kind::Team(true) => format!("{name}  T arrival times  {} u/s", o.speed),
            Kind::Team(false) => format!("{name}  CT arrival times  {} u/s", o.speed),
        };
        let (tw, th) = text.size(&title);
        rect(&mut pm, 4.0, 4.0, tw + 14.0, th + 12.0, [0, 0, 0], 200);
        text.draw(&mut pm, 9.0, 8.0, &title, [255, 255, 255]);

        let lw = 190.0;
        let (lx, ly) = (fw - lw - 8.0, fh - 92.0);
        rect(&mut pm, lx - 8.0, ly - 8.0, fw - 4.0, fh - 4.0, [0, 0, 0], 200);
        match kind {
            Kind::Both => {
                let items: [([u8; 3], String); 4] = [
                    ([225, 110, 60], "T arrives first".into()),
                    ([70, 130, 220], "CT arrives first".into()),
                    ([255, 255, 255], "both arrive together".into()),
                    ([30, 30, 30], format!("lines every {} s", o.interval)),
                ];
                for (k, (c, s)) in items.iter().enumerate() {
                    let yy = ly + k as f32 * 20.0;
                    rect(&mut pm, lx, yy + 3.0, lx + 12.0, yy + 15.0, *c, 255);
                    if k == 3 {
                        outline(&mut pm, lx, yy + 3.0, lx + 12.0, yy + 15.0, [200, 200, 200], 1.0);
                    }
                    small.draw(&mut pm, lx + 20.0, yy + 1.0, s, [255, 255, 255]);
                }
            }
            Kind::Team(_) => {
                let bw = lw - 10.0;
                for k in 0..bw as usize {
                    let c = viridis(k as f64 / bw as f64).map(|v| v as u8);
                    line(&mut pm, (lx + k as f32, ly + 24.0), (lx + k as f32, ly + 44.0), c, 255, 1.2);
                }
                small.draw(&mut pm, lx, ly, &format!("seconds, lines every {} s", o.interval), [255, 255, 255]);
                small.draw(&mut pm, lx, ly + 50.0, "0", [255, 255, 255]);
                let e = format!("{tmax:.0}+");
                let (ew, _) = small.size(&e);
                small.draw(&mut pm, lx + bw - ew, ly + 50.0, &e, [255, 255, 255]);
            }
        }

        let rgb: Vec<u8> = pm.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let f = free_name(out, &format!("{name}{cut_tag}_timing{suffix}"), ".png");
        files.add(f.clone());
        image::RgbImage::from_raw(g.wpx, g.hpx, rgb).context("image")?.save(&f)?;
        rep.log(format!("  {}", f.display()));
    }
    rep.step(1.0)?;
    let f = free_name(out, &format!("{name}{cut_tag}_timing"), ".txt");
    files.add(f.clone());
    std::fs::write(&f, txt)?;
    rep.log(format!("  {}", f.display()));
    Ok(files.keep())
}
