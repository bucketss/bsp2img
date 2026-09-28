use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Result, bail};
use glam::{DAffine3, DVec3, Vec3};
use rayon::prelude::*;

use super::bsp::{LUMP_BRUSHES, LUMP_BRUSHSIDES, LUMP_GAME_LUMP, LUMP_LEAFBRUSHES, LUMP_LEAFS, LUMP_NODES, Map, Plane, World};
use super::disp_geo::{self, NO_HULL};
use super::entities::rotation;
use super::sprp::StaticProps;
use crate::bsp::Entity;
use crate::phys::{Builder, Class, Phys};
use crate::s2::vpk::Fs;

pub const CONTENTS_SOLID: i32 = 0x1;
pub const CONTENTS_WINDOW: i32 = 0x2;
pub const CONTENTS_GRATE: i32 = 0x8;
pub const CONTENTS_MOVEABLE: i32 = 0x4000;
pub const CONTENTS_PLAYERCLIP: i32 = 0x10000;
pub const CONTENTS_MONSTER: i32 = 0x2000000;
pub const CONTENTS_LADDER: i32 = 0x20000000;
pub const MASK_VISIBLE: i32 = CONTENTS_SOLID | CONTENTS_WINDOW | CONTENTS_GRATE;
pub const MASK_PLAYERSOLID: i32 = MASK_VISIBLE | CONTENTS_MOVEABLE | CONTENTS_PLAYERCLIP | CONTENTS_MONSTER;
pub const SOLID_BBOX: u8 = 2;
pub const SOLID_OBB: u8 = 3;
pub const SOLID_VPHYSICS: u8 = 6;
const MDL_HEAD: usize = 152;
const EPS: f64 = 0.01;
pub const DISP_THICK: f64 = 16.0;

pub const SOLID_ENTS: &[&str] = &[
    "func_brush",
    "func_wall",
    "func_wall_toggle",
    "func_door",
    "func_door_rotating",
    "func_movelinear",
    "func_breakable",
    "func_breakable_surf",
    "func_physbox",
    "func_physbox_multiplayer",
    "func_rotating",
    "func_tracktrain",
    "func_train",
    "func_button",
    "func_rot_button",
    "func_pushable",
    "func_conveyor",
];
pub const PROP_ENTS: &[&str] = &["prop_dynamic", "prop_dynamic_override", "prop_door_rotating"];

pub fn class(contents: i32) -> Option<Class> {
    if contents & MASK_VISIBLE != 0 {
        Some(Class::Solid)
    } else if contents & MASK_PLAYERSOLID != 0 {
        Some(Class::Clip)
    } else {
        None
    }
}

struct Brush {
    side: usize,
    n: usize,
    contents: i32,
}

struct Side {
    plane: usize,
    bevel: bool,
}

pub struct Brushes {
    brushes: Vec<Brush>,
    sides: Vec<Side>,
    planes: Vec<Plane>,
    pub models: Vec<Vec<usize>>,
}

fn key<'a>(e: &'a Entity, k: &str) -> &'a str {
    e.get(k).map_or("", str::trim)
}

fn int(e: &Entity, k: &str) -> i64 {
    key(e, k).parse().unwrap_or(0)
}

pub fn solid_ent(e: &Entity) -> bool {
    let off = key(e, "startdisabled") == "1";
    match e.class() {
        "func_brush" => key(e, "solidity") == "2" || (key(e, "solidity") != "1" && !off),
        "func_wall_toggle" => int(e, "spawnflags") & 1 == 0,
        "func_door" | "func_door_rotating" => int(e, "spawnflags") & 8 == 0,
        c => SOLID_ENTS.contains(&c) && !off,
    }
}

