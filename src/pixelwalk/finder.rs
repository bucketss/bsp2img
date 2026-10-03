use std::collections::{HashMap, HashSet};

use rayon::prelude::*;

use super::hull::{CONTENTS_EMPTY, CONTENTS_SOLID, PLAYER_MINS, PwMap, hull_index};
use super::pmove::Pm;
use super::seams::Seam;
use super::vec::{V3, arr_lt, atan2, cmax, cmin, lround};
use super::world::{Physent, WorldModels, select_physents};

const PW_EPS: f32 = 0.03125;
const CATCH_MIN: i32 = 3;
const NF: i32 = 15;
const BAND: f32 = 0.2;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Find {
    pub pos: V3,
    pub usehull: usize,
    pub by_slope: bool,
    pub floor_normal: V3,
    pub floor_dist: f32,
    pub approach: V3,
    pub floor_model: i32,
    pub wall_model: i32,
    pub catch_frames: i32,
    pub cluster_size: i32,
    pub to: V3,
    pub length: f32,
}

#[derive(Clone, Debug)]
pub struct FinderConfig {
    pub standing: bool,
    pub duck: bool,
    pub grid: f32,
    pub along_step: f32,
    pub max_finds: usize,
    pub min_samples: i32,
    pub skip_categorize: bool,
    pub zones: bool,
    pub zone_gap: f32,
}

impl Default for FinderConfig {
    fn default() -> Self {
        FinderConfig {
            standing: true,
            duck: true,
            grid: 1.0 / 64.0,
            along_step: 1.0,
            max_finds: 0,
            min_samples: 2,
            skip_categorize: false,
            zones: true,
            zone_gap: 8.0,
        }
    }
}

fn floor_z_at(s: &Seam, x: f32, y: f32) -> f32 {
    let nz = s.fn_[2];
    if nz.abs() < 1e-4 {
        return s.a[2];
    }
    (s.fd - s.fn_[0] * x - s.fn_[1] * y) / nz
}

fn point_contents_multi(map: &PwMap, phys: &[Physent], usehull: usize, p: &V3) -> i32 {
    let hi = hull_index(usehull);
    for pe in phys {
        let hull = map.hull(pe.model, hi);
        let local: V3 = std::array::from_fn(|k| p[k] - (hull.clip_mins[k] - PLAYER_MINS[usehull][k] + pe.origin[k]));
        if map.point_contents(&hull, hull.first, &local) == CONTENTS_SOLID {
            return CONTENTS_SOLID;
        }
    }
    CONTENTS_EMPTY
}

fn settles_on_ground(map: &PwMap, phys: &[Physent], usehull: usize, p: &V3) -> bool {
    let mut pm = Pm::new(map, phys, usehull, PW_EPS);
    pm.origin = *p;
    pm.player_move_frame();
    pm.onground != -1
}

struct SimOut {
    catch_frames: i32,
    catch_floor_ent: i32,
    catch_wall_ent: i32,
}

fn sim_drive(map: &PwMap, phys: &[Physent], usehull: usize, start: &V3, md: &V3, eps: f32, nframes: i32) -> SimOut {
    let mut pm = Pm::new(map, phys, usehull, eps);
    pm.fmove = 400.0;
    let yawd = atan2(md[1] as f64, md[0] as f64) * (180.0 / 3.14159265358979323846);
    let yaw = ((yawd * (65536.0 / 360.0)).round() * (360.0 / 65536.0)) as f32;
    pm.angles = [0.0, yaw, 0.0];
    pm.origin = *start;
    let mut caught = 0;
    let (mut cfe, mut cwe) = (-1, -1);
    for _ in 0..nframes {
        pm.player_move_frame();
        let free = point_contents_multi(map, phys, usehull, &pm.origin) != CONTENTS_SOLID;
        if pm.blocked & 1 != 0 && pm.onground == -1 && free {
            caught += 1;
            cfe = pm.floor_ent;
            if pm.wall_ent >= 0 {
                cwe = pm.wall_ent;
            }
        }
    }
    SimOut { catch_frames: caught, catch_floor_ent: cfe, catch_wall_ent: cwe }
}

