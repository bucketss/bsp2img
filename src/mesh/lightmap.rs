use std::collections::{BTreeSet, HashMap};

use glam::{DVec2, DVec3};
use rayon::prelude::*;

use super::Mode;
use super::goldsrc::{Atlas, Record, signed_area};

fn sample_block(b: &(u32, u32, Vec<u8>), x: f64, y: f64) -> [f64; 3] {
    let (w, h) = (b.0 as usize, b.1 as usize);
    let x = (x - 0.5).clamp(0.0, (w - 1) as f64);
    let y = (y - 0.5).clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let g = |xx: usize, yy: usize| b.2[(yy * w + xx) * 3 + c] as f64;
        *o = (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy;
    }
    out
}

const TOUCH: f64 = 0.5;

fn seg_dist(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let d = b - a;
    let l2 = d.length_squared();
    let t = if l2 < 1e-12 { 0.0 } else { ((p - a).dot(d) / l2).clamp(0.0, 1.0) };
    (p - (a + d * t)).length()
}

fn polys_touch(a: &[DVec2], b: &[DVec2]) -> bool {
    let near = |p: &[DVec2], q: &[DVec2]| {
        p.iter().any(|&v| (0..q.len()).any(|e| seg_dist(v, q[e], q[(e + 1) % q.len()]) <= TOUCH))
    };
    near(a, b) || near(b, a)
}

