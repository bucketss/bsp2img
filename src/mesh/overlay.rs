use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use glam::{DVec2, DVec3};
use rayon::prelude::*;

use super::{Image, Mesh, Mode};

pub const TEXEL: f64 = 4.0;
pub const SIGMA: f64 = 8.0;
pub const MAX_ALPHA: f64 = 0.8;
const BUDGET: u64 = 1 << 23;
const PLANE_TOL: f64 = 1.0;
const CELL: f64 = 64.0;
const MIN_ALPHA: f64 = 0.12;
const NORM_PCT: f64 = 0.6;
const NORM_FLOOR: f32 = 1e-3;
const WELD: f64 = 64.0;
const COPLANAR: f64 = 0.94;
const FLAT_TOL: f64 = 2.0;
const EDGE_TOL: f64 = 0.01;
const MIN_FACE: f64 = TEXEL;
pub const UNPAINTED: [f32; 2] = [0.0, 0.0];

pub fn painted(ov: [f32; 2]) -> bool {
    ov != UNPAINTED
}

#[derive(Clone, Copy, Debug)]
pub struct Splat {
    pub p: DVec3,
    pub facing: Option<DVec3>,
    pub model: Option<u32>,
    pub layer: u8,
    pub sigma: f64,
    pub snap: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Painted {
    pub splats: usize,
    pub missed: usize,
    pub faces: usize,
    pub texel: f64,
    pub w: u32,
    pub h: u32,
}

enum Src {
    Span(usize),
    Tris(u32, Vec<u32>),
}

enum Shape {
    Poly(Vec<DVec2>, f64),
    Tris(Vec<[DVec2; 3]>),
}

struct Face {
    src: Src,
    n: DVec3,
    d: f64,
    u: DVec3,
    v: DVec3,
    shape: Shape,
    lo: DVec2,
    hi: DVec2,
    blo: DVec3,
    bhi: DVec3,
    slack: f64,
    model: u32,
}

fn poly_dist(poly: &[DVec2], sign: f64, q: DVec2) -> f64 {
    let n = poly.len();
    let mut inside = true;
    let mut best = f64::INFINITY;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let e = b - a;
        if sign * e.perp_dot(q - a) < 0.0 {
            inside = false;
        }
        let t = ((q - a).dot(e) / e.length_squared().max(1e-12)).clamp(0.0, 1.0);
        best = best.min((a + e * t).distance(q));
    }
    if inside { 0.0 } else { best }
}

fn area_sign(p: &[DVec2]) -> f64 {
    let area: f64 = (0..p.len()).map(|i| p[i].perp_dot(p[(i + 1) % p.len()])).sum();
    if area < 0.0 { -1.0 } else { 1.0 }
}

impl Face {
    fn to2(&self, p: DVec3) -> DVec2 {
        DVec2::new(p.dot(self.u), p.dot(self.v))
    }

    fn dist(&self, q: DVec2) -> f64 {
        match &self.shape {
            Shape::Poly(poly, sign) => poly_dist(poly, *sign, q),
            Shape::Tris(ts) => ts.iter().map(|t| poly_dist(t, area_sign(t), q)).fold(f64::INFINITY, f64::min),
        }
    }

    fn new(src: Src, n: DVec3, p: &[DVec3], shape: impl FnOnce(&dyn Fn(DVec3) -> DVec2) -> Shape, model: u32) -> Face {
        let u = n.any_orthonormal_vector();
        let v = n.cross(u);
        let to2 = |q: DVec3| DVec2::new(q.dot(u), q.dot(v));
        let lo = p.iter().fold(DVec2::INFINITY, |m, q| m.min(to2(*q)));
        let hi = p.iter().fold(DVec2::NEG_INFINITY, |m, q| m.max(to2(*q)));
        let blo = p.iter().fold(DVec3::INFINITY, |m, q| m.min(*q));
        let bhi = p.iter().fold(DVec3::NEG_INFINITY, |m, q| m.max(*q));
        let d = p.iter().map(|q| q.dot(n)).sum::<f64>() / p.len() as f64;
        let slack = match src {
            Src::Span(_) => 0.0,
            Src::Tris(..) => p.iter().map(|q| (q.dot(n) - d).abs()).fold(0.0, f64::max),
        };
        Face { src, n, d, u, v, shape: shape(&to2), lo, hi, blo, bhi, slack, model }
    }
}

