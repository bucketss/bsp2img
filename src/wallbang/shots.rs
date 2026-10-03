use std::collections::BTreeMap;

use glam::DVec3;
use rayon::prelude::*;

use super::World;
use super::hull0::{CONTENTS_SOLID, V3};
use super::sim::{self, IT_HIT, Weapon};
use crate::nav::{Nav, STAND_OFS};

pub const STAND: u8 = 0;
pub const DUCK: u8 = 1;
const STAND_EYE: f64 = 53.0;
const DUCK_EYE: f64 = 30.0;
const DUCK_ORIGIN: f64 = 18.0;
const LEVEL_GAP: f32 = 72.0;
const CAP: f64 = 4096.0;
const BEHIND: f64 = 512.0;
const PLAY_R: f64 = 40.0;
const PLAY_DZ: f64 = 36.0;
const PLAY_TRACES: usize = 6;
const COLUMN_STEP: f64 = 24.0;
const FLOOR_R: f64 = 96.0;
const FLOOR_TOL: f64 = 96.0;
const FLOOR_BELOW: f64 = 512.0;
const DEATH_R: f64 = 128.0;
const DEATH_DZ: f64 = 160.0;
const CORNER_PATH: f64 = 4.0;
const CORNER_NORMAL: f64 = 8.0;
const GRAZE: f64 = 0.01;

#[derive(Clone, Debug)]
pub struct ShotOpts {
    pub weapon: &'static Weapon,
    pub stand: bool,
    pub duck: bool,
    pub yaw_step: f64,
    pub pitch: (f64, f64, f64),
    pub spot_cell: f64,
    pub max_hit: f64,
    pub min_thickness: f64,
    pub slab: f64,
}

impl ShotOpts {
    pub fn yaws(&self) -> Vec<f64> {
        let n = (360.0 / self.yaw_step).round().max(1.0) as usize;
        (0..n).map(|k| k as f64 * self.yaw_step).collect()
    }

    pub fn pitches(&self) -> Vec<f64> {
        let (a, b, s) = self.pitch;
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            let p = a + i as f64 * s;
            if p > b + 1e-9 || out.len() > 1000 {
                break;
            }
            out.push(p);
            i += 1;
        }
        out
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Spot {
    pub feet: DVec3,
    pub stance: u8,
}

impl Spot {
    pub fn eye(&self) -> DVec3 {
        self.feet + DVec3::Z * if self.stance == STAND { STAND_EYE } else { DUCK_EYE }
    }

    pub fn origin(&self) -> DVec3 {
        self.feet + DVec3::Z * if self.stance == STAND { STAND_OFS } else { DUCK_ORIGIN }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Crossing {
    pub model: u16,
    pub entry: DVec3,
    pub entry_face: Option<u32>,
    pub exit: DVec3,
    pub made: bool,
    pub death: Option<DVec3>,
    pub damage: u16,
    pub material: u8,
    pub thickness: f32,
    pub path: f32,
}

#[derive(Clone, Debug)]
pub struct Shot {
    pub shooter: u32,
    pub stance: u8,
    pub pitch_idx: u8,
    pub yaw_idx: u16,
    pub dir: DVec3,
    pub crossings: Vec<Crossing>,
}

impl Shot {
    pub fn made(&self) -> bool {
        self.crossings.iter().all(|c| c.made)
    }

    pub fn start(&self) -> DVec3 {
        self.crossings[0].entry - self.dir * 24.0
    }

    pub fn end(&self) -> DVec3 {
        let last = self.crossings.last().unwrap();
        match last.death {
            Some(d) if !last.made => d,
            _ => last.exit + self.dir * 48.0,
        }
    }

    pub fn path(&self) -> f32 {
        self.crossings.iter().map(|c| c.path).sum()
    }

    pub fn damage(&self) -> u16 {
        self.crossings.last().map_or(0, |c| if c.made { c.damage } else { 0 })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub fired: u64,
    pub hits: u64,
    pub far: u64,
    pub no_exit: u64,
    pub behind: u64,
    pub slab: u64,
    pub column: u64,
    pub void: u64,
    pub corner: u64,
    pub thin: u64,
    pub cut: u64,
    pub made: u64,
    pub died: u64,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.fired += o.fired;
        self.hits += o.hits;
        self.far += o.far;
        self.no_exit += o.no_exit;
        self.behind += o.behind;
        self.slab += o.slab;
        self.column += o.column;
        self.void += o.void;
        self.corner += o.corner;
        self.thin += o.thin;
        self.cut += o.cut;
        self.made += o.made;
        self.died += o.died;
    }
}

pub struct Ctx<'a> {
    pub world: &'a World,
    pub nav: &'a Nav,
    pub reach: &'a [bool],
    pub opts: &'a ShotOpts,
}

fn v3(p: DVec3) -> V3 {
    [p.x as f32, p.y as f32, p.z as f32]
}

fn dv(p: &V3) -> DVec3 {
    DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
}

enum Drop {
    NoExit,
    Behind,
    Slab,
    Column,
    Void,
    Corner,
    Thin,
}

impl Ctx<'_> {
    fn model_ref(&self, model: u16) -> (i32, DVec3) {
        if model != 0
            && let Some(e) = self.world.ents.iter().find(|e| e.model == model as usize)
        {
            return (e.head, dv(&e.origin));
        }
        (self.world.world_head, DVec3::ZERO)
    }

