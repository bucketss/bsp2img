use std::collections::{BTreeMap, BTreeSet, HashMap};

use glam::{DMat3, DVec3};
use rayon::prelude::*;

use super::bsp::{LUMP_GAME_LUMP, Map};
use super::cube::{self, Cube, Cubes};
use super::model::{self, Model};
use super::sprp::StaticProps;
use super::vhv::{self, Vhv};
use super::vmt::{self, Material};
use super::vtf;
use crate::bsp::Entity;
use crate::light::LightParams;
use crate::mesh::{Batch, Chunk, Image, Mesh, Mode, Vertex, pack_rgb9e5};
use crate::s2::texture::{Compressed, Tex};
use crate::s2::vpk::Fs;
use crate::s2::world::Xf;

const BATCH_BYTES: usize = 240 << 20;
const CHUNK_TILE: f64 = 1024.0;
const POINT_CELL: f64 = 32.0;
const TINT_LEVELS: f32 = 255.0;
const LUT_RANGE: f32 = 4.0;
const LUT_SIZE: usize = 4096;
pub const VHV_SCALE: f32 = 2.0;
pub const VHV_GAMMA: f32 = 2.2;
const IGNORE_NORMALS: u32 = 0x8;

pub const CLASSES: &[&str] = &[
    "prop_dynamic",
    "prop_dynamic_override",
    "prop_physics",
    "prop_physics_multiplayer",
    "prop_physics_override",
    "prop_door_rotating",
    "dz_door",
];

pub struct Opts<'a> {
    pub tex_max: usize,
    pub crop: Option<[f64; 4]>,
    pub band: Option<[f64; 2]>,
    pub light: &'a LightParams,
}

#[derive(Default, Debug)]
pub struct Built {
    pub static_props: usize,
    pub ent_props: usize,
    pub models: usize,
    pub missing_models: Vec<String>,
    pub bad_models: Vec<String>,
    pub draws: usize,
    pub dropped: usize,
    pub skipped: usize,
    pub tris: usize,
    pub vhv: usize,
    pub vhv_bad: usize,
    pub cube_lit: usize,
    pub cube_hdr: bool,
    pub materials: usize,
    pub missing: Vec<String>,
    pub no_base: usize,
    pub textures: usize,
}

type Key = (usize, Mode, u32, [u8; 3]);

struct Inst {
    model: u32,
    xf: Xf,
    nrm: DMat3,
    flip: bool,
    skin: usize,
    tint: [f32; 3],
    alpha: f32,
    mode: Option<Mode>,
    prop: Option<usize>,
    origin: Option<DVec3>,
    flat: bool,
}

struct Entry {
    inst: u32,
    mesh: u32,
    tris: usize,
    verts: usize,
    clip: bool,
    centre: DVec3,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.verts * std::mem::size_of::<Vertex>() + self.tris * 12
    }
}

enum Light {
    Cube(Cube),
    Vhv(Vec<Vec<[u8; 3]>>),
}

#[derive(Clone)]
enum MatRef {
    Mat(usize),
    Missing,
    Skip,
}

fn num(e: &Entity, k: &str, d: f64) -> f64 {
    e.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d)
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

pub fn vhv_linear(c: [u8; 3]) -> [f32; 3] {
    c.map(|v| (VHV_SCALE * v as f32 / 255.0).powf(VHV_GAMMA))
}

fn inst(model: u32, xf: Xf) -> Inst {
    let nrm = xf.r.inverse().transpose();
    Inst { model, flip: xf.r.determinant() < 0.0, xf, nrm, skin: 0, tint: [1.0; 3], alpha: 1.0, mode: None, prop: None, origin: None, flat: false }
}

