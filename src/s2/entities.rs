use std::collections::HashMap;

use anyhow::{Context, Result};
use glam::{DMat3, DVec3};

use super::kv3::Value;
use super::resource::Res;
use super::vpk::Fs;
use super::world::Xf;

pub const RENDER_CLASSES: &[&str] = &[
    "func_brush",
    "func_door",
    "func_door_rotating",
    "prop_door_rotating",
    "func_physbox",
    "func_physbox_multiplayer",
    "func_water",
    "func_breakable",
    "func_movelinear",
    "func_rotating",
    "func_button",
];

pub struct Ent {
    pub kv: HashMap<String, String>,
    pub vmdl: Option<String>,
    pub xf: Xf,
    pub render: bool,
    pub tint: [f32; 3],
    pub alpha: f32,
    pub actor: bool,
}

pub struct Lumps {
    pub ents: Vec<Ent>,
    pub legacy: usize,
    pub disabled: Vec<(String, DVec3)>,
}

fn text(v: &Value) -> Option<String> {
    let num = |f: f64| {
        let s = format!("{}", f as f32);
        if s == "-0" { "0".to_string() } else { s }
    };
    Some(match v {
        Value::Null | Value::Blob(_) | Value::Object(_) => return None,
        Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => num(*f),
        Value::Str(s) => s.clone(),
        Value::Array(a) => a.iter().map(|x| text(x).unwrap_or_default()).collect::<Vec<_>>().join(" "),
    })
}

pub fn angle_matrix(a: DVec3) -> DMat3 {
    let (sp, cp) = a.x.to_radians().sin_cos();
    let (sy, cy) = a.y.to_radians().sin_cos();
    let (sr, cr) = a.z.to_radians().sin_cos();
    DMat3::from_cols(
        DVec3::new(cp * cy, cp * sy, -sp),
        DVec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp),
        DVec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp),
    )
}

fn nums(v: &Value) -> Vec<f64> {
    match v {
        Value::Str(s) => s.split_whitespace().filter_map(|x| x.parse().ok()).collect(),
        Value::Array(_) => v.fv(),
        Value::Null => Vec::new(),
        v => vec![v.f()],
    }
}

fn vec3(v: &Value, d: f64) -> DVec3 {
    let f = nums(v);
    match f.len() {
        0 => DVec3::splat(d),
        1 => DVec3::splat(f[0]),
        _ => DVec3::new(f[0], f.get(1).copied().unwrap_or(d), f.get(2).copied().unwrap_or(d)),
    }
}

fn is_false(kv: &HashMap<String, String>, k: &str) -> bool {
    kv.get(k).is_some_and(|v| v == "0" || v.eq_ignore_ascii_case("false"))
}

