use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use glam::{DMat3, DVec3};

use super::kv3::Value;
use super::resource::Res;
use super::vpk::Fs;

pub const DECAL: u32 = 0x4;
pub const BLOCK_LIGHT: u32 = 0x10;
pub const OVERLAY: u32 = 0x2000;

#[derive(Clone, Copy, Debug)]
pub struct Xf {
    pub r: DMat3,
    pub t: DVec3,
}

impl Xf {
    pub const IDENTITY: Xf = Xf { r: DMat3::IDENTITY, t: DVec3::ZERO };

    pub fn from34(v: &[f64]) -> Xf {
        if v.len() < 12 {
            return Xf::IDENTITY;
        }
        Xf { r: DMat3::from_cols_array(&[v[0], v[4], v[8], v[1], v[5], v[9], v[2], v[6], v[10]]), t: DVec3::new(v[3], v[7], v[11]) }
    }

    pub fn apply(&self, p: DVec3) -> DVec3 {
        self.r * p + self.t
    }
}

pub struct Draw {
    pub model: u32,
    pub dc: Option<u32>,
    pub xf: Xf,
    pub tint: [f32; 3],
    pub flags: u32,
    pub skin: String,
    pub probe: i64,
}

pub struct Lighting {
    pub uv_scale: [f32; 2],
    pub maps: Vec<String>,
    pub shadows: Vec<(u32, u32)>,
    pub baked: bool,
}

pub struct World {
    pub models: Vec<String>,
    pub draws: Vec<Draw>,
    pub lighting: Lighting,
    pub lumps: Vec<String>,
    pub bounds: (DVec3, DVec3),
    pub stats: BTreeMap<&'static str, usize>,
}

pub fn flatten(v: &Value) -> Vec<f64> {
    let mut out = Vec::new();
    for r in v.arr() {
        match r {
            Value::Array(a) => out.extend(a.iter().map(|x| x.f())),
            x => out.push(x.f()),
        }
    }
    out
}

pub fn flag_bits(v: &Value) -> u32 {
    match v {
        Value::Str(s) => s
            .split('|')
            .map(|p| match p.trim() {
                "OBJECT_TYPE_DECAL" => DECAL,
                "OBJECT_TYPE_MODEL" => 0x8,
                "OBJECT_TYPE_BLOCK_LIGHT" => BLOCK_LIGHT,
                "OBJECT_TYPE_NO_SHADOWS" => 0x20,
                "OBJECT_TYPE_RENDER_TO_CUBEMAPS" => 0x400,
                "OBJECT_TYPE_OVERLAY" => OVERLAY,
                _ => 0,
            })
            .fold(0, |a, b| a | b),
        v => v.i() as u32,
    }
}

fn vec3(v: &Value) -> Option<DVec3> {
    let f = v.fv();
    (f.len() >= 3).then(|| DVec3::new(f[0], f[1], f[2]))
}

