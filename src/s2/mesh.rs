use std::collections::{BTreeSet, HashMap};

use glam::{DMat3, DVec3};
use rayon::prelude::*;

use super::entities::Ent;
use super::material::{self, Fill, Material};
use super::model::Model;
use super::texture;
use super::vpk::Fs;
use super::world::{OVERLAY, World, Xf};
use crate::mesh::{Batch, Image, Mesh, Mode, Vertex};

const SPLIT: usize = 4_000_000;
const POINT_CELL: f64 = 32.0;
const TINT_LEVELS: f32 = 255.0;

pub struct Lightmap {
    pub atlas: Image,
    pub rect: [f32; 4],
    pub flat: [f32; 2],
}

pub struct Opts<'a> {
    pub tex_max: usize,
    pub keep: Option<&'a (dyn Fn(DVec3, DVec3) -> bool + Sync)>,
    pub lightmap: Option<Lightmap>,
}

pub struct Built {
    pub mesh: Mesh,
    pub draws: usize,
    pub dropped: usize,
    pub models: usize,
    pub materials: usize,
    pub bad_models: Vec<String>,
}

struct Inst {
    model: u32,
    xf: Xf,
    nrm: DMat3,
    flip: bool,
    tint: [f32; 3],
    alpha: f32,
    dcs: Option<u32>,
}

struct Entry {
    inst: u32,
    dc: u32,
    tris: usize,
}

fn white() -> Image {
    Image { w: 4, h: 4, rgba: vec![255; 64] }
}

fn checker() -> Image {
    let mut rgba = vec![0u8; 16 * 16 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let c = (x / 8 + y / 8) % 2 == 1;
            rgba[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4].copy_from_slice(if c { &[255, 0, 255, 255] } else { &[0, 0, 0, 255] });
        }
    }
    Image { w: 16, h: 16, rgba }
}

fn quant(t: [f32; 3]) -> [u8; 3] {
    t.map(|v| (v.clamp(0.0, 1.0) * TINT_LEVELS).round() as u8)
}

fn mode(m: &Material, alpha: f32) -> Mode {
    match m.mode {
        _ if alpha < 0.99 && m.mode != material::Mode::Additive => Mode::Blend,
        material::Mode::Opaque => Mode::Opaque,
        material::Mode::AlphaTest => Mode::AlphaTest,
        material::Mode::Blend => Mode::Blend,
        material::Mode::Additive => Mode::Additive,
    }
}

