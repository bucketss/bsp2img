use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use fast_surface_nets::ndshape::RuntimeShape;
use fast_surface_nets::{SurfaceNetsBuffer, surface_nets};
use glam::DVec3;
use rayon::prelude::*;

use crate::bsp::{Bsp, CONTENTS_SKY, CONTENTS_SOLID};
use crate::nav::{BLOCKERS, Nav, STAND_OFS};
use crate::paths::{Partial, free_name};
use crate::render::Cuts;
use crate::scene::Report;

const NAV_CELL: f64 = 8.0;
const HEADROOM: f64 = 36.0;
const MIN_PRINT_MM: f64 = 0.8;
const SMOOTH_MIN: f32 = 0.25;
const MAX_VOXELS: usize = 1_500_000_000;
const CLOSED: &[&str] = &["func_door", "func_door_rotating"];

#[derive(Clone, Debug, PartialEq)]
pub struct StlOpts {
    pub voxel: f64,
    pub wall: f64,
    pub base: f64,
    pub print_width: f64,
    pub smooth: bool,
}

impl Default for StlOpts {
    fn default() -> Self {
        StlOpts { voxel: 8.0, wall: 32.0, base: 16.0, print_width: 200.0, smooth: false }
    }
}

struct Brush {
    model: usize,
    origin: DVec3,
    lo: DVec3,
    hi: DVec3,
}

fn brushes(bsp: &Bsp) -> Vec<Brush> {
    bsp.entities
        .iter()
        .filter(|e| {
            let flags: i64 = e.get("spawnflags").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
            BLOCKERS.contains(&e.class())
                || CLOSED.contains(&e.class())
                || (e.class() == "func_breakable" && flags & 1 != 0)
        })
        .filter_map(|e| {
            let mi = e.model().filter(|&m| m > 0 && m < bsp.models.len())?;
            let m = &bsp.models[mi];
            let origin = e.origin().unwrap_or(DVec3::ZERO);
            Some(Brush { model: mi, origin, lo: m.mins + origin, hi: m.maxs + origin })
        })
        .collect()
}

fn solid_at(bsp: &Bsp, br: &[Brush], p: DVec3) -> bool {
    let c = bsp.leaf_contents(0, p);
    if c == CONTENTS_SOLID || c == CONTENTS_SKY {
        return true;
    }
    br.iter().any(|b| {
        p.cmpge(b.lo).all() && p.cmple(b.hi).all() && bsp.leaf_contents(b.model, p - b.origin) == CONTENTS_SOLID
    })
}

struct Grid {
    n: [usize; 3],
    lo: DVec3,
    v: f64,
    s: Vec<u8>,
}

impl Grid {
    fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        x + self.n[0] * (y + self.n[1] * z)
    }

    fn block(&self, x: usize, y: usize, z: usize) -> u8 {
        (0..8).fold(0u8, |m, i| m | (self.s[self.idx(x + (i & 1), y + ((i >> 1) & 1), z + (i >> 2))] & 1) << i)
    }

    fn inner(&self, p: [usize; 3]) -> bool {
        (0..3).all(|a| p[a] > 0 && p[a] + 1 < self.n[a])
    }
}

struct Region {
    grid: Grid,
    foot: Vec<bool>,
    floor: f64,
    top: f64,
}