    fn open_dist(&self, model: u16, p: DVec3, d: DVec3, cap: f64) -> Option<f64> {
        let (head, o) = self.model_ref(model);
        self.world.hull.first_open(head, p - o, p - o + d * cap).map(|f| f * cap)
    }

    fn solid_at(&self, model: u16, p: DVec3) -> bool {
        let (head, o) = self.model_ref(model);
        self.world.hull.contents64(head, p - o) == CONTENTS_SOLID
    }

    fn trace_frac(&self, a: DVec3, b: DVec3) -> (f32, DVec3, bool) {
        let (t, _) = self.world.trace(&v3(a), &v3(b), true);
        (t.fraction, dv(&t.endpos), t.startsolid)
    }

    fn nodes_near(&self, p: DVec3, r: f64) -> impl Iterator<Item = usize> + '_ {
        self.nav
            .cols_in(p.x - r, p.y - r, p.x + r, p.y + r)
            .flat_map(|c| self.nav.column(c))
            .filter(|&n| self.reach[n])
    }

    fn node_origin_z(&self, n: usize) -> f64 {
        let nd = &self.nav.nodes[n];
        nd.z as f64 + if nd.stand { STAND_OFS } else { DUCK_ORIGIN }
    }

    fn playable(&self, p: DVec3) -> bool {
        let mut c: Vec<(f64, usize)> = self
            .nodes_near(p, PLAY_R)
            .filter_map(|n| {
                let (x, y) = self.nav.cell_xy(self.nav.col[n] as usize);
                let d = (x - p.x).hypot(y - p.y);
                (d <= PLAY_R && (self.node_origin_z(n) - p.z).abs() <= PLAY_DZ).then_some((d, n))
            })
            .collect();
        c.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        c.iter().take(PLAY_TRACES).any(|&(_, n)| {
            let (x, y) = self.nav.cell_xy(self.nav.col[n] as usize);
            let (f, _, ss) = self.trace_frac(p, DVec3::new(x, y, p.z));
            f == 1.0 && !ss
        })
    }

    fn floor_supports_play(&self, x: f64, y: f64, fz: f64) -> bool {
        self.nodes_near(DVec3::new(x, y, fz), FLOOR_R).any(|n| (self.nav.nodes[n].z as f64 - fz).abs() <= FLOOR_TOL)
    }

    fn in_play(&self, p: DVec3) -> bool {
        self.nodes_near(p, DEATH_R).any(|n| (self.node_origin_z(n) - p.z).abs() <= DEATH_DZ)
    }

    fn ground_or_ceiling(&self, model: u16, q: DVec3) -> bool {
        let tu = self.open_dist(model, q, DVec3::Z, CAP);
        let td = self.open_dist(model, q, -DVec3::Z, CAP);
        if tu.unwrap_or(CAP) + td.unwrap_or(CAP) <= self.opts.slab {
            return false;
        }
        if let Some(u) = tu
            && self.floor_supports_play(q.x, q.y, q.z + u)
        {
            return true;
        }
        if let Some(d) = td {
            let o = q - DVec3::Z * (d + 0.5);
            let (f, end, _) = self.trace_frac(o, o - DVec3::Z * FLOOR_BELOW);
            if f < 1.0 && self.floor_supports_play(o.x, o.y, end.z) {
                return true;
            }
        }
        false
    }

