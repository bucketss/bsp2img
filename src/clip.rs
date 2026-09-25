use crate::mesh::Vertex;
use crate::reach::HullMask;
use crate::render::Cuts;

const OPEN: f64 = 1e8;
const EDGE_EPS: f64 = 1e-4;
const MAX_DEPTH: u32 = 48;

pub fn lerp(a: &Vertex, b: &Vertex, t: f32) -> Vertex {
    let l2 = |p: [f32; 2], q: [f32; 2]| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
    let l3 = |p: [f32; 3], q: [f32; 3]| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t, p[2] + (q[2] - p[2]) * t];
    let mut n = l3(a.normal, b.normal);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-12 {
        n = n.map(|c| c / len);
    }
    Vertex {
        pos: l3(a.pos, b.pos),
        uv: l2(a.uv, b.uv),
        lm: l2(a.lm, b.lm),
        bias: a.bias + (b.bias - a.bias) * t,
        normal: n,
    }
}

pub fn split(poly: &[Vertex], axis: usize, v: f64) -> (Vec<Vertex>, Vec<Vertex>) {
    let (mut lo, mut hi) = (Vec::new(), Vec::new());
    let n = poly.len();
    for i in 0..n {
        let (p, q) = (&poly[i], &poly[(i + 1) % n]);
        let (dp, dq) = (p.pos[axis] as f64 - v, q.pos[axis] as f64 - v);
        if dp <= 0.0 {
            lo.push(*p);
        }
        if dp >= 0.0 {
            hi.push(*p);
        }
        if (dp < 0.0 && dq > 0.0) || (dp > 0.0 && dq < 0.0) {
            let mut m = lerp(p, q, (dp / (dp - dq)) as f32);
            m.pos[axis] = v as f32;
            lo.push(m);
            hi.push(m);
        }
    }
    (lo, hi)
}

fn range(poly: &[Vertex], axis: usize) -> (f64, f64) {
    poly.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
        let c = p.pos[axis] as f64;
        (a.min(c), b.max(c))
    })
}

fn keep_below(poly: Vec<Vertex>, axis: usize, v: f64) -> Vec<Vertex> {
    if v >= OPEN || range(&poly, axis).1 <= v {
        return poly;
    }
    split(&poly, axis, v).0
}

fn keep_above(poly: Vec<Vertex>, axis: usize, v: f64) -> Vec<Vertex> {
    if v <= -OPEN || range(&poly, axis).0 >= v {
        return poly;
    }
    split(&poly, axis, v).1
}

pub fn fan(poly: &[Vertex], out: &mut Vec<Vertex>) {
    if poly.len() < 3 {
        return;
    }
    let p0 = poly[0].pos;
    for i in 1..poly.len() - 1 {
        let (a, b) = (poly[i].pos, poly[i + 1].pos);
        let e1 = [a[0] - p0[0], a[1] - p0[1], a[2] - p0[2]];
        let e2 = [b[0] - p0[0], b[1] - p0[1], b[2] - p0[2]];
        let c = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        if c[0] * c[0] + c[1] * c[1] + c[2] * c[2] <= 1e-12 {
            continue;
        }
        out.extend([poly[0], poly[i], poly[i + 1]]);
    }
}

pub fn clip_tris_z(verts: &[Vertex], planes: &[f64]) -> Vec<(usize, Vec<Vertex>)> {
    let mut planes = planes.to_vec();
    planes.sort_by(f64::total_cmp);
    planes.dedup();
    let mut bands: Vec<(usize, Vec<Vertex>)> = (0..=planes.len()).map(|b| (b, Vec::new())).collect();
    for tri in verts.chunks_exact(3) {
        let (z0, z1) = range(tri, 2);
        let first = planes.partition_point(|&p| p < z0);
        if planes.get(first).is_none_or(|&p| p >= z1) {
            bands[first].1.extend_from_slice(tri);
            continue;
        }
        let mut rest = tri.to_vec();
        for (k, &z) in planes.iter().enumerate().skip(first) {
            if rest.len() < 3 {
                break;
            }
            if range(&rest, 2).1 <= z {
                fan(&rest, &mut bands[k].1);
                rest.clear();
                break;
            }
            let (lo, hi) = split(&rest, 2, z);
            fan(&lo, &mut bands[k].1);
            rest = hi;
        }
        fan(&rest, &mut bands[planes.len()].1);
    }
    bands
}