fn region(bsp: &Bsp, cuts: &Cuts, o: &StlOpts, rep: &mut Report) -> Result<Region> {
    let nav = Nav::build(bsp, NAV_CELL, 250.0)?;
    let seeds: Vec<usize> = bsp
        .entities
        .iter()
        .filter(|e| matches!(e.class(), "info_player_deathmatch" | "info_player_start"))
        .filter_map(|e| e.origin())
        .filter_map(|p| nav.nearest(p, STAND_OFS))
        .collect();
    let dist = if seeds.is_empty() {
        rep.log("  no spawn on walkable ground, using every floor cell".into());
        vec![0.0; nav.len()]
    } else {
        nav.flood(&seeds, 250.0)
    };
    let [cx0, cy0, cx1, cy1] = cuts.clip;
    let inside = |x: f64, y: f64| x >= cx0 && x <= cx1 && y >= cy0 && y <= cy1;
    let reach: Vec<usize> = (0..nav.len())
        .filter(|&n| dist[n].is_finite())
        .filter(|&n| {
            let (x, y) = nav.cell_xy(nav.col[n] as usize);
            inside(x, y)
        })
        .collect();
    if reach.is_empty() {
        bail!("no walkable floor inside the cuts");
    }
    let floor = reach.iter().map(|&n| nav.nodes[n].z as f64).fold(f64::INFINITY, f64::min);
    let ceil = reach.iter().map(|&n| nav.nodes[n].top as f64 + HEADROOM).fold(f64::NEG_INFINITY, f64::max);
    let top = ceil.min(cuts.zmax);
    if top <= floor + o.voxel {
        bail!("top {top:.0} is at or below the lowest floor {floor:.0}");
    }
    let mut col_reach = vec![false; nav.nx * nav.ny];
    let (mut bx0, mut by0, mut bx1, mut by1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &n in &reach {
        let c = nav.col[n] as usize;
        col_reach[c] = true;
        let (x, y) = nav.cell_xy(c);
        (bx0, by0, bx1, by1) = (bx0.min(x), by0.min(y), bx1.max(x), by1.max(y));
    }
    let h = nav.cell / 2.0 + o.wall;
    let (bx0, by0, bx1, by1) = ((bx0 - h).max(cx0), (by0 - h).max(cy0), (bx1 + h).min(cx1), (by1 + h).min(cy1));
    let v = o.voxel;
    let lo = DVec3::new((bx0 / v).floor() * v - v, (by0 / v).floor() * v - v, ((floor - o.base) / v).floor() * v - v);
    let n = [
        ((bx1 - lo.x) / v).ceil() as usize + 1,
        ((by1 - lo.y) / v).ceil() as usize + 1,
        ((top - lo.z) / v).ceil() as usize + 1,
    ];
    let total = n[0] * n[1] * n[2];
    if total > MAX_VOXELS {
        bail!("{}x{}x{} voxels is too many; raise --voxel", n[0], n[1], n[2]);
    }
    let (nx, ny) = (n[0], n[1]);
    let center = |i: usize, j: usize| (lo.x + (i as f64 + 0.5) * v, lo.y + (j as f64 + 0.5) * v);
    let walk: Vec<bool> = (0..nx * ny)
        .map(|k| {
            let (x, y) = center(k % nx, k / nx);
            let ci = ((x - nav.lo.x) / nav.cell).floor();
            let cj = ((y - nav.lo.y) / nav.cell).floor();
            ci >= 0.0
                && cj >= 0.0
                && (ci as usize) < nav.nx
                && (cj as usize) < nav.ny
                && col_reach[ci as usize * nav.ny + cj as usize]
        })
        .collect();
    let r = (o.wall / v).max(0.0);
    let ri = r.floor() as i64;
    let offs: Vec<(i64, i64)> = (-ri..=ri)
        .flat_map(|a| (-ri..=ri).map(move |b| (a, b)))
        .filter(|&(a, b)| ((a * a + b * b) as f64) <= r * r + 1e-9)
        .collect();
    let mut foot = walk.clone();
    for j in 0..ny {
        for i in 0..nx {
            if !walk[i + nx * j] {
                continue;
            }
            let edge = [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)].iter().any(|&(a, b)| {
                let (x, y) = (i as i64 + a, j as i64 + b);
                x < 0 || y < 0 || x >= nx as i64 || y >= ny as i64 || !walk[x as usize + nx * y as usize]
            });
            if !edge {
                continue;
            }
            for &(a, b) in &offs {
                let (x, y) = (i as i64 + a, j as i64 + b);
                if x >= 0 && y >= 0 && x < nx as i64 && y < ny as i64 {
                    foot[x as usize + nx * y as usize] = true;
                }
            }
        }
    }
    for j in 0..ny {
        for i in 0..nx {
            let (x, y) = center(i, j);
            if i == 0 || j == 0 || i + 1 == nx || j + 1 == ny || !inside(x, y) {
                foot[i + nx * j] = false;
            }
        }
    }
    Ok(Region { grid: Grid { n, lo, v, s: Vec::new() }, foot, floor, top })
}