fn dv(p: [f32; 3]) -> DVec3 {
    DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
}

fn span_faces(mesh: &Mesh) -> Vec<Face> {
    mesh.face_spans
        .iter()
        .enumerate()
        .filter_map(|(span, fs)| {
            let b = mesh.batches.get(fs.batch as usize)?;
            let vs = b.verts.get(fs.first as usize..(fs.first + fs.count) as usize)?;
            let p: Vec<DVec3> = vs.iter().map(|v| dv(v.pos)).collect();
            let n = dv(vs[0].normal).normalize_or_zero();
            if n == DVec3::ZERO || p.len() < 3 {
                return None;
            }
            let shape = |to2: &dyn Fn(DVec3) -> DVec2| {
                let poly: Vec<DVec2> = p.iter().map(|q| to2(*q)).collect();
                let sign = area_sign(&poly);
                Shape::Poly(poly, sign)
            };
            Some(Face::new(Src::Span(span), n, &p, shape, fs.model))
        })
        .collect()
}

fn touches(keep: &HashSet<Key>, lo: DVec3, hi: DVec3) -> bool {
    let (a, b) = (cell(lo), cell(hi));
    (a.0..=b.0).any(|x| (a.1..=b.1).any(|y| (a.2..=b.2).any(|z| keep.contains(&(x, y, z)))))
}

fn tri_faces(mesh: &Mesh, keep: &HashSet<Key>, pad: f64) -> Vec<Face> {
    let pad = DVec3::splat(pad);
    let jobs: Vec<(usize, u32, u32)> = mesh
        .batches
        .iter()
        .enumerate()
        .filter(|(_, b)| b.mode == Mode::Opaque)
        .flat_map(|(bi, b)| match b.chunks.is_empty() {
            true => vec![(bi, 0, b.idx.len() as u32)],
            false => b.chunks.iter().filter(|c| touches(keep, dv(c.lo) - pad, dv(c.hi) + pad)).map(|c| (bi, c.first, c.count)).collect(),
        })
        .collect();
    jobs.par_iter().flat_map_iter(|&(bi, first, count)| group(mesh, bi, first / 3, (first + count) / 3, keep, pad)).collect()
}

