use std::collections::{BTreeSet, HashMap};

use glam::{DMat3, DVec3};
use rayon::prelude::*;

use super::entities::Ent;
use super::material::{self, Blend, Fill, Material};
use super::model::{Indices, Model, Stream};
use super::probe::Probes;
use super::texture;
use super::vpk::Fs;
use super::world::{BLOCK_LIGHT, World, Xf};
use crate::mesh::{Batch, HdrLight, Image, Mesh, Mode, Vertex, pack_rgb9e5, unorm16};

const BATCH_BYTES: usize = 240 << 20;
const POINT_CELL: f64 = 32.0;
const TINT_LEVELS: f32 = 255.0;

pub struct Lightmap {
    pub atlas: Image,
    pub rect: [f32; 4],
    pub flat: [f32; 2],
}

pub struct Opts {
    pub tex_max: usize,
    pub crop: Option<[f64; 4]>,
    pub lightmap: Option<Lightmap>,
    pub hdr: Option<HdrLight>,
    pub probes: Option<Probes>,
}

pub struct Built {
    pub mesh: Mesh,
    pub draws: usize,
    pub dropped: usize,
    pub models: usize,
    pub materials: usize,
    pub bad_models: Vec<String>,
    pub probe_draws: usize,
    pub casters: usize,
}

struct Inst {
    model: u32,
    xf: Xf,
    nrm: DMat3,
    flip: bool,
    tint: [f32; 3],
    alpha: f32,
    dcs: Option<u32>,
    skin: String,
    probe: i64,
    caster: bool,
}

struct Entry {
    inst: u32,
    dc: u32,
    tris: usize,
    verts: usize,
    clip: bool,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.verts * std::mem::size_of::<Vertex>() + self.tris * 12
    }
}

struct Ctx<'a> {
    lm_rect: [f32; 4],
    flat: [f32; 2],
    probes: Option<&'a Probes>,
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
        material::Mode::Mod2x => Mode::Mod2x,
        _ if alpha < 0.99 && m.mode != material::Mode::Additive => Mode::Blend,
        material::Mode::Opaque => Mode::Opaque,
        material::Mode::AlphaTest => Mode::AlphaTest,
        material::Mode::Blend => Mode::Blend,
        material::Mode::Additive => Mode::Additive,
    }
}

fn relative(uv: [f32; 4], s1: [f32; 2], o1: [f32; 2]) -> [f32; 4] {
    let a = [uv[0] / s1[0].max(1e-6), uv[1] / s1[1].max(1e-6)];
    [a[0], a[1], uv[2] - o1[0] * a[0], uv[3] - o1[1] * a[1]]
}