fn classify(bsp: &Bsp, reg: &mut Region, base: f64) {
    let br = brushes(bsp);
    let g = &mut reg.grid;
    let [nx, ny, nz] = g.n;
    let (lo, v) = (g.lo, g.v);
    let (floor, top, bottom) = (reg.floor, reg.top, reg.floor - base);
    let foot = &reg.foot;
    let mut s = vec![0u8; nx * ny * nz];
    s.par_chunks_mut(nx).enumerate().for_each(|(row, line)| {
        let (j, z) = (row % ny, row / ny);
        let zc = lo.z + (z as f64 + 0.5) * v;
        if zc <= bottom || zc >= top {
            return;
        }
        let y = lo.y + (j as f64 + 0.5) * v;
        for (i, c) in line.iter_mut().enumerate() {
            if !foot[i + nx * j] {
                continue;
            }
            let x = lo.x + (i as f64 + 0.5) * v;
            *c = (zc < floor || solid_at(bsp, &br, DVec3::new(x, y, zc))) as u8;
        }
    });
    g.s = s;
}

fn comps(m: u8) -> (u32, [u8; 8]) {
    let mut label = [u8::MAX; 8];
    let mut count = 0;
    for i in 0..8 {
        if m & 1 << i == 0 || label[i] != u8::MAX {
            continue;
        }
        let mut stack = vec![i];
        label[i] = count as u8;
        while let Some(a) = stack.pop() {
            for b in [a ^ 1, a ^ 2, a ^ 4] {
                if m & 1 << b != 0 && label[b] == u8::MAX {
                    label[b] = count as u8;
                    stack.push(b);
                }
            }
        }
        count += 1;
    }
    (count, label)
}

fn bad(m: u8) -> bool {
    m != 0 && m != 255 && (comps(m).0 > 1 || comps(!m).0 > 1)
}

fn fix(mut m: u8, allowed: u8) -> u8 {
    for _ in 0..8 {
        if comps(m).0 > 1 {
            let best = (0..8)
                .filter(|&i| m & 1 << i == 0 && allowed & 1 << i != 0)
                .max_by_key(|&i: &usize| [i ^ 1, i ^ 2, i ^ 4].iter().filter(|&&b| m & 1 << b != 0).count());
            match best {
                Some(i) => m |= 1 << i,
                None => break,
            }
        } else {
            let e = !m;
            let (count, label) = comps(e);
            if count <= 1 {
                break;
            }
            let size = |c: u8| (0..8).filter(|&i| label[i] == c).count();
            let keep = (0..8)
                .find(|&i| e & 1 << i != 0 && allowed & 1 << i == 0)
                .map(|i| label[i])
                .unwrap_or_else(|| (0..count as u8).max_by_key(|&c| size(c)).unwrap_or(0));
            let fill = (0..8).filter(|&i| e & 1 << i != 0 && label[i] != keep).fold(0u8, |f, i| f | 1 << i);
            if fill & allowed == 0 {
                break;
            }
            m |= fill & allowed;
        }
    }
    m
}