fn entity_inst(e: &Entity, model: u32) -> Option<Inst> {
    if e.get("startdisabled").is_some_and(vmt::truthy) {
        return None;
    }
    let rmode = num(e, "rendermode", 0.0) as i32;
    let amt = (num(e, "renderamt", 255.0) / 255.0).clamp(0.0, 1.0) as f32;
    if rmode == 10 || (rmode != 0 && amt <= 0.01) {
        return None;
    }
    let mode = match rmode {
        0 => None,
        4 => Some(Mode::AlphaTest),
        5 => Some(Mode::Additive),
        _ => Some(Mode::Blend),
    };
    let scale = num(e, "modelscale", 1.0);
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let xf = Xf { r: super::entities::rotation(e) * scale, t: e.origin().unwrap_or(DVec3::ZERO) };
    let c: Vec<f32> = e.get("rendercolor").map(|s| s.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    let mut it = inst(model, xf);
    if c.len() >= 3 {
        it.tint = [c[0] / 255.0, c[1] / 255.0, c[2] / 255.0];
    }
    it.mode = mode;
    it.alpha = if matches!(mode, Some(Mode::Blend | Mode::Additive)) { amt } else { 1.0 };
    it.skin = num(e, "skin", 0.0).max(0.0) as usize;
    Some(it)
}

fn resolve(fs: &Fs, m: &Model, tex: usize) -> Option<String> {
    let paths = m.vmt_paths(tex);
    paths.iter().find(|p| fs.has(p)).cloned().or_else(|| paths.into_iter().next())
}

pub fn add(map: &Map, fs: &Fs, ents: &[Entity], mesh: &mut Mesh, tex_ids: &mut HashMap<String, usize>, o: Opts) -> Built {
    let mut tm = crate::prof::Timer::new("props");
    let mut out = Built::default();
    let sp = match map.lump(LUMP_GAME_LUMP).and_then(|gl| StaticProps::from_bsp(map.bytes(), &gl)) {
        Ok(s) => s,
        Err(e) => {
            out.bad_models.push(format!("static props: {e:#}"));
            None
        }
    };
    let mut names: Vec<String> = Vec::new();
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut id = |n: &str| -> u32 {
        let n = model::norm_name(n);
        let k = names.len() as u32;
        *ids.entry(n.clone()).or_insert_with(|| {
            names.push(n);
            k
        })
    };
    let mut insts: Vec<Inst> = Vec::new();
    if let Some(sp) = &sp {
        for (i, p) in sp.props.iter().enumerate() {
            let Some(n) = sp.name(p) else {
                out.bad_models.push(format!("static prop {i}: model index {} out of range", p.model));
                continue;
            };
            let mut it = inst(id(n), p.xf());
            it.skin = p.skin.max(0) as usize;
            it.tint = std::array::from_fn(|k| p.color[k] as f32 / 255.0);
            it.alpha = p.color[3] as f32 / 255.0;
            it.prop = Some(i);
            it.origin = Some(DVec3::from(p.lighting_origin.map(|v| v as f64))).filter(|v| *v != DVec3::ZERO);
            it.flat = p.flags & IGNORE_NORMALS != 0;
            insts.push(it);
        }
        out.static_props = insts.len();
    }
    for e in ents.iter().skip(1) {
        if !CLASSES.contains(&e.class()) {
            continue;
        }
        let Some(m) = e.get("model").map(str::trim).filter(|m| m.to_lowercase().ends_with(".mdl")) else { continue };
        let mid = id(m);
        if let Some(it) = entity_inst(e, mid) {
            insts.push(it);
            out.ent_props += 1;
        }
    }
    if insts.is_empty() {
        return out;
    }

    let loaded: Vec<Result<Model, String>> =
        names.par_iter().map(|n| Model::load(n, |p| fs.read(p).ok()).map_err(|e| format!("{e:#}"))).collect();
    let mut models: Vec<Option<Model>> = Vec::with_capacity(loaded.len());
    for (n, r) in names.iter().zip(loaded) {
        match r {
            Ok(m) => models.push(Some(m)),
            Err(e) => {
                if e.ends_with(": not found") {
                    out.missing_models.push(n.clone());
                }
                out.bad_models.push(e);
                models.push(None);
            }
        }
    }
    out.models = names.len();
    tm.lap("models");

    let mut entries: Vec<Entry> = Vec::new();
    for (ii, it) in insts.iter().enumerate() {
        let Some(m) = &models[it.model as usize] else { continue };
        for (mi, me) in m.meshes.iter().enumerate() {
            if me.tris() > 0 {
                entries.push(Entry { inst: ii as u32, mesh: mi as u32, tris: me.tris(), verts: me.pos.len(), clip: false, centre: DVec3::ZERO });
            }
        }
    }
    let total = entries.len();
    let bounds: Vec<(DVec3, DVec3)> = entries
        .par_iter()
        .map(|e| {
            let it = &insts[e.inst as usize];
            let me = &models[it.model as usize].as_ref().unwrap().meshes[e.mesh as usize];
            me.pos.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(lo, hi), p| {
                let q = it.xf.apply(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
                (lo.min(q), hi.max(q))
            })
        })
        .collect();
    let mut keep = vec![true; entries.len()];
    for ((e, &(lo, hi)), k) in entries.iter_mut().zip(&bounds).zip(keep.iter_mut()) {
        e.centre = (lo + hi) * 0.5;
        if let Some([x0, y0, x1, y1]) = o.crop {
            *k = hi.x >= x0 && lo.x <= x1 && hi.y >= y0 && lo.y <= y1;
            e.clip = !(lo.x >= x0 && hi.x <= x1 && lo.y >= y0 && hi.y <= y1);
        }
        if let Some([z0, z1]) = o.band {
            *k &= hi.z >= z0 && lo.z <= z1;
        }
    }
    let mut i = 0;
    entries.retain(|_| {
        i += 1;
        keep[i - 1]
    });
    out.dropped = total - entries.len();
    tm.lap("crop");

    let tex_of = |e: &Entry| -> Option<(u32, usize)> {
        let it = &insts[e.inst as usize];
        let m = models[it.model as usize].as_ref().unwrap();
        m.texture(it.skin, m.meshes[e.mesh as usize].material).map(|t| (it.model, t))
    };
    let keys: Vec<(u32, usize)> = entries.iter().filter_map(tex_of).collect::<BTreeSet<_>>().into_iter().collect();
    let paths: Vec<Option<String>> = keys.par_iter().map(|&(m, t)| resolve(fs, models[m as usize].as_ref().unwrap(), t)).collect();
    let vmts: Vec<String> = paths.iter().flatten().cloned().collect::<BTreeSet<_>>().into_iter().collect();
    let loaded: Vec<Result<Material, String>> = vmts.par_iter().map(|p| vmt::load(fs, p).map_err(|e| format!("{e:#}"))).collect();
    let mut mats: Vec<Material> = Vec::new();
    let mut by_path: HashMap<&str, MatRef> = HashMap::new();
    let mut missing: BTreeSet<String> = BTreeSet::new();
    for (p, r) in vmts.iter().zip(loaded) {
        let mr = match r {
            Ok(m) if m.skip || m.base.is_none() => {
                if !m.skip {
                    out.no_base += 1;
                }
                MatRef::Skip
            }
            Ok(m) => {
                mats.push(m);
                MatRef::Mat(mats.len() - 1)
            }
            Err(_) => {
                missing.insert(p.clone());
                MatRef::Missing
            }
        };
        by_path.insert(p.as_str(), mr);
    }
    let mat_of: HashMap<(u32, usize), MatRef> =
        keys.iter().zip(&paths).map(|(k, p)| (*k, p.as_deref().and_then(|p| by_path.get(p).cloned()).unwrap_or(MatRef::Missing))).collect();
    out.materials = vmts.len();
    tm.lap("materials");

    let mut bases: Vec<String> = mats.iter().filter_map(|m| m.base.clone()).filter(|b| !tex_ids.contains_key(b)).collect();
    bases.sort();
    bases.dedup();
    let decoded: Vec<Result<Tex, ()>> = bases.par_iter().map(|t| fs.read(t).ok().and_then(|d| vtf::texture(&d, o.tex_max).ok()).ok_or(())).collect();
    for (b, r) in bases.into_iter().zip(decoded) {
        let label = b.strip_prefix("materials/").unwrap_or(&b).trim_end_matches(".vtf").to_string();
        let (im, bc) = match r {
            Ok(Tex::Plain(i)) => (Image { w: i.w as u32, h: i.h as u32, rgba: i.rgba }, None),
            Ok(Tex::Bc(c)) => (Image { w: c.w as u32, h: c.h as u32, rgba: Vec::new() }, Some(c)),
            Err(()) => {
                missing.insert(b.clone());
                (checker(), None)
            }
        };
        add_tex(mesh, tex_ids, b, im, bc, label);
        out.textures += 1;
    }
    let missing_tex = match tex_ids.get("#missing") {
        Some(&t) => t,
        None => add_tex(mesh, tex_ids, "#missing".into(), checker(), None, "missing".into()),
    };
    tm.lap("textures");

    let mut index: HashMap<Key, usize> = HashMap::new();
    let mut groups: Vec<(Key, Vec<usize>)> = Vec::new();
    let mut skipped = 0;
    for (ei, e) in entries.iter().enumerate() {
        let it = &insts[e.inst as usize];
        let mr = tex_of(e).and_then(|k| mat_of.get(&k).cloned()).unwrap_or(MatRef::Missing);
        let (tex, mode, alpha, tint) = match mr {
            MatRef::Skip => {
                skipped += 1;
                continue;
            }
            MatRef::Missing => (missing_tex, Mode::Opaque, 1.0, [1.0; 3]),
            MatRef::Mat(k) => {
                let m = &mats[k];
                (tex_ids[m.base.as_ref().unwrap()], m.mode, m.alpha, m.tint)
            }
        };
        let (mode, alpha) = match it.mode {
            Some(md) if mode != Mode::Additive => (md, alpha * it.alpha),
            _ => (mode, alpha * it.alpha),
        };
        let mode = if alpha < 0.99 && matches!(mode, Mode::Opaque | Mode::AlphaTest) { Mode::Blend } else { mode };
        let alpha = if matches!(mode, Mode::Blend | Mode::Additive) { alpha } else { 1.0 };
        let t = quant(std::array::from_fn(|k| tint[k] * it.tint[k]));
        let key = (tex, mode, alpha.to_bits(), t);
        let gi = *index.entry(key).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[gi].1.push(ei);
    }
    let used: BTreeSet<u32> = groups.iter().flat_map(|g| g.1.iter().map(|&ei| entries[ei].inst)).collect();
    missing.extend(out.missing_models.iter().cloned());
    out.missing = missing.into_iter().collect();

    let cubes = match Cubes::load(map) {
        Ok(c) => c,
        Err(e) => {
            out.bad_models.push(format!("leaf ambient lighting: {e:#}"));
            None
        }
    };
    out.cube_hdr = cubes.as_ref().is_some_and(|c| c.hdr);
    let vhv_hdr = cubes.as_ref().is_none_or(|c| c.hdr);
    let grey: Cube = [[0.5; 3]; 6];
    let lights: HashMap<u32, (Light, bool)> = used
        .par_iter()
        .map(|&ii| {
            let it = &insts[ii as usize];
            let m = models[it.model as usize].as_ref().unwrap();
            let mut bad = false;
            if let Some(pi) = it.prop {
                let bytes = if vhv_hdr { fs.read(&vhv::prop_name(pi, true)).or_else(|_| fs.read(&vhv::prop_name(pi, false))) } else { fs.read(&vhv::prop_name(pi, false)) };
                if let Ok(b) = bytes {
                    match Vhv::parse(&b).and_then(|v| v.for_model(m)) {
                        Ok(c) => return (ii, (Light::Vhv(c.into_iter().map(|v| v.iter().map(vhv::average).collect()).collect()), false)),
                        Err(_) => bad = true,
                    }
                }
            }
            let p = it.origin.unwrap_or_else(|| it.xf.apply(DVec3::from(m.lo.map(|v| v as f64)).midpoint(DVec3::from(m.hi.map(|v| v as f64)))));
            let c = cubes.as_ref().map_or(grey, |c| c.at(p));
            (ii, (Light::Cube(c), bad))
        })
        .collect();
    for (l, bad) in lights.values() {
        match l {
            Light::Vhv(_) => out.vhv += 1,
            Light::Cube(_) => out.cube_lit += 1,
        }
        out.vhv_bad += *bad as usize;
    }
    tm.lap("lighting");

    let hdr_world = mesh.hdr.is_some();
    let lut: Vec<f32> = (0..LUT_SIZE).map(|i| o.light.light_to_screen((i as f32 / LUT_SIZE as f32 * LUT_RANGE) as f64) as f32 / 255.0).collect();
    let conv = |c: [f32; 3]| -> u32 {
        let c = if hdr_world { c } else { c.map(|v| lut[((v.max(0.0) / LUT_RANGE * LUT_SIZE as f32) as usize).min(LUT_SIZE - 1)]) };
        pack_rgb9e5(c).max(1)
    };
    let em: Vec<(u32, u32)> = entries.iter().map(|e| (e.inst, e.mesh)).collect();
    let fill = |ei: usize, out_v: &mut [Vertex], out_i: &mut [u32]| {
        let (inst, mesh) = em[ei];
        let it = &insts[inst as usize];
        let me = &models[it.model as usize].as_ref().unwrap().meshes[mesh as usize];
        let light = &lights[&inst].0;
        for (k, v) in out_v.iter_mut().enumerate() {
            let p = it.xf.apply(DVec3::from(me.pos[k].map(|x| x as f64)));
            let n = (it.nrm * DVec3::from(me.normal[k].map(|x| x as f64))).normalize_or_zero();
            let l = match light {
                Light::Vhv(c) => c.get(mesh as usize).and_then(|c| c.get(k)).map_or([0.0; 3], |&c| vhv_linear(c)),
                Light::Cube(c) => cube::apply(c, if it.flat { DVec3::ZERO } else { n }),
            };
            *v = Vertex {
                pos: [p.x as f32, p.y as f32, p.z as f32],
                uv: me.uv[k],
                lm: [0.5, 0.5],
                normal: [n.x as f32, n.y as f32, n.z as f32],
                light: conv(l),
                ..Vertex::BLANK
            };
        }
        for (t, d) in me.idx.chunks_exact(3).zip(out_i.chunks_exact_mut(3)) {
            d.copy_from_slice(&if it.flip { [t[0], t[2], t[1]] } else { [t[0], t[1], t[2]] });
        }
    };
    let clipped: HashMap<usize, Vec<Vertex>> = groups
        .iter()
        .flat_map(|g| g.1.iter().copied())
        .filter(|&ei| entries[ei].clip)
        .collect::<Vec<_>>()
        .par_iter()
        .map(|&ei| {
            let e = &entries[ei];
            let mut v = vec![Vertex::BLANK; e.verts];
            let mut ix = vec![0u32; e.tris * 3];
            fill(ei, &mut v, &mut ix);
            let soup: Vec<Vertex> = ix.iter().map(|&i| v[i as usize]).collect();
            (ei, crate::s2::mesh::clip_rect(&soup, o.crop.unwrap()))
        })
        .collect();
    for (&ei, v) in &clipped {
        entries[ei].tris = v.len() / 3;
        entries[ei].verts = v.len();
    }
    tm.lap("clip");

    let tile = |ei: usize| {
        let c = entries[ei].centre;
        ((c.x / CHUNK_TILE).floor() as i32, (c.y / CHUNK_TILE).floor() as i32)
    };
    for g in &mut groups {
        g.1.retain(|&ei| entries[ei].tris > 0);
        g.1.sort_by_key(|&ei| tile(ei));
    }
    let mut parts: Vec<(usize, usize, usize)> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let list = &g.1;
        let mut start = 0;
        while start < list.len() {
            let mut end = start;
            let mut n = 0;
            while end < list.len() && (end == start || n + entries[list[end]].bytes() <= BATCH_BYTES) {
                n += entries[list[end]].bytes();
                end += 1;
            }
            parts.push((gi, start, end));
            start = end;
        }
    }
    let batches: Vec<Batch> = parts
        .par_iter()
        .map(|&(gi, start, end)| {
            let ((tex, mode, alpha, tint), list) = &groups[gi];
            let part = &list[start..end];
            let (verts, idx) = gather(part, &entries, |ei, v, ix| match clipped.get(&ei) {
                Some(c) => {
                    v.copy_from_slice(c);
                    ix.iter_mut().enumerate().for_each(|(k, x)| *x = k as u32);
                }
                None => fill(ei, v, ix),
            });
            let mut chunks = Vec::new();
            let (mut v0, mut i0, mut v, mut i) = (0, 0, 0, 0);
            for (k, &ei) in part.iter().enumerate() {
                v += entries[ei].verts;
                i += entries[ei].tris * 3;
                if part.get(k + 1).is_none_or(|&n| tile(n) != tile(ei)) {
                    chunks.push(Chunk::new(&verts[v0..v], i0, i - i0));
                    (v0, i0) = (v, i);
                }
            }
            Batch {
                tex: *tex,
                mode: *mode,
                alpha: f32::from_bits(*alpha),
                warp: None,
                tint: tint.map(|v| v as f32 / TINT_LEVELS),
                layer2: None,
                layer3: None,
                tint_mask: None,
                verts,
                idx,
                chunks,
            }
        })
        .collect();
    tm.lap("fill");
    out.draws = groups.iter().map(|g| g.1.len()).sum();
    out.skipped = skipped;
    out.tris = batches.iter().map(|b| b.idx.len() / 3).sum();
    mesh.faces += out.tris;
    mesh.points.extend(sample_points(&batches));
    mesh.missing.extend(out.missing.iter().cloned());
    mesh.missing.sort();
    mesh.missing.dedup();
    mesh.batches.extend(batches);
    tm.lap("points");
    out
}

