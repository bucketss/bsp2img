use std::collections::HashSet;

use glam::DVec3;
use rayon::prelude::*;

use crate::bsp::{Bsp, CONTENTS_SKY, CONTENTS_SOLID};

pub const SEED_CLASSES: &[&str] =
    &["info_player_start", "info_player_deathmatch", "info_vip_start", "hostage_entity", "monster_scientist"];

pub fn seeds(bsp: &Bsp) -> Vec<DVec3> {
    bsp.entities.iter().filter(|e| SEED_CLASSES.contains(&e.class())).filter_map(|e| e.origin()).collect()
}

struct Grid {
    lo: DVec3,
    spacing: f64,
    shape: [usize; 3],
}

impl Grid {
    fn new(bsp: &Bsp, max_cells: f64, min_spacing: f64, round: f64) -> Grid {
        let (lo, hi) = bsp.world_bounds();
        let size = (hi - lo).max(DVec3::ONE);
        let spacing = min_spacing.max(((size.x * size.y * size.z / max_cells).cbrt() / round).ceil() * round);
        let shape = [
            ((size.x / spacing).ceil() as usize).max(1),
            ((size.y / spacing).ceil() as usize).max(1),
            ((size.z / spacing).ceil() as usize).max(1),
        ];
        Grid { lo, spacing, shape }
    }
    fn len(&self) -> usize {
        self.shape[0] * self.shape[1] * self.shape[2]
    }
    fn idx(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }
    fn ijk(&self, n: usize) -> [usize; 3] {
        let k = n % self.shape[2];
        let j = (n / self.shape[2]) % self.shape[1];
        [n / (self.shape[1] * self.shape[2]), j, k]
    }
    fn center(&self, c: [usize; 3]) -> DVec3 {
        self.lo + (DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64) + 0.5) * self.spacing
    }
    fn cell_of(&self, p: DVec3) -> [usize; 3] {
        let f = ((p - self.lo) / self.spacing).floor();
        let cl = |v: f64, n: usize| (v.max(0.0) as usize).min(n - 1);
        [cl(f.x, self.shape[0]), cl(f.y, self.shape[1]), cl(f.z, self.shape[2])]
    }
    fn boxed(&self, c: [usize; 3], r: usize) -> impl Iterator<Item = usize> + '_ {
        let lo = |a: usize| c[a].saturating_sub(r);
        let hi = |a: usize| (c[a] + r + 1).min(self.shape[a]);
        let (i0, i1, j0, j1, k0, k1) = (lo(0), hi(0), lo(1), hi(1), lo(2), hi(2));
        (i0..i1).flat_map(move |i| (j0..j1).flat_map(move |j| (k0..k1).map(move |k| self.idx(i, j, k))))
    }
}

fn find(p: &mut [usize], mut x: usize) -> usize {
    while p[x] != x {
        p[x] = p[p[x]];
        x = p[x];
    }
    x
}

pub struct Reach {
    pub reach: Vec<bool>,
    pub sampled: Vec<bool>,
    pub spacing: f64,
    pub rect: [f64; 4],
    pub faces: HashSet<usize>,
}

impl Reach {
    pub fn keep_point(&self, bsp: &Bsp, p: DVec3) -> bool {
        let l = bsp.point_leaf(p);
        let [x0, y0, x1, y1] = self.rect;
        let inr = p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1;
        self.reach[l] || (!self.sampled[l] && bsp.leaf_open(l) && inr)
    }
    pub fn keep_face(&self, fi: usize) -> bool {
        self.faces.contains(&fi)
    }
}