fn well_composed(g: &mut Grid) -> usize {
    let table: Vec<bool> = (0..256).map(|m| bad(m as u8)).collect();
    let [nx, ny, nz] = g.n;
    let mut work: Vec<[usize; 3]> = (0..nz - 1)
        .into_par_iter()
        .flat_map_iter(|z| {
            let g = &*g;
            let table = &table;
            (0..ny - 1).flat_map(move |y| {
                (0..nx - 1).filter(move |&x| table[g.block(x, y, z) as usize]).map(move |x| [x, y, z])
            })
        })
        .collect();
    let mut filled = 0;
    while let Some([x, y, z]) = work.pop() {
        let m = g.block(x, y, z);
        if !table[m as usize] {
            continue;
        }
        let corner = |i: usize| [x + (i & 1), y + ((i >> 1) & 1), z + (i >> 2)];
        let allowed = (0..8).filter(|&i| g.inner(corner(i))).fold(0u8, |a, i| a | 1 << i);
        let m2 = fix(m, allowed);
        if m2 == m {
            continue;
        }
        for i in (0..8).filter(|&i| (m2 & !m) & 1 << i != 0) {
            let p = corner(i);
            let k = g.idx(p[0], p[1], p[2]);
            g.s[k] = 1;
            filled += 1;
            for a in p[0] - 1..=p[0] {
                for b in p[1] - 1..=p[1] {
                    for c in p[2] - 1..=p[2] {
                        if a + 1 < nx && b + 1 < ny && c + 1 < nz {
                            work.push([a, b, c]);
                        }
                    }
                }
            }
        }
    }
    filled
}

fn flood(g: &mut Grid, start: usize, from: u8, to: u8) -> usize {
    let [nx, ny, nz] = g.n;
    let sz = nx * ny;
    let mut stack = vec![start];
    g.s[start] = to;
    let mut count = 0;
    while let Some(k) = stack.pop() {
        count += 1;
        let (x, y, z) = (k % nx, (k / nx) % ny, k / sz);
        let near = [
            (x > 0, k.wrapping_sub(1)),
            (x + 1 < nx, k + 1),
            (y > 0, k.wrapping_sub(nx)),
            (y + 1 < ny, k + nx),
            (z > 0, k.wrapping_sub(sz)),
            (z + 1 < nz, k + sz),
        ];
        for (ok, k2) in near {
            if ok && g.s[k2] == from {
                g.s[k2] = to;
                stack.push(k2);
            }
        }
    }
    count
}

fn drop_islands(g: &mut Grid) -> (usize, usize) {
    let mut parts = Vec::new();
    for k in 0..g.s.len() {
        if g.s[k] == 1 {
            parts.push((flood(g, k, 1, 3), k));
        }
    }
    let Some(&(_, main)) = parts.iter().max() else { return (0, 0) };
    let mut gone = 0;
    for &(n, k) in &parts {
        if k != main {
            flood(g, k, 3, 0);
            gone += n;
        }
    }
    g.s.par_iter_mut().for_each(|c| {
        if *c == 3 {
            *c = 1;
        }
    });
    (parts.len() - 1, gone)
}

fn fill_pockets(g: &mut Grid) -> usize {
    flood(g, 0, 0, 2);
    g.s.par_iter_mut()
        .map(|c| match *c {
            0 => {
                *c = 1;
                1
            }
            2 => {
                *c = 0;
                0
            }
            _ => 0,
        })
        .sum()
}

struct Mesh {
    pos: Vec<DVec3>,
    tris: Vec<[u32; 3]>,
}

struct Quad {
    a: usize,
    k: i32,
    u0: i32,
    v0: i32,
    u1: i32,
    v1: i32,
    pos: bool,
}

fn point(a: usize, k: i32, u: i32, v: i32) -> [i32; 3] {
    let mut p = [0; 3];
    p[a] = k;
    p[(a + 1) % 3] = u;
    p[(a + 2) % 3] = v;
    p
}