pub fn build(fs: &Fs, world: &World, ents: &[Ent], o: Opts) -> Built {
    let mut tm = crate::prof::Timer::new("mesh");
    let mut names = world.models.clone();
    let mut ids: HashMap<String, u32> = names.iter().enumerate().map(|(i, n)| (n.clone(), i as u32)).collect();
    let mut insts: Vec<Inst> = Vec::new();
    let inst = |model: u32, xf: Xf, tint: [f32; 3], alpha: f32, dcs: Option<u32>, skin: String, probe: i64, caster: bool| {
        let nrm = xf.r.inverse().transpose();
        Inst { model, flip: xf.r.determinant() < 0.0, xf, nrm, tint, alpha, dcs, skin, probe, caster }
    };
    for d in &world.draws {
        insts.push(inst(d.model, d.xf, d.tint, 1.0, d.dc, d.skin.clone(), d.probe, d.flags & BLOCK_LIGHT != 0));
    }
    for e in ents.iter().filter(|e| e.render) {
        let Some(v) = &e.vmdl else { continue };
        let n = names.len() as u32;
        let id = *ids.entry(v.clone()).or_insert_with(|| {
            names.push(v.clone());
            n
        });
        let skin = e.kv.get("skin").cloned().unwrap_or_default();
        insts.push(inst(id, e.xf, e.tint, e.alpha, None, skin, -1, false));
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
    tm.lap("models");
    let skins: Vec<Option<usize>> =
        insts.iter().map(|it| models[it.model as usize].as_ref().and_then(|m| m.skin(&it.skin))).collect();

    let mut entries: Vec<Entry> = Vec::new();
    let mut caster_entries: Vec<Entry> = Vec::new();
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
                let e = Entry { inst: ii as u32, dc, tris: c.index_count / 3, verts: 0, clip: false };
                if it.caster { caster_entries.push(e) } else { entries.push(e) }
            }
        }
    }
    let total = entries.len();
    if let Some([x0, y0, x1, y1]) = o.crop {
        let flags: Vec<(bool, bool)> = entries
            .par_iter()
            .map(|e| {
                let it = &insts[e.inst as usize];
                let m = models[it.model as usize].as_ref().unwrap();
                let c = &m.draws[e.dc as usize];
                let (Some(ps), Ok(ix)) = (m.stream(c, "POSITION", 0), m.index_view(c)) else { return (false, false) };
                let mut lo = DVec3::splat(f64::INFINITY);
                let mut hi = DVec3::splat(f64::NEG_INFINITY);
                for k in 0..ix.len() {
                    let p = ps.get(ix.get(k) as usize);
                    let q = it.xf.apply(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
                let meets = hi.x >= x0 && lo.x <= x1 && hi.y >= y0 && lo.y <= y1;
                let inside = lo.x >= x0 && hi.x <= x1 && lo.y >= y0 && hi.y <= y1;
                (meets, !inside)
            })
            .collect();
        for (e, f) in entries.iter_mut().zip(&flags) {
            e.clip = f.1;
        }
        let mut i = 0;
        entries.retain(|_| {
            i += 1;
            flags[i - 1].0
        });
    }
    let dropped = total - entries.len();
    tm.lap("crop");
    let mat_of = |e: &Entry| -> String {
        let it = &insts[e.inst as usize];
        let m = models[it.model as usize].as_ref().unwrap();
        m.material(&m.draws[e.dc as usize], skins[e.inst as usize])
    };
    let mut mat_names: Vec<String> = entries.iter().map(mat_of).collect::<BTreeSet<_>>().into_iter().collect();
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
                (n, Some(Material::solid([255, 0, 255, 255])))
            }
        })
        .collect();
    tm.lap("materials");
    let mut keys: HashMap<(String, [u8; 3], Mode, u32), usize> = HashMap::new();
    let mut groups: Vec<(String, [u8; 3], Mode, f32, Vec<usize>)> = Vec::new();
    for (ei, e) in entries.iter().enumerate() {
        let it = &insts[e.inst as usize];
        let c = &models[it.model as usize].as_ref().unwrap().draws[e.dc as usize];
        let mn = mat_of(e);
        let Some(Some(mat)) = mats.get(&mn) else { continue };
        let t: [f32; 3] = std::array::from_fn(|k| mat.tint[k] * c.tint[k] * it.tint[k]);
        let alpha = mat.alpha * c.alpha * it.alpha;
        let md = mode(mat, alpha);
        let a = if matches!(md, Mode::Blend | Mode::Additive | Mode::Mod2x) { alpha } else { 1.0 };
        let key = (mn.clone(), quant(t), md, a.to_bits());
        let gi = *keys.entry(key).or_insert_with(|| {
            groups.push((mn, quant(t), md, a, Vec::new()));
            groups.len() - 1
        });
        groups[gi].4.push(ei);
    }

    let mut tex_names: Vec<String> = groups
        .iter()
        .flat_map(|g| {
            let m = mats[&g.0].as_ref().unwrap();
            let base = match &m.fill {
                Fill::Texture(t) => Some(t.clone()),
                Fill::Solid(_) => None,
            };
            base.into_iter()
                .chain(m.layer2.as_ref().map(|l| l.tex.clone()))
                .chain(m.layer3.as_ref().map(|l| l.tex.clone()))
                .chain(m.tint_mask.clone())
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
    tm.lap("textures");
    for (t, im) in &decoded {
        if im.is_none() {
            missing.insert(t.clone());
        }
    }

    let mut images: Vec<Option<Image>> = Vec::new();
    let mut tex_ids: HashMap<String, usize> = HashMap::new();
    let mut tex_label: Vec<String> = Vec::new();
    let mut group_tex = Vec::with_capacity(groups.len());
    let mut group_l2 = Vec::with_capacity(groups.len());
    let mut group_l3 = Vec::with_capacity(groups.len());
    let mut group_tm = Vec::with_capacity(groups.len());
    let mut decoded = decoded;
    for (mn, _, _, _, _) in &groups {
        let mat = mats[mn].as_ref().unwrap();
        let label = mn.rsplit('/').next().unwrap_or(mn).trim_end_matches(".vmat").to_string();
        let mut add = |src: String, fill: &Fill| {
            *tex_ids.entry(src).or_insert_with(|| {
                images.push(Some(match fill {
                    Fill::Texture(t) => decoded.remove(t).flatten().unwrap_or_else(checker),
                    Fill::Solid(c) => Image { w: 4, h: 4, rgba: c.repeat(16) },
                }));
                tex_label.push(label.clone());
                images.len() - 1
            })
        };
        let src = match &mat.fill {
            Fill::Texture(t) => t.clone(),
            Fill::Solid(c) => format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]),
        };
        group_tex.push(add(src, &mat.fill));
        group_l2.push(mat.layer2.as_ref().map(|l| (add(l.tex.clone(), &Fill::Texture(l.tex.clone())), l.uv)));
        group_l3.push(mat.layer3.as_ref().map(|l| {
            let idx = add(l.tex.clone(), &Fill::Texture(l.tex.clone()));
            match &mat.layer2 {
                Some(l2) if l.set == l2.set || l.set != mat.uv_set => (idx, l.uv, true),
                _ => (idx, relative(l.uv, mat.uv_scale, mat.uv_offset), false),
            }
        }));
        group_tm.push(mat.tint_mask.as_ref().map(|t| add(t.clone(), &Fill::Texture(t.clone()))));
    }
    let (atlas, lm_rect, flat) = match o.lightmap {
        Some(l) => (l.atlas, l.rect, l.flat),
        None => (white(), [0.0; 4], [0.5, 0.5]),
    };
    let ctx = Ctx { lm_rect, flat, probes: o.probes.as_ref() };
    let to_clip: Vec<(usize, usize)> =
        groups.iter().enumerate().flat_map(|(gi, g)| g.4.iter().filter(|&&ei| entries[ei].clip).map(move |&ei| (gi, ei))).collect();
    let size = |list: &mut Vec<Entry>| {
        let counts: Vec<(usize, usize)> = list
            .par_iter()
            .map(|e| {
                let it = &insts[e.inst as usize];
                let m = models[it.model as usize].as_ref().unwrap();
                let c = &m.draws[e.dc as usize];
                match (m.stream(c, "POSITION", 0), m.index_view(c)) {
                    (Some(_), Ok(ix)) => (unique(&ix), ix.len() / 3),
                    _ => (0, 0),
                }
            })
            .collect();
        for (e, (v, t)) in list.iter_mut().zip(counts) {
            (e.verts, e.tris) = (v, t);
        }
    };
    size(&mut entries);
    size(&mut caster_entries);
    let clipped: HashMap<usize, Vec<Vertex>> = to_clip
        .par_iter()
        .map(|&(gi, ei)| {
            let mat = mats[&groups[gi].0].as_ref().unwrap();
            let e = &entries[ei];
            let it = &insts[e.inst as usize];
            let m = models[it.model as usize].as_ref().unwrap();
            let mut v = vec![Vertex::BLANK; e.verts];
            let mut ix = vec![0u32; e.tris * 3];
            fill(m, e.dc as usize, it, mat, &ctx, &mut v, &mut ix);
            let soup: Vec<Vertex> = ix.iter().map(|&i| v[i as usize]).collect();
            (ei, clip_rect(&soup, o.crop.unwrap()))
        })
        .collect();
    for (&ei, v) in &clipped {
        entries[ei].tris = v.len() / 3;
        entries[ei].verts = v.len();
    }
    crate::prof::note(|| {
        let (v, t) = entries.iter().fold((0, 0), |(v, t), e| (v + e.verts, t + e.tris));
        format!("{v} vertices for {t} triangles ({:.3} per triangle)", v as f64 / t.max(1) as f64)
    });
    tm.lap("clip");
    let probe_draws = if ctx.probes.is_some() {
        entries
            .iter()
            .filter(|e| {
                let it = &insts[e.inst as usize];
                let m = models[it.model as usize].as_ref().unwrap();
                !m.draws[e.dc as usize].lightmapped
            })
            .count()
    } else {
        0
    };
    let mut batches: Vec<Batch> = Vec::new();
    let mut faces = 0;
    for (gi, (mn, tq, md, alpha, list)) in groups.iter().enumerate() {
        let mat = mats[mn].as_ref().unwrap();
        let mut start = 0;
        while start < list.len() {
            let mut end = start;
            let mut n = 0;
            while end < list.len() && (end == start || n + entries[list[end]].bytes() <= BATCH_BYTES) {
                n += entries[list[end]].bytes();
                end += 1;
            }
            let part = &list[start..end];
            let (verts, idx) = gather(part, &entries, |ei, out, ix| {
                let e = &entries[ei];
                let it = &insts[e.inst as usize];
                let m = models[it.model as usize].as_ref().unwrap();
                match clipped.get(&ei) {
                    Some(v) => {
                        out.copy_from_slice(v);
                        ix.iter_mut().enumerate().for_each(|(k, x)| *x = k as u32);
                    }
                    None => fill(m, e.dc as usize, it, mat, &ctx, out, ix),
                }
            });
            faces += idx.len() / 3;
            batches.push(Batch {
                tex: group_tex[gi],
                mode: *md,
                alpha: *alpha,
                warp: None,
                tint: tq.map(|v| v as f32 / TINT_LEVELS),
                layer2: group_l2[gi],
                layer3: group_l3[gi],
                tint_mask: group_tm[gi],
                verts,
                idx,
            });
            start = end;
        }
    }
    tm.lap("fill");
    let plain = Material::solid([255; 4]);
    let all: Vec<usize> = (0..caster_entries.len()).collect();
    let nocast = Ctx { lm_rect: [0.0; 4], flat, probes: None };
    let (casters, caster_idx) = gather(&all, &caster_entries, |ei, out, ix| {
        let e = &caster_entries[ei];
        let it = &insts[e.inst as usize];
        fill(models[it.model as usize].as_ref().unwrap(), e.dc as usize, it, &plain, &nocast, out, ix)
    });

    tm.lap("casters");
    let points = sample_points(&batches);
    tm.lap("points");
    crate::prof::note(|| format!("{} sample points", points.len()));
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
            hdr: o.hdr,
            casters,
            caster_idx,
        },
        draws: entries.len(),
        dropped,
        models: names.len(),
        materials: mats.len(),
        bad_models,
        probe_draws,
        casters: caster_entries.len(),
    }
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

