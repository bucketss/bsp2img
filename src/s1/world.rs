use std::collections::{BTreeSet, HashMap};

use glam::{DMat3, DVec3};
use rayon::prelude::*;

use super::bsp::{Map, SURF_HIDDEN, World};
use super::disp;
use super::light::{self, Block, FULL, Lighting};
use super::vmt::{self, Material};
use super::vtf;
use crate::bsp::Entity;
use crate::light::LightParams;
use crate::mesh::{Batch, FourWay, Image, Mesh, Mode, Vertex, unorm16};
use crate::s2::texture::{Compressed, Tex};
use crate::s2::vpk::Fs;

pub const WATER_ALPHA: f32 = 0.75;
const WATER_DEFAULT: [f32; 3] = [0.16, 0.24, 0.26];

pub const SKIP_CLASSES: &[&str] = &[
    "func_areaportal",
    "func_areaportalwindow",
    "func_occluder",
    "func_viscluster",
    "func_precipitation",
    "func_dustmotes",
    "func_dustcloud",
    "func_smokevolume",
    "func_clip_vphysics",
    "func_nav_blocker",
    "func_nav_avoid",
    "func_nav_prefer",
    "func_nav_prerequisite",
    "func_no_defuse",
    "func_bomb_target",
    "func_buyzone",
    "func_hostage_rescue",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Ok,
    ToolWater,
    NoBase,
    MissingVmt,
    BadVmt,
    MissingVtf,
    BadVtf,
}

#[derive(Default, Debug)]
pub struct MatStats {
    pub counts: HashMap<Status, usize>,
    pub bad: Vec<String>,
}

impl MatStats {
    pub fn get(&self, s: Status) -> usize {
        self.counts.get(&s).copied().unwrap_or(0)
    }

    pub fn summary(&self) -> String {
        let n: usize = self.counts.values().sum();
        format!(
            "{n} materials: {} ok, {} tool/water, {} without $basetexture, {} missing vmt, {} bad vmt, {} missing vtf, {} bad vtf",
            self.get(Status::Ok),
            self.get(Status::ToolWater),
            self.get(Status::NoBase),
            self.get(Status::MissingVmt),
            self.get(Status::BadVmt),
            self.get(Status::MissingVtf),
            self.get(Status::BadVtf)
        )
    }
}

pub struct Opts<'a> {
    pub tex_max: usize,
    pub crop: Option<[f64; 4]>,
    pub band: Option<[f64; 2]>,
    pub map: &'a Map,
    pub light: &'a LightParams,
    pub lm_cap: usize,
}

pub struct Built {
    pub mesh: Mesh,
    pub materials: MatStats,
    pub faces: usize,
    pub dropped: usize,
    pub displacements: usize,
    pub brush_ents: usize,
    pub lightmap: Option<(u32, u32)>,
    pub notes: Vec<String>,
    pub tex_ids: HashMap<String, usize>,
}

pub struct Draw {
    pub model: usize,
    pub rot: DMat3,
    pub origin: DVec3,
    pub mode: Option<Mode>,
    pub alpha: f32,
    pub tint: [f32; 3],
}

impl Draw {
    pub fn world() -> Draw {
        Draw { model: 0, rot: DMat3::IDENTITY, origin: DVec3::ZERO, mode: None, alpha: 1.0, tint: [1.0; 3] }
    }

    pub fn apply(&self, p: DVec3) -> DVec3 {
        self.rot * p + self.origin
    }
}

fn num(e: &Entity, k: &str, d: f64) -> f64 {
    e.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d)
}

pub fn skip_class(cls: &str) -> bool {
    crate::mesh::skip_class(cls) || SKIP_CLASSES.contains(&cls)
}