fn quads(g: &Grid) -> Vec<Quad> {
    let n = g.n;
    let planes: Vec<(usize, usize)> = (0..3).flat_map(|a| (1..n[a]).map(move |k| (a, k))).collect();
    planes
        .into_par_iter()
        .flat_map_iter(|(a, k)| {
            let (ua, va) = ((a + 1) % 3, (a + 2) % 3);
            let (nu, nv) = (n[ua], n[va]);
            let solid = |p: [usize; 3]| g.s[g.idx(p[0], p[1], p[2])] != 0;
            let mut m = vec![0i8; nu * nv];
            for vv in 0..nv {
                for uu in 0..nu {
                    let mut p = [0; 3];
                    p[a] = k;
                    p[ua] = uu;
                    p[va] = vv;
                    let s1 = solid(p);
                    p[a] = k - 1;
                    let s0 = solid(p);
                    m[uu + nu * vv] = if s0 && !s1 {
                        1
                    } else if s1 && !s0 {
                        -1
                    } else {
                        0
                    };
                }
            }
            let mut out = Vec::new();
            for vv in 0..nv {
                let mut uu = 0;
                while uu < nu {
                    let c = m[uu + nu * vv];
                    if c == 0 {
                        uu += 1;
                        continue;
                    }
                    let mut w = 1;
                    while uu + w < nu && m[uu + w + nu * vv] == c {
                        w += 1;
                    }
                    let mut h = 1;
                    while vv + h < nv && (0..w).all(|t| m[uu + t + nu * (vv + h)] == c) {
                        h += 1;
                    }
                    for dv in 0..h {
                        m[uu + nu * (vv + dv)..uu + w + nu * (vv + dv)].fill(0);
                    }
                    out.push(Quad {
                        a,
                        k: k as i32,
                        u0: uu as i32,
                        v0: vv as i32,
                        u1: (uu + w) as i32,
                        v1: (vv + h) as i32,
                        pos: c > 0,
                    });
                    uu += w;
                }
            }
            out
        })
        .collect()
}

fn blocky(g: &Grid) -> Mesh {
    let qs = quads(g);
    let lines: Vec<Vec<[i32; 3]>> = (0..3)
        .map(|t| {
            let mut l: Vec<[i32; 3]> = qs
                .par_iter()
                .flat_map_iter(|q| {
                    [(q.u0, q.v0), (q.u1, q.v0), (q.u1, q.v1), (q.u0, q.v1)].map(|(u, v)| {
                        let p = point(q.a, q.k, u, v);
                        [p[(t + 1) % 3], p[(t + 2) % 3], p[t]]
                    })
                })
                .collect();
            l.par_sort_unstable();
            l.dedup();
            l
        })
        .collect();
    let between = |t: usize, p: [i32; 3], lo: i32, hi: i32| -> &[[i32; 3]] {
        let l = &lines[t];
        let (o1, o2) = (p[(t + 1) % 3], p[(t + 2) % 3]);
        let a = l.partition_point(|e| *e <= [o1, o2, lo]);
        let b = l.partition_point(|e| *e < [o1, o2, hi]);
        &l[a..b.max(a)]
    };
    let loops: Vec<(Vec<[i32; 3]>, [i32; 3])> = qs
        .par_iter()
        .map(|q| {
            let (ua, va) = ((q.a + 1) % 3, (q.a + 2) % 3);
            let c = [
                point(q.a, q.k, q.u0, q.v0),
                point(q.a, q.k, q.u1, q.v0),
                point(q.a, q.k, q.u1, q.v1),
                point(q.a, q.k, q.u0, q.v1),
            ];
            let mut lp = Vec::with_capacity(4);
            let set = |e: &[i32; 3], t: usize, fixed: [i32; 3]| {
                let mut p = fixed;
                p[t] = e[2];
                p
            };
            lp.push(c[0]);
            lp.extend(between(ua, c[0], q.u0, q.u1).iter().map(|e| set(e, ua, c[0])));
            lp.push(c[1]);
            lp.extend(between(va, c[1], q.v0, q.v1).iter().map(|e| set(e, va, c[1])));
            lp.push(c[2]);
            lp.extend(between(ua, c[3], q.u0, q.u1).iter().rev().map(|e| set(e, ua, c[3])));
            lp.push(c[3]);
            lp.extend(between(va, c[0], q.v0, q.v1).iter().rev().map(|e| set(e, va, c[0])));
            if !q.pos {
                lp.reverse();
            }
            let mid = [c[0][0] + c[2][0], c[0][1] + c[2][1], c[0][2] + c[2][2]];
            (lp, mid)
        })
        .collect();
    let mut ids: HashMap<[i32; 3], u32> = HashMap::new();
    let mut pos = Vec::new();
    let mut tris = Vec::new();
    let mut id = |k: [i32; 3], pos: &mut Vec<DVec3>| {
        *ids.entry(k).or_insert_with(|| {
            pos.push(g.lo + DVec3::new(k[0] as f64, k[1] as f64, k[2] as f64) * (g.v / 2.0));
            pos.len() as u32 - 1
        })
    };
    for (lp, mid) in &loops {
        let v: Vec<u32> = lp.iter().map(|p| id([p[0] * 2, p[1] * 2, p[2] * 2], &mut pos)).collect();
        if v.len() == 4 {
            tris.push([v[0], v[1], v[2]]);
            tris.push([v[0], v[2], v[3]]);
        } else {
            let m = id(*mid, &mut pos);
            for i in 0..v.len() {
                tris.push([m, v[i], v[(i + 1) % v.len()]]);
            }
        }
    }
    Mesh { pos, tris }
}