fn span(ix: &Indices) -> (u32, u32) {
    (0..ix.len()).fold((u32::MAX, 0), |(lo, hi), k| {
        let g = ix.get(k);
        (lo.min(g), hi.max(g))
    })
}

fn unique(ix: &Indices) -> usize {
    if ix.len() == 0 {
        return 0;
    }
    let (lo, hi) = span(ix);
    let mut seen = vec![false; (hi - lo + 1) as usize];
    let mut n = 0;
    for k in 0..ix.len() {
        let s = &mut seen[(ix.get(k) - lo) as usize];
        n += !*s as usize;
        *s = true;
    }
    n
}

fn remap(ix: &Indices, flip: bool) -> (Vec<u32>, Vec<u32>) {
    if ix.len() == 0 {
        return (Vec::new(), Vec::new());
    }
    let (lo, hi) = span(ix);
    let mut map = vec![u32::MAX; (hi - lo + 1) as usize];
    let mut ids = Vec::new();
    let mut idx = Vec::with_capacity(ix.len());
    let order = if flip { [0, 2, 1] } else { [0, 1, 2] };
    for t in 0..ix.len() / 3 {
        for o in order {
            let g = ix.get(t * 3 + o);
            let s = &mut map[(g - lo) as usize];
            if *s == u32::MAX {
                *s = ids.len() as u32;
                ids.push(g);
            }
            idx.push(*s);
        }
    }
    (ids, idx)
}