pub fn analyze(bsp: &Bsp) -> Option<Reach> {
    let step = 2.0;
    let per_pair = 4;
    let sp = seeds(bsp);
    if sp.is_empty() {
        return None;
    }
    let g = Grid::new(bsp, 4_000_000.0, 16.0, 8.0);
    let leaf: Vec<i32> = (0..g.len())
        .into_par_iter()
        .map(|n| {
            let l = bsp.point_leaf(g.center(g.ijk(n)));
            if bsp.leaf_open(l) { l as i32 } else { -1 }
        })
        .collect();

    let nl = bsp.leaves.len();
    let mut pairs: Vec<(u64, usize, usize)> = Vec::new();
    for ax in 0..3 {
        let [si, sj, sk] = g.shape;
        let d = [si - (ax == 0) as usize, sj - (ax == 1) as usize, sk - (ax == 2) as usize];
        for i in 0..d[0] {
            for j in 0..d[1] {
                for k in 0..d[2] {
                    let a = g.idx(i, j, k);
                    let b = g.idx(i + (ax == 0) as usize, j + (ax == 1) as usize, k + (ax == 2) as usize);
                    let (la, lb) = (leaf[a], leaf[b]);
                    if la >= 0 && lb >= 0 && la != lb {
                        let key = la.min(lb) as u64 * (nl as u64 + 1) + la.max(lb) as u64;
                        pairs.push((key, a, b));
                    }
                }
            }
        }
    }
    pairs.sort_by_key(|p| p.0);
    let mut sel = Vec::with_capacity(pairs.len());
    let mut rank = 0;
    for (i, p) in pairs.iter().enumerate() {
        rank = if i > 0 && pairs[i - 1].0 == p.0 { rank + 1 } else { 0 };
        if rank < per_pair {
            sel.push(*p);
        }
    }
    let n = ((g.spacing / step) as usize).max(1);
    let clear: Vec<bool> = sel
        .par_iter()
        .map(|&(_, a, b)| {
            let (pa, pb) = (g.center(g.ijk(a)), g.center(g.ijk(b)));
            (1..n).all(|s| {
                let t = s as f64 / n as f64;
                bsp.leaf_open(bsp.point_leaf(pa * (1.0 - t) + pb * t))
            })
        })
        .collect();

    let mut parent: Vec<usize> = (0..nl).collect();
    for (&(key, _, _), &c) in sel.iter().zip(&clear) {
        if c {
            let a = find(&mut parent, (key / (nl as u64 + 1)) as usize);
            let b = find(&mut parent, (key % (nl as u64 + 1)) as usize);
            parent[a] = b;
        }
    }

    let mut keep = HashSet::new();
    for p in &sp {
        let l = bsp.point_leaf(*p);
        if bsp.leaf_open(l) {
            keep.insert(find(&mut parent, l));
            continue;
        }
        let c = g.cell_of(*p);
        for r in 1..4 {
            if let Some(f) = g.boxed(c, r).map(|n| leaf[n]).find(|&l| l >= 0) {
                keep.insert(find(&mut parent, f as usize));
                break;
            }
        }
    }
    if keep.is_empty() {
        return None;
    }
    let mut sampled = vec![false; nl];
    for &l in &leaf {
        if l >= 0 {
            sampled[l as usize] = true;
        }
    }
    let reach: Vec<bool> = (0..nl).map(|l| sampled[l] && keep.contains(&find(&mut parent, l))).collect();

    let h = g.spacing;
    let mut rect = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for (n, &l) in leaf.iter().enumerate() {
        if l >= 0 && reach[l as usize] {
            let c = g.center(g.ijk(n));
            rect = [rect[0].min(c.x), rect[1].min(c.y), rect[2].max(c.x), rect[3].max(c.y)];
        }
    }
    let rect = [rect[0] - h, rect[1] - h, rect[2] + h, rect[3] + h];

    let mut r = Reach { reach, sampled, spacing: g.spacing, rect, faces: HashSet::new() };
    for (li, lf) in bsp.leaves.iter().enumerate() {
        if r.reach[li] {
            let a = lf.firstmarksurface as usize;
            r.faces.extend(bsp.marksurfaces[a..a + lf.nummarksurfaces as usize].iter().map(|&m| m as usize));
        }
    }
    let m = &bsp.models[0];
    let rest: Vec<usize> =
        (m.firstface as usize..(m.firstface + m.numfaces) as usize).filter(|fi| !r.faces.contains(fi)).collect();
    let extra: Vec<usize> = rest
        .into_par_iter()
        .filter(|&fi| {
            let pts = bsp.face_points(fi);
            if pts.is_empty() {
                return false;
            }
            let c = pts.iter().copied().sum::<DVec3>() / pts.len() as f64 + bsp.face_normal(fi);
            r.keep_point(bsp, c)
        })
        .collect();
    r.faces.extend(extra);
    Some(r)
}

#[derive(Clone)]
pub struct HullMask {
    pub mask: Vec<bool>,
    pub nx: usize,
    pub ny: usize,
    pub rect: [f64; 4],
    pub spacing: f64,
    pub cells: usize,
}

impl HullMask {
    pub fn test(&self, x: f64, y: f64) -> bool {
        let [x0, y0, x1, y1] = self.rect;
        let i = ((x - x0) / (x1 - x0) * self.nx as f64).floor();
        let j = ((y - y0) / (y1 - y0) * self.ny as f64).floor();
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 {
            return false;
        }
        self.mask[i as usize * self.ny + j as usize]
    }

    pub fn texture(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.nx * self.ny];
        for i in 0..self.nx {
            for j in 0..self.ny {
                if self.mask[i * self.ny + j] {
                    out[j * self.nx + i] = 255;
                }
            }
        }
        out
    }
}

fn open_contents(c: i32) -> bool {
    c != CONTENTS_SOLID && c != CONTENTS_SKY
}

