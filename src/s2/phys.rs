use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result};
use glam::{DAffine3, DVec3, Vec3};
use rayon::prelude::*;

use super::entities::Ent;
use super::kv3::Value;
use super::resource::Res;
use super::vpk::Fs;
use super::world::{Xf, flatten};
use crate::phys::{Builder, Class, Phys};

pub const SOLID_ENTS: &[&str] = &["func_door", "func_door_rotating", "prop_door_rotating", "func_brush", "func_breakable", "prop_dynamic"];

pub struct Src {
    pub map: String,
    pub ents: Vec<(String, Xf)>,
}

fn solid_ent(kv: &HashMap<String, String>) -> bool {
    let cls = kv.get("classname").map_or("", |c| c.as_str());
    let key = |k: &str| kv.get(k).map_or("", |v| v.as_str());
    let off = key("startdisabled") == "1" || key("startdisabled").eq_ignore_ascii_case("true");
    match cls {
        "func_brush" => key("solidity") == "2" || (key("solidity") != "1" && !off),
        "prop_dynamic" => key("solid") != "0" && !off,
        c => !off && (SOLID_ENTS.contains(&c) || c.starts_with("func_door")),
    }
}

impl Src {
    pub fn new(map: &str, ents: &[Ent]) -> Src {
        let ents = ents.iter().filter(|e| solid_ent(&e.kv)).filter_map(|e| Some((e.vmdl.clone()?, e.xf))).collect();
        Src { map: map.to_string(), ents }
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.arr().iter().map(|s| s.s().to_lowercase()).collect()
}

pub fn class_of(a: &Value) -> Option<Class> {
    let g = a.get("m_CollisionGroupString").s().to_lowercase();
    let as_ = strs(a.get("m_InteractAsStrings"));
    let ex = strs(a.get("m_InteractExcludeStrings"));
    let has = |l: &[String], k: &str| l.iter().any(|s| s == k);
    match g.as_str() {
        "default" | "" if has(&ex, "player") => Some(Class::Decor),
        "default" | "" => Some(Class::Solid),
        "conditionallysolid" if has(&as_, "playerclip") => Some(Class::Clip),
        "conditionallysolid" if has(&as_, "passbullets") || has(&as_, "window") => Some(Class::Solid),
        _ => None,
    }
}

fn vecs(b: &[u8]) -> Vec<Vec3> {
    b.chunks_exact(12)
        .map(|c| {
            let f = |k: usize| f32::from_le_bytes(c[k * 4..k * 4 + 4].try_into().unwrap());
            Vec3::new(f(0), f(1), f(2))
        })
        .collect()
}

fn planes(b: &[u8]) -> Vec<[f32; 4]> {
    b.chunks_exact(16).map(|c| std::array::from_fn(|k| f32::from_le_bytes(c[k * 4..k * 4 + 4].try_into().unwrap()))).collect()
}

fn indices(b: &[u8]) -> Vec<[u32; 3]> {
    b.chunks_exact(12).map(|c| std::array::from_fn(|k| i32::from_le_bytes(c[k * 4..k * 4 + 4].try_into().unwrap()) as u32)).collect()
}

fn dv(v: &Value, d: f64) -> DVec3 {
    let f = v.fv();
    if f.len() >= 3 { DVec3::new(f[0], f[1], f[2]) } else { DVec3::splat(d) }
}

pub fn affine(xf: &Xf) -> DAffine3 {
    DAffine3::from_mat3_translation(xf.r, xf.t)
}

pub fn add(b: &mut Builder, phys: &Value, xf: &DAffine3) {
    let classes: Vec<Option<Class>> = phys.get("m_collisionAttributes").arr().iter().map(class_of).collect();
    b.stats.attrs += classes.len();
    let poses = phys.get("m_bindPose").arr();
    for (pi, part) in phys.get("m_parts").arr().iter().enumerate() {
        b.stats.parts += 1;
        let pose = poses.get(pi).map(|p| affine(&Xf::from34(&flatten(p)))).unwrap_or(DAffine3::IDENTITY);
        let xf = *xf * pose;
        let class = |v: &Value| {
            let i = v.get("m_nCollisionAttributeIndex").i();
            classes.get(i as usize).copied().flatten()
        };
        let s = part.get("m_rnShape");
        for h in s.get("m_hulls").arr() {
            let hv = h.get("m_Hull");
            match class(h) {
                Some(c) => b.hull(&planes(hv.get("m_Planes").blob()), &vecs(hv.get("m_VertexPositions").blob()), &xf, c),
                None => b.stats.skipped_hulls += 1,
            }
        }
        for m in s.get("m_meshes").arr() {
            let mv = m.get("m_Mesh");
            let tris = indices(mv.get("m_Triangles").blob());
            match class(m) {
                Some(c) => b.mesh(&vecs(mv.get("m_Vertices").blob()), &tris, &xf, c),
                None => b.stats.skipped_tris += tris.len(),
            }
        }
        for sp in s.get("m_spheres").arr() {
            b.stats.spheres += 1;
            let Some(c) = class(sp) else { continue };
            let v = sp.get("m_Sphere");
            let (o, r) = (dv(v.get("m_vCenter"), 0.0), v.get("m_flRadius").f());
            b.aabb(o - DVec3::splat(r), o + DVec3::splat(r), &xf, c);
        }
        for cp in s.get("m_capsules").arr() {
            b.stats.capsules += 1;
            let Some(c) = class(cp) else { continue };
            let v = cp.get("m_Capsule");
            let r = DVec3::splat(v.get("m_flRadius").f());
            let ends: Vec<DVec3> = v.get("m_vCenter").arr().iter().map(|e| dv(e, 0.0)).collect();
            let (Some(lo), Some(hi)) = (ends.iter().copied().reduce(DVec3::min), ends.iter().copied().reduce(DVec3::max)) else { continue };
            b.aabb(lo - r, hi + r, &xf, c);
        }
    }
}

pub fn model_phys(fs: &Fs, vmdl: &str) -> Option<Value> {
    let res = Res::new(fs.read(&format!("{vmdl}_c")).ok()?).ok()?;
    res.kv("PHYS").ok()
}

pub fn load(fs: &Fs, src: &Src) -> Result<Phys> {
    let t = Instant::now();
    let mut b = Builder::default();
    let name = format!("maps/{}/world_physics.vmdl_c", src.map);
    let world = Res::new(fs.read(&name)?)?.kv("PHYS").with_context(|| name.clone())?;
    add(&mut b, &world, &DAffine3::IDENTITY);
    drop(world);
    let mut names: Vec<&str> = src.ents.iter().map(|(m, _)| m.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let models: HashMap<&str, Value> = names.par_iter().filter_map(|&m| Some((m, model_phys(fs, m)?))).collect();
    for (m, xf) in &src.ents {
        if let Some(p) = models.get(m.as_str()) {
            add(&mut b, p, &affine(xf));
            b.stats.ents += 1;
        }
    }
    b.stats.secs = t.elapsed().as_secs_f64();
    Ok(b.finish())
}