fn pos_key(p: &V3, usehull: usize) -> u64 {
    let x = lround(p[0] / 2.0) as u64;
    let y = lround(p[1] / 2.0) as u64;
    let z = lround(p[2] / 2.0) as u64;
    ((x & 0x1FFFFF) << 43) ^ ((y & 0x1FFFFF) << 22) ^ ((z & 0x1FFFFF) << 1) ^ usehull as u64
}

struct SeamCtx<'a> {
    map: &'a PwMap,
    seam: &'a Seam,
    phys: Vec<Physent>,
    a: V3,
    dir: V3,
    outn: V3,
    md: V3,
    ts: Vec<f32>,
    os: Vec<f32>,
    len: f32,
    skip_categorize: bool,
}

fn seam_ctx<'a>(map: &'a PwMap, seam: &'a Seam, wm: &WorldModels, cfg: &FinderConfig) -> Option<SeamCtx<'a>> {
    let usehull = seam.usehull;
    if usehull == 0 && !cfg.standing || usehull == 1 && !cfg.duck {
        return None;
    }
    let a = seam.a;
    let b = seam.b;
    let mut outn = [seam.outn[0], seam.outn[1], 0.0];
    let onl = (outn[0] * outn[0] + outn[1] * outn[1]).sqrt();
    if onl < 1e-4 {
        return None;
    }
    outn[0] /= onl;
    outn[1] /= onl;
    let mut dir = [b[0] - a[0], b[1] - a[1], 0.0];
    let len = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
    if len < 1e-3 {
        return None;
    }
    dir[0] /= len;
    dir[1] /= len;
    let za = floor_z_at(seam, a[0], a[1]);
    let zb = floor_z_at(seam, b[0], b[1]);
    let (zlo, zhi) = (cmin(za, zb), cmax(za, zb));
    let qmins = [cmin(a[0], b[0]) - 64.0, cmin(a[1], b[1]) - 64.0, zlo - 96.0];
    let qmaxs = [cmax(a[0], b[0]) + 64.0, cmax(a[1], b[1]) + 64.0, zhi + 96.0];
    let phys = select_physents(wm, &qmins, &qmaxs);
    let mut ts = Vec::new();
    let mut t = 0.0f32;
    while t <= len + 1e-3 {
        ts.push(t);
        t += cfg.along_step;
    }
    let mut os = Vec::new();
    let mut o = -BAND;
    while o <= BAND + 1e-6 {
        os.push(o);
        o += cfg.grid;
    }
    Some(SeamCtx {
        map,
        seam,
        phys,
        a,
        dir,
        outn,
        md: [-outn[0], -outn[1], 0.0],
        ts,
        os,
        len,
        skip_categorize: cfg.skip_categorize,
    })
}

impl SeamCtx<'_> {
    fn sample(&self, ti: usize, oi: usize) -> Option<(Find, u64)> {
        let usehull = self.seam.usehull;
        let tt = cmin(self.ts[ti], self.len);
        let o = self.os[oi];
        let x = self.a[0] + self.dir[0] * tt + self.outn[0] * o;
        let y = self.a[1] + self.dir[1] * tt + self.outn[1] * o;
        let start = [x, y, floor_z_at(self.seam, x, y)];
        let (map, phys) = (self.map, &self.phys[..]);
        if point_contents_multi(map, phys, usehull, &start) == CONTENTS_SOLID {
            return None;
        }
        if !self.skip_categorize && settles_on_ground(map, phys, usehull, &start) {
            return None;
        }
        if sim_drive(map, phys, usehull, &start, &self.md, PW_EPS, 3).catch_frames < 1 {
            return None;
        }
        let ea = sim_drive(map, phys, usehull, &start, &self.md, PW_EPS, NF);
        if ea.catch_frames < CATCH_MIN {
            return None;
        }
        if sim_drive(map, phys, usehull, &start, &self.md, 0.0, NF).catch_frames >= CATCH_MIN {
            return None;
        }
        let model_of = |e: i32| if e >= 0 && (e as usize) < phys.len() { phys[e as usize].info } else { -1 };
        let (cfm, cwm) = (model_of(ea.catch_floor_ent), model_of(ea.catch_wall_ent));
        let floor_model = if cfm >= 0 { cfm } else { self.seam.floor_model };
        let wall_model = if cwm >= 0 { cwm } else { self.seam.wall_model };
        if wall_model < 0 || (floor_model >= 0 && wall_model <= floor_model) {
            return None;
        }
        let f = Find {
            pos: start,
            usehull,
            by_slope: self.seam.slope,
            floor_normal: self.seam.fn_,
            floor_dist: self.seam.fd,
            approach: [-self.outn[0], -self.outn[1], 0.0],
            floor_model,
            wall_model,
            catch_frames: ea.catch_frames,
            cluster_size: 1,
            to: [0.0; 3],
            length: 0.0,
        };
        Some((f, pos_key(&start, usehull)))
    }
}