pub fn hull_mask(bsp: &Bsp, hull: usize, pad: f64) -> Option<HullMask> {
    if bsp.clipnodes.is_empty() {
        return None;
    }
    let sp = seeds(bsp);
    if sp.is_empty() {
        return None;
    }
    let g = Grid::new(bsp, 24_000_000.0, 8.0, 4.0);
    let open: Vec<bool> =
        (0..g.len()).into_par_iter().map(|n| open_contents(bsp.hull_contents(g.center(g.ijk(n)), hull))).collect();

    let mut label = vec![0u32; g.len()];
    let mut next = 1u32;
    let mut stack = Vec::new();
    let mut flood = |start: usize, label: &mut Vec<u32>, id: u32| {
        label[start] = id;
        stack.push(start);
        while let Some(n) = stack.pop() {
            let [i, j, k] = g.ijk(n);
            let mut nb = |c: usize| {
                if open[c] && label[c] == 0 {
                    label[c] = id;
                    stack.push(c);
                }
            };
            if i > 0 {
                nb(g.idx(i - 1, j, k));
            }
            if i + 1 < g.shape[0] {
                nb(g.idx(i + 1, j, k));
            }
            if j > 0 {
                nb(g.idx(i, j - 1, k));
            }
            if j + 1 < g.shape[1] {
                nb(g.idx(i, j + 1, k));
            }
            if k > 0 {
                nb(g.idx(i, j, k - 1));
            }
            if k + 1 < g.shape[2] {
                nb(g.idx(i, j, k + 1));
            }
        }
    };

    let mut keep: HashSet<u32> = HashSet::new();
    for p in &sp {
        let c = g.cell_of(*p);
        for r in 0..4 {
            let cells: Vec<usize> = g.boxed(c, r).filter(|&n| open[n]).collect();
            if cells.is_empty() {
                continue;
            }
            for &n in &cells {
                if label[n] == 0 {
                    flood(n, &mut label, next);
                    next += 1;
                }
            }
            let mut counts: std::collections::BTreeMap<u32, usize> = Default::default();
            for &n in &cells {
                *counts.entry(label[n]).or_default() += 1;
            }
            let best = counts.iter().max_by_key(|(l, c)| (**c, std::cmp::Reverse(**l))).map(|(l, _)| *l).unwrap();
            keep.insert(best);
            break;
        }
    }
    if keep.is_empty() {
        return None;
    }

    let (nx, ny, nz) = (g.shape[0], g.shape[1], g.shape[2]);
    let mut cells = 0;
    let mut foot = vec![false; nx * ny];
    for i in 0..nx {
        for j in 0..ny {
            for k in 0..nz {
                if keep.contains(&label[g.idx(i, j, k)]) {
                    cells += 1;
                    foot[i * ny + j] = true;
                }
            }
        }
    }

    let mut outside = vec![false; nx * ny];
    let mut st: Vec<usize> = Vec::new();
    for i in 0..nx {
        for j in 0..ny {
            if (i == 0 || j == 0 || i == nx - 1 || j == ny - 1) && !foot[i * ny + j] {
                outside[i * ny + j] = true;
                st.push(i * ny + j);
            }
        }
    }
    while let Some(n) = st.pop() {
        let (i, j) = (n / ny, n % ny);
        let nbs = [
            (i > 0).then(|| n - ny),
            (i + 1 < nx).then(|| n + ny),
            (j > 0).then(|| n - 1),
            (j + 1 < ny).then(|| n + 1),
        ];
        for c in nbs.into_iter().flatten() {
            if !foot[c] && !outside[c] {
                outside[c] = true;
                st.push(c);
            }
        }
    }
    let filled: Vec<bool> = outside.iter().map(|&o| !o).collect();

    let rad = (pad / g.spacing).ceil().max(0.0) as i64;
    let disc: Vec<(i64, i64)> = (-rad..=rad)
        .flat_map(|x| (-rad..=rad).map(move |y| (x, y)))
        .filter(|&(x, y)| x * x + y * y <= rad * rad)
        .collect();
    let mut mask = filled.clone();
    for i in 0..nx {
        for j in 0..ny {
            if !filled[i * ny + j] {
                continue;
            }
            let edge = (i == 0 || !filled[(i - 1) * ny + j])
                || (i + 1 == nx || !filled[(i + 1) * ny + j])
                || (j == 0 || !filled[i * ny + j - 1])
                || (j + 1 == ny || !filled[i * ny + j + 1]);
            if !edge {
                continue;
            }
            for &(dx, dy) in &disc {
                let (x, y) = (i as i64 + dx, j as i64 + dy);
                if x >= 0 && y >= 0 && (x as usize) < nx && (y as usize) < ny {
                    mask[x as usize * ny + y as usize] = true;
                }
            }
        }
    }
    let lo = g.lo;
    let rect = [lo.x, lo.y, lo.x + nx as f64 * g.spacing, lo.y + ny as f64 * g.spacing];
    Some(HullMask { mask, nx, ny, rect, spacing: g.spacing, cells })
}