impl Brushes {
    pub fn read(map: &Map, models: usize) -> Result<Brushes> {
        let brushes = map.read(LUMP_BRUSHES, 12, |r, o| Brush { side: r.i32(o).max(0) as usize, n: r.i32(o + 4).max(0) as usize, contents: r.i32(o + 8) })?;
        let sides = map.read(LUMP_BRUSHSIDES, 8, |r, o| Side { plane: r.u16(o) as usize, bevel: r.u8(o + 6) != 0 })?;
        let planes = map.planes()?;
        let nodes = map.read(LUMP_NODES, 32, |r, o| [r.i32(o + 4), r.i32(o + 8)])?;
        let len = map.lump(LUMP_LEAFS)?.len();
        let size = match map.lumps[LUMP_LEAFS].version {
            0 if len % 56 == 0 => 56,
            _ if len % 32 == 0 => 32,
            _ => 56,
        };
        let leaves = map.read(LUMP_LEAFS, size, |r, o| (r.u16(o + 24) as usize, r.u16(o + 26) as usize))?;
        let lb = map.read(LUMP_LEAFBRUSHES, 2, |r, o| r.u16(o) as usize)?;
        let heads: Vec<i32> = map.models()?.iter().map(|m| m.headnode).take(models).collect();
        let mut out = Vec::with_capacity(heads.len());
        for h in heads {
            let mut list = Vec::new();
            let mut stack = vec![h];
            let mut steps = 0;
            while let Some(n) = stack.pop() {
                steps += 1;
                if steps > nodes.len() * 2 + 2 {
                    bail!("bsp node tree loops");
                }
                if n < 0 {
                    let Some(&(a, c)) = leaves.get((-1 - n) as usize) else { continue };
                    list.extend(lb.iter().skip(a).take(c).copied().filter(|&b| b < brushes.len()));
                } else if let Some(ch) = nodes.get(n as usize) {
                    stack.extend(ch);
                }
            }
            list.sort_unstable();
            list.dedup();
            out.push(list);
        }
        Ok(Brushes { brushes, sides, planes, models: out })
    }

    pub fn contents(&self, b: usize) -> i32 {
        self.brushes[b].contents
    }

    pub fn planes(&self, b: usize) -> Vec<([f64; 4], bool)> {
        let br = &self.brushes[b];
        self.sides
            .iter()
            .skip(br.side)
            .take(br.n)
            .filter_map(|s| {
                let p = self.planes.get(s.plane)?;
                Some(([p.normal.x, p.normal.y, p.normal.z, p.dist], s.bevel))
            })
            .collect()
    }

    pub fn hull(&self, b: usize) -> Hull {
        let ps = self.planes(b);
        let cut: Vec<[f64; 4]> = ps.iter().filter(|p| !p.1).map(|p| p.0).collect();
        (ps.iter().map(|p| p.0.map(|v| v as f32)).collect(), vertices(&cut))
    }
}

pub fn vertices(planes: &[[f64; 4]]) -> Vec<Vec3> {
    let n = |p: &[f64; 4]| DVec3::new(p[0], p[1], p[2]);
    let mut out: Vec<Vec3> = Vec::new();
    for i in 0..planes.len() {
        for j in i + 1..planes.len() {
            let jk = n(&planes[j]);
            for k in j + 1..planes.len() {
                let (a, b, c) = (n(&planes[i]), jk, n(&planes[k]));
                let det = a.dot(b.cross(c));
                if det.abs() < 1e-9 {
                    continue;
                }
                let p = (b.cross(c) * planes[i][3] + c.cross(a) * planes[j][3] + a.cross(b) * planes[k][3]) / det;
                if planes.iter().all(|q| n(q).dot(p) <= q[3] + EPS) {
                    let v = p.as_vec3();
                    if !out.iter().any(|o| o.distance_squared(v) < 1e-4) {
                        out.push(v);
                    }
                }
            }
        }
    }
    out
}

pub fn prism(t: [DVec3; 3], off: DVec3) -> Option<Hull> {
    let v: Vec<DVec3> = t.iter().copied().chain(t.iter().map(|p| *p + off)).collect();
    let c = v.iter().copied().sum::<DVec3>() / 6.0;
    let n = (t[1] - t[0]).cross(t[2] - t[0]);
    if n.length_squared() < 1e-6 || off.length_squared() < 1e-6 {
        return None;
    }
    let mut planes = Vec::with_capacity(5);
    let mut add = |n: DVec3, p: DVec3| {
        let Some(mut n) = n.try_normalize() else { return };
        if n.dot(c - p) > 0.0 {
            n = -n;
        }
        planes.push([n.x as f32, n.y as f32, n.z as f32, n.dot(p) as f32]);
    };
    add(n, t[0]);
    add(n, t[0] + off);
    for k in 0..3 {
        let (a, b) = (t[k], t[(k + 1) % 3]);
        add((b - a).cross(off), a);
    }
    (planes.len() == 5).then(|| (planes, v.iter().map(|p| p.as_vec3()).collect()))
}