struct SampleRec {
    cands: Vec<(Find, u64)>,
    stopped: bool,
    next: usize,
}

fn speculate(ctx: &SeamCtx) -> Vec<SampleRec> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(ctx.ts.len());
    for ti in 0..ctx.ts.len() {
        let mut rec = SampleRec { cands: Vec::new(), stopped: false, next: ctx.os.len() };
        for oi in 0..ctx.os.len() {
            if let Some((f, key)) = ctx.sample(ti, oi) {
                rec.cands.push((f, key));
                if seen.insert(key) {
                    rec.stopped = true;
                    rec.next = oi + 1;
                    break;
                }
            }
        }
        out.push(rec);
    }
    out
}

fn seam_finds(map: &PwMap, wm: &WorldModels, seams: &[Seam], cfg: &FinderConfig) -> Vec<Find> {
    let specs: Vec<Option<Vec<SampleRec>>> =
        seams.par_iter().map(|s| seam_ctx(map, s, wm, cfg).map(|ctx| speculate(&ctx))).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (s, spec) in seams.iter().zip(specs) {
        let Some(spec) = spec else { continue };
        let mut ctx = None;
        for (ti, rec) in spec.into_iter().enumerate() {
            if let Some((f, _)) = rec.cands.iter().find(|(_, k)| seen.insert(*k)) {
                out.push(*f);
                continue;
            }
            if !rec.stopped {
                continue;
            }
            let c = ctx.get_or_insert_with(|| seam_ctx(map, s, wm, cfg).unwrap());
            for oi in rec.next..c.os.len() {
                if let Some((f, key)) = c.sample(ti, oi)
                    && seen.insert(key)
                {
                    out.push(f);
                    break;
                }
            }
        }
    }
    out
}

fn line_key(f: &Find) -> u64 {
    let parts: [i64; 9] = [
        f.usehull as i64,
        lround(f.approach[0] * 100.0),
        lround(f.approach[1] * 100.0),
        lround(f.floor_normal[0] * 100.0),
        lround(f.floor_normal[1] * 100.0),
        lround(f.floor_normal[2] * 100.0),
        lround(f.floor_dist),
        f.floor_model as i64,
        f.wall_model as i64,
    ];
    let mut h: u64 = 1469598103934665603;
    for v in parts {
        h = (h ^ (v as u64 & 0xFFFFF)).wrapping_mul(1099511628211);
    }
    h
}

fn nonzero(n: &V3) -> bool {
    n[0] != 0.0 || n[1] != 0.0 || n[2] != 0.0
}