fn box3(src: &[u8], n: [usize; 3], stride: usize, axis: usize) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    out.par_iter_mut().enumerate().for_each(|(k, o)| {
        let c = [k % n[0], (k / n[0]) % n[1], k / (n[0] * n[1])][axis];
        let mut s = src[k];
        if c > 0 {
            s += src[k - stride];
        }
        if c + 1 < n[axis] {
            s += src[k + stride];
        }
        *o = s;
    });
    out
}

fn smooth(g: &Grid) -> Mesh {
    let n = g.n;
    let occ = {
        let a = box3(&g.s, n, 1, 0);
        let b = box3(&a, n, n[0], 1);
        drop(a);
        box3(&b, n, n[0] * n[1], 2)
    };
    let m = [n[0] + 1, n[1] + 1, n[2] + 1];
    let sdf: Vec<f32> = (0..m[0] * m[1] * m[2])
        .into_par_iter()
        .map(|k| {
            let (x, y, z) = (k % m[0], (k / m[0]) % m[1], k / (m[0] * m[1]));
            if x >= n[0] || y >= n[1] || z >= n[2] {
                return 1.0;
            }
            let i = g.idx(x, y, z);
            let b = 2.0 * occ[i] as f32 / 27.0 - 1.0;
            if g.s[i] != 0 { -b.max(SMOOTH_MIN) } else { (-b).max(SMOOTH_MIN) }
        })
        .collect();
    drop(occ);
    let shape = RuntimeShape::<u32, 3>::new([m[0] as u32, m[1] as u32, m[2] as u32]);
    let mut buf = SurfaceNetsBuffer::default();
    surface_nets(&sdf, &shape, [0; 3], [n[0] as u32, n[1] as u32, n[2] as u32], &mut buf);
    drop(sdf);
    let pos = buf
        .positions
        .iter()
        .map(|p| g.lo + (DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64) + 0.5) * g.v)
        .collect();
    let tris = buf.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    Mesh { pos, tris }
}

fn volume(m: &Mesh) -> f64 {
    m.tris.par_iter().map(|t| m.pos[t[0] as usize].dot(m.pos[t[1] as usize].cross(m.pos[t[2] as usize])) / 6.0).sum()
}