    fn crossing(&self, f: &sim::Fired, k: usize, dir: DVec3) -> Result<Crossing, Drop> {
        let it = &f.it[k - 1];
        let next = &f.it[k];
        let made = !next.own_dead;
        let model = it.model;
        let hp = dv(&it.endpos);
        let mut n = dv(&it.normal);
        if n.length_squared() < 0.5 {
            n = -dir;
        }
        let mut dn = dir.dot(n);
        if dn > 0.0 {
            n = -n;
            dn = -dn;
        }
        if -dn < GRAZE {
            return Err(Drop::Corner);
        }
        let step = (0.25 / (-dn).max(1e-3)).min(64.0);
        let inside = hp + dir * step;
        let solid = self.open_dist(model, inside, dir, CAP).ok_or(Drop::NoExit)?;
        let exit = inside + dir * (solid + 0.1);
        if made && (exit - hp).dot(dir) > (dv(&next.endpos) - hp).dot(dir) + 1.0 {
            return Err(Drop::NoExit);
        }
        let q = hp - n * 0.5;
        let normal_thick = self.open_dist(model, q, -n, CAP).map_or(CAP, |t| t + 0.5);
        let path = step + solid;
        if path < CORNER_PATH && normal_thick > CORNER_NORMAL {
            return Err(Drop::Corner);
        }
        if self.opts.min_thickness > 0.0 && normal_thick < self.opts.min_thickness {
            return Err(Drop::Thin);
        }
        if n.z.abs() > 0.3 {
            let v = self.open_dist(model, q, DVec3::new(0.0, 0.0, if n.z > 0.0 { -1.0 } else { 1.0 }), CAP);
            if v.is_none_or(|v| v > self.opts.slab) {
                return Err(Drop::Slab);
            }
        }
        let mut death = None;
        if made {
            let (fr, _, _) = self.trace_frac(exit, exit + dir * BEHIND);
            let room = fr as f64 * BEHIND;
            let mut t = 8.0;
            let mut ok = false;
            while t < room {
                if self.playable(exit + dir * t) {
                    ok = true;
                    break;
                }
                t += 16.0;
            }
            if !ok {
                return Err(Drop::Behind);
            }
        } else {
            let d = next.own_abort.map_or(dv(&next.src), |a| dv(&a));
            if !self.in_play(d) {
                return Err(Drop::Void);
            }
            let span = hp.distance(d).min(solid);
            let mut t = COLUMN_STEP / 2.0;
            while t < span {
                let p = hp + dir * t;
                if self.solid_at(model, p) && self.ground_or_ceiling(model, p) {
                    return Err(Drop::Column);
                }
                t += COLUMN_STEP;
            }
            death = Some(d);
        }
        let w = self.opts.weapon;
        let src = dv(&next.src);
        let to_target = ((exit - src).dot(dir)).max(0.0);
        let damage = if made {
            (next.dmg_in as f64 * (w.rangemod as f64).powf(to_target / 500.0)).max(0.0) as u16
        } else {
            0
        };
        Ok(Crossing {
            model,
            entry: hp,
            entry_face: u32::try_from(it.face).ok(),
            exit,
            made,
            death,
            damage,
            material: it.tex,
            thickness: normal_thick as f32,
            path: path as f32,
        })
    }