fn add_tex(mesh: &mut Mesh, tex_ids: &mut HashMap<String, usize>, key: String, im: Image, bc: Option<Compressed>, label: String) -> usize {
    mesh.textures.push(Some(im));
    mesh.compressed.push(bc);
    mesh.anim.push(None);
    mesh.names.push(label);
    tex_ids.insert(key, mesh.textures.len() - 1);
    mesh.textures.len() - 1
}

fn gather(part: &[usize], entries: &[Entry], f: impl Fn(usize, &mut [Vertex], &mut [u32]) + Sync) -> (Vec<Vertex>, Vec<u32>) {
    let nv: usize = part.iter().map(|&ei| entries[ei].verts).sum();
    let ni: usize = part.iter().map(|&ei| entries[ei].tris * 3).sum();
    let mut verts = vec![Vertex::BLANK; nv];
    let mut idx = vec![0u32; ni];
    let mut jobs: Vec<(usize, u32, &mut [Vertex], &mut [u32])> = Vec::with_capacity(part.len());
    let (mut rv, mut ri): (&mut [Vertex], &mut [u32]) = (&mut verts, &mut idx);
    let mut base = 0u32;
    for &ei in part {
        let (a, b) = rv.split_at_mut(entries[ei].verts);
        let (c, d) = ri.split_at_mut(entries[ei].tris * 3);
        jobs.push((ei, base, a, c));
        base += entries[ei].verts as u32;
        (rv, ri) = (b, d);
    }
    jobs.into_par_iter().for_each(|(ei, base, out, ix)| {
        f(ei, out, ix);
        ix.iter_mut().for_each(|x| *x += base);
    });
    (verts, idx)
}

