pub mod hull0;
pub mod sim;
pub mod texture;

use std::collections::BTreeMap;

use crate::bsp::Bsp;
use hull0::{Hull0, Trace, V3};
use texture::{Materials, Textures};

#[derive(Clone, Copy, Debug, Default)]
pub struct EntOpts {
    pub skip_translucent: Option<u8>,
    pub skip_doors: bool,
}

#[derive(Clone, Debug)]
pub struct Placed {
    pub model: usize,
    pub head: i32,
    pub origin: V3,
    pub mins: V3,
    pub maxs: V3,
}

pub struct World {
    pub hull: Hull0,
    pub tex: Textures,
    pub mats: Materials,
    pub world_head: i32,
    pub ents: Vec<Placed>,
    pub rotated: BTreeMap<String, usize>,
}

fn flag(e: &crate::bsp::Entity, k: &str) -> i64 {
    e.get(k).and_then(|v| v.trim().parse::<f64>().ok()).map_or(0, |v| v as i64)
}

fn solid_class(e: &crate::bsp::Entity) -> Option<bool> {
    let sf = flag(e, "spawnflags");
    let (solid, keeps_angles) = match e.class().to_ascii_lowercase().as_str() {
        "func_wall" | "momentary_door" | "func_button" | "func_plat" => (true, false),
        "func_wall_toggle" => (sf & 1 == 0 && sf & 8 == 0, false),
        "func_conveyor" => (sf & 2 == 0, false),
        "func_door" | "func_water" => (flag(e, "skin") == 0 && sf & 8 == 0, false),
        "func_door_rotating" | "func_pendulum" | "func_train" | "func_tracktrain" | "func_vehicle" => (sf & 8 == 0, true),
        "func_rot_button" => (sf & 1 == 0, true),
        "momentary_rot_button" => (sf & 1 != 0, true),
        "func_rotating" => (sf & 64 == 0, true),
        "func_platrot" | "func_guntarget" | "func_tank" | "func_tanklaser" | "func_tankrocket" | "func_tankmortar"
        | "func_pushable" => (true, true),
        _ => (false, false),
    };
    solid.then_some(keeps_angles)
}

fn parse_v3(s: Option<&str>) -> V3 {
    let mut out = [0.0f32; 3];
    if let Some(s) = s {
        for (o, t) in out.iter_mut().zip(s.split_whitespace()) {
            *o = t.parse().unwrap_or(0.0);
        }
    }
    out
}

impl World {
    pub fn new(bsp: &Bsp, mats: Materials, eo: Option<EntOpts>) -> World {
        let mut w = World {
            hull: Hull0::new(bsp),
            tex: Textures::new(bsp),
            mats,
            world_head: bsp.models[0].headnode[0],
            ents: Vec::new(),
            rotated: BTreeMap::new(),
        };
        let Some(eo) = eo else { return w };
        for e in &bsp.entities {
            let Some(m) = e.model() else { continue };
            if m == 0 || m >= bsp.models.len() {
                continue;
            }
            let Some(keeps) = solid_class(e) else { continue };
            let class = e.class().to_ascii_lowercase();
            if eo.skip_doors && matches!(class.as_str(), "func_door" | "func_door_rotating" | "momentary_door") {
                continue;
            }
            if let Some(a) = eo.skip_translucent
                && flag(e, "rendermode") != 0
                && flag(e, "renderamt") < a as i64
            {
                continue;
            }
            if keeps && parse_v3(e.get("angles")).iter().any(|&v| v != 0.0) {
                *w.rotated.entry(class).or_default() += 1;
                continue;
            }
            let head = bsp.models[m].headnode[0];
            if head < 0 || head as usize >= bsp.nodes.len() {
                continue;
            }
            let origin = parse_v3(e.get("origin"));
            let (lo, hi) = (bsp.models[m].mins, bsp.models[m].maxs);
            w.ents.push(Placed {
                model: m,
                head,
                origin,
                mins: [lo.x as f32 + origin[0], lo.y as f32 + origin[1], lo.z as f32 + origin[2]],
                maxs: [hi.x as f32 + origin[0], hi.y as f32 + origin[1], hi.z as f32 + origin[2]],
            });
        }
        w.ents.sort_by_key(|p| p.model);
        w
    }