fn check(m: &Mesh) -> Result<()> {
    let degenerate = m.tris.iter().filter(|t| t[0] == t[1] || t[1] == t[2] || t[0] == t[2]).count();
    let mut e: Vec<u64> = m
        .tris
        .par_iter()
        .flat_map_iter(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
        .map(|(a, b)| (a as u64) << 32 | b as u64)
        .collect();
    e.par_sort_unstable();
    let shared = e.windows(2).filter(|w| w[0] == w[1]).count();
    let open = e.par_iter().filter(|&&k| e.binary_search(&((k & 0xffff_ffff) << 32 | k >> 32)).is_err()).count();
    if degenerate + shared + open > 0 {
        bail!("mesh is not watertight: {open} open edges, {shared} edges shared by more than 2 triangles, {degenerate} degenerate triangles");
    }
    Ok(())
}

fn write_stl(path: &Path, name: &str, m: &Mesh, min: DVec3, scale: f64) -> Result<()> {
    let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    let mut head = [b' '; 80];
    let text = format!("bsp2img {name}");
    let t = text.as_bytes();
    head[..t.len().min(80)].copy_from_slice(&t[..t.len().min(80)]);
    w.write_all(&head)?;
    w.write_all(&(m.tris.len() as u32).to_le_bytes())?;
    let mut rec = [0u8; 50];
    for t in &m.tris {
        let p = t.map(|i| (m.pos[i as usize] - min) * scale);
        let nrm = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        for (i, v) in [nrm, p[0], p[1], p[2]].iter().enumerate() {
            for (j, c) in [v.x, v.y, v.z].iter().enumerate() {
                let o = i * 12 + j * 4;
                rec[o..o + 4].copy_from_slice(&(*c as f32).to_le_bytes());
            }
        }
        w.write_all(&rec)?;
    }
    w.flush()?;
    Ok(())
}

pub fn export_stl(
    bsp: &Bsp,
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &StlOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<Vec<PathBuf>> {
    if !(o.voxel >= 1.0) || !(o.print_width > 0.0) || o.wall < 0.0 || o.base < 0.0 {
        bail!("voxel must be at least 1, print width above 0, wall and base not negative");
    }
    let t0 = Instant::now();
    rep.step(0.0)?;
    let mut reg = region(bsp, cuts, o, rep)?;
    rep.step(0.15)?;
    classify(bsp, &mut reg, o.base);
    let n = reg.grid.n;
    rep.log(format!(
        "  grid {}x{}x{} at {} units, floor {:.0}, top {:.0} ({:.1}s)",
        n[0],
        n[1],
        n[2],
        o.voxel,
        reg.floor,
        reg.top,
        t0.elapsed().as_secs_f64()
    ));
    rep.step(0.5)?;
    let g = &mut reg.grid;
    let fixed = well_composed(g);
    let pockets = fill_pockets(g);
    let (islands, loose) = drop_islands(g);
    let solid = g.s.par_iter().filter(|&&c| c != 0).count();
    rep.log(format!(
        "  {solid} solid voxels, {fixed} added at diagonal contacts, {pockets} in sealed pockets, {islands} floating parts ({loose} voxels) removed"
    ));
    if solid == 0 {
        bail!("nothing solid in the region");
    }
    rep.step(0.6)?;
    let mut mesh = if o.smooth { smooth(g) } else { blocky(g) };
    drop(std::mem::take(&mut g.s));
    rep.step(0.85)?;
    if volume(&mesh) < 0.0 {
        for t in &mut mesh.tris {
            t.swap(1, 2);
        }
    }
    check(&mesh)?;
    rep.step(0.9)?;
    let (mut min, mut max) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for p in &mesh.pos {
        (min, max) = (min.min(*p), max.max(*p));
    }
    let size = max - min;
    let scale = o.print_width / size.max_element();
    let mm = size * scale;
    if o.voxel * scale < MIN_PRINT_MM {
        rep.log(format!(
            "  warning: a voxel is {:.2} mm at this size; walls under {MIN_PRINT_MM} mm may not print (raise --voxel or --print-width)",
            o.voxel * scale
        ));
    }
    let mut files = Partial::default();
    let tag = if o.smooth { "_smooth" } else { "" };
    let f = free_name(out, &format!("{name}{cut_tag}{tag}"), ".stl");
    files.add(f.clone());
    write_stl(&f, name, &mesh, min, scale)?;
    rep.log(format!(
        "  {} triangles, {:.0} x {:.0} x {:.0} mm, {:.1} MB, watertight ({:.1}s)",
        mesh.tris.len(),
        mm.x,
        mm.y,
        mm.z,
        (84 + 50 * mesh.tris.len()) as f64 / 1e6,
        t0.elapsed().as_secs_f64()
    ));
    rep.log(format!("  {}", f.display()));
    rep.step(1.0)?;
    Ok(files.keep())
}