fn sample_points(batches: &[Batch]) -> Vec<DVec3> {
    let key = |v: &Vertex| {
        let q = |x: f32| (x as f64 / POINT_CELL).floor() as i32;
        (q(v.pos[0]), q(v.pos[1]), q(v.pos[2]))
    };
    let mut all: BTreeMap<(i32, i32, i32), [f32; 3]> = BTreeMap::new();
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
    for s in sets {
        for (k, v) in s {
            all.entry(k).or_insert(v);
        }
    }
    all.into_values().map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect()
}

impl Built {
    pub fn summary(&self) -> String {
        format!(
            "props: {} static + {} entity, {} models, {} draws ({} cropped, {} nodraw), {} triangles, {} materials, {} new textures; lighting {} vhv, {} {} ambient cube{}",
            self.static_props,
            self.ent_props,
            self.models,
            self.draws,
            self.dropped,
            self.skipped,
            self.tris,
            self.materials,
            self.textures,
            self.vhv,
            self.cube_lit,
            if self.cube_hdr { "HDR" } else { "LDR" },
            if self.vhv_bad > 0 { format!(" ({} vhv files do not match their model)", self.vhv_bad) } else { String::new() }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    #[ignore]
    fn vhv_fit() {
        let Ok(p) = std::env::var("S1_MAP") else { return };
        let map = Arc::new(Map::open(std::path::Path::new(&p)).unwrap());
        let fs = super::super::fs::open(&map, None).unwrap().fs;
        let gl = map.lump(LUMP_GAME_LUMP).unwrap();
        let sp = StaticProps::from_bsp(map.bytes(), &gl).unwrap().unwrap();
        let cubes = Cubes::load(&map).unwrap().unwrap();
        let lum = |c: [f32; 3]| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
        let mut bins: Vec<Vec<f32>> = vec![Vec::new(); 20];
        let mut alpha = [[0f64; 2]; 20];
        let mut cache: HashMap<String, Option<Model>> = HashMap::new();
        for (i, pr) in sp.props.iter().enumerate().step_by(3) {
            let name = sp.name(pr).unwrap().to_string();
            let m = cache.entry(name.clone()).or_insert_with(|| Model::load(&name, |q| fs.read(q).ok()).ok());
            let Some(m) = m else { continue };
            let Ok(b) = fs.read(&vhv::prop_name(i, true)) else { continue };
            let Ok(cols) = Vhv::parse(&b).and_then(|v| v.for_model(m)) else { continue };
            let xf = pr.xf();
            let nrm = xf.r.inverse().transpose();
            let cube = cubes.at(DVec3::from(pr.lighting_origin.map(|v| v as f64)));
            for (me, c) in m.meshes.iter().zip(&cols) {
                for (k, x) in c.iter().enumerate().step_by(7) {
                    let n = nrm * DVec3::from(me.normal[k].map(|v| v as f64));
                    let l = lum(cube::apply(&cube, n));
                    let a = vhv::average(x);
                    let v = lum(a.map(|q| q as f32 / 255.0));
                    let bi = ((v * 20.0) as usize).min(19);
                    bins[bi].push(l);
                    alpha[bi][0] += x[0][3] as f64;
                    alpha[bi][1] += 1.0;
                }
            }
        }
        for (bi, b) in bins.iter_mut().enumerate() {
            if b.len() < 20 {
                continue;
            }
            b.sort_by(f32::total_cmp);
            let v = (bi as f32 + 0.5) / 20.0;
            let med = b[b.len() / 2];
            println!("vhv {v:.3}: n {} cube median {med:.4} p25 {:.4} p75 {:.4}; ratio med/v^2.2 {:.3}; mean alpha {:.1}", b.len(), b[b.len() / 4], b[b.len() * 3 / 4], med / v.powf(2.2), alpha[bi][0] / alpha[bi][1]);
        }
    }
}