pub fn load(fs: &Fs, map: &str) -> Result<World> {
    let w = Res::new(fs.read(&format!("maps/{map}/world.vwrld_c"))?)?.kv("DATA").context("world.vwrld")?;
    let li = w.get("m_worldLightingInfo");
    let s = li.get("m_vLightmapUvScale").fv();
    let lighting = Lighting {
        uv_scale: if s.len() >= 2 { [s[0] as f32, s[1] as f32] } else { [1.0, 1.0] },
        maps: li.get("m_lightMaps").arr().iter().map(|v| v.s().to_string()).collect(),
        shadows: li
            .get("m_bakedShadows")
            .arr()
            .iter()
            .map(|b| (b.get("m_nLightHash").i() as u32, b.get("m_nShadowChannel").i() as u32))
            .collect(),
        baked: li.get("m_bHasLightmaps").b(),
    };
    let mut world = World {
        models: Vec::new(),
        draws: Vec::new(),
        lighting,
        lumps: w.get("m_entityLumps").arr().iter().map(|v| v.s().to_string()).collect(),
        bounds: (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
        stats: BTreeMap::new(),
    };
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut id = |models: &mut Vec<String>, m: &str| -> u32 {
        let k = m.to_lowercase();
        *ids.entry(k.clone()).or_insert_with(|| {
            models.push(k);
            models.len() as u32 - 1
        })
    };
    for node in w.get("m_worldNodes").arr() {
        if let (Some(lo), Some(hi)) = (vec3(node.get("m_vMinBounds")), vec3(node.get("m_vMaxBounds"))) {
            world.bounds = (world.bounds.0.min(lo), world.bounds.1.max(hi));
        }
        let prefix = node.get("m_worldNodePrefix").s().replace('\\', "/");
        let name = format!("{prefix}.vwnod_c");
        let nd = Res::new(fs.read(&name)?)?.kv("DATA").with_context(|| name.clone())?;
        for so in nd.get("m_sceneObjects").arr().iter().chain(nd.get("m_clutterSceneObjects").arr()) {
            let m = so.get("m_renderableModel").s();
            if m.is_empty() {
                continue;
            }
            let flags = flag_bits(so.get("m_nObjectTypeFlags"));
            if flags & BLOCK_LIGHT != 0 {
                *world.stats.entry("blocklight objects").or_default() += 1;
            }
            let t = so.get("m_vTintColor").fv();
            world.draws.push(Draw {
                model: id(&mut world.models, m),
                dc: None,
                xf: Xf::from34(&flatten(so.get("m_vTransform"))),
                tint: if t.len() >= 3 { [t[0] as f32, t[1] as f32, t[2] as f32] } else { [1.0; 3] },
                flags,
                skin: so.get("m_skin").s().to_string(),
                probe: so.get("m_nLightProbeVolumePrecomputedHandshake").i(),
            });
            *world.stats.entry("scene objects").or_default() += 1;
        }
        for ag in nd.get("m_aggregateSceneObjects").arr() {
            let m = ag.get("m_renderableModel").s();
            if m.is_empty() {
                continue;
            }
            let mi = id(&mut world.models, m);
            let frags = ag.get("m_aggregateMeshes").arr();
            let mut combined: HashMap<i64, i64> = HashMap::new();
            for fr in frags {
                *combined.entry(fr.get("m_nLODSetupIndex").i()).or_default() |= fr.get("m_nLODGroupMask").i();
            }
            let xfs = ag.get("m_fragmentTransforms").arr();
            let mut ti = 0;
            for fr in frags {
                let mut xf = Xf::IDENTITY;
                if fr.get("m_bHasTransform").b() {
                    xf = xfs.get(ti).map(|v| Xf::from34(&flatten(v))).unwrap_or(Xf::IDENTITY);
                    ti += 1;
                }
                let flags = flag_bits(fr.get("m_objectFlags"));
                if flags & BLOCK_LIGHT != 0 {
                    *world.stats.entry("blocklight fragments").or_default() += 1;
                }
                let mask = fr.get("m_nLODGroupMask").i();
                let c = combined.get(&fr.get("m_nLODSetupIndex").i()).copied().unwrap_or(0);
                if mask != 0 && mask & (c & c.wrapping_neg()) == 0 {
                    *world.stats.entry("fragments skipped by LOD").or_default() += 1;
                    continue;
                }
                let t = fr.get("m_vTintColor").fv();
                world.draws.push(Draw {
                    model: mi,
                    dc: Some(fr.get("m_nDrawCallIndex").i().max(0) as u32),
                    xf,
                    tint: if t.len() >= 3 { [t[0] as f32 / 255.0, t[1] as f32 / 255.0, t[2] as f32 / 255.0] } else { [1.0; 3] },
                    flags,
                    skin: String::new(),
                    probe: -1,
                });
                *world.stats.entry("aggregate fragments").or_default() += 1;
            }
        }
    }
    Ok(world)
}