fn uv_stream<'a>(m: &'a Model, c: &super::model::DrawCall, set: u32) -> Option<Stream<'a>> {
    (set > 0).then(|| m.stream(c, "TEXCOORD", set)).flatten().or_else(|| m.stream(c, "TEXCOORD", 0))
}

fn fill(m: &Model, dc: usize, it: &Inst, mat: &Material, ctx: &Ctx, out: &mut [Vertex], out_idx: &mut [u32]) {
    let c = &m.draws[dc];
    let (Some(ps), Ok(ix)) = (m.stream(c, "POSITION", 0), m.index_view(c)) else { return };
    let (ids, idx) = remap(&ix, it.flip);
    if ids.len() != out.len() || idx.len() != out_idx.len() {
        return;
    }
    out_idx.copy_from_slice(&idx);
    let uvs = uv_stream(m, c, mat.uv_set);
    let uvs2 = mat.layer2.as_ref().and_then(|l| uv_stream(m, c, l.set));
    let lms = if c.lightmapped && ctx.lm_rect[2] > 0.0 { m.map_stream(c, "TEXCOORD", 3) } else { None };
    let probes = if lms.is_none() { ctx.probes } else { None };
    let ns = m.stream(c, "NORMAL", 0);
    let blend = mat.layer2.as_ref().and_then(|_| m.stream(c, "TEXCOORD", 4));
    let paint = if mat.paint { m.stream(c, "COLOR", 0) } else { None };
    let fade = matches!(mat.mode, material::Mode::Blend | material::Mode::AlphaTest | material::Mode::Mod2x);
    let bias = if mat.decal { 1.0 } else { 0.0 };
    let weight = |v: f32| match mat.blend {
        Blend::Painted => unorm16(v),
        Blend::Height => unorm16(v * 1.1 - 0.05),
    };
    let p: Vec<DVec3> = ids
        .iter()
        .map(|&i| {
            let v = ps.get(i as usize);
            it.xf.apply(DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64))
        })
        .collect();
    let faces = ns.is_none().then(|| {
        let mut acc = vec![DVec3::ZERO; p.len()];
        for t in idx.chunks_exact(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            let n = (p[b] - p[a]).cross(p[c] - p[a]);
            for k in [a, b, c] {
                acc[k] += n;
            }
        }
        acc.into_iter().map(|n| n.normalize_or_zero()).collect::<Vec<_>>()
    });
    for (k, &id) in ids.iter().enumerate() {
        let i = id as usize;
        let uv = uvs.map_or([0.0; 4], |s| s.get(i));
        let lm = match lms {
            Some(s) => {
                let l = s.get(i);
                [ctx.lm_rect[0] + l[0] * ctx.lm_rect[2], ctx.lm_rect[1] + l[1] * ctx.lm_rect[3]]
            }
            None => ctx.flat,
        };
        let n = match (ns.and_then(|s| s.normal(i)), &faces) {
            (Some(n), _) => (it.nrm * DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64)).normalize_or_zero(),
            (None, Some(f)) => f[k],
            (None, None) => DVec3::ZERO,
        };
        let b = blend.map_or([0.0; 4], |s| s.get(i));
        let col = paint.map(|s| s.get(i));
        let mut v = Vertex {
            pos: [p[k].x as f32, p[k].y as f32, p[k].z as f32],
            uv: [uv[0] * mat.uv_scale[0] + mat.uv_offset[0], uv[1] * mat.uv_scale[1] + mat.uv_offset[1]],
            lm,
            bias,
            normal: [n.x as f32, n.y as f32, n.z as f32],
            uv2: uvs2.map_or([0.0; 2], |s| {
                let q = s.get(i);
                [q[0], q[1]]
            }),
            blend: [if blend.is_some() { weight(b[0]) } else { 0 }, if mat.layer3.is_some() { weight(b[1]) } else { 0 }],
            ..Vertex::BLANK
        };
        if let Some(cv) = col {
            let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            v.paint = [q(cv[0]), q(cv[1]), q(cv[2]), if fade { q(cv[3]) } else { 255 }];
        }
        if let Some(s) = probes.and_then(|pr| pr.sample(p[k], n, it.probe)) {
            v.light = pack_rgb9e5(s.light).max(1);
            v.mask = s.mask;
        }
        out[k] = v;
    }
}

fn clip_rect(v: &[Vertex], [x0, y0, x1, y1]: [f64; 4]) -> Vec<Vertex> {
    let planes: [(usize, f32, f32); 4] = [(0, 1.0, x0 as f32), (0, -1.0, -x1 as f32), (1, 1.0, y0 as f32), (1, -1.0, -y1 as f32)];
    let mut out = Vec::new();
    for tri in v.chunks_exact(3) {
        let mut poly: Vec<Vertex> = tri.to_vec();
        for &(axis, s, d) in &planes {
            let side = |p: &Vertex| s * p.pos[axis] - d;
            let input = std::mem::take(&mut poly);
            for i in 0..input.len() {
                let (a, b) = (&input[i], &input[(i + 1) % input.len()]);
                let (sa, sb) = (side(a), side(b));
                if sa >= 0.0 {
                    poly.push(*a);
                }
                if (sa >= 0.0) != (sb >= 0.0) {
                    poly.push(Vertex::lerp(a, b, sa / (sa - sb)));
                }
            }
            if poly.len() < 3 {
                break;
            }
        }
        for i in 1..poly.len().saturating_sub(1) {
            out.extend([poly[0], poly[i], poly[i + 1]]);
        }
    }
    out
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