pub fn build(fs: &Fs, world: &World, ents: &[Ent], o: Opts) -> Built {
    let mut names = world.models.clone();
    let mut ids: HashMap<String, u32> = names.iter().enumerate().map(|(i, n)| (n.clone(), i as u32)).collect();
    let mut insts: Vec<Inst> = Vec::new();
    let inst = |model: u32, xf: Xf, tint: [f32; 3], alpha: f32, dcs: Option<u32>| {
        let nrm = xf.r.inverse().transpose();
        Inst { model, flip: xf.r.determinant() < 0.0, xf, nrm, tint, alpha, dcs }
    };
    for d in &world.draws {
        if d.flags & OVERLAY != 0 {
            continue;
        }
        insts.push(inst(d.model, d.xf, d.tint, 1.0, d.dc));
    }
    for e in ents.iter().filter(|e| e.render) {
        let Some(v) = &e.vmdl else { continue };
        let n = names.len() as u32;
        let id = *ids.entry(v.clone()).or_insert_with(|| {
            names.push(v.clone());
            n
        });
        insts.push(inst(id, e.xf, e.tint, e.alpha, None));
    }

    let loaded: Vec<Result<Model, String>> = names
        .par_iter()
        .map(|n| fs.read(&format!("{n}_c")).and_then(Model::load).map_err(|e| format!("{n}: {e:#}")))
        .collect();
    let mut bad_models = Vec::new();
    let models: Vec<Option<Model>> = loaded
        .into_iter()
        .map(|r| match r {
            Ok(m) => Some(m),
            Err(e) => {
                bad_models.push(e);
                None
            }
        })
        .collect();

    let mut entries: Vec<Entry> = Vec::new();
    for (ii, it) in insts.iter().enumerate() {
        let Some(m) = &models[it.model as usize] else { continue };
        let dcs: Vec<u32> = match it.dcs {
            Some(d) if (d as usize) < m.draws.len() => vec![d],
            Some(_) => Vec::new(),
            None => (0..m.draws.len() as u32).collect(),
        };
        for dc in dcs {
            let c = &m.draws[dc as usize];
            if c.triangles && c.index_count >= 3 {
                entries.push(Entry { inst: ii as u32, dc, tris: c.index_count / 3 });
            }
        }
    }
    let total = entries.len();
    if let Some(keep) = o.keep {
        let flags: Vec<bool> = entries
            .par_iter()
            .map(|e| {
                let it = &insts[e.inst as usize];
                let m = models[it.model as usize].as_ref().unwrap();
                let c = &m.draws[e.dc as usize];
                let (Some(ps), Ok(ix)) = (m.stream(c, "POSITION", 0), m.index_view(c)) else { return false };
                let mut lo = DVec3::splat(f64::INFINITY);
                let mut hi = DVec3::splat(f64::NEG_INFINITY);
                for k in 0..ix.len() {
                    let p = ps.get(ix.get(k) as usize);
                    let q = it.xf.apply(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
                keep(lo, hi)
            })
            .collect();
        let mut i = 0;
        entries.retain(|_| {
            i += 1;
            flags[i - 1]
        });
    }
    let dropped = total - entries.len();

    let mut mat_names: Vec<String> = entries
        .iter()
        .map(|e| {
            let it = &insts[e.inst as usize];
            models[it.model as usize].as_ref().unwrap().draws[e.dc as usize].material.to_lowercase()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    mat_names.retain(|n| !n.is_empty());
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mats: HashMap<String, Option<Material>> = mat_names
        .par_iter()
        .map(|n| (n.clone(), material::load(fs, n)))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(n, r)| match r {
            Ok(m) => (n, m),
            Err(_) => {
                missing.insert(n.clone());
                (n, Some(Material {
                    shader: String::new(),
                    fill: Fill::Solid([255, 0, 255, 255]),
                    layer2: None,
                    mode: material::Mode::Opaque,
                    alpha: 1.0,
                    tint: [1.0; 3],
                    uv_scale: [1.0; 2],
                    uv_offset: [0.0; 2],
                    decal: false,
                }))
            }
        })
        .collect();

    let mut keys: HashMap<(String, [u8; 3], Mode, u32), usize> = HashMap::new();
    let mut groups: Vec<(String, [u8; 3], Mode, f32, Vec<usize>)> = Vec::new();
    for (ei, e) in entries.iter().enumerate() {
        let it = &insts[e.inst as usize];
        let c = &models[it.model as usize].as_ref().unwrap().draws[e.dc as usize];
        let mn = c.material.to_lowercase();
        let Some(Some(mat)) = mats.get(&mn) else { continue };
        let t: [f32; 3] = std::array::from_fn(|k| mat.tint[k] * c.tint[k] * it.tint[k]);
        let alpha = mat.alpha * c.alpha * it.alpha;
        let md = mode(mat, alpha);
        let a = if matches!(md, Mode::Blend | Mode::Additive) { alpha } else { 1.0 };
        let key = (mn.clone(), quant(t), md, a.to_bits());
        let gi = *keys.entry(key).or_insert_with(|| {
            groups.push((mn, quant(t), md, a, Vec::new()));
            groups.len() - 1
        });
        groups[gi].4.push(ei);
    }

    let mut tex_names: Vec<String> = groups
        .iter()
        .filter_map(|g| match &mats[&g.0].as_ref().unwrap().fill {
            Fill::Texture(t) => Some(t.clone()),
            Fill::Solid(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    tex_names.sort();
    let decoded: HashMap<String, Option<Image>> = tex_names
        .par_iter()
        .map(|t| {
            let im = texture::load(fs, &format!("{t}_c"), o.tex_max).ok().map(|i| Image { w: i.w as u32, h: i.h as u32, rgba: i.rgba });
            (t.clone(), im)
        })
        .collect();
    for (t, im) in &decoded {
        if im.is_none() {
            missing.insert(t.clone());
        }
    }

    let mut images: Vec<Option<Image>> = Vec::new();
    let mut tex_ids: HashMap<String, usize> = HashMap::new();
    let mut tex_label: Vec<String> = Vec::new();
    let mut group_tex = Vec::with_capacity(groups.len());
    let mut decoded = decoded;
    for (mn, _, _, _, _) in &groups {
        let mat = mats[mn].as_ref().unwrap();
        let src = match &mat.fill {
            Fill::Texture(t) => t.clone(),
            Fill::Solid(c) => format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]),
        };
        let id = *tex_ids.entry(src.clone()).or_insert_with(|| {
            images.push(Some(match &mat.fill {
                Fill::Texture(t) => decoded.remove(t).flatten().unwrap_or_else(checker),
                Fill::Solid(c) => Image { w: 4, h: 4, rgba: c.repeat(16) },
            }));
            tex_label.push(mn.rsplit('/').next().unwrap_or(mn).trim_end_matches(".vmat").to_string());
            images.len() - 1
        });
        group_tex.push(id);
    }
    let (atlas, lm_rect, flat) = match o.lightmap {
        Some(l) => (l.atlas, l.rect, l.flat),
        None => (white(), [0.0; 4], [0.5, 0.5]),
    };
    let mut batches: Vec<Batch> = Vec::new();
    let mut faces = 0;
    for (gi, (mn, tq, md, alpha, list)) in groups.iter().enumerate() {
        let mat = mats[mn].as_ref().unwrap();
        let mut start = 0;
        while start < list.len() {
            let mut end = start;
            let mut n = 0;
            while end < list.len() && (end == start || n + entries[list[end]].tris * 3 <= SPLIT) {
                n += entries[list[end]].tris * 3;
                end += 1;
            }
            let part = &list[start..end];
            let mut verts = vec![Vertex { pos: [0.0; 3], uv: [0.0; 2], lm: [0.0; 2], bias: 0.0, normal: [0.0; 3] }; n];
            let mut slices: Vec<&mut [Vertex]> = Vec::with_capacity(part.len());
            let mut rest: &mut [Vertex] = &mut verts;
            for &ei in part {
                let (a, b) = rest.split_at_mut(entries[ei].tris * 3);
                slices.push(a);
                rest = b;
            }
            slices.into_par_iter().zip(part.par_iter()).for_each(|(out, &ei)| {
                let e = &entries[ei];
                let it = &insts[e.inst as usize];
                let m = models[it.model as usize].as_ref().unwrap();
                fill(m, e.dc as usize, it, mat, lm_rect, flat, out);
            });
            faces += n / 3;
            batches.push(Batch { tex: group_tex[gi], mode: *md, alpha: *alpha, warp: None, tint: tq.map(|v| v as f32 / TINT_LEVELS), verts });
            start = end;
        }
    }

    let points = sample_points(&batches);
    let anim = vec![None; images.len()];
    Built {
        mesh: Mesh {
            batches,
            textures: images,
            anim,
            atlas,
            faces,
            missing: missing.into_iter().collect(),
            points,
            nudged: 0,
            names: tex_label,
        },
        draws: entries.len(),
        dropped,
        models: names.len(),
        materials: mats.len(),
        bad_models,
    }
}

fn fill(m: &Model, dc: usize, it: &Inst, mat: &Material, lm_rect: [f32; 4], flat: [f32; 2], out: &mut [Vertex]) {
    let c = &m.draws[dc];
    let (Some(ps), Ok(ix)) = (m.stream(c, "POSITION", 0), m.index_view(c)) else { return };
    let uvs = m.stream(c, "TEXCOORD", 0);
    let lms = if c.lightmapped && lm_rect[2] > 0.0 { m.map_stream(c, "TEXCOORD", 3) } else { None };
    let ns = m.stream(c, "NORMAL", 0);
    let bias = if mat.decal { 1.0 } else { 0.0 };
    for (t, tri) in out.chunks_exact_mut(3).enumerate() {
        let order = if it.flip { [0, 2, 1] } else { [0, 1, 2] };
        let mut p = [DVec3::ZERO; 3];
        for (k, &o) in order.iter().enumerate() {
            let i = ix.get(t * 3 + o) as usize;
            let v = ps.get(i);
            p[k] = it.xf.apply(DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64));
            let uv = uvs.map_or([0.0; 4], |s| s.get(i));
            let lm = match lms {
                Some(s) => {
                    let l = s.get(i);
                    [lm_rect[0] + l[0] * lm_rect[2], lm_rect[1] + l[1] * lm_rect[3]]
                }
                None => flat,
            };
            tri[k] = Vertex {
                pos: [p[k].x as f32, p[k].y as f32, p[k].z as f32],
                uv: [uv[0] * mat.uv_scale[0] + mat.uv_offset[0], uv[1] * mat.uv_scale[1] + mat.uv_offset[1]],
                lm,
                bias,
                normal: [0.0; 3],
            };
            if let Some(n) = ns.and_then(|s| s.normal(i)) {
                let w = (it.nrm * DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64)).normalize_or_zero();
                tri[k].normal = [w.x as f32, w.y as f32, w.z as f32];
            }
        }
        if ns.is_none() {
            let f = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
            for v in tri.iter_mut() {
                v.normal = [f.x as f32, f.y as f32, f.z as f32];
            }
        }
    }
}

fn sample_points(batches: &[Batch]) -> Vec<DVec3> {
    let key = |v: &Vertex| {
        let q = |x: f32| (x as f64 / POINT_CELL).floor() as i32;
        (q(v.pos[0]), q(v.pos[1]), q(v.pos[2]))
    };
    let sets: Vec<HashMap<(i32, i32, i32), [f32; 3]>> = batches
        .par_iter()
        .flat_map(|b| b.verts.par_chunks(1 << 18))
        .map(|chunk| {
            let mut s = HashMap::new();
            for v in chunk {
                s.entry(key(v)).or_insert(v.pos);
            }
            s
        })
        .collect();
    let mut all: HashMap<(i32, i32, i32), [f32; 3]> = HashMap::new();
    for s in sets {
        for (k, v) in s {
            all.entry(k).or_insert(v);
        }
    }
    all.into_values().map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect()
}