fn group(mesh: &Mesh, bi: usize, t0: u32, t1: u32, keep: &HashSet<Key>, pad: DVec3) -> Vec<Face> {
    let b = &mesh.batches[bi];
    let corner = |t: u32, k: usize| dv(b.verts[b.idx[(t * 3) as usize + k] as usize].pos);
    let key = |p: DVec3| {
        let q = (p * WELD).round();
        (q.x as i64, q.y as i64, q.z as i64)
    };
    let n = (t1 - t0) as usize;
    let mut normal = vec![DVec3::ZERO; n];
    let mut edges: HashMap<((i64, i64, i64), (i64, i64, i64)), Vec<u32>> = HashMap::new();
    for t in t0..t1 {
        let p = [0, 1, 2].map(|k| corner(t, k));
        let c = (p[1] - p[0]).cross(p[2] - p[0]);
        if c.length() < 1e-6 {
            continue;
        }
        normal[(t - t0) as usize] = c.normalize();
        for k in 0..3 {
            let (a, z) = (key(p[k]), key(p[(k + 1) % 3]));
            edges.entry(if a < z { (a, z) } else { (z, a) }).or_default().push(t);
        }
    }
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for s in t0..t1 {
        let si = (s - t0) as usize;
        if seen[si] || normal[si] == DVec3::ZERO {
            continue;
        }
        seen[si] = true;
        let n0 = normal[si];
        let d0 = corner(s, 0).dot(n0);
        let mut tris = vec![s];
        stack.push(s);
        while let Some(t) = stack.pop() {
            let p = [0, 1, 2].map(|k| corner(t, k));
            for k in 0..3 {
                let (a, z) = (key(p[k]), key(p[(k + 1) % 3]));
                for &o in &edges[&if a < z { (a, z) } else { (z, a) }] {
                    let oi = (o - t0) as usize;
                    if seen[oi] || normal[oi].dot(n0) < COPLANAR || (0..3).any(|j| (corner(o, j).dot(n0) - d0).abs() > FLAT_TOL) {
                        continue;
                    }
                    seen[oi] = true;
                    tris.push(o);
                    stack.push(o);
                }
            }
        }
        let p: Vec<DVec3> = tris.iter().flat_map(|&t| [0, 1, 2].map(|k| corner(t, k))).collect();
        let blo = p.iter().fold(DVec3::INFINITY, |m, q| m.min(*q));
        let bhi = p.iter().fold(DVec3::NEG_INFINITY, |m, q| m.max(*q));
        if !touches(keep, blo - pad, bhi + pad) {
            continue;
        }
        let vn: DVec3 = tris.iter().flat_map(|&t| (0..3).map(move |k| dv(b.verts[b.idx[(t * 3) as usize + k] as usize].normal))).sum();
        let n0 = if vn.dot(n0) < 0.0 { -n0 } else { n0 };
        let shape = |to2: &dyn Fn(DVec3) -> DVec2| Shape::Tris(p.chunks_exact(3).map(|c| [to2(c[0]), to2(c[1]), to2(c[2])]).collect());
        let f = Face::new(Src::Tris(bi as u32, tris), n0, &p, shape, 0);
        if (f.hi - f.lo).max_element() >= MIN_FACE {
            out.push(f);
        }
    }
    out
}

fn faces(mesh: &Mesh, splats: &[Splat], pad: f64) -> Vec<Face> {
    if !mesh.face_spans.is_empty() {
        return span_faces(mesh);
    }
    let keep: HashSet<Key> = splats.par_iter().map(|s| cell(s.p)).collect();
    tri_faces(mesh, &keep, pad)
}

type Key = (i32, i32, i32);
type Packing = (u32, u32, Vec<(u32, u32)>);

fn cell(p: DVec3) -> Key {
    ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32, (p.z / CELL).floor() as i32)
}

fn grid(faces: &[Face], pad: f64) -> HashMap<Key, Vec<u32>> {
    let mut g: HashMap<Key, Vec<u32>> = HashMap::new();
    for (i, f) in faces.iter().enumerate() {
        let (a, b) = (cell(f.blo - DVec3::splat(pad)), cell(f.bhi + DVec3::splat(pad)));
        for x in a.0..=b.0 {
            for y in a.1..=b.1 {
                for z in a.2..=b.2 {
                    g.entry((x, y, z)).or_default().push(i as u32);
                }
            }
        }
    }
    g
}

fn ray_tri(p: DVec3, f: DVec3, c: [DVec3; 3]) -> Option<f64> {
    let (e1, e2) = (c[1] - c[0], c[2] - c[0]);
    let h = f.cross(e2);
    let det = e1.dot(h);
    if det.abs() < 1e-12 {
        return None;
    }
    let s = p - c[0];
    let a = s.dot(h) / det;
    let q = s.cross(e1);
    let b = f.dot(q) / det;
    let tol = -1e-6;
    (a >= tol && b >= tol && a + b <= 1.0 - tol).then(|| e2.dot(q) / det)
}

fn snap(mesh: &Mesh, fs: &[Face], cands: &[u32], p: DVec3, f: DVec3, reach: f64) -> Option<DVec3> {
    cands
        .iter()
        .filter_map(|&i| {
            let face = &fs[i as usize];
            let dn = face.n.dot(f);
            if dn > -0.01 {
                return None;
            }
            let t = match &face.src {
                Src::Span(_) => Some((face.d - p.dot(face.n)) / dn).filter(|&t| face.dist(face.to2(p + f * t)) <= EDGE_TOL),
                Src::Tris(bi, tris) => {
                    let b = &mesh.batches[*bi as usize];
                    tris.iter()
                        .filter_map(|&t| ray_tri(p, f, [0, 1, 2].map(|k| dv(b.verts[b.idx[(t * 3) as usize + k] as usize].pos))))
                        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                }
            }?;
            (t.abs() <= reach).then_some(t)
        })
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        .map(|t| p + f * t)
}

