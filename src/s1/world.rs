use std::collections::{BTreeSet, HashMap};

use glam::{DMat3, DVec3};
use rayon::prelude::*;

use super::bsp::{SURF_HIDDEN, World};
use super::vmt::{self, Material};
use super::vtf;
use crate::bsp::Entity;
use crate::mesh::{Batch, Image, Mesh, Mode, Vertex};
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

pub struct Opts {
    pub tex_max: usize,
    pub crop: Option<[f64; 4]>,
}

pub struct Built {
    pub mesh: Mesh,
    pub materials: MatStats,
    pub faces: usize,
    pub dropped: usize,
    pub displacements: usize,
    pub brush_ents: usize,
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

struct Poly {
    key: (usize, Mode, u32, [u32; 3]),
    pts: Vec<(DVec3, [f64; 2])>,
    normal: DVec3,
}

fn clip(mut poly: Vec<(DVec3, [f64; 2])>, [x0, y0, x1, y1]: [f64; 4]) -> Vec<(DVec3, [f64; 2])> {
    for (axis, s, d) in [(0, 1.0, x0), (0, -1.0, -x1), (1, 1.0, y0), (1, -1.0, -y1)] {
        let side = |p: &DVec3| s * p[axis] - d;
        let input = std::mem::take(&mut poly);
        for i in 0..input.len() {
            let (a, b) = (&input[i], &input[(i + 1) % input.len()]);
            let (sa, sb) = (side(&a.0), side(&b.0));
            if sa >= 0.0 {
                poly.push(*a);
            }
            if (sa >= 0.0) != (sb >= 0.0) {
                let t = sa / (sa - sb);
                poly.push((a.0 + (b.0 - a.0) * t, [a.1[0] + (b.1[0] - a.1[0]) * t, a.1[1] + (b.1[1] - a.1[1]) * t]));
            }
        }
        if poly.len() < 3 {
            return Vec::new();
        }
    }
    poly
}

pub fn build(fs: &Fs, w: &World, ents: &[Entity], o: Opts) -> Built {
    let mut tm = crate::prof::Timer::new("source");
    let mats = materials(fs, w);
    tm.lap("materials");
    let draws = draws(w, ents);
    let mut used: BTreeSet<usize> = BTreeSet::new();
    let mut polys: Vec<Poly> = Vec::new();
    let (mut total, mut disps) = (0, 0);
    for d in &draws {
        let m = &w.models[d.model];
        for fi in m.first_face as usize..(m.first_face + m.num_faces) as usize {
            let f = &w.faces[fi];
            if f.dispinfo >= 0 {
                disps += 1;
                continue;
            }
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
            let (tw, th) = (td.width.max(1) as f64, td.height.max(1) as f64);
            let sv = DVec3::new(ti.s[0] as f64, ti.s[1] as f64, ti.s[2] as f64);
            let tv = DVec3::new(ti.t[0] as f64, ti.t[1] as f64, ti.t[2] as f64);
            let mut pts: Vec<(DVec3, [f64; 2])> = w
                .face_points(fi)
                .into_iter()
                .map(|p| (d.apply(p), [(p.dot(sv) + ti.s[3] as f64) / tw, (p.dot(tv) + ti.t[3] as f64) / th]))
                .collect();
            let normal = (d.rot * w.face_normal(fi)).normalize_or_zero();
            let n = pts.len();
            let c = pts.iter().map(|p| p.0).sum::<DVec3>() / n as f64;
            let newell: DVec3 = (0..n).map(|i| (pts[i].0 - c).cross(pts[(i + 1) % n].0 - c)).sum();
            if newell.dot(normal) < 0.0 {
                pts.reverse();
            }
            if let Some(b) = o.crop {
                pts = clip(pts, b);
                if pts.is_empty() {
                    continue;
                }
            }
            used.insert(tdi);
            polys.push(Poly { key: (tdi, mode, alpha.to_bits(), d.tint.map(f32::to_bits)), pts, normal });
        }
    }
    tm.lap("faces");

    let mut stats = MatStats::default();
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut texs: Vec<String> = Vec::new();
    for (i, m) in mats.iter().enumerate() {
        let st = match m {
            Ok(m) if m.skip || m.water => Status::ToolWater,
            Ok(m) => match &m.base {
                None => Status::NoBase,
                Some(b) => {
                    texs.push(b.clone());
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
    let mut decoded = decoded;
    for &tdi in &used {
        let (key, label) = match &mats[tdi] {
            Ok(m) if m.water => (format!("#water {}", m.name), m.name.clone()),
            Ok(m) => (m.base.clone().unwrap_or_default(), m.name.clone()),
            Err(_) => ("#missing".to_string(), "missing".to_string()),
        };
        if let Some(&k) = ids.get(&key) {
            tex_of.insert(tdi, k);
            continue;
        }
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

    let mut index: HashMap<(usize, Mode, u32, [u32; 3]), usize> = HashMap::new();
    let mut batches: Vec<Batch> = Vec::new();
    for p in &polys {
        let (tdi, mode, a, tint) = p.key;
        let mt = mats[tdi].as_ref().ok();
        let bi = *index.entry(p.key).or_insert_with(|| {
            let t0 = mt.map_or([1.0; 3], |m| if m.water { [1.0; 3] } else { m.tint });
            let t1 = tint.map(f32::from_bits);
            batches.push(Batch {
                tex: tex_of[&tdi],
                mode,
                alpha: f32::from_bits(a),
                warp: None,
                tint: std::array::from_fn(|k| t0[k] * t1[k]),
                layer2: None,
                layer3: None,
                tint_mask: None,
                verts: Vec::new(),
                idx: Vec::new(),
                chunks: Vec::new(),
            });
            batches.len() - 1
        });
        let b = &mut batches[bi];
        let base = b.verts.len() as u32;
        b.verts.extend(p.pts.iter().map(|(q, uv)| Vertex {
            pos: [q.x as f32, q.y as f32, q.z as f32],
            uv: [uv[0] as f32, uv[1] as f32],
            lm: [0.5, 0.5],
            normal: [p.normal.x as f32, p.normal.y as f32, p.normal.z as f32],
            ..Vertex::BLANK
        }));
        for i in 1..p.pts.len() as u32 - 1 {
            b.idx.extend([base, base + i, base + i + 1]);
        }
    }
    let points = batches.iter().flat_map(|b| b.verts.iter().map(|v| DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64))).collect();
    tm.lap("batches");
    let anim = vec![None; images.len()];
    let faces = polys.len();
    Built {
        mesh: Mesh {
            batches,
            textures: images,
            anim,
            atlas: Image { w: 4, h: 4, rgba: vec![255; 64] },
            faces,
            missing: missing.into_iter().collect(),
            points,
            nudged: 0,
            names,
            hdr: None,
            casters: Vec::new(),
            caster_idx: Vec::new(),
            point_cell: None,
            compressed,
        },
        materials: stats,
        faces,
        dropped: total - faces,
        displacements: disps,
        brush_ents: draws.len() - 1,
    }
}
