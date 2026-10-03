use std::collections::HashMap;

use super::hull::{CONTENTS_EMPTY, CONTENTS_SOLID, PwMap};
use super::vec::{V3, cmax, cmin};
use super::world::WorldModels;

const ON_EPS: f32 = 0.01;
const FLOOR_NZ: f32 = 0.7;
const SLOPE_FLAT_NZ: f32 = 0.99;
const WALL_NZ: f32 = 0.1;
const MIN_SEAM: f32 = 2.0;
const Z_MATCH: f32 = 2.0;
const XY_MARGIN: f32 = 1.0;

#[derive(Clone, Copy, Debug)]
struct Plane {
    n: V3,
    d: f32,
}

#[derive(Clone, Debug)]
pub struct DecompFace {
    pub n: V3,
    pub d: f32,
    pub w: Vec<V3>,
}

#[derive(Clone, Debug)]
pub struct DecompBrush {
    pub faces: Vec<DecompFace>,
    pub contents: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct Seam {
    pub a: V3,
    pub b: V3,
    pub outn: V3,
    pub floor_model: i32,
    pub wall_model: i32,
    pub fn_: V3,
    pub fd: f32,
    pub slope: bool,
    pub usehull: usize,
}

fn base_winding(n: &V3, d: f32, c: &V3, radius: f32) -> Vec<V3> {
    let mut ax = 0;
    let mut m = n[0].abs();
    if n[1].abs() > m {
        ax = 1;
        m = n[1].abs();
    }
    if n[2].abs() > m {
        ax = 2;
    }
    let mut up = [0f32; 3];
    up[if ax == 2 { 0 } else { 2 }] = 1.0;
    let dn = up[0] * n[0] + up[1] * n[1] + up[2] * n[2];
    for i in 0..3 {
        up[i] -= n[i] * dn;
    }
    let ul = (up[0] * up[0] + up[1] * up[1] + up[2] * up[2]).sqrt();
    if ul < 1e-6 {
        return Vec::new();
    }
    for v in up.iter_mut() {
        *v /= ul;
    }
    let rt = [n[1] * up[2] - n[2] * up[1], n[2] * up[0] - n[0] * up[2], n[0] * up[1] - n[1] * up[0]];
    let cd = c[0] * n[0] + c[1] * n[1] + c[2] * n[2] - d;
    let proj = [c[0] - n[0] * cd, c[1] - n[1] * cd, c[2] - n[2] * cd];
    let mut w = vec![[0f32; 3]; 4];
    for i in 0..3 {
        w[0][i] = proj[i] - rt[i] * radius - up[i] * radius;
        w[1][i] = proj[i] - rt[i] * radius + up[i] * radius;
        w[2][i] = proj[i] + rt[i] * radius + up[i] * radius;
        w[3][i] = proj[i] + rt[i] * radius - up[i] * radius;
    }
    w
}

fn clip_winding(w: Vec<V3>, n: &V3, d: f32) -> Vec<V3> {
    let nw = w.len();
    if nw < 3 {
        return Vec::new();
    }
    let mut dist = vec![0f32; nw];
    let mut side = vec![0usize; nw];
    let mut cnt = [0usize; 3];
    for i in 0..nw {
        let dd = w[i][0] * n[0] + w[i][1] * n[1] + w[i][2] * n[2] - d;
        dist[i] = dd;
        side[i] = if dd > ON_EPS {
            0
        } else if dd < -ON_EPS {
            1
        } else {
            2
        };
        cnt[side[i]] += 1;
    }
    if cnt[0] == 0 {
        return w;
    }
    if cnt[1] == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..nw {
        if side[i] != 0 {
            out.push(w[i]);
        }
        let j = (i + 1) % nw;
        if side[i] == 2 || side[j] == 2 || side[i] == side[j] {
            continue;
        }
        let t = dist[i] / (dist[i] - dist[j]);
        out.push(std::array::from_fn(|k| w[i][k] + t * (w[j][k] - w[i][k])));
    }
    out
}

fn remove_colinear(w: &mut Vec<V3>) {
    let nw = w.len();
    if nw < 3 {
        return;
    }
    let mut out = Vec::new();
    for i in 0..nw {
        let a = w[(i + nw - 1) % nw];
        let b = w[i];
        let c = w[(i + 1) % nw];
        let mut e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let mut e2 = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
        let l1 = (e1[0] * e1[0] + e1[1] * e1[1] + e1[2] * e1[2]).sqrt();
        let l2 = (e2[0] * e2[0] + e2[1] * e2[1] + e2[2] * e2[2]).sqrt();
        if l1 < 1e-5 {
            continue;
        }
        if l2 < 1e-5 {
            out.push(b);
            continue;
        }
        for k in 0..3 {
            e1[k] /= l1;
            e2[k] /= l2;
        }
        let dot = e1[0] * e2[0] + e1[1] * e2[1] + e1[2] * e2[2];
        if dot < 0.9999 {
            out.push(b);
        }
    }
    if out.len() >= 3 {
        *w = out;
    }
}

fn emit_brush(stack: &[Plane], contents: i32, center: &V3, radius: f32, out: &mut Vec<DecompBrush>) {
    let mut b = DecompBrush { faces: Vec::new(), contents };
    for i in 0..stack.len() {
        let mut w = base_winding(&stack[i].n, stack[i].d, center, radius);
        for (j, p) in stack.iter().enumerate() {
            if w.is_empty() {
                break;
            }
            if i == j {
                continue;
            }
            w = clip_winding(w, &p.n, p.d);
        }
        if w.len() < 3 {
            continue;
        }
        remove_colinear(&mut w);
        if w.len() < 3 {
            continue;
        }
        b.faces.push(DecompFace { n: stack[i].n, d: stack[i].d, w });
    }
    if b.faces.len() >= 4 {
        out.push(b);
    }
}

fn recurse(map: &PwMap, num: i32, stack: &mut Vec<Plane>, center: &V3, radius: f32, out: &mut Vec<DecompBrush>) {
    if num < 0 {
        if num != CONTENTS_EMPTY {
            emit_brush(stack, num, center, radius, out);
        }
        return;
    }
    if num as usize >= map.clipnodes.len() {
        return;
    }
    let node = map.clipnodes[num as usize];
    let p = map.planes[node.plane as usize];
    stack.push(Plane { n: [-p.n[0], -p.n[1], -p.n[2]], d: -p.d });
    recurse(map, node.children[0] as i32, stack, center, radius, out);
    stack.pop();
    stack.push(Plane { n: p.n, d: p.d });
    recurse(map, node.children[1] as i32, stack, center, radius, out);
    stack.pop();
}

pub fn decompile_clip_hull(map: &PwMap, root: i32, mins: &V3, maxs: &V3) -> Vec<DecompBrush> {
    let mut out = Vec::new();
    if root < 0 {
        return out;
    }
    const PAD: f32 = 40.0;
    let lo = [mins[0] - PAD, mins[1] - PAD, mins[2] - PAD];
    let hi = [maxs[0] + PAD, maxs[1] + PAD, maxs[2] + PAD];
    let center = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let (dx, dy, dz) = (hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]);
    let radius = 0.5 * (dx * dx + dy * dy + dz * dz).sqrt() + 16.0;
    let mut stack = vec![
        Plane { n: [1.0, 0.0, 0.0], d: hi[0] },
        Plane { n: [-1.0, 0.0, 0.0], d: -lo[0] },
        Plane { n: [0.0, 1.0, 0.0], d: hi[1] },
        Plane { n: [0.0, -1.0, 0.0], d: -lo[1] },
        Plane { n: [0.0, 0.0, 1.0], d: hi[2] },
        Plane { n: [0.0, 0.0, -1.0], d: -lo[2] },
    ];
    recurse(map, root, &mut stack, &center, radius, &mut out);
    out
}