fn is_true(kv: &HashMap<String, String>, k: &str) -> bool {
    kv.get(k).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn normalize(vals: &Value, disabled: &mut Vec<(String, DVec3)>) -> Option<Ent> {
    let mut kv: HashMap<String, String> = HashMap::new();
    for (k, v) in vals.obj() {
        if let Some(s) = text(v) {
            kv.insert(k.to_lowercase(), s);
        }
    }
    let cls = kv.get("classname").cloned().unwrap_or_default();
    let spawn = matches!(cls.as_str(), "info_player_terrorist" | "info_player_counterterrorist");
    if spawn && is_false(&kv, "enabled") {
        disabled.push((cls, vec3(vals.get("origin"), 0.0)));
        return None;
    }
    let renamed = match cls.as_str() {
        "info_player_terrorist" => "info_player_deathmatch",
        "info_player_counterterrorist" => "info_player_start",
        "info_player_start" => "info_player_start_cs2",
        _ => "",
    };
    if !renamed.is_empty() {
        kv.insert("classname".into(), renamed.into());
    }
    let origin = vec3(vals.get("origin"), 0.0);
    let angles = vec3(vals.get("angles"), 0.0);
    let scales = vec3(vals.get("scales"), 1.0);
    if cls == "light_environment" {
        kv.insert("pitch".into(), format!("{}", -angles.x));
        kv.insert("angles".into(), format!("0 {} 0", angles.y));
        if let Some(c) = kv.get("color").cloned() {
            let c: Vec<&str> = c.split_whitespace().take(3).collect();
            kv.insert("_light".into(), format!("{} 200", c.join(" ")));
        }
        if let Some(c) = kv.get("skycolor").cloned() {
            kv.insert("_diffuse_light".into(), c);
        }
    }
    let vmdl = kv.get("model").filter(|m| m.to_lowercase().ends_with(".vmdl")).map(|m| m.to_lowercase().replace('\\', "/"));
    let xf = Xf { r: angle_matrix(angles) * DMat3::from_diagonal(scales), t: origin };
    let render = vmdl.is_some()
        && (RENDER_CLASSES.contains(&cls.as_str()) || cls.starts_with("prop_dynamic") || cls.starts_with("prop_physics"))
        && !is_true(&kv, "startdisabled");
    let c = nums(vals.get("rendercolor"));
    let tint = if c.len() >= 3 { [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0] } else { [1.0; 3] };
    let alpha = nums(vals.get("renderamt")).first().map_or(1.0, |a| (*a as f32 / 255.0).clamp(0.0, 1.0));
    Some(Ent { kv, vmdl, xf, render, tint, alpha, actor: false })
}

fn read_lump(fs: &Fs, name: &str, out: &mut Lumps, depth: usize) -> Result<()> {
    let d = Res::new(fs.read(&format!("{name}_c"))?)?.kv("DATA").with_context(|| name.to_string())?;
    for e in d.get("m_entityKeyValues").arr() {
        let k3 = e.get("keyValues3Data");
        if k3.is_null() {
            if !e.get("m_keyValuesData").blob().is_empty() {
                out.legacy += 1;
            }
            continue;
        }
        out.ents.extend(normalize(k3.get("values"), &mut out.disabled));
    }
    if depth < 8 {
        for c in d.get("m_childLumps").arr() {
            read_lump(fs, &c.s().to_lowercase().replace('\\', "/"), out, depth + 1)?;
        }
    }
    Ok(())
}

pub fn load(fs: &Fs, lumps: &[String]) -> Result<Lumps> {
    let mut out = Lumps { ents: Vec::new(), legacy: 0, disabled: Vec::new() };
    for l in lumps {
        read_lump(fs, &l.to_lowercase().replace('\\', "/"), &mut out, 0)?;
    }
    if let Some(i) = out.ents.iter().position(|e| e.kv.get("classname").is_some_and(|c| c == "worldspawn")) {
        let w = out.ents.remove(i);
        out.ents.insert(0, w);
    } else {
        let kv = HashMap::from([("classname".to_string(), "worldspawn".to_string())]);
        out.ents.insert(0, Ent { kv, vmdl: None, xf: Xf::IDENTITY, render: false, tint: [1.0; 3], alpha: 1.0, actor: false });
    }
    Ok(out)
}

fn points(blob: &[u8]) -> impl Iterator<Item = DVec3> + '_ {
    blob.chunks_exact(12).map(|c| {
        let f = |k: usize| f32::from_le_bytes(c[k * 4..k * 4 + 4].try_into().unwrap()) as f64;
        DVec3::new(f(0), f(1), f(2))
    })
}

pub fn phys_points(phys: &Value) -> Vec<DVec3> {
    let mut out = Vec::new();
    for part in phys.get("m_parts").arr() {
        let s = part.get("m_rnShape");
        for h in s.get("m_hulls").arr() {
            out.extend(points(h.get("m_Hull").get("m_VertexPositions").blob()));
        }
        for m in s.get("m_meshes").arr() {
            out.extend(points(m.get("m_Mesh").get("m_Vertices").blob()));
        }
        for sp in s.get("m_spheres").arr() {
            let sp = sp.get("m_Sphere");
            let (c, r) = (vec3(sp.get("m_vCenter"), 0.0), sp.get("m_flRadius").f());
            out.extend([c - DVec3::splat(r), c + DVec3::splat(r)]);
        }
        for cp in s.get("m_capsules").arr() {
            let cp = cp.get("m_Capsule");
            let r = cp.get("m_flRadius").f();
            for c in cp.get("m_vCenter").arr() {
                let c = vec3(c, 0.0);
                out.extend([c - DVec3::splat(r), c + DVec3::splat(r)]);
            }
        }
    }
    out
}

pub fn model_bounds(fs: &Fs, vmdl: &str) -> Option<(DVec3, DVec3)> {
    let res = Res::new(fs.read(&format!("{vmdl}_c")).ok()?).ok()?;
    let pts = phys_points(&res.kv("PHYS").ok()?);
    let lo = pts.iter().copied().reduce(DVec3::min)?;
    let hi = pts.iter().copied().reduce(DVec3::max)?;
    Some((lo, hi))
}

pub fn world_bounds(xf: &Xf, lo: DVec3, hi: DVec3) -> (DVec3, DVec3) {
    let mut a = DVec3::splat(f64::INFINITY);
    let mut b = DVec3::splat(f64::NEG_INFINITY);
    for i in 0..8 {
        let p = DVec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        let q = xf.apply(p);
        a = a.min(q);
        b = b.max(q);
    }
    (a, b)
}