    pub fn contents(&self, p: &V3) -> i32 {
        self.hull.point_contents(self.world_head, p)
    }

    pub fn solid(&self, p: &V3, ents: bool) -> bool {
        if self.contents(p) == hull0::CONTENTS_SOLID {
            return true;
        }
        ents && self.ents.iter().any(|e| {
            (0..3).all(|i| p[i] >= e.mins[i] - 1.0 && p[i] <= e.maxs[i] + 1.0)
                && self.hull.point_contents(e.head, &sub(p, &e.origin)) == hull0::CONTENTS_SOLID
        })
    }

    pub fn trace(&self, src: &V3, end: &V3, ents: bool) -> (Trace, usize) {
        let mut best = self.hull.trace(self.world_head, src, end);
        let mut model = 0;
        if ents {
            for e in &self.ents {
                if (0..3).any(|i| src[i].min(end[i]) > e.maxs[i] + 1.0 || src[i].max(end[i]) < e.mins[i] - 1.0) {
                    continue;
                }
                let mut t = self.hull.trace(e.head, &sub(src, &e.origin), &sub(end, &e.origin));
                if t.fraction < best.fraction {
                    t.endpos = add(&t.endpos, &e.origin);
                    t.abortpos = add(&t.abortpos, &e.origin);
                    best = t;
                    model = e.model;
                }
            }
        }
        (best, model)
    }

    fn placed(&self, model: usize) -> Option<&Placed> {
        self.ents.iter().find(|e| e.model == model)
    }

    pub fn surface_at(&self, model: usize, src: &V3, end: &V3) -> Option<u32> {
        match self.placed(model).filter(|_| model != 0) {
            Some(e) => self.tex.surface_at(&self.hull.planes, e.head, &sub(src, &e.origin), &sub(end, &e.origin)),
            None => self.tex.surface_at(&self.hull.planes, self.world_head, src, end),
        }
    }

    pub fn texture_hit(&self, model: usize, src: &V3, end: &V3) -> (Option<u32>, u8) {
        match self.surface_at(model, src, end) {
            Some(s) => (Some(s), self.mats.material(self.tex.name(s))),
            None => (None, 0),
        }
    }
}