pub fn point_in_brush(b: &DecompBrush, p: &V3, eps: f32) -> bool {
    b.faces.iter().all(|f| !(p[0] * f.n[0] + p[1] * f.n[1] + p[2] * f.n[2] - f.d > eps))
}

#[derive(Clone, Copy)]
struct Box2 {
    xmin: f32,
    ymin: f32,
    xmax: f32,
    ymax: f32,
}

struct CFloor {
    model: i32,
    n: V3,
    d: f32,
    bx: Box2,
}

struct CWall {
    model: i32,
    nx: f32,
    ny: f32,
    dd: f32,
    z0: f32,
    z1: f32,
    bx: Box2,
    outn: V3,
}

fn box_of(w: &[V3]) -> Box2 {
    let mut b = Box2 { xmin: 1e30, ymin: 1e30, xmax: -1e30, ymax: -1e30 };
    for v in w {
        b.xmin = cmin(b.xmin, v[0]);
        b.ymin = cmin(b.ymin, v[1]);
        b.xmax = cmax(b.xmax, v[0]);
        b.ymax = cmax(b.ymax, v[1]);
    }
    b
}

fn clip_line_to_box(nx: f32, ny: f32, dd: f32, b: &Box2) -> Option<([f32; 2], [f32; 2])> {
    let l2 = nx * nx + ny * ny;
    if l2 < 1e-8 {
        return None;
    }
    let p0x = nx * dd / l2;
    let p0y = ny * dd / l2;
    let (mut dx, mut dy) = (-ny, nx);
    let dl = (dx * dx + dy * dy).sqrt();
    dx /= dl;
    dy /= dl;
    let (mut tmin, mut tmax) = (-1e30f32, 1e30f32);
    if dx.abs() > 1e-8 {
        let (mut t1, mut t2) = ((b.xmin - p0x) / dx, (b.xmax - p0x) / dx);
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        tmin = cmax(tmin, t1);
        tmax = cmin(tmax, t2);
    } else if p0x < b.xmin || p0x > b.xmax {
        return None;
    }
    if dy.abs() > 1e-8 {
        let (mut t1, mut t2) = ((b.ymin - p0y) / dy, (b.ymax - p0y) / dy);
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        tmin = cmax(tmin, t1);
        tmax = cmin(tmax, t2);
    } else if p0y < b.ymin || p0y > b.ymax {
        return None;
    }
    if tmax - tmin < MIN_SEAM {
        return None;
    }
    Some(([p0x + dx * tmin, p0y + dy * tmin], [p0x + dx * tmax, p0y + dy * tmax]))
}

