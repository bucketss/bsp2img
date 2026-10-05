use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Result, bail};
use glam::{DVec3, Vec3};
use rayon::prelude::*;

use super::hull0::CONTENTS_SOLID;
use super::cs2::Cs2;
use super::sim::{self, IT_HIT, Iter, Weapon};
use super::{Gold, World, dv, v3};
use crate::nav::{Nav, STAND_OFS, Stance};

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
const EXIT_PAD: f64 = 0.1;
const EXIT_HOPS: usize = 8;
pub const MAX_WALLS: usize = 2;
pub const MAX_PITCHES: usize = 256;
pub const MAX_YAWS: usize = 65536;
pub const LETHAL_HEAD: u16 = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Died,
    Weak,
    Made,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Died, Kind::Weak, Kind::Made];

    fn of(made: bool, damage: u16, by_damage: bool) -> Kind {
        match (made, by_damage && damage < LETHAL_HEAD) {
            (false, _) => Kind::Died,
            (true, true) => Kind::Weak,
            (true, false) => Kind::Made,
        }
    }
}

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

pub fn pitch_list(a: f64, b: f64, step: f64, max: usize) -> Vec<f64> {
    (0..).map(|i| a + i as f64 * step).take_while(|&p| p <= b + 1e-9).take(max).collect()
}

pub fn yaw_count(step: f64) -> f64 {
    (360.0 / step).round().max(1.0)
}

impl ShotOpts {
    pub fn new(weapon: &'static Weapon, stand: bool, duck: bool) -> ShotOpts {
        ShotOpts { weapon, stand, duck, yaw_step: 2.0, pitch: (-20.0, 20.0, 4.0), spot_cell: 64.0, max_hit: 2000.0, min_thickness: 0.0, slab: 256.0 }
    }

    pub fn check(&self) -> Result<()> {
        let ny = yaw_count(self.yaw_step);
        if !ny.is_finite() || ny > MAX_YAWS as f64 {
            bail!("yaw step {} gives {ny} directions; at most {MAX_YAWS}", self.yaw_step);
        }
        let (a, b, s) = self.pitch;
        if pitch_list(a, b, s, MAX_PITCHES + 1).len() > MAX_PITCHES {
            bail!("pitch {a}..{b}/{s} gives more than {MAX_PITCHES} pitches");
        }
        Ok(())
    }

    pub fn yaws(&self) -> Vec<f64> {
        let n = yaw_count(self.yaw_step) as usize;
        (0..n).map(|k| k as f64 * self.yaw_step).collect()
    }