    fn shoot(&self, si: u32, spot: &Spot, pi: usize, yi: usize, pitch: f64, yaw: f64, c: &mut Counts) -> Option<Shot> {
        let (p, y) = (pitch.to_radians(), yaw.to_radians());
        let dir = DVec3::new(p.cos() * y.cos(), p.cos() * y.sin(), -p.sin());
        let d32 = v3(dir);
        let dir = dv(&d32);
        let eye = v3(spot.eye());
        c.fired += 1;
        let f = sim::fire(self.world, self.opts.weapon, &eye, &d32, true);
        let w = self.opts.weapon;
        if f.it[0].flags & IT_HIT == 0 {
            return None;
        }
        c.hits += 1;
        if f.it[0].fraction as f64 * w.distance as f64 > self.opts.max_hit {
            c.far += 1;
            return None;
        }
        let mut crossings = Vec::new();
        let walls = (w.penetration - 1) as usize;
        for k in 1..=walls {
            if f.it[k - 1].flags & IT_HIT == 0 || f.n <= k {
                break;
            }
            match self.crossing(&f, k, dir) {
                Ok(x) => {
                    let made = x.made;
                    crossings.push(x);
                    if !made {
                        break;
                    }
                }
                Err(e) => {
                    let slot = match e {
                        Drop::NoExit => &mut c.no_exit,
                        Drop::Behind => &mut c.behind,
                        Drop::Slab => &mut c.slab,
                        Drop::Column => &mut c.column,
                        Drop::Void => &mut c.void,
                        Drop::Corner => &mut c.corner,
                        Drop::Thin => &mut c.thin,
                    };
                    *slot += 1;
                    if k > 1 {
                        c.cut += 1;
                    }
                    break;
                }
            }
        }
        if crossings.is_empty() {
            return None;
        }
        let shot = Shot { shooter: si, stance: spot.stance, pitch_idx: pi as u8, yaw_idx: yi as u16, dir, crossings };
        if shot.made() {
            c.made += 1;
        } else {
            c.died += 1;
        }
        Some(shot)
    }
}

pub fn reachable(bsp: &crate::bsp::Bsp, nav: &Nav) -> Vec<bool> {
    let seeds: Vec<usize> = crate::reach::seeds(bsp).into_iter().filter_map(|p| nav.nearest(p, STAND_OFS)).collect();
    nav.flood(&seeds, 250.0).iter().map(|d| d.is_finite()).collect()
}

pub fn spots(nav: &Nav, reach: &[bool], o: &ShotOpts) -> Vec<Spot> {
    let mut out = Vec::new();
    for stance in [STAND, DUCK] {
        if (stance == STAND && !o.stand) || (stance == DUCK && !o.duck) {
            continue;
        }
        let mut cells: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
        for n in 0..nav.len() {
            if !reach[n] || (stance == STAND && !nav.nodes[n].stand) {
                continue;
            }
            let (x, y) = nav.cell_xy(nav.col[n] as usize);
            cells.entry(((x / o.spot_cell).floor() as i64, (y / o.spot_cell).floor() as i64)).or_default().push(n);
        }
        for ((i, j), mut ns) in cells {
            let cx = (i as f64 + 0.5) * o.spot_cell;
            let cy = (j as f64 + 0.5) * o.spot_cell;
            ns.sort_by(|&a, &b| nav.nodes[a].z.total_cmp(&nav.nodes[b].z).then(a.cmp(&b)));
            let mut k = 0;
            while k < ns.len() {
                let z0 = nav.nodes[ns[k]].z;
                let mut e = k;
                while e < ns.len() && nav.nodes[ns[e]].z - z0 <= LEVEL_GAP {
                    e += 1;
                }
                let best = ns[k..e]
                    .iter()
                    .copied()
                    .min_by(|&a, &b| {
                        let d = |n: usize| {
                            let (x, y) = nav.cell_xy(nav.col[n] as usize);
                            (x - cx).hypot(y - cy)
                        };
                        d(a).total_cmp(&d(b)).then(a.cmp(&b))
                    })
                    .unwrap();
                let (x, y) = nav.cell_xy(nav.col[best] as usize);
                out.push(Spot { feet: DVec3::new(x, y, nav.nodes[best].z as f64), stance });
                k = e;
            }
        }
    }
    out
}

pub fn simulate(ctx: &Ctx, spots: &[Spot], progress: &(dyn Fn(f32) + Sync)) -> (Vec<Shot>, Counts) {
    let yaws = ctx.opts.yaws();
    let pitches = ctx.opts.pitches();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let per: Vec<(Vec<Shot>, Counts)> = spots
        .par_iter()
        .enumerate()
        .map(|(si, s)| {
            let mut c = Counts::default();
            let mut v = Vec::new();
            for (pi, &p) in pitches.iter().enumerate() {
                for (yi, &y) in yaws.iter().enumerate() {
                    if let Some(sh) = ctx.shoot(si as u32, s, pi, yi, p, y, &mut c) {
                        v.push(sh);
                    }
                }
            }
            let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if d % 64 == 0 {
                progress(d as f32 / spots.len().max(1) as f32);
            }
            (v, c)
        })
        .collect();
    let mut counts = Counts::default();
    let mut shots = Vec::new();
    for (v, c) in per {
        counts.add(&c);
        shots.extend(v);
    }
    (shots, counts)
}