fn pack(sizes: &[(u32, u32)], max_dim: u32) -> Option<Packing> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| (Reverse(sizes[i].1), i));
    let widest = sizes.iter().map(|s| s.0 + 4).max().unwrap_or(4);
    let area: u64 = sizes.iter().map(|s| (s.0 + 2) as u64 * (s.1 + 2) as u64).sum::<u64>() + 4;
    let mut width = widest.max((area as f64).sqrt() as u32).next_power_of_two().max(64);
    loop {
        if width > max_dim {
            return None;
        }
        let (mut x, mut y, mut shelf) = (2u32, 0u32, 2u32);
        let mut pos = vec![(0, 0); sizes.len()];
        for &i in &order {
            let (w, h) = sizes[i];
            if x + w + 2 > width {
                x = 0;
                y += shelf;
                shelf = 0;
            }
            pos[i] = (x + 1, y + 1);
            x += w + 2;
            shelf = shelf.max(h + 2);
        }
        let height = (y + shelf).max(2).next_power_of_two();
        if height > width && width < max_dim {
            width *= 2;
            continue;
        }
        if height > max_dim {
            return None;
        }
        return Some((width, height, pos));
    }
}

struct Chart {
    face: usize,
    w: u32,
    h: u32,
    acc: Vec<f32>,
}

fn percentile(mut v: Vec<f32>) -> f64 {
    if v.is_empty() {
        return 1.0;
    }
    let k = ((v.len() - 1) as f64 * NORM_PCT) as usize;
    (*v.select_nth_unstable_by(k, f32::total_cmp).1 as f64).max(1e-6)
}