    pub fn pitches(&self) -> Vec<f64> {
        let (a, b, s) = self.pitch;
        pitch_list(a, b, s, MAX_PITCHES)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Spot {
    pub feet: DVec3,
    pub stance: Stance,
}

impl Spot {
    pub fn eye(&self, w: &World) -> DVec3 {
        self.feet + DVec3::Z * w.eye(self.stance)
    }

    pub fn origin(&self) -> DVec3 {
        self.feet + DVec3::Z * self.stance.feet()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Crossing {
    pub entry: Vec3,
    pub entry_face: i32,
    pub thickness: f32,
    pub path: f32,
    pub death_at: f32,
    pub model: u16,
    pub damage: u16,
    pub material: u8,
    pub made: bool,
}

impl Crossing {
    pub fn kind(&self, by_damage: bool) -> Kind {
        Kind::of(self.made, self.damage, by_damage)
    }

    pub fn entry(&self) -> DVec3 {
        self.entry.as_dvec3()
    }

    pub fn face(&self) -> Option<u32> {
        u32::try_from(self.entry_face).ok()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub dir: Vec3,
    pub shooter: u32,
    pub yaw_idx: u16,
    pub pitch_idx: u8,
    pub stance: Stance,
    pub(super) walls: u8,
    pub(super) cross: [Crossing; MAX_WALLS],
}

impl Shot {
    pub fn crossings(&self) -> &[Crossing] {
        &self.cross[..self.walls as usize]
    }

    pub fn last(&self) -> &Crossing {
        &self.cross[self.walls as usize - 1]
    }

    pub fn dir(&self) -> DVec3 {
        self.dir.as_dvec3()
    }

    pub fn exit(&self, c: &Crossing) -> DVec3 {
        c.entry() + self.dir() * (c.path as f64 + EXIT_PAD)
    }

    pub fn death(&self, c: &Crossing) -> Option<DVec3> {
        (!c.made).then(|| c.entry() + self.dir() * c.death_at as f64)
    }

    pub fn made(&self) -> bool {
        self.crossings().iter().all(|c| c.made)
    }

    pub fn start(&self) -> DVec3 {
        self.cross[0].entry() - self.dir() * 24.0
    }

    pub fn end(&self) -> DVec3 {
        let last = self.last();
        self.death(last).unwrap_or_else(|| self.exit(last) + self.dir() * 48.0)
    }

    pub fn path(&self) -> f32 {
        self.crossings().iter().map(|c| c.path).sum()
    }

    pub fn damage(&self) -> u16 {
        let c = self.last();
        if c.made { c.damage } else { 0 }
    }

    pub fn kind(&self, by_damage: bool) -> Kind {
        Kind::of(self.made(), self.damage(), by_damage)
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
    pub graze: u64,
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
        self.graze += o.graze;
        self.corner += o.corner;
        self.thin += o.thin;
        self.cut += o.cut;
        self.made += o.made;
        self.died += o.died;
    }

    fn slot(&mut self, d: Drop) -> &mut u64 {
        match d {
            Drop::NoExit => &mut self.no_exit,
            Drop::Behind => &mut self.behind,
            Drop::Slab => &mut self.slab,
            Drop::Column => &mut self.column,
            Drop::Void => &mut self.void,
            Drop::Graze => &mut self.graze,
            Drop::Corner => &mut self.corner,
            Drop::Thin => &mut self.thin,
        }
    }
}

pub struct Ctx<'a> {
    pub world: &'a World,
    pub nav: &'a Nav,
    pub reach: &'a [bool],
    pub opts: &'a ShotOpts,
    pub stop: Option<&'a AtomicBool>,
    cs2_weapon: Option<&'a crate::s2::surfprop::Weapon>,
}

#[derive(Clone, Copy)]
enum Drop {
    NoExit,
    Behind,
    Slab,
    Column,
    Void,
    Graze,
    Corner,
    Thin,
}

struct Geom {
    hp: DVec3,
    n: DVec3,
    q: DVec3,
    path: f64,
    exit: DVec3,
    normal_thick: f64,
}

fn model_ref(g: &Gold, model: u16) -> (i32, DVec3) {
    let (head, o) = g.model_ref(model as usize);
    (head, dv(&o))
}

impl<'a> Ctx<'a> {
    pub fn new(world: &'a World, nav: &'a Nav, reach: &'a [bool], opts: &'a ShotOpts, stop: Option<&'a AtomicBool>) -> Ctx<'a> {
        let cs2_weapon = match world {
            World::Cs2(w) => w.weapon(opts.weapon.name),
            World::Gold(_) => None,
        };
        Ctx { world, nav, reach, opts, stop, cs2_weapon }
    }
}

impl Ctx<'_> {
    fn open_dist(&self, model: u16, p: DVec3, d: DVec3, cap: f64) -> Option<f64> {
        match self.world {
            World::Gold(g) => {
                let (head, o) = model_ref(g, model);
                g.hull.first_open(head, p - o, p - o + d * cap).map(|f| f * cap)
            }
            World::Cs2(w) => w.open_dist(p, d, cap),
        }
    }

    fn solid_at(&self, model: u16, p: DVec3) -> bool {
        match self.world {
            World::Gold(g) => {
                let (head, o) = model_ref(g, model);
                g.hull.contents64(head, p - o) == CONTENTS_SOLID
            }
            World::Cs2(w) => w.solid_at(p, CAP),
        }
    }

    fn solid_other(&self, model: u16, p: DVec3) -> Option<u16> {
        let World::Gold(g) = self.world else { return None };
        if model != 0 && self.solid_at(0, p) {
            return Some(0);
        }
        let q = v3(p);
        g.ents
            .iter()
            .filter(|e| e.model != model as usize && (0..3).all(|i| q[i] >= e.mins[i] && q[i] <= e.maxs[i]))
            .map(|e| e.model as u16)
            .find(|&m| self.solid_at(m, p))
    }

    fn exit_dist(&self, model: u16, p: DVec3, d: DVec3) -> Option<f64> {
        let mut t = self.open_dist(model, p, d, CAP)?;
        for _ in 0..EXIT_HOPS {
            let q = p + d * (t + EXIT_PAD);
            let Some(m) = self.solid_other(model, q) else { return Some(t) };
            t += EXIT_PAD + self.open_dist(m, q, d, CAP)?;
        }
        None
    }

    fn trace_frac(&self, a: DVec3, b: DVec3) -> (f32, DVec3, bool) {
        match self.world {
            World::Gold(g) => {
                let (t, _) = g.trace(&v3(a), &v3(b), true);
                (t.fraction, dv(&t.endpos), t.startsolid)
            }
            World::Cs2(w) => w.trace(a, b),
        }
    }

    fn nodes_near(&self, p: DVec3, r: f64) -> impl Iterator<Item = usize> + '_ {
        self.nav
            .cols_in(p.x - r, p.y - r, p.x + r, p.y + r)
            .flat_map(|c| self.nav.column(c))
            .filter(|&n| self.reach[n])
    }

    fn node_origin_z(&self, n: usize) -> f64 {
        let nd = &self.nav.nodes[n];
        nd.z as f64 + if nd.stand { STAND_OFS } else { Stance::Duck.feet() }
    }

    fn playable(&self, p: DVec3) -> bool {
        let mut best = [(f64::INFINITY, usize::MAX); PLAY_TRACES];
        for n in self.nodes_near(p, PLAY_R) {
            let (x, y) = self.nav.cell_xy(self.nav.col[n] as usize);
            let d = (x - p.x).hypot(y - p.y);
            if d > PLAY_R || (self.node_origin_z(n) - p.z).abs() > PLAY_DZ {
                continue;
            }
            if let Some(i) = best.iter().position(|b| d.total_cmp(&b.0).then(n.cmp(&b.1)).is_lt()) {
                best.copy_within(i..PLAY_TRACES - 1, i + 1);
                best[i] = (d, n);
            }
        }
        best.iter().take_while(|b| b.1 != usize::MAX).any(|&(_, n)| {
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

    fn geometry(&self, it: &Iter, next: &Iter, made: bool, dir: DVec3) -> Result<Geom, Drop> {
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
            return Err(Drop::Graze);
        }
        let step = (0.25 / (-dn).max(1e-3)).min(64.0);
        let solid = self.exit_dist(it.model, hp + dir * step, dir).ok_or(Drop::NoExit)?;
        let path = step + solid;
        let exit = hp + dir * (path + EXIT_PAD);
        if made && (exit - hp).dot(dir) > (dv(&next.endpos) - hp).dot(dir) + 1.0 {
            return Err(Drop::NoExit);
        }
        let q = hp - n * 0.5;
        let normal_thick = self.open_dist(it.model, q, -n, CAP).map_or(CAP, |t| t + 0.5);
        Ok(Geom { hp, n, q, path, exit, normal_thick })
    }

    fn filters(&self, g: &Geom, model: u16, death: Option<DVec3>, dir: DVec3) -> Result<(), Drop> {
        if g.path < CORNER_PATH && g.normal_thick > CORNER_NORMAL {
            return Err(Drop::Corner);
        }
        if self.opts.min_thickness > 0.0 && g.normal_thick < self.opts.min_thickness {
            return Err(Drop::Thin);
        }
        if g.n.z.abs() > 0.3 {
            let v = self.open_dist(model, g.q, DVec3::new(0.0, 0.0, if g.n.z > 0.0 { -1.0 } else { 1.0 }), CAP);
            if v.is_none_or(|v| v > self.opts.slab) {
                return Err(Drop::Slab);
            }
        }
        let Some(d) = death else {
            return if self.playable_behind(g.exit, dir) { Ok(()) } else { Err(Drop::Behind) };
        };
        if !self.in_play(d) {
            return Err(Drop::Void);
        }
        let span = g.hp.distance(d).min(g.path);
        let mut t = COLUMN_STEP / 2.0;
        while t < span {
            let p = g.hp + dir * t;
            if self.solid_at(model, p) && self.ground_or_ceiling(model, p) {
                return Err(Drop::Column);
            }
            t += COLUMN_STEP;
        }
        Ok(())
    }

    fn crossing(&self, f: &sim::Fired, k: usize, dir: DVec3) -> Result<Crossing, Drop> {
        let (it, next) = (&f.it[k - 1], &f.it[k]);
        let made = !next.dead();
        let g = self.geometry(it, next, made, dir)?;
        let death = (!made).then(|| dv(&next.death()));
        self.filters(&g, it.model, death, dir)?;
        let w = self.opts.weapon;
        let to_target = ((g.exit - dv(&next.src)).dot(dir)).max(0.0);
        let damage = if made { (next.dmg_in as f64 * (w.rangemod as f64).powf(to_target / 500.0)).max(0.0) as u16 } else { 0 };
        Ok(Crossing {
            entry: g.hp.as_vec3(),
            entry_face: it.face,
            thickness: g.normal_thick as f32,
            path: g.path as f32,
            death_at: death.map_or(0.0, |d| (d - g.hp).dot(dir) as f32),
            model: it.model,
            damage,
            material: it.tex,
            made,
        })
    }

    fn playable_behind(&self, exit: DVec3, dir: DVec3) -> bool {
        let (fr, _, _) = self.trace_frac(exit, exit + dir * BEHIND);
        let room = fr as f64 * BEHIND;
        let mut t = 8.0;
        while t < room {
            if self.playable(exit + dir * t) {
                return true;
            }
            t += 16.0;
        }
        false
    }

    fn wall_geom(&self, hp: DVec3, n: DVec3, path: f64, dir: DVec3) -> Result<Geom, Drop> {
        let mut n = n;
        if n.length_squared() < 0.5 {
            n = -dir;
        }
        let mut dn = dir.dot(n);
        if dn > 0.0 {
            n = -n;
            dn = -dn;
        }
        if -dn < GRAZE {
            return Err(Drop::Graze);
        }
        let exit = hp + dir * (path + EXIT_PAD);
        let q = hp - n * 0.5;
        let normal_thick = self.open_dist(0, q, -n, CAP).map_or(CAP, |t| t + 0.5);
        Ok(Geom { hp, n, q, path, exit, normal_thick })
    }

    fn walls_cs2(&self, w: &Cs2, eye: DVec3, dir: DVec3, shot: &mut Shot, c: &mut Counts) -> Option<()> {
        let wp = self.cs2_weapon?;
        let start = eye.as_vec3();
        let ray = super::sim_s2::trace_ray(&w.bullet, start, start + dir.as_vec3() * wp.range)?;
        let (out, _) = super::sim_s2::handle(&w.bullet, &ray, wp.damage, wp.range_mod, wp.pen);
        let first = ray.segs.iter().find(|s| s.solid)?;
        let l = super::sim_s2::length(ray.delta) as f64;
        c.hits += 1;
        if first.t0 as f64 * l > self.opts.max_hit {
            c.far += 1;
            return None;
        }
        let (origin, delta) = (ray.start.as_dvec3(), ray.delta.as_dvec3());
        let mut k = 0;
        for (s, o) in ray.segs.iter().zip(&out).filter(|(s, _)| s.solid) {
            if k == MAX_WALLS {
                break;
            }
            k += 1;
            let made = o.damage >= 1.0 && s.t1 < 1.0;
            let x = &ray.xs[s.a];
            let hp = origin + delta * s.t0 as f64;
            let run = (s.t1 - s.t0).max(0.0) as f64 * l;
            let r = if s.t1 < 1.0 { Ok(run) } else { self.open_dist(0, hp + dir * EXIT_PAD, dir, CAP).map(|t| t + EXIT_PAD).ok_or(Drop::NoExit) };
            let r = r.and_then(|path| self.wall_geom(hp, x.normal.as_dvec3(), path, dir)).and_then(|g| {
                if !made && !self.playable_behind(g.exit, dir) {
                    return Err(Drop::Behind);
                }
                self.filters(&g, 0, (!made).then_some(hp), dir)?;
                Ok(g)
            });
            match r {
                Ok(g) => {
                    shot.cross[shot.walls as usize] = Crossing {
                        entry: hp.as_vec3(),
                        entry_face: -1,
                        thickness: g.normal_thick as f32,
                        path: g.path as f32,
                        death_at: 0.0,
                        model: 0,
                        damage: if made { o.damage as u16 } else { 0 },
                        material: w.bullet.surf(x.surf).mat,
                        made,
                    };
                    shot.walls += 1;
                    if !made {
                        break;
                    }
                }
                Err(e) => {
                    *c.slot(e) += 1;
                    if k > 1 {
                        c.cut += 1;
                    }
                    break;
                }
            }
        }
        Some(())
    }

    fn shoot(&self, si: u32, spot: &Spot, (pi, pitch): (usize, f64), (yi, yaw): (usize, f64), c: &mut Counts) -> Option<Shot> {
        let (p, y) = (pitch.to_radians(), yaw.to_radians());
        let d32 = v3(DVec3::new(p.cos() * y.cos(), p.cos() * y.sin(), -p.sin()));
        let dir = dv(&d32);
        let eye = spot.eye(self.world);
        c.fired += 1;
        let mut shot = Shot {
            dir: dir.as_vec3(),
            shooter: si,
            yaw_idx: yi as u16,
            pitch_idx: pi as u8,
            stance: spot.stance,
            walls: 0,
            cross: [Crossing::default(); MAX_WALLS],
        };
        match self.world {
            World::Gold(g) => self.walls_gold(g, v3(eye), &d32, dir, &mut shot, c)?,
            World::Cs2(w) => self.walls_cs2(w, eye, dir, &mut shot, c)?,
        }
        if shot.walls == 0 {
            return None;
        }
        if shot.made() {
            c.made += 1;
        } else {
            c.died += 1;
        }
        Some(shot)
    }

    fn walls_gold(&self, g: &Gold, eye: [f32; 3], d32: &[f32; 3], dir: DVec3, shot: &mut Shot, c: &mut Counts) -> Option<()> {
        let w = self.opts.weapon;
        let f = sim::fire(g, w, &eye, d32, true, false);
        if f.it[0].flags & IT_HIT == 0 {
            return None;
        }
        c.hits += 1;
        if f.it[0].fraction as f64 * w.distance as f64 > self.opts.max_hit {
            c.far += 1;
            return None;
        }
        let walls = ((w.penetration - 1).max(0) as usize).min(MAX_WALLS);
        for k in 1..=walls {
            if f.it[k - 1].flags & IT_HIT == 0 || f.n <= k {
                break;
            }
            match self.crossing(&f, k, dir) {
                Ok(x) => {
                    shot.cross[shot.walls as usize] = x;
                    shot.walls += 1;
                    if !x.made {
                        break;
                    }
                }
                Err(e) => {
                    *c.slot(e) += 1;
                    if k > 1 {
                        c.cut += 1;
                    }
                    break;
                }
            }
        }
        Some(())
    }
}

pub fn reachable(bsp: &crate::bsp::Bsp, nav: &Nav) -> Vec<bool> {
    let seeds: Vec<usize> = crate::reach::seeds(bsp).into_iter().filter_map(|p| nav.nearest(p, STAND_OFS)).collect();
    nav.flood(&seeds, 250.0).iter().map(|d| d.is_finite()).collect()
}

pub fn spots(nav: &Nav, reach: &[bool], o: &ShotOpts) -> Vec<Spot> {
    let mut out = Vec::new();
    for stance in [Stance::Stand, Stance::Duck] {
        if (stance == Stance::Stand && !o.stand) || (stance == Stance::Duck && !o.duck) {
            continue;
        }
        let mut cells: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
        for (n, _) in reach.iter().enumerate().take(nav.len()).filter(|r| *r.1) {
            if stance == Stance::Stand && !nav.nodes[n].stand {
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
    let done = AtomicUsize::new(0);
    let per: Vec<(Vec<Shot>, Counts)> = spots
        .par_iter()
        .enumerate()
        .map(|(si, s)| {
            let mut c = Counts::default();
            let mut v = Vec::new();
            if ctx.stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                return (v, c);
            }
            for (pi, &p) in pitches.iter().enumerate() {
                for (yi, &y) in yaws.iter().enumerate() {
                    if let Some(sh) = ctx.shoot(si as u32, s, (pi, p), (yi, y), &mut c) {
                        v.push(sh);
                    }
                }
            }
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if d.is_multiple_of(64) {
                progress(d as f32 / spots.len().max(1) as f32);
            }
            (v, c)
        })
        .collect();
    let mut counts = Counts::default();
    let mut shots = Vec::with_capacity(per.iter().map(|p| p.0.len()).sum());
    for (v, c) in per {
        counts.add(&c);
        shots.extend(v);
    }
    (shots, counts)
}