fn floor_z(f: &CFloor, x: f32, y: f32) -> f32 {
    (f.d - f.n[0] * x - f.n[1] * y) / f.n[2]
}

pub fn enumerate_seams(map: &PwMap, wm: &WorldModels) -> Vec<Seam> {
    let mut seams = Vec::new();
    let mut floors: [Vec<CFloor>; 2] = [Vec::new(), Vec::new()];
    let mut walls: [Vec<CWall>; 2] = [Vec::new(), Vec::new()];
    for (m, bm) in map.models.iter().enumerate() {
        if m >= wm.model_solid.len() || !wm.model_solid[m] {
            continue;
        }
        let org = wm.model_origin[m];
        for h in [1usize, 3] {
            let uh = if h == 1 { 0 } else { 1 };
            let brushes = decompile_clip_hull(map, bm.headnode[h], &bm.mins, &bm.maxs);
            for b in &brushes {
                if b.contents != CONTENTS_SOLID {
                    continue;
                }
                for face in &b.faces {
                    if face.w.len() < 3 {
                        continue;
                    }
                    let w: Vec<V3> = face.w.iter().map(|v| [v[0] + org[0], v[1] + org[1], v[2] + org[2]]).collect();
                    if face.n[2] >= FLOOR_NZ {
                        let cf = CFloor {
                            model: m as i32,
                            n: face.n,
                            d: face.d + face.n[0] * org[0] + face.n[1] * org[1] + face.n[2] * org[2],
                            bx: box_of(&w),
                        };
                        let nv = w.len();
                        let (mut cx, mut cy) = (0f32, 0f32);
                        for v in &w {
                            cx += v[0];
                            cy += v[1];
                        }
                        cx /= nv as f32;
                        cy /= nv as f32;
                        for i in 0..nv {
                            let a = w[i];
                            let bb = w[(i + 1) % nv];
                            let (mut ex, mut ey) = (bb[0] - a[0], bb[1] - a[1]);
                            let el = (ex * ex + ey * ey).sqrt();
                            if el < MIN_SEAM {
                                continue;
                            }
                            ex /= el;
                            ey /= el;
                            let (mut px, mut py) = (ey, -ex);
                            let mx = (a[0] + bb[0]) * 0.5 - cx;
                            let my = (a[1] + bb[1]) * 0.5 - cy;
                            if px * mx + py * my < 0.0 {
                                px = -px;
                                py = -py;
                            }
                            let smx = (a[0] + bb[0]) * 0.5 + px;
                            let smy = (a[1] + bb[1]) * 0.5 + py;
                            let lp = [smx - org[0], smy - org[1], floor_z(&cf, smx, smy) - 1.0 - org[2]];
                            if brushes.iter().any(|b2| b2.contents == CONTENTS_SOLID && point_in_brush(b2, &lp, 0.0)) {
                                continue;
                            }
                            seams.push(Seam {
                                a,
                                b: bb,
                                outn: [px, py, 0.0],
                                floor_model: m as i32,
                                wall_model: -1,
                                fn_: face.n,
                                fd: cf.d,
                                slope: face.n[2] < SLOPE_FLAT_NZ,
                                usehull: uh,
                            });
                        }
                        floors[uh].push(cf);
                    } else if face.n[2].abs() < WALL_NZ {
                        let (mut z0, mut z1) = (1e30f32, -1e30f32);
                        for v in &w {
                            z0 = cmin(z0, v[2]);
                            z1 = cmax(z1, v[2]);
                        }
                        walls[uh].push(CWall {
                            model: m as i32,
                            nx: face.n[0],
                            ny: face.n[1],
                            dd: face.d + face.n[0] * org[0] + face.n[1] * org[1],
                            z0,
                            z1,
                            bx: box_of(&w),
                            outn: [face.n[0], face.n[1], 0.0],
                        });
                    }
                }
            }
        }
    }
    const CELL: f32 = 128.0;
    for uh in 0..2 {
        let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (wi, wl) in walls[uh].iter().enumerate() {
            let (ix0, ix1) = ((wl.bx.xmin / CELL).floor() as i32, (wl.bx.xmax / CELL).floor() as i32);
            let (iy0, iy1) = ((wl.bx.ymin / CELL).floor() as i32, (wl.bx.ymax / CELL).floor() as i32);
            for ix in ix0..=ix1 {
                for iy in iy0..=iy1 {
                    grid.entry((ix, iy)).or_default().push(wi);
                }
            }
        }
        for f in &floors[uh] {
            if f.n[2] < SLOPE_FLAT_NZ {
                continue;
            }
            let (ix0, ix1) = ((f.bx.xmin / CELL).floor() as i32, (f.bx.xmax / CELL).floor() as i32);
            let (iy0, iy1) = ((f.bx.ymin / CELL).floor() as i32, (f.bx.ymax / CELL).floor() as i32);
            let mut seen = Vec::new();
            for ix in ix0..=ix1 {
                for iy in iy0..=iy1 {
                    if let Some(v) = grid.get(&(ix, iy)) {
                        seen.extend_from_slice(v);
                    }
                }
            }
            seen.sort_unstable();
            seen.dedup();
            let fz = f.d / f.n[2];
            for wi in seen {
                let wl = &walls[uh][wi];
                if f.model > wl.model {
                    continue;
                }
                if fz < wl.z0 - Z_MATCH || fz > wl.z1 + Z_MATCH {
                    continue;
                }
                if wl.bx.xmax + XY_MARGIN < f.bx.xmin || wl.bx.xmin - XY_MARGIN > f.bx.xmax {
                    continue;
                }
                if wl.bx.ymax + XY_MARGIN < f.bx.ymin || wl.bx.ymin - XY_MARGIN > f.bx.ymax {
                    continue;
                }
                let clip = Box2 {
                    xmin: cmax(f.bx.xmin, wl.bx.xmin) - XY_MARGIN,
                    ymin: cmax(f.bx.ymin, wl.bx.ymin) - XY_MARGIN,
                    xmax: cmin(f.bx.xmax, wl.bx.xmax) + XY_MARGIN,
                    ymax: cmin(f.bx.ymax, wl.bx.ymax) + XY_MARGIN,
                };
                let Some((oa, ob)) = clip_line_to_box(wl.nx, wl.ny, wl.dd, &clip) else { continue };
                seams.push(Seam {
                    a: [oa[0], oa[1], fz],
                    b: [ob[0], ob[1], fz],
                    outn: wl.outn,
                    floor_model: f.model,
                    wall_model: wl.model,
                    fn_: f.n,
                    fd: f.d,
                    slope: false,
                    usehull: uh,
                });
            }
        }
    }
    seams
}