struct Cells<'a> {
    m: &'a HullMask,
    dx: f64,
    dy: f64,
}

impl Cells<'_> {
    fn span(&self, poly: &[Vertex], axis: usize) -> (i64, i64) {
        let (lo, hi) = range(poly, axis);
        let (o, d) = if axis == 0 { (self.m.rect[0], self.dx) } else { (self.m.rect[1], self.dy) };
        (((lo - o) / d + EDGE_EPS).floor() as i64, ((hi - o) / d - EDGE_EPS).floor() as i64)
    }

    fn on(&self, i: i64, j: i64) -> bool {
        i >= 0 && j >= 0 && (i as usize) < self.m.nx && (j as usize) < self.m.ny && self.m.mask[i as usize * self.m.ny + j as usize]
    }

    fn keep(&self, poly: Vec<Vertex>, depth: u32, out: &mut Vec<Vertex>) {
        if poly.len() < 3 {
            return;
        }
        let (i0, i1) = self.span(&poly, 0);
        let (j0, j1) = self.span(&poly, 1);
        let (i1, j1) = (i1.max(i0), j1.max(j0));
        let (mut any, mut all) = (false, true);
        'scan: for i in i0..=i1 {
            for j in j0..=j1 {
                let v = self.on(i, j);
                any |= v;
                all &= v;
                if any && !all {
                    break 'scan;
                }
            }
        }
        if all {
            fan(&poly, out);
            return;
        }
        if !any {
            return;
        }
        if depth >= MAX_DEPTH {
            let n = poly.len() as f64;
            let cx = poly.iter().map(|p| p.pos[0] as f64).sum::<f64>() / n;
            let cy = poly.iter().map(|p| p.pos[1] as f64).sum::<f64>() / n;
            if self.m.test(cx, cy) {
                fan(&poly, out);
            }
            return;
        }
        let (axis, v) = if i1 - i0 >= j1 - j0 {
            (0, self.m.rect[0] + ((i0 + i1 + 1) / 2) as f64 * self.dx)
        } else {
            (1, self.m.rect[1] + ((j0 + j1 + 1) / 2) as f64 * self.dy)
        };
        let (lo, hi) = split(&poly, axis, v);
        self.keep(lo, depth + 1, out);
        self.keep(hi, depth + 1, out);
    }
}

pub fn clip_tris_cuts(verts: &[Vertex], cuts: &Cuts, mask: Option<&HullMask>) -> Vec<Vertex> {
    let mask = mask.filter(|m| cuts.use_mask && m.nx > 0 && m.ny > 0);
    let cells = mask.map(|m| Cells {
        m,
        dx: (m.rect[2] - m.rect[0]) / m.nx as f64,
        dy: (m.rect[3] - m.rect[1]) / m.ny as f64,
    });
    let [x0, y0, x1, y1] = cuts.clip;
    let mut out = Vec::with_capacity(verts.len());
    for tri in verts.chunks_exact(3) {
        let mut p = tri.to_vec();
        p = keep_above(p, 2, cuts.zmin);
        p = keep_below(p, 2, cuts.zmax);
        p = keep_above(p, 0, x0);
        p = keep_below(p, 0, x1);
        p = keep_above(p, 1, y0);
        p = keep_below(p, 1, y1);
        if p.len() < 3 {
            continue;
        }
        match &cells {
            Some(c) => c.keep(p, 0, &mut out),
            None if p.len() == 3 => out.extend_from_slice(&p),
            None => fan(&p, &mut out),
        }
    }
    out
}
