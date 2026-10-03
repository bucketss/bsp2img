use std::cmp::Reverse;
use std::collections::HashMap;

use glam::{DVec2, DVec3};
use rayon::prelude::*;

use super::{Image, Mesh};

pub const TEXEL: f64 = 4.0;
pub const SIGMA: f64 = 8.0;
pub const MAX_ALPHA: f64 = 0.8;
const BUDGET: u64 = 1 << 23;
const PLANE_TOL: f64 = 1.0;
const CELL: f64 = 64.0;
const MIN_ALPHA: f64 = 0.12;
const NORM_PCT: f64 = 0.6;
const NORM_FLOOR: f32 = 1e-3;

#[derive(Clone, Copy, Debug)]
pub struct Splat {
    pub p: DVec3,
    pub facing: Option<DVec3>,
    pub model: Option<u32>,
    pub layer: u8,
    pub sigma: f64,
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

struct Face {
    span: usize,
    n: DVec3,
    d: f64,
    u: DVec3,
    v: DVec3,
    poly: Vec<DVec2>,
    sign: f64,
    lo: DVec2,
    hi: DVec2,
    model: u32,
}

impl Face {
    fn to2(&self, p: DVec3) -> DVec2 {
        DVec2::new(p.dot(self.u), p.dot(self.v))
    }

    fn dist(&self, q: DVec2) -> f64 {
        let n = self.poly.len();
        let mut inside = true;
        let mut best = f64::INFINITY;
        for i in 0..n {
            let (a, b) = (self.poly[i], self.poly[(i + 1) % n]);
            let e = b - a;
            if self.sign * e.perp_dot(q - a) < 0.0 {
                inside = false;
            }
            let t = ((q - a).dot(e) / e.length_squared().max(1e-12)).clamp(0.0, 1.0);
            best = best.min((a + e * t).distance(q));
        }
        if inside { 0.0 } else { best }
    }
}

fn faces(mesh: &Mesh) -> Vec<Face> {
    mesh.face_spans
        .iter()
        .enumerate()
        .filter_map(|(span, fs)| {
            let b = mesh.batches.get(fs.batch as usize)?;
            let vs = b.verts.get(fs.first as usize..(fs.first + fs.count) as usize)?;
            let p: Vec<DVec3> = vs.iter().map(|v| DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64)).collect();
            let n = DVec3::new(vs[0].normal[0] as f64, vs[0].normal[1] as f64, vs[0].normal[2] as f64).normalize_or_zero();
            if n == DVec3::ZERO || p.len() < 3 {
                return None;
            }
            let u = n.any_orthonormal_vector();
            let v = n.cross(u);
            let poly: Vec<DVec2> = p.iter().map(|q| DVec2::new(q.dot(u), q.dot(v))).collect();
            let area: f64 = (0..poly.len()).map(|i| poly[i].perp_dot(poly[(i + 1) % poly.len()])).sum();
            let lo = poly.iter().fold(DVec2::INFINITY, |m, q| m.min(*q));
            let hi = poly.iter().fold(DVec2::NEG_INFINITY, |m, q| m.max(*q));
            let d = p.iter().map(|q| q.dot(n)).sum::<f64>() / p.len() as f64;
            Some(Face { span, n, d, u, v, poly, sign: if area < 0.0 { -1.0 } else { 1.0 }, lo, hi, model: fs.model })
        })
        .collect()
}

type Key = (i32, i32, i32);

fn cell(p: DVec3) -> Key {
    ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32, (p.z / CELL).floor() as i32)
}

fn grid(faces: &[Face], mesh: &Mesh, pad: f64) -> HashMap<Key, Vec<u32>> {
    let mut g: HashMap<Key, Vec<u32>> = HashMap::new();
    for (i, f) in faces.iter().enumerate() {
        let fs = &mesh.face_spans[f.span];
        let vs = &mesh.batches[fs.batch as usize].verts[fs.first as usize..(fs.first + fs.count) as usize];
        let (lo, hi) = vs.iter().fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(lo, hi), v| {
            let p = DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64);
            (lo.min(p), hi.max(p))
        });
        let (a, b) = (cell(lo - DVec3::splat(pad)), cell(hi + DVec3::splat(pad)));
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

fn pack(sizes: &[(u32, u32)], max_dim: u32) -> Option<(u32, u32, Vec<(u32, u32)>)> {
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
    let fs = faces(mesh);
    if fs.is_empty() || splats.is_empty() {
        mesh.overlay = None;
        return out;
    }
    let reach = 3.0 * splats.iter().map(|s| s.sigma).fold(0.0, f64::max);
    let g = grid(&fs, mesh, reach + PLANE_TOL);
    let hits: Vec<Vec<(u32, DVec2, u8, f64)>> = splats
        .par_iter()
        .map(|s| {
            let Some(cands) = g.get(&cell(s.p)) else { return Vec::new() };
            cands
                .iter()
                .filter_map(|&i| {
                    let f = &fs[i as usize];
                    if (s.p.dot(f.n) - f.d).abs() > PLANE_TOL {
                        return None;
                    }
                    if s.facing.is_some_and(|d| f.n.dot(d) > -0.01) || s.model.is_some_and(|m| m != f.model) {
                        return None;
                    }
                    let q = f.to2(s.p);
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
        let span = mesh.face_spans[f.span];
        let b = &mut mesh.batches[span.batch as usize];
        for v in &mut b.verts[span.first as usize..(span.first + span.count) as usize] {
            let q = f.to2(DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64));
            v.uv2 = [
                ((px as f64 + 0.5 + (q.x - f.lo.x) / texel) / aw as f64) as f32,
                ((py as f64 + 0.5 + (q.y - f.lo.y) / texel) / ah as f64) as f32,
            ];
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