fn uf_find(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

struct Merged {
    g: Vec<usize>,
    u: DVec3,
    w: DVec3,
    lo: DVec2,
    texel: f64,
    gw: usize,
    gh: usize,
    rgb: Vec<u8>,
}

fn plane_groups(records: &[Record], ids: &[usize]) -> Vec<Vec<usize>> {
    if ids.len() < 2 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..ids.len()).collect();
    let n0 = records[ids[0]].normal;
    let u0 = n0.any_orthonormal_vector();
    let w0 = n0.cross(u0);
    let shapes: Vec<(Vec<DVec2>, DVec2, DVec2)> = ids
        .iter()
        .map(|&ri| {
            let poly: Vec<DVec2> = records[ri].p.iter().map(|q| DVec2::new(q.dot(u0), q.dot(w0))).collect();
            let lo = poly.iter().fold(DVec2::INFINITY, |m, p| m.min(*p));
            let hi = poly.iter().fold(DVec2::NEG_INFINITY, |m, p| m.max(*p));
            (poly, lo, hi)
        })
        .collect();
    let ds: Vec<f64> = ids.iter().map(|&ri| n0.dot(records[ri].p[0])).collect();
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.sort_by(|&a, &b| ds[a].total_cmp(&ds[b]).then(a.cmp(&b)));
    for (k, &x) in order.iter().enumerate() {
        for &y in &order[k + 1..] {
            if ds[y] - ds[x] >= 0.2 {
                break;
            }
            let (a, b) = (&shapes[x], &shapes[y]);
            if a.1.x > b.2.x + TOUCH || b.1.x > a.2.x + TOUCH || a.1.y > b.2.y + TOUCH || b.1.y > a.2.y + TOUCH {
                continue;
            }
            if polys_touch(&a.0, &b.0) {
                let (p, q) = (uf_find(&mut parent, x), uf_find(&mut parent, y));
                parent[p] = q;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, &ri) in ids.iter().enumerate() {
        let root = uf_find(&mut parent, k);
        groups.entry(root).or_default().push(ri);
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

fn merge_group(records: &[Record], atlas: &Atlas, g: Vec<usize>) -> Option<Merged> {
    let r0 = &records[g[0]];
    let n = r0.normal;
    let m0 = r0.lmap.unwrap();
    let mut u = m0.sv - n * m0.sv.dot(n);
    if u.length() < 1e-9 {
        u = n.any_orthonormal_vector();
    }
    let u = u.normalize();
    let w = n.cross(u);
    let texel = g
        .iter()
        .flat_map(|&i| {
            let m = records[i].lmap.unwrap();
            [16.0 / m.sv.length().max(1e-9), 16.0 / m.tv.length().max(1e-9)]
        })
        .fold(16.0f64, f64::min)
        .max(1.0);
    let (mut lo, mut hi) = (DVec2::INFINITY, DVec2::NEG_INFINITY);
    for &i in &g {
        for p in &records[i].p {
            let q = DVec2::new(p.dot(u), p.dot(w));
            lo = lo.min(q);
            hi = hi.max(q);
        }
    }
    let gw = ((hi.x - lo.x) / texel).ceil() as usize + 1;
    let gh = ((hi.y - lo.y) / texel).ceil() as usize + 1;
    if gw * gh > 1 << 18 {
        return None;
    }
    let polys: Vec<(Vec<DVec2>, f64)> = g
        .iter()
        .map(|&i| {
            let poly: Vec<DVec2> = records[i].p.iter().map(|p| DVec2::new(p.dot(u), p.dot(w))).collect();
            let a = signed_area(&poly);
            (poly, if a < 0.0 { -1.0 } else { 1.0 })
        })
        .collect();
    let origin = n * n.dot(r0.p[0]);
    let mut rgb = vec![0u8; gw * gh * 3];
    for gy in 0..gh {
        for gx in 0..gw {
            let q2 = DVec2::new(lo.x + gx as f64 * texel, lo.y + gy as f64 * texel);
            let q = origin + u * q2.x + w * q2.y;
            let (mut acc, mut wsum) = ([0.0f64; 3], 0.0);
            let mut best = (f64::NEG_INFINITY, [0.0f64; 3]);
            for (k, &i) in g.iter().enumerate() {
                let (poly, sgn) = &polys[k];
                let mut sd = f64::INFINITY;
                for e in 0..poly.len() {
                    let (a, b) = (poly[e], poly[(e + 1) % poly.len()]);
                    let d = b - a;
                    let l = d.length().max(1e-9);
                    let dist = sgn * (d.x * (q2.y - a.y) - d.y * (q2.x - a.x)) / l;
                    sd = sd.min(dist);
                }
                let m = records[i].lmap.unwrap();
                let c = sample_block(&atlas.blocks[records[i].block], q.dot(m.ax) + m.c[0], q.dot(m.ay) + m.c[1]);
                if sd > best.0 {
                    best = (sd, c);
                }
                let wt = ((sd + texel) / (2.0 * texel)).clamp(0.0, 1.0);
                if wt > 0.0 {
                    for (a, v) in acc.iter_mut().zip(c) {
                        *a += v * wt;
                    }
                    wsum += wt;
                }
            }
            let c = if wsum > 0.0 { acc.map(|v| v / wsum) } else { best.1 };
            let d = (gy * gw + gx) * 3;
            for k in 0..3 {
                rgb[d + k] = c[k].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(Merged { g, u, w, lo, texel, gw, gh, rgb })
}

pub(super) fn merge_lightmaps(records: &mut [Record], atlas: &mut Atlas) {
    let mut planes: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        if r.model != 0 || r.lmap.is_none() || r.mode != Mode::Opaque {
            continue;
        }
        let n = r.normal;
        let q = |v: f64| (v * 1e3).round() as i64;
        planes.entry([q(n.x), q(n.y), q(n.z)]).or_default().push(i);
    }
    let recs: &[Record] = records;
    let mut jobs: Vec<Vec<usize>> = planes.into_par_iter().flat_map_iter(|(_, ids)| plane_groups(recs, &ids)).collect();
    jobs.sort_unstable_by_key(|g| g[0]);
    let merged: Vec<Merged> = jobs.into_par_iter().filter_map(|g| merge_group(recs, atlas, g)).collect();
    for m in merged {
        let block = atlas.add(m.gw as u32, m.gh as u32, m.rgb);
        for &i in &m.g {
            atlas.blocks[records[i].block] = (1, 1, vec![0, 0, 0]);
            let r = &mut records[i];
            r.block = block;
            let (a, b): (Vec<f64>, Vec<f64>) = r
                .p
                .iter()
                .map(|p| ((p.dot(m.u) - m.lo.x) / m.texel + 0.5, (p.dot(m.w) - m.lo.y) / m.texel + 0.5))
                .unzip();
            r.lm = Some((a, b));
            r.lmap = None;
        }
    }
}

const EDGE_MAX_DIHEDRAL: f64 = 60.0;
const EDGE_FALL: f64 = 1.5;
const EDGE_STEP: f64 = 4.0;
const EDGE_ITERS: usize = 4;
const EDGE_GAIN: f64 = 0.7;

struct Affine {
    gu: DVec3,
    gv: DVec3,
    c: [f64; 2],
}

impl Affine {
    fn uv(&self, p: DVec3) -> [f64; 2] {
        [p.dot(self.gu) + self.c[0], p.dot(self.gv) + self.c[1]]
    }
}

fn record_affine(r: &Record) -> Option<Affine> {
    let (a, b) = r.lm.as_ref()?;
    let n = r.p.len();
    let mut best = (1e-6, 0, 0, 0);
    for i in 1..n - 1 {
        let ar = (r.p[i] - r.p[0]).cross(r.p[i + 1] - r.p[0]).length();
        if ar > best.0 {
            best = (ar, 0, i, i + 1);
        }
    }
    if best.0 <= 1e-6 {
        return None;
    }
    let (p0, p1, p2) = (r.p[best.1], r.p[best.2], r.p[best.3]);
    let (e1, e2) = (p1 - p0, p2 - p0);
    let nn = e1.cross(e2).normalize();
    let t = e1.dot(e2.cross(nn));
    let solve = |d1: f64, d2: f64| (e2.cross(nn) * d1 + nn.cross(e1) * d2) / t;
    let (u0, u1, u2) = (a[best.1], a[best.2], a[best.3]);
    let (v0, v1, v2) = (b[best.1], b[best.2], b[best.3]);
    let gu = solve(u1 - u0, u2 - u0);
    let gv = solve(v1 - v0, v2 - v0);
    Some(Affine { gu, gv, c: [u0 - p0.dot(gu), v0 - p0.dot(gv)] })
}

fn shared_segment(a: &[DVec3], b: &[DVec3]) -> Vec<(DVec3, DVec3)> {
    let mut out = Vec::new();
    for i in 0..a.len() {
        let (a0, a1) = (a[i], a[(i + 1) % a.len()]);
        let d = a1 - a0;
        let l = d.length();
        if l < 1.0 {
            continue;
        }
        let d = d / l;
        for j in 0..b.len() {
            let (b0, b1) = (b[j], b[(j + 1) % b.len()]);
            let off = |p: DVec3| (p - a0 - d * (p - a0).dot(d)).length();
            if off(b0) > 0.5 || off(b1) > 0.5 {
                continue;
            }
            let (s0, s1) = ((b0 - a0).dot(d), (b1 - a0).dot(d));
            let (lo, hi) = (s0.min(s1).max(0.0), s0.max(s1).min(l));
            if hi - lo > 1.0 {
                out.push((a0 + d * lo, a0 + d * hi));
            }
        }
    }
    out
}

pub(super) fn equalize_edges(records: &[Record], atlas: &mut Atlas) {
    let cos_max = EDGE_MAX_DIHEDRAL.to_radians().cos();
    let aff: Vec<Option<Affine>> = records.iter().map(|r| if r.model == 0 && r.mode == Mode::Opaque { record_affine(r) } else { None }).collect();
    let bbox: Vec<(DVec3, DVec3)> = records
        .iter()
        .map(|r| {
            r.p.iter().fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
        })
        .collect();
    let cell = 256.0;
    let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (i, (lo, hi)) in bbox.iter().enumerate() {
        if aff[i].is_none() {
            continue;
        }
        let (l, h) = ((*lo - 1.0) / cell, (*hi + 1.0) / cell);
        for x in l.x.floor() as i64..=h.x.floor() as i64 {
            for y in l.y.floor() as i64..=h.y.floor() as i64 {
                for z in l.z.floor() as i64..=h.z.floor() as i64 {
                    grid.entry([x, y, z]).or_default().push(i);
                }
            }
        }
    }
    let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    for v in grid.values() {
        for (k, &i) in v.iter().enumerate() {
            for &j in &v[k + 1..] {
                let (a, b) = (i.min(j), i.max(j));
                let (ba, bb) = (&bbox[a], &bbox[b]);
                if ba.0.cmpgt(bb.1 + 1.0).any() || bb.0.cmpgt(ba.1 + 1.0).any() {
                    continue;
                }
                pairs.insert((a, b));
            }
        }
    }
    let mut work: Vec<(usize, usize, Vec<(DVec3, DVec3)>)> = Vec::new();
    for (a, b) in pairs {
        let (ra, rb) = (&records[a], &records[b]);
        if ra.block == rb.block || ra.lstyles != rb.lstyles {
            continue;
        }
        let dot = ra.normal.dot(rb.normal);
        if dot > 0.9999 || dot < cos_max {
            continue;
        }
        let segs = shared_segment(&ra.p, &rb.p);
        if !segs.is_empty() {
            work.push((a, b, segs));
        }
    }
    for _ in 0..EDGE_ITERS {
        let mut deltas: HashMap<usize, Vec<f32>> = HashMap::new();
        for (a, b, segs) in &work {
            let (ra, rb) = (&records[*a], &records[*b]);
            let (fa, fb) = (aff[*a].as_ref().unwrap(), aff[*b].as_ref().unwrap());
            for &(p0, p1) in segs {
                let len = (p1 - p0).length();
                let steps = ((len / EDGE_STEP).ceil() as usize).max(1);
                let diffs: Vec<[f64; 3]> = (0..=steps)
                    .map(|k| {
                        let p = p0.lerp(p1, k as f64 / steps as f64);
                        let (ua, ub) = (fa.uv(p), fb.uv(p));
                        let ca = sample_block(&atlas.blocks[ra.block], ua[0], ua[1]);
                        let cb = sample_block(&atlas.blocks[rb.block], ub[0], ub[1]);
                        [cb[0] - ca[0], cb[1] - ca[1], cb[2] - ca[2]]
                    })
                    .collect();
                for (rec, f, sign) in [(ra, fa, 0.5), (rb, fb, -0.5)] {
                    let (q0, q1) = (f.uv(p0), f.uv(p1));
                    let (w, h) = (atlas.blocks[rec.block].0 as usize, atlas.blocks[rec.block].1 as usize);
                    let (x0, x1) = (((q0[0].min(q1[0]) - EDGE_FALL - 1.0).floor().max(0.0)) as usize, ((q0[0].max(q1[0]) + EDGE_FALL + 1.0).ceil().min(w as f64)) as usize);
                    let (y0, y1) = (((q0[1].min(q1[1]) - EDGE_FALL - 1.0).floor().max(0.0)) as usize, ((q0[1].max(q1[1]) + EDGE_FALL + 1.0).ceil().min(h as f64)) as usize);
                    let seg = [q1[0] - q0[0], q1[1] - q0[1]];
                    let l2 = (seg[0] * seg[0] + seg[1] * seg[1]).max(1e-9);
                    let buf = deltas.entry(rec.block).or_insert_with(|| vec![0.0; w * h * 3]);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
                            let t = (((cx - q0[0]) * seg[0] + (cy - q0[1]) * seg[1]) / l2).clamp(0.0, 1.0);
                            let (dx, dy) = (cx - q0[0] - seg[0] * t, cy - q0[1] - seg[1] * t);
                            let wt = 1.0 - (dx * dx + dy * dy).sqrt() / EDGE_FALL;
                            if wt <= 0.0 {
                                continue;
                            }
                            let fpos = t * steps as f64;
                            let (i0, fr) = (fpos.floor() as usize, fpos.fract());
                            let i1 = (i0 + 1).min(steps);
                            for c in 0..3 {
                                let d = diffs[i0][c] * (1.0 - fr) + diffs[i1][c] * fr;
                                buf[(y * w + x) * 3 + c] += (d * sign * wt * EDGE_GAIN) as f32;
                            }
                        }
                    }
                }
            }
        }
        for (bi, buf) in deltas {
            for (px, d) in atlas.blocks[bi].2.iter_mut().zip(buf) {
                *px = (*px as f64 + d as f64).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