pub fn ladders(map: &Map) -> Result<Vec<(DVec3, DVec3)>> {
    let br = Brushes::read(map, 1)?;
    Ok(br.models[0]
        .iter()
        .filter(|&&b| br.contents(b) & CONTENTS_LADDER != 0)
        .filter_map(|&b| {
            let v = br.hull(b).1;
            let lo = v.iter().copied().reduce(Vec3::min)?;
            let hi = v.iter().copied().reduce(Vec3::max)?;
            Some((lo.as_dvec3(), hi.as_dvec3()))
        })
        .collect())
}

pub type Hull = (Vec<[f32; 4]>, Vec<Vec3>);

pub enum Shape {
    Hulls(Vec<Hull>),
    Box(DVec3, DVec3),
}

pub fn phy_hulls(fs: &Fs, mdl: &str) -> Option<Vec<Hull>> {
    let n = super::fs::norm(mdl);
    let b = fs.read(&format!("{}.phy", n.strip_suffix(".mdl")?)).ok()?;
    let cs = super::phy::parse(&b).ok()?;
    (!cs.is_empty()).then(|| cs.iter().map(|c| (c.planes(), c.pts.clone())).collect())
}

pub fn shape(fs: &Fs, mdl: &str, vphysics: bool) -> Option<Shape> {
    if vphysics && let Some(h) = phy_hulls(fs, mdl) {
        return Some(Shape::Hulls(h));
    }
    mdl_box(fs, mdl).map(|(lo, hi)| Shape::Box(lo, hi))
}

pub fn mdl_box(fs: &Fs, name: &str) -> Option<(DVec3, DVec3)> {
    let h = fs.read_limit(&super::fs::norm(name), MDL_HEAD).ok()?;
    if h.len() < MDL_HEAD || &h[..4] != b"IDST" {
        return None;
    }
    let v = |o: usize| DVec3::from(std::array::from_fn::<f64, 3, _>(|k| f32::from_le_bytes(h[o + k * 4..o + k * 4 + 4].try_into().unwrap()) as f64));
    let ok = |lo: DVec3, hi: DVec3| (hi - lo).min_element() > 0.0 && lo.is_finite() && hi.is_finite();
    [(v(104), v(116)), (v(128), v(140))].into_iter().find(|&(lo, hi)| ok(lo, hi))
}