pub fn sub(a: &V3, b: &V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: &V3, b: &V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[cfg(test)]
mod tests {
    use super::sim::{self, Fired, WEAPONS};
    use super::*;
    use std::path::Path;

    const MAPS: [(&str, &str); 5] = [
        ("de_dust2", r"F:\servers\cs-coftest\cstrike\maps\de_dust2.bsp"),
        ("de_dust2_cz", r"F:\servers\cs-cz\czero\maps\de_dust2_cz.bsp"),
        ("de_inferno", r"F:\servers\cs-coftest\cstrike\maps\de_inferno.bsp"),
        ("de_inferno_cz", r"F:\servers\cs-cz\czero\maps\de_inferno_cz.bsp"),
        ("de_oc2", r"F:\servers\cs-coftest\cstrike\maps\de_oc2.bsp"),
    ];

    fn rd_f32(b: &[u8], o: usize) -> f32 {
        f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }

    fn rd_v3(b: &[u8], o: usize) -> V3 {
        [rd_f32(b, o), rd_f32(b, o + 4), rd_f32(b, o + 8)]
    }

    fn bits(v: &V3) -> [u32; 3] {
        v.map(f32::to_bits)
    }

    fn compare(f: &Fired, rec: &[u8], maxit: usize) -> Option<String> {
        let niter = rec[0] as usize;
        let outcome = rec[1];
        if f.n != niter {
            return Some(format!("niter {} vs {}", f.n, niter));
        }
        let o = match f.outcome() {
            sim::Outcome::Missed => 0,
            sim::Outcome::Made => 1,
            sim::Outcome::Died => 2,
            sim::Outcome::Stopped => 3,
        };
        if o != outcome {
            return Some(format!("outcome {o} vs {outcome}"));
        }
        for k in 0..niter.min(maxit) {
            let b = &rec[4 + k * 54..4 + (k + 1) * 54];
            let it = &f.it[k];
            let checks: [(&str, bool); 10] = [
                ("src", bits(&it.src) == bits(&rd_v3(b, 0))),
                ("dist", it.dist.to_bits() == rd_f32(b, 12).to_bits()),
                ("fraction", it.fraction.to_bits() == rd_f32(b, 16).to_bits()),
                ("endpos", bits(&it.endpos) == bits(&rd_v3(b, 20))),
                ("abortpos", bits(&it.abortpos) == bits(&rd_v3(b, 32))),
                ("face", it.face == i32::from_le_bytes(b[44..48].try_into().unwrap())),
                ("power", it.power == i16::from_le_bytes([b[48], b[49]])),
                ("damage", it.damage == i16::from_le_bytes([b[50], b[51]])),
                ("flags", it.flags == b[52]),
                ("tex", it.tex == b[53]),
            ];
            if let Some((name, _)) = checks.iter().find(|c| !c.1) {
                return Some(format!("iter {k} field {name}: {:?}", it));
            }
        }
        None
    }

    #[test]
    #[ignore]
    fn golden() {
        let Ok(dir) = std::env::var("WALLBANG_GOLDEN") else { return };
        let dir = Path::new(&dir);
        let only = std::env::var("WALLBANG_MAPS").ok();
        let mut total = 0usize;
        let mut bad = 0usize;
        for (name, path) in MAPS {
            if only.as_ref().is_some_and(|o| !o.split(',').any(|m| m == name)) {
                continue;
            }
            let path = Path::new(path);
            if !path.exists() {
                continue;
            }
            let bsp = Bsp::load(path).unwrap();
            let mats = Materials::locate(path).map(|p| Materials::load(&p).unwrap()).unwrap_or_else(Materials::empty);
            let world = World::new(&bsp, mats, None);
            for wname in ["ak47", "awp", "m4a1", "deagle", "scout"] {
                let Ok(shots) = std::fs::read(dir.join(format!("{name}_{wname}_shots.bin"))) else { continue };
                let res = std::fs::read(dir.join(format!("{name}_{wname}.bin"))).unwrap();
                assert_eq!(&shots[..4], b"WBS1");
                assert_eq!(&res[..4], b"WBR1");
                let count = u32::from_le_bytes(shots[4..8].try_into().unwrap()) as usize;
                let maxit = u32::from_le_bytes(res[8..12].try_into().unwrap()) as usize;
                let rsize = 4 + maxit * 54;
                let mut mism = 0usize;
                let mut first = None;
                for i in 0..count {
                    let s = &shots[8 + i * 25..8 + (i + 1) * 25];
                    let w = &WEAPONS[s[24] as usize];
                    let f = sim::fire(&world, w, &rd_v3(s, 0), &rd_v3(s, 12), false);
                    let rec = &res[16 + i * rsize..16 + (i + 1) * rsize];
                    if let Some(m) = compare(&f, rec, maxit) {
                        mism += 1;
                        if first.is_none() {
                            first = Some(format!("shot {i}: {m}"));
                        }
                    }
                }
                println!("{name} {wname}: {count} shots, {mism} mismatches {}", first.unwrap_or_default());
                total += count;
                bad += mism;
            }
        }
        println!("total {total} shots, {bad} mismatches");
        assert_eq!(bad, 0);
    }
}