pub fn run_finder(map: &PwMap, wm: &WorldModels, seams: &[Seam], cfg: &FinderConfig) -> Vec<Find> {
    let raw = seam_finds(map, wm, seams, cfg);
    let mut finds: Vec<Find> = Vec::new();
    let mut idx: HashMap<u64, usize> = HashMap::new();
    for f in raw {
        let key = pos_key(&f.pos, f.usehull);
        match idx.get(&key) {
            None => {
                idx.insert(key, finds.len());
                finds.push(f);
            }
            Some(&i) => {
                let g = &mut finds[i];
                g.by_slope |= f.by_slope;
                if f.catch_frames > g.catch_frames {
                    g.catch_frames = f.catch_frames;
                }
            }
        }
    }
    if !cfg.zones {
        const CELL: f32 = 64.0;
        let mut cidx: HashMap<u64, usize> = HashMap::new();
        let mut clustered: Vec<Find> = Vec::new();
        for f in finds {
            let kx = lround(f.pos[0] / CELL) as u64;
            let ky = lround(f.pos[1] / CELL) as u64;
            let kz = lround(f.pos[2] / CELL) as u64;
            let key = ((kx & 0x1FFFFF) << 43) ^ ((ky & 0x1FFFFF) << 22) ^ ((kz & 0x1FFFFF) << 1) ^ f.usehull as u64;
            match cidx.get(&key) {
                None => {
                    cidx.insert(key, clustered.len());
                    clustered.push(f);
                }
                Some(&i) => {
                    let g = &mut clustered[i];
                    g.cluster_size += 1;
                    g.by_slope |= f.by_slope;
                    if f.catch_frames > g.catch_frames {
                        g.pos = f.pos;
                        g.catch_frames = f.catch_frames;
                        g.floor_normal = f.floor_normal;
                        g.floor_dist = f.floor_dist;
                        g.approach = f.approach;
                        g.floor_model = f.floor_model;
                        g.wall_model = f.wall_model;
                    }
                }
            }
        }
        finds = clustered;
    } else {
        const PERP_TOL: f32 = 4.0;
        let mut order: Vec<u64> = Vec::new();
        let mut groups: HashMap<u64, Vec<Find>> = HashMap::new();
        for f in finds {
            let k = line_key(&f);
            groups
                .entry(k)
                .or_insert_with(|| {
                    order.push(k);
                    Vec::new()
                })
                .push(f);
        }
        let mut zones = Vec::new();
        for k in order {
            let mut g = groups.remove(&k).unwrap();
            let (ax, ay) = (g[0].approach[0], g[0].approach[1]);
            let (adx, ady) = (-ay, ax);
            let along = |f: &Find| f.pos[0] * adx + f.pos[1] * ady;
            let perp = |f: &Find| f.pos[0] * ax + f.pos[1] * ay;
            g.sort_by(|p, q| {
                use std::cmp::Ordering;
                let (ap, aq) = (along(p), along(q));
                if ap != aq {
                    return if ap < aq { Ordering::Less } else { Ordering::Greater };
                }
                let (pp, pq) = (perp(p), perp(q));
                if pp != pq {
                    return if pp < pq { Ordering::Less } else { Ordering::Greater };
                }
                if arr_lt(&p.pos, &q.pos) {
                    Ordering::Less
                } else if arr_lt(&q.pos, &p.pos) {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            });
            let mut i = 0;
            while i < g.len() {
                let mut j = i;
                while j + 1 < g.len() {
                    if along(&g[j + 1]) - along(&g[j]) > cfg.zone_gap {
                        break;
                    }
                    if (perp(&g[j + 1]) - perp(&g[j])).abs() > PERP_TOL {
                        break;
                    }
                    j += 1;
                }
                let mut z = g[i];
                z.to = g[j].pos;
                z.length = along(&g[j]) - along(&g[i]);
                z.cluster_size = (j - i + 1) as i32;
                z.catch_frames = 0;
                let mut have_n = nonzero(&z.floor_normal);
                for gk in &g[i..=j] {
                    z.by_slope |= gk.by_slope;
                    if gk.catch_frames > z.catch_frames {
                        z.catch_frames = gk.catch_frames;
                        z.approach = gk.approach;
                        z.floor_model = gk.floor_model;
                        z.wall_model = gk.wall_model;
                    }
                    if !have_n && nonzero(&gk.floor_normal) {
                        z.floor_normal = gk.floor_normal;
                        z.floor_dist = gk.floor_dist;
                        have_n = true;
                    }
                }
                zones.push(z);
                i = j + 1;
            }
        }
        finds = zones;
    }
    finds.sort_by(|a, b| {
        use std::cmp::Ordering;
        if a.usehull != b.usehull {
            return a.usehull.cmp(&b.usehull);
        }
        if a.cluster_size != b.cluster_size {
            return b.cluster_size.cmp(&a.cluster_size);
        }
        if a.catch_frames != b.catch_frames {
            return b.catch_frames.cmp(&a.catch_frames);
        }
        if a.pos != b.pos {
            return if arr_lt(&a.pos, &b.pos) { Ordering::Less } else { Ordering::Greater };
        }
        if arr_lt(&a.to, &b.to) {
            Ordering::Less
        } else if arr_lt(&b.to, &a.to) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    });
    if cfg.min_samples > 1 {
        finds.retain(|f| f.cluster_size >= cfg.min_samples);
    }
    if cfg.max_finds > 0 && finds.len() > cfg.max_finds {
        finds.truncate(cfg.max_finds);
    }
    finds
}