pub fn load(fs: &Fs, map: &Map) -> Result<Phys> {
    let t = Instant::now();
    let w = World::read(map)?;
    let ents = super::entities::parse(&map.entities()?).list;
    let br = Brushes::read(map, w.models.len())?;
    let mut b = Builder::default();
    let mut jobs: Vec<(usize, DAffine3)> = br.models[0].iter().map(|&k| (k, DAffine3::IDENTITY)).collect();
    for e in ents.iter().skip(1).filter(|e| solid_ent(e)) {
        let Some(mi) = e.model().filter(|&m| m > 0 && m < br.models.len()) else { continue };
        let xf = DAffine3::from_mat3_translation(rotation(e), e.origin().unwrap_or(DVec3::ZERO));
        jobs.extend(br.models[mi].iter().map(|&k| (k, xf)));
        b.stats.ents += 1;
    }
    let hulls: Vec<Option<(Class, Hull)>> = jobs
        .par_iter()
        .map(|&(k, _)| class(br.contents(k)).map(|c| (c, br.hull(k))))
        .collect();
    for ((_, xf), h) in jobs.iter().zip(&hulls) {
        match h {
            Some((c, h)) => b.hull(&h.0, &h.1, xf, *c),
            None => b.stats.skipped_hulls += 1,
        }
    }
    for d in disp_geo::build(map, &w)? {
        let c = if d.flags & NO_HULL != 0 { Class::Decor } else { Class::Solid };
        let off = w.face_normal(d.face) * -DISP_THICK;
        for t in &d.tris {
            match prism(t.map(|i| d.pos[i as usize]), off) {
                Some(h) => b.hull(&h.0, &h.1, &DAffine3::IDENTITY, c),
                None => b.stats.skipped_tris += 1,
            }
        }
    }
    let mut placed: Vec<(String, bool, DAffine3)> = Vec::new();
    if let Some(sp) = StaticProps::from_bsp(map.bytes(), map.raw(LUMP_GAME_LUMP)).ok().flatten() {
        for p in sp.props.iter().filter(|p| matches!(p.solid, SOLID_BBOX | SOLID_OBB | SOLID_VPHYSICS)) {
            if let Some(n) = sp.name(p) {
                placed.push((n.to_string(), p.solid == SOLID_VPHYSICS, crate::s2::phys::affine(&p.xf())));
            }
        }
    }
    for e in ents.iter().filter(|e| PROP_ENTS.contains(&e.class())) {
        let solid = e.get("solid").map_or(SOLID_VPHYSICS as i64, |s| s.trim().parse().unwrap_or(0));
        let Some(m) = e.get("model").filter(|_| solid != 0 && key(e, "startdisabled") != "1") else { continue };
        let s = e.get("modelscale").and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| *s > 0.0).unwrap_or(1.0);
        placed.push((m.to_string(), solid == SOLID_VPHYSICS as i64, DAffine3::from_mat3_translation(rotation(e) * s, e.origin().unwrap_or(DVec3::ZERO))));
        b.stats.ents += 1;
    }
    let mut names: Vec<(&str, bool)> = placed.iter().map(|(n, v, _)| (n.as_str(), *v)).collect();
    names.sort_unstable();
    names.dedup();
    let shapes: HashMap<(&str, bool), Shape> = names.par_iter().filter_map(|&k| Some((k, shape(fs, k.0, k.1)?))).collect();
    for (n, v, xf) in &placed {
        match shapes.get(&(n.as_str(), *v)) {
            Some(Shape::Hulls(hs)) => {
                for h in hs {
                    b.hull(&h.0, &h.1, xf, Class::Solid);
                }
            }
            Some(Shape::Box(lo, hi)) => b.aabb(*lo, *hi, xf, Class::Solid),
            None => b.stats.skipped_hulls += 1,
        }
    }
    b.stats.secs = t.elapsed().as_secs_f64();
    Ok(b.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_vertices() {
        let p = [[1.0, 0.0, 0.0, 10.0], [-1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 20.0], [0.0, -1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 30.0], [0.0, 0.0, -1.0, 0.0]];
        let v = vertices(&p);
        assert_eq!(v.len(), 8);
        assert!(v.contains(&Vec3::new(10.0, 20.0, 30.0)));
        assert_eq!(class(CONTENTS_PLAYERCLIP), Some(Class::Clip));
        assert_eq!(class(CONTENTS_SOLID | CONTENTS_PLAYERCLIP), Some(Class::Solid));
        assert_eq!(class(0x20), None);
    }

    #[test]
    #[ignore]
    fn real_props() {
        use std::sync::Arc;
        let root = std::env::var("PHY_MAP").unwrap_or_else(|_| r"E:\steam\steamapps\common\Counter-Strike Global Offensive\csgo\maps\de_dust2.bsp".into());
        let map = Arc::new(Map::open(std::path::Path::new(&root)).unwrap());
        let fs = super::super::fs::open(&map, None).unwrap().fs;
        let sp = StaticProps::from_bsp(map.bytes(), map.raw(LUMP_GAME_LUMP)).unwrap().unwrap();
        let mut names: Vec<&str> = sp.props.iter().filter(|p| p.solid == SOLID_VPHYSICS).filter_map(|p| sp.name(p)).collect();
        names.sort_unstable();
        names.dedup();
        let (mut phy, mut boxed, mut none, mut worst) = (0, 0, 0, (0.0f32, String::new()));
        for n in &names {
            match (phy_hulls(&fs, n), mdl_box(&fs, n)) {
                (Some(h), Some((lo, hi))) => {
                    phy += 1;
                    let pts: Vec<Vec3> = h.iter().flat_map(|c| c.1.iter().copied()).collect();
                    let a = pts.iter().copied().reduce(Vec3::min).unwrap();
                    let b = pts.iter().copied().reduce(Vec3::max).unwrap();
                    let bad = (a - lo.as_vec3()).min(Vec3::ZERO).abs().max_element().max((b - hi.as_vec3()).max(Vec3::ZERO).max_element());
                    if bad > worst.0 {
                        worst = (bad, format!("{n}: phy {a} {b} mdl {lo} {hi}, {} hulls", h.len()));
                    }
                    let viol = h.iter().flat_map(|c| c.1.iter().flat_map(|p| c.0.iter().map(|q| Vec3::new(q[0], q[1], q[2]).dot(*p) - q[3]))).fold(0f32, f32::max);
                    assert!(viol < 1.0, "{n}: a hull point is {viol} units outside its planes");
                }
                (None, Some(_)) => boxed += 1,
                _ => none += 1,
            }
        }
        println!("{} vphysics models: {phy} with .phy, {boxed} box only, {none} nothing; worst overshoot {:?}", names.len(), worst);
    }
}