pub fn draws(w: &World, ents: &[Entity]) -> Vec<Draw> {
    let mut out = vec![Draw::world()];
    for e in ents.iter().skip(1) {
        let Some(mi) = e.model().filter(|&m| m > 0 && m < w.models.len()) else { continue };
        if skip_class(e.class()) || e.get("startdisabled").is_some_and(vmt::truthy) {
            continue;
        }
        let rmode = num(e, "rendermode", 0.0) as i32;
        let amt = (num(e, "renderamt", 255.0) / 255.0).clamp(0.0, 1.0) as f32;
        if rmode == 10 || (rmode != 0 && amt <= 0.01) {
            continue;
        }
        let mode = match rmode {
            0 => None,
            4 => Some(Mode::AlphaTest),
            5 => Some(Mode::Additive),
            _ => Some(Mode::Blend),
        };
        let c: Vec<f32> = e.get("rendercolor").map(|s| s.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
        let tint = if c.len() >= 3 && rmode != 0 { [c[0] / 255.0, c[1] / 255.0, c[2] / 255.0] } else { [1.0; 3] };
        let alpha = if matches!(mode, Some(Mode::Blend | Mode::Additive)) { amt } else { 1.0 };
        out.push(Draw { model: mi, rot: super::entities::rotation(e), origin: e.origin().unwrap_or(DVec3::ZERO), mode, alpha, tint });
    }
    out
}

pub fn materials(fs: &Fs, w: &World) -> Vec<Result<Material, (Status, String)>> {
    w.texdata
        .par_iter()
        .map(|t| {
            let name = super::fs::norm(&t.name);
            vmt::load(fs, &name).map_err(|e| {
                let file = vmt::path(&name);
                if fs.has(&file) { (Status::BadVmt, format!("{e:#}")) } else { (Status::MissingVmt, file) }
            })
        })
        .collect()
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

fn solid(c: [f32; 3]) -> Image {
    let px = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    Image { w: 4, h: 4, rgba: [px[0], px[1], px[2], 255].repeat(16) }
}

type Key = (usize, Mode, u32, [u32; 3]);

struct Poly {
    key: Key,
    verts: Vec<Vertex>,
    idx: Vec<u32>,
    block: usize,
}

struct DispPoly {
    key: Key,
    draw: usize,
    grid: disp::Grid,
    block: usize,
    uv: [DVec3; 2],
    uv0: [f64; 2],
    size: [f64; 2],
}

fn clip(mut poly: Vec<Vertex>, [x0, y0, x1, y1]: [f64; 4]) -> Vec<Vertex> {
    for (axis, s, d) in [(0, 1.0, x0), (0, -1.0, -x1), (1, 1.0, y0), (1, -1.0, -y1)] {
        let side = |v: &Vertex| s * v.pos[axis] as f64 - d;
        let input = std::mem::take(&mut poly);
        for i in 0..input.len() {
            let (a, b) = (&input[i], &input[(i + 1) % input.len()]);
            let (sa, sb) = (side(a), side(b));
            if sa >= 0.0 {
                poly.push(*a);
            }
            if (sa >= 0.0) != (sb >= 0.0) {
                poly.push(Vertex::lerp(a, b, (sa / (sa - sb)) as f32));
            }
        }
        if poly.len() < 3 {
            return Vec::new();
        }
    }
    poly
}

fn inside(p: [f32; 3], [x0, y0, x1, y1]: [f64; 4]) -> bool {
    (x0..=x1).contains(&(p[0] as f64)) && (y0..=y1).contains(&(p[1] as f64))
}

fn f3(v: DVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

pub fn build(fs: &Fs, w: &World, ents: &[Entity], o: Opts) -> Built {
    let mut tm = crate::prof::Timer::new("source");
    let mut notes = Vec::new();
    let mats = materials(fs, w);
    tm.lap("materials");
    let lighting = match Lighting::open(o.map, w, o.light.all_styles) {
        Ok(l) => {
            if let Some(l) = &l {
                let (a, b) = l.check(w);
                if a < b {
                    notes.push(format!("lightmap layout: {a} of {b} face spans match"));
                }
            }
            l
        }
        Err(e) => {
            notes.push(format!("lightmaps: {e:#}"));
            None
        }
    };
    let disps = match w.faces.iter().any(|f| f.dispinfo >= 0).then(|| disp::read(o.map)) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            notes.push(format!("displacements: {e:#}"));
            None
        }
        None => None,
    };
    tm.lap("lumps");
    let draws = draws(w, ents);
    let mut used: BTreeSet<usize> = BTreeSet::new();
    let mut polys: Vec<Poly> = Vec::new();
    let mut dpolys: Vec<DispPoly> = Vec::new();
    let mut blocks: Vec<Block> = vec![Block::solid([FULL; 3])];
    let (mut total, mut disp_bad, mut unlit) = (0, 0, 0);
    for (di, d) in draws.iter().enumerate() {
        let m = &w.models[d.model];
        for fi in m.first_face as usize..(m.first_face + m.num_faces) as usize {
            let f = &w.faces[fi];
            if f.num_edges < 3 {
                continue;
            }
            let Some((ti, td)) = w.material(fi) else { continue };
            if ti.flags & SURF_HIDDEN != 0 {
                continue;
            }
            let tdi = ti.texdata as usize;
            let (mode, alpha) = match &mats[tdi] {
                Ok(mat) if mat.skip => continue,
                Ok(mat) if mat.water => (Mode::Blend, WATER_ALPHA),
                Ok(mat) if mat.base.is_none() => continue,
                Ok(mat) => (mat.mode, mat.alpha),
                Err(_) if super::fs::norm(&td.name).starts_with("tools/") => continue,
                Err(_) => (Mode::Opaque, 1.0),
            };
            let (mode, alpha) = match d.mode {
                Some(m) if mode != Mode::Additive => (m, alpha * d.alpha),
                _ => (mode, alpha * d.alpha),
            };
            let alpha = if matches!(mode, Mode::Blend | Mode::Additive) { alpha } else { 1.0 };
            total += 1;
            let key = (tdi, mode, alpha.to_bits(), d.tint.map(f32::to_bits));
            let fullbright = mode == Mode::Additive || mats[tdi].as_ref().is_ok_and(|m| m.shader == "unlitgeneric");
            let lf = lighting.as_ref().map(|l| &l.faces[fi]);
            let block = match lighting.as_ref().filter(|_| !fullbright).and_then(|l| l.sample(w, fi)) {
                Some(b) => {
                    blocks.push(b);
                    blocks.len() - 1
                }
                None => {
                    unlit += 1;
                    0
                }
            };
            let (tw, th) = (td.width.max(1) as f64, td.height.max(1) as f64);
            let sv = DVec3::new(ti.s[0] as f64, ti.s[1] as f64, ti.s[2] as f64);
            let tv = DVec3::new(ti.t[0] as f64, ti.t[1] as f64, ti.t[2] as f64);
            if f.dispinfo >= 0 {
                let g = disps.as_ref().and_then(|ds| disp::grid(w, ds, f.dispinfo as usize, fi));
                let Some(grid) = g else {
                    disp_bad += 1;
                    continue;
                };
                let size = lf.map_or([0.0; 2], |f| [f.lm_size[0].max(0) as f64, f.lm_size[1].max(0) as f64]);
                dpolys.push(DispPoly { key, draw: di, grid, block, uv: [sv / tw, tv / th], uv0: [ti.s[3] as f64 / tw, ti.t[3] as f64 / th], size });
                used.insert(tdi);
                continue;
            }
            let ls = DVec3::new(ti.lm_s[0] as f64, ti.lm_s[1] as f64, ti.lm_s[2] as f64);
            let lt = DVec3::new(ti.lm_t[0] as f64, ti.lm_t[1] as f64, ti.lm_t[2] as f64);
            let mins = lf.map_or([0.0; 2], |f| [f.lm_mins[0] as f64, f.lm_mins[1] as f64]);
            let normal = (d.rot * w.face_normal(fi)).normalize_or_zero();
            let mut verts: Vec<Vertex> = w
                .face_points(fi)
                .into_iter()
                .map(|p| Vertex {
                    pos: f3(d.apply(p)),
                    uv: [((p.dot(sv) + ti.s[3] as f64) / tw) as f32, ((p.dot(tv) + ti.t[3] as f64) / th) as f32],
                    lm: [(p.dot(ls) + ti.lm_s[3] as f64 - mins[0]) as f32, (p.dot(lt) + ti.lm_t[3] as f64 - mins[1]) as f32],
                    normal: f3(normal),
                    ..Vertex::BLANK
                })
                .collect();
            let n = verts.len();
            let c = verts.iter().map(|v| DVec3::from(v.pos.map(|x| x as f64))).sum::<DVec3>() / n as f64;
            let at = |i: usize| DVec3::from(verts[i].pos.map(|x| x as f64)) - c;
            let newell: DVec3 = (0..n).map(|i| at(i).cross(at((i + 1) % n))).sum();
            if newell.dot(normal) < 0.0 {
                verts.reverse();
            }
            if let Some(b) = o.crop {
                verts = clip(verts, b);
                if verts.is_empty() {
                    continue;
                }
            }
            if o.band.is_some_and(|[lo, hi]| verts.iter().all(|v| (v.pos[2] as f64) < lo) || verts.iter().all(|v| (v.pos[2] as f64) > hi)) {
                continue;
            }
            used.insert(tdi);
            polys.push(Poly { key, verts, idx: Vec::new(), block });
        }
    }
    let ndisp = dpolys.len();
    {
        let mut gs: Vec<&mut disp::Grid> = dpolys.iter_mut().map(|p| &mut p.grid).collect();
        disp::weld(&mut gs);
    }
    for p in dpolys {
        let d = &draws[p.draw];
        let g = &p.grid;
        let four = mats[p.key.0].as_ref().is_ok_and(|m| m.four_way().is_some());
        let verts: Vec<Vertex> = (0..g.pos.len())
            .map(|k| {
                let fl = g.flat[k];
                let uv = [(fl.dot(p.uv[0]) + p.uv0[0]) as f32, (fl.dot(p.uv[1]) + p.uv0[1]) as f32];
                Vertex {
                    pos: f3(d.apply(g.pos[k])),
                    uv,
                    uv2: uv,
                    lm: [(g.frac[k][0] * p.size[0]) as f32, (g.frac[k][1] * p.size[1]) as f32],
                    normal: f3((d.rot * g.normal[k]).normalize_or_zero()),
                    blend: match g.multi.get(k) {
                        Some(m) if four => [unorm16(m[1]), unorm16(m[2]), unorm16(m[3]), 0],
                        _ if four => [0; 4],
                        _ => [unorm16(g.alpha[k] / 255.0), 0, 0, 0],
                    },
                    ..Vertex::BLANK
                }
            })
            .collect();
        let idx: Vec<u32> = match (o.crop, o.band) {
            (None, None) => g.idx.clone(),
            (b, z) => g
                .idx
                .chunks_exact(3)
                .filter(|t| {
                    let c: [f32; 3] = std::array::from_fn(|k| t.iter().map(|&i| verts[i as usize].pos[k]).sum::<f32>() / 3.0);
                    b.is_none_or(|b| inside(c, b)) && z.is_none_or(|[lo, hi]| (lo..=hi).contains(&(c[2] as f64)))
                })
                .flatten()
                .copied()
                .collect(),
        };
        if idx.is_empty() {
            continue;
        }
        polys.push(Poly { key: p.key, verts, idx, block: p.block });
    }
    tm.lap("faces");

    let lm_cap = o.lm_cap as u32;
    let lit = lighting.as_ref().map(|l| {
        let atlas = light::pack(&blocks, lm_cap);
        for p in &mut polys {
            for v in &mut p.verts {
                v.lm = atlas.uv(p.block, v.lm);
            }
        }
        let (img, hdr) = light::finish(&atlas, o.light);
        (img, hdr, l.hdr, atlas.shrunk)
    });
    if lit.is_none() {
        for p in &mut polys {
            for v in &mut p.verts {
                v.lm = [0.5, 0.5];
            }
        }
    }
    tm.lap("lightmap");

    let mut stats = MatStats::default();
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut texs: Vec<String> = Vec::new();
    let layers = |m: &Material| -> Vec<Option<String>> {
        if m.shader == "worldvertextransition" {
            vec![m.texture("$basetexture2")]
        } else if let Some(f) = m.four_way() {
            f.tex.to_vec()
        } else {
            Vec::new()
        }
    };
    for (i, m) in mats.iter().enumerate() {
        let st = match m {
            Ok(m) if m.skip || m.water => Status::ToolWater,
            Ok(m) => match &m.base {
                None => Status::NoBase,
                Some(b) => {
                    texs.push(b.clone());
                    if used.contains(&i) {
                        texs.extend(layers(m).into_iter().flatten());
                    }
                    Status::Ok
                }
            },
            Err((s, what)) => {
                if used.contains(&i) || (*s == Status::MissingVmt && !what.starts_with("materials/tools/")) {
                    missing.insert(what.clone());
                }
                *s
            }
        };
        *stats.counts.entry(st).or_default() += 1;
    }
    texs.sort();
    texs.dedup();
    let decoded: HashMap<String, Result<Tex, Status>> = texs
        .par_iter()
        .map(|t| {
            let r = match fs.read(t) {
                Ok(d) => vtf::texture(&d, o.tex_max).map_err(|_| Status::BadVtf),
                Err(_) => Err(Status::MissingVtf),
            };
            (t.clone(), r)
        })
        .collect();
    tm.lap("textures");
    for m in mats.iter().flatten() {
        if let (false, false, Some(b)) = (m.skip, m.water, &m.base)
            && let Err(s) = &decoded[b]
        {
            *stats.counts.entry(Status::Ok).or_default() -= 1;
            *stats.counts.entry(*s).or_default() += 1;
            missing.insert(b.clone());
            stats.bad.push(format!("{}: {}", m.name, b));
        }
    }

    let mut images: Vec<Option<Image>> = Vec::new();
    let mut compressed: Vec<Option<Compressed>> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut ids: HashMap<String, usize> = HashMap::new();
    let mut tex_of: HashMap<usize, usize> = HashMap::new();
    let mut lx_of: HashMap<usize, Vec<Option<usize>>> = HashMap::new();
    let mut decoded = decoded;
    for &tdi in &used {
        let (key, label) = match &mats[tdi] {
            Ok(m) if m.water => (format!("#water {}", m.name), m.name.clone()),
            Ok(m) => (m.base.clone().unwrap_or_default(), m.name.clone()),
            Err(_) => ("#missing".to_string(), "missing".to_string()),
        };
        if let Some(&k) = ids.get(&key) {
            tex_of.insert(tdi, k);
        } else {
            let (im, bc) = match &mats[tdi] {
                Ok(m) if m.water => (solid(m.fog().unwrap_or(WATER_DEFAULT)), None),
                Ok(_) => match decoded.remove(&key) {
                    Some(Ok(Tex::Plain(i))) => (Image { w: i.w as u32, h: i.h as u32, rgba: i.rgba }, None),
                    Some(Ok(Tex::Bc(c))) => (Image { w: c.w as u32, h: c.h as u32, rgba: Vec::new() }, Some(c)),
                    _ => (checker(), None),
                },
                Err(_) => (checker(), None),
            };
            ids.insert(key, images.len());
            tex_of.insert(tdi, images.len());
            images.push(Some(im));
            compressed.push(bc);
            names.push(label);
        }
        let Some(m) = mats[tdi].as_ref().ok().filter(|m| !m.water) else { continue };
        let mut slots = Vec::new();
        for t in layers(m) {
            let Some(t) = t else {
                slots.push(None);
                continue;
            };
            if let Some(&k) = ids.get(&t) {
                slots.push(Some(k));
                continue;
            }
            let (im, bc) = match decoded.remove(&t) {
                Some(Ok(Tex::Plain(i))) => (Image { w: i.w as u32, h: i.h as u32, rgba: i.rgba }, None),
                Some(Ok(Tex::Bc(c))) => (Image { w: c.w as u32, h: c.h as u32, rgba: Vec::new() }, Some(c)),
                _ => {
                    missing.insert(t.clone());
                    slots.push(None);
                    continue;
                }
            };
            ids.insert(t.clone(), images.len());
            slots.push(Some(images.len()));
            images.push(Some(im));
            compressed.push(bc);
            names.push(t);
        }
        if !slots.is_empty() {
            lx_of.insert(tdi, slots);
        }
    }

    let mut index: HashMap<Key, usize> = HashMap::new();
    let mut batches: Vec<Batch> = Vec::new();
    for p in &polys {
        let (tdi, mode, a, tint) = p.key;
        let mt = mats[tdi].as_ref().ok();
        let bi = *index.entry(p.key).or_insert_with(|| {
            let t0 = mt.map_or([1.0; 3], |m| if m.water { [1.0; 3] } else { m.tint });
            let t1 = tint.map(f32::from_bits);
            let slots = lx_of.get(&tdi);
            let slot = |k: usize| slots.and_then(|s| s.get(k).copied().flatten());
            let fp = mt.and_then(|m| m.four_way());
            let uv = |k: usize| fp.as_ref().map_or([1.0, 1.0, 0.0, 0.0], |f| [f.uv[k][0], f.uv[k][1], 0.0, 0.0]);
            let layer2 = slot(0).map(|t| (t, uv(0)));
            let layer3 = fp.as_ref().and_then(|_| slot(1).map(|t| (t, uv(1), false)));
            let four = fp.as_ref().map(|f| FourWay { tex4: slot(2).map(|t| (t, uv(2))), ..f.four });
            batches.push(Batch {
                tex: tex_of[&tdi],
                mode,
                alpha: f32::from_bits(a),
                warp: None,
                tint: std::array::from_fn(|k| t0[k] * t1[k]),
                layer2,
                layer3,
                four,
                tint_mask: None,
                verts: Vec::new(),
                idx: Vec::new(),
                chunks: Vec::new(),
            });
            batches.len() - 1
        });
        let b = &mut batches[bi];
        let base = b.verts.len() as u32;
        b.verts.extend_from_slice(&p.verts);
        if p.idx.is_empty() {
            for i in 1..p.verts.len() as u32 - 1 {
                b.idx.extend([base, base + i, base + i + 1]);
            }
        } else {
            b.idx.extend(p.idx.iter().map(|i| base + i));
        }
    }
    let points = batches.iter().flat_map(|b| b.verts.iter().map(|v| DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64))).collect();
    tm.lap("batches");
    let anim = vec![None; images.len()];
    let faces = polys.len();
    let (atlas, hdr, lightmap) = match lit {
        Some((img, hdr, is_hdr, shrunk)) => {
            let size = (img.w, img.h);
            notes.push(format!(
                "{} lightmaps {}x{}{}, {} faces unlit",
                if is_hdr { "HDR" } else { "LDR" },
                size.0,
                size.1,
                if shrunk { " (reduced to fit)" } else { "" },
                unlit
            ));
            (img, Some(hdr), Some(size))
        }
        None => {
            notes.push("no lightmaps".into());
            (Image { w: 4, h: 4, rgba: vec![255; 64] }, None, None)
        }
    };
    if disp_bad > 0 {
        notes.push(format!("{disp_bad} displacements skipped (bad base face)"));
    }
    if let Some(d) = disps.as_ref().filter(|d| d.multiblend > 0) {
        notes.push(format!("{} multiblend displacements", d.multiblend));
    }
    Built {
        mesh: Mesh {
            batches,
            textures: images.into(),
            anim,
            atlas: atlas.into(),
            faces,
            missing: missing.into_iter().collect(),
            points,
            nudged: 0,
            names,
            hdr,
            casters: Vec::new(),
            caster_idx: Vec::new(),
            point_cell: None,
            compressed,
            face_spans: Vec::new(),
            overlay: None,
        },
        materials: stats,
        faces,
        dropped: total - faces,
        displacements: ndisp,
        brush_ents: draws.len() - 1,
        lightmap,
        notes,
        tex_ids: ids,
    }
}