pub fn paint(mesh: &mut Mesh, splats: &[Splat], colors: &[[u8; 3]], max_dim: u32) -> Painted {
    let nl = colors.len();
    let mut out = Painted { splats: splats.len(), ..Default::default() };
    let reach = 3.0 * splats.iter().map(|s| s.sigma).fold(0.0, f64::max);
    let fs = faces(mesh, splats, reach + PLANE_TOL);
    if fs.is_empty() || splats.is_empty() {
        mesh.overlay = None;
        return out;
    }
    let slack = fs.iter().map(|f| f.slack).fold(0.0, f64::max);
    let g = grid(&fs, reach + PLANE_TOL + slack);
    let hits: Vec<Vec<(u32, DVec2, u8, f64)>> = splats
        .par_iter()
        .map(|s| {
            let mut p = s.p;
            if s.snap > 0.0
                && let Some(f) = s.facing
                && let Some(c) = g.get(&cell(p))
                && let Some(q) = snap(mesh, &fs, c, p, f, s.snap)
            {
                p = q;
            }
            let Some(cands) = g.get(&cell(p)) else { return Vec::new() };
            cands
                .iter()
                .filter_map(|&i| {
                    let f = &fs[i as usize];
                    if (p.dot(f.n) - f.d).abs() > PLANE_TOL + f.slack {
                        return None;
                    }
                    if s.facing.is_some_and(|d| f.n.dot(d) > -0.01) || s.model.is_some_and(|m| m != f.model) {
                        return None;
                    }
                    let q = f.to2(p);
                    ((s.layer as usize) < nl && f.dist(q) <= 3.0 * s.sigma).then_some((i, q, s.layer, s.sigma))
                })
                .collect()
        })
        .collect();
    out.missed = hits.iter().filter(|h| h.is_empty()).count();
    let mut per: HashMap<u32, Vec<(DVec2, u8, f64)>> = HashMap::new();
    for (i, q, l, sg) in hits.into_iter().flatten() {
        per.entry(i).or_default().push((q, l, sg));
    }
    let mut painted: Vec<u32> = per.keys().copied().collect();
    painted.sort_unstable();
    if painted.is_empty() {
        mesh.overlay = None;
        return out;
    }
    let dims = |t: f64| -> Vec<(u32, u32)> {
        painted
            .iter()
            .map(|&i| {
                let f = &fs[i as usize];
                (((f.hi.x - f.lo.x) / t).ceil() as u32 + 1, ((f.hi.y - f.lo.y) / t).ceil() as u32 + 1)
            })
            .collect()
    };
    let mut texel = TEXEL;
    let (sizes, (aw, ah, pos)) = loop {
        let sizes = dims(texel);
        let area: u64 = sizes.iter().map(|s| (s.0 + 2) as u64 * (s.1 + 2) as u64).sum();
        if area <= BUDGET
            && let Some(p) = pack(&sizes, max_dim)
        {
            break (sizes, p);
        }
        texel *= 2.0;
    };
    let charts: Vec<Chart> = painted
        .par_iter()
        .zip(&sizes)
        .map(|(&i, &(w, h))| {
            let f = &fs[i as usize];
            let mut acc = vec![0f32; (w * h) as usize * nl];
            for &(q, l, s) in &per[&i] {
                let sigma = s.max(texel);
                let rad = 3.0 * sigma / texel;
                let (cx, cy) = ((q.x - f.lo.x) / texel, (q.y - f.lo.y) / texel);
                let x0 = (cx - rad).floor().max(0.0) as i64;
                let x1 = ((cx + rad).ceil() as i64).min(w as i64 - 1);
                let y0 = (cy - rad).floor().max(0.0) as i64;
                let y1 = ((cy + rad).ceil() as i64).min(h as i64 - 1);
                let k = -texel * texel / (2.0 * sigma * sigma);
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let d2 = (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2);
                        acc[(y as u32 * w + x as u32) as usize * nl + l as usize] += (d2 * k).exp() as f32;
                    }
                }
            }
            Chart { face: i as usize, w, h, acc }
        })
        .collect();
    let norms: Vec<f64> = (0..nl)
        .map(|l| percentile(charts.iter().flat_map(|c| c.acc.iter().skip(l).step_by(nl).copied()).filter(|&t| t > NORM_FLOOR).collect()))
        .collect();
    let mut rgba = vec![0u8; (aw * ah * 4) as usize];
    for (c, &(px, py)) in charts.iter().zip(&pos) {
        let texel_at = |x: u32, y: u32| -> [u8; 4] {
            let i = (y * c.w + x) as usize * nl;
            let mut o = [0f64; 4];
            for l in 0..nl {
                let a = (c.acc[i + l] as f64 / norms[l]).clamp(0.0, 1.0).sqrt();
                if a < MIN_ALPHA {
                    continue;
                }
                let a = a * MAX_ALPHA;
                for k in 0..3 {
                    o[k] = o[k] * (1.0 - a) + colors[l][k] as f64 * a;
                }
                o[3] = o[3] * (1.0 - a) + a;
            }
            [o[0].round() as u8, o[1].round() as u8, o[2].round() as u8, (o[3] * 255.0).round() as u8]
        };
        for yy in 0..c.h + 2 {
            let sy = yy.saturating_sub(1).min(c.h - 1);
            for xx in 0..c.w + 2 {
                let sx = xx.saturating_sub(1).min(c.w - 1);
                let d = (((py - 1 + yy) * aw + px - 1 + xx) * 4) as usize;
                rgba[d..d + 4].copy_from_slice(&texel_at(sx, sy));
            }
        }
        let f = &fs[c.face];
        let uv = |pos: [f32; 3]| {
            let q = f.to2(dv(pos));
            [
                ((px as f64 + 0.5 + (q.x - f.lo.x) / texel) / aw as f64) as f32,
                ((py as f64 + 0.5 + (q.y - f.lo.y) / texel) / ah as f64) as f32,
            ]
        };
        match &f.src {
            Src::Span(i) => {
                let span = mesh.face_spans[*i];
                let b = &mut mesh.batches[span.batch as usize];
                for v in &mut b.verts[span.first as usize..(span.first + span.count) as usize] {
                    v.ov = uv(v.pos);
                }
            }
            Src::Tris(bi, tris) => {
                let b = &mut mesh.batches[*bi as usize];
                let mut copies: HashMap<u32, u32> = HashMap::new();
                for &t in tris {
                    for slot in (t * 3) as usize..(t * 3 + 3) as usize {
                        let i = b.idx[slot];
                        b.idx[slot] = match copies.get(&i) {
                            Some(&j) => j,
                            None => {
                                let mut v = b.verts[i as usize];
                                v.ov = uv(v.pos);
                                b.verts.push(v);
                                let j = (b.verts.len() - 1) as u32;
                                copies.insert(i, j);
                                j
                            }
                        };
                    }
                }
            }
        }
    }
    mesh.overlay = Some(Image { w: aw, h: ah, rgba });
    out.faces = charts.len();
    out.texel = texel;
    out.w = aw;
    out.h = ah;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tri_faces_copy_shared_verts() {
        use super::super::{Batch, Vertex};
        let v = |p: [f32; 3], n: [f32; 3]| Vertex { pos: p, normal: n, ..Vertex::BLANK };
        let x = [1.0, 0.0, 0.0];
        let z = [0.0, 0.0, 1.0];
        let verts = vec![
            v([0.0, 0.0, 0.0], x),
            v([0.0, 64.0, 0.0], x),
            v([0.0, 64.0, 64.0], x),
            v([0.0, 0.0, 64.0], x),
            v([10.0, 0.0, 0.0], z),
            v([0.0, -10.0, 0.0], z),
        ];
        let batch = Batch {
            tex: 0,
            mode: Mode::Opaque,
            alpha: 1.0,
            warp: None,
            tint: [1.0; 3],
            layer2: None,
            layer3: None,
            four: None,
            tint_mask: None,
            verts,
            idx: vec![0, 1, 2, 0, 2, 3, 0, 5, 4],
            chunks: Vec::new(),
            actor: false,
        };
        let mut mesh = Mesh {
            batches: vec![batch],
            textures: std::sync::Arc::new(Vec::new()),
            anim: Vec::new(),
            atlas: std::sync::Arc::new(Image { w: 1, h: 1, rgba: vec![0; 4] }),
            faces: 0,
            missing: Vec::new(),
            points: std::sync::Arc::from(Vec::new()),
            nudged: 0,
            names: Vec::new(),
            hdr: None,
            casters: Vec::new(),
            caster_idx: Vec::new(),
            point_cell: None,
            compressed: Vec::new(),
            face_spans: Vec::new(),
            overlay: None,
        };
        let s = Splat { p: DVec3::new(3.0, 32.0, 32.0), facing: Some(DVec3::NEG_X), model: None, layer: 0, sigma: SIGMA, snap: 8.0 };
        let p = paint(&mut mesh, &[s], &[[255, 0, 0]], 4096);
        assert_eq!((p.missed, p.faces), (0, 1));
        let b = &mesh.batches[0];
        assert_eq!(b.verts.len(), 10);
        assert!(b.idx[..6].iter().all(|&i| i >= 6 && painted(b.verts[i as usize].ov)));
        assert_eq!(&b.idx[6..], &[0, 5, 4]);
        assert!(b.verts[..6].iter().all(|v| !painted(v.ov)));
    }

    #[test]
    fn pack_keeps_origin_free() {
        let sizes = [(10, 3), (1, 1), (60, 20), (5, 40)];
        let (w, h, pos) = pack(&sizes, 4096).unwrap();
        let rects: Vec<(u32, u32, u32, u32)> = pos.iter().zip(&sizes).map(|(&(x, y), &(a, b))| (x - 1, y - 1, x + a + 1, y + b + 1)).collect();
        for (i, r) in rects.iter().enumerate() {
            assert!(r.2 <= w && r.3 <= h);
            assert!(r.0 >= 2 || r.1 >= 2);
            for q in &rects[i + 1..] {
                assert!(r.2 <= q.0 || q.2 <= r.0 || r.3 <= q.1 || q.3 <= r.1);
            }
        }
    }
}
