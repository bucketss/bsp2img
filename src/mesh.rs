use std::collections::{BTreeSet, HashMap};

use anyhow::{Result, bail};
use bytemuck::{Pod, Zeroable};
use glam::{DVec2, DVec3};
use rayon::prelude::*;

use crate::bsp::{Bsp, TEX_SPECIAL};
use crate::light::LightParams;
use crate::reach::Reach;
use crate::wad::TextureSource;

pub const SKIP_TEXTURES: &[&str] = &[
    "sky", "aaatrigger", "clip", "origin", "null", "skip", "hint", "bevel", "solidhint", "bevelhint", "noclip",
    "clipbevel", "clipbevelbrush", "nodraw", "enemyclip", "botclip",
];

pub const SKIP_CLASSES: &[&str] = &[
    "func_buyzone", "func_bomb_target", "func_hostage_rescue", "func_escapezone", "func_vip_safetyzone",
    "func_ladder", "func_friction", "func_monsterclip", "func_mortar_field", "func_weaponcheck", "env_bubbles",
    "func_traincontrols", "func_clip", "func_detail_clip", "info_bomb_target", "info_hostage_rescue",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Opaque = 0,
    AlphaTest = 1,
    Blend = 2,
    Additive = 3,
    Mod2x = 4,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub lm: [f32; 2],
    pub bias: f32,
    pub normal: [f32; 3],
    pub uv2: [f32; 2],
    pub blend: [u16; 4],
    pub paint: [u8; 4],
    pub light: u32,
    pub mask: [u8; 4],
}

impl Vertex {
    pub const BLANK: Vertex = Vertex {
        pos: [0.0; 3],
        uv: [0.0; 2],
        lm: [0.0; 2],
        bias: 0.0,
        normal: [0.0; 3],
        uv2: [0.0; 2],
        blend: [0; 4],
        paint: [255; 4],
        light: 0,
        mask: [0; 4],
    };

    pub fn lerp(a: &Vertex, b: &Vertex, t: f32) -> Vertex {
        let l2 = |p: [f32; 2], q: [f32; 2]| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
        let l3 = |p: [f32; 3], q: [f32; 3]| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t, p[2] + (q[2] - p[2]) * t];
        let lb = |p: [u8; 4], q: [u8; 4]| std::array::from_fn(|k| (p[k] as f32 + (q[k] as f32 - p[k] as f32) * t).round() as u8);
        let mut n = l3(a.normal, b.normal);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 1e-12 {
            n = n.map(|c| c / len);
        }
        Vertex {
            pos: l3(a.pos, b.pos),
            uv: l2(a.uv, b.uv),
            lm: l2(a.lm, b.lm),
            bias: a.bias + (b.bias - a.bias) * t,
            normal: n,
            uv2: l2(a.uv2, b.uv2),
            blend: std::array::from_fn(|k| (a.blend[k] as f32 + (b.blend[k] as f32 - a.blend[k] as f32) * t).round() as u16),
            paint: lb(a.paint, b.paint),
            light: if a.light == b.light { a.light } else { pack_rgb9e5(l3(unpack_rgb9e5(a.light), unpack_rgb9e5(b.light))) },
            mask: lb(a.mask, b.mask),
        }
    }
}

pub fn unorm16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0).round() as u16
}

pub fn pack_rgb9e5(c: [f32; 3]) -> u32 {
    const MAXV: f32 = 65408.0;
    let c = c.map(|v| if v.is_finite() { v.clamp(0.0, MAXV) } else { 0.0 });
    let m = c[0].max(c[1]).max(c[2]);
    if m <= 0.0 {
        return 0;
    }
    let mut e = (m.log2().floor() as i32).max(-16) + 1 + 15;
    let mut d = 2f32.powi(e - 15 - 9);
    if (m / d).round() >= 512.0 {
        e += 1;
        d *= 2.0;
    }
    let e = e.clamp(0, 31) as u32;
    let q = c.map(|v| ((v / d).round() as u32).min(511));
    q[0] | (q[1] << 9) | (q[2] << 18) | (e << 27)
}

pub fn unpack_rgb9e5(v: u32) -> [f32; 3] {
    let d = 2f32.powi((v >> 27) as i32 - 15 - 9);
    [(v & 511) as f32 * d, ((v >> 9) & 511) as f32 * d, ((v >> 18) & 511) as f32 * d]
}

#[derive(Clone, Copy, Debug)]
pub struct FourWay {
    pub tex4: Option<(usize, [f32; 4])>,
    pub lum: [f32; 8],
    pub blend: [f32; 6],
    pub lumblend: [f32; 3],
    pub mul3: bool,
    pub mul4: bool,
}

pub struct Batch {
    pub tex: usize,
    pub mode: Mode,
    pub alpha: f32,
    pub warp: Option<[f32; 4]>,
    pub tint: [f32; 3],
    pub layer2: Option<(usize, [f32; 4])>,
    pub layer3: Option<(usize, [f32; 4], bool)>,
    pub four: Option<FourWay>,
    pub tint_mask: Option<usize>,
    pub verts: Vec<Vertex>,
    pub idx: Vec<u32>,
    pub chunks: Vec<Chunk>,
}

#[derive(Clone, Copy, Debug)]
pub struct Chunk {
    pub first: u32,
    pub count: u32,
    pub lo: [f32; 3],
    pub hi: [f32; 3],
}

impl Chunk {
    pub fn new(verts: &[Vertex], first: usize, count: usize) -> Chunk {
        let (lo, hi) = verts.iter().fold(([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]), |(lo, hi), v| {
            (std::array::from_fn(|k| lo[k].min(v.pos[k])), std::array::from_fn(|k| hi[k].max(v.pos[k])))
        });
        Chunk { first: first as u32, count: count as u32, lo, hi }
    }
}

impl Batch {
    pub fn tris(&self) -> impl Iterator<Item = [&Vertex; 3]> + '_ {
        tris(&self.verts, &self.idx)
    }
}

pub fn tris<'a>(verts: &'a [Vertex], idx: &'a [u32]) -> impl Iterator<Item = [&'a Vertex; 3]> + 'a {
    idx.chunks_exact(3).map(move |t| [&verts[t[0] as usize], &verts[t[1] as usize], &verts[t[2] as usize]])
}

#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub struct Mesh {
    pub batches: Vec<Batch>,
    pub textures: Vec<Option<Image>>,
    pub anim: Vec<Option<Vec<usize>>>,
    pub atlas: Image,
    pub faces: usize,
    pub missing: Vec<String>,
    pub points: std::sync::Arc<[DVec3]>,
    pub nudged: usize,
    pub names: Vec<String>,
    pub hdr: Option<HdrLight>,
    pub casters: Vec<Vertex>,
    pub caster_idx: Vec<u32>,
    pub point_cell: Option<f64>,
    pub compressed: Vec<Option<crate::s2::texture::Compressed>>,
}

impl Mesh {
    pub fn texture(&self, i: usize) -> Option<std::borrow::Cow<'_, Image>> {
        let t = self.textures.get(i)?.as_ref()?;
        match self.compressed.get(i).and_then(|c| c.as_ref()) {
            Some(c) => c.decode().ok().map(|d| std::borrow::Cow::Owned(Image { w: d.w as u32, h: d.h as u32, rgba: d.rgba })),
            None => Some(std::borrow::Cow::Borrowed(t)),
        }
    }
}

pub struct PointLight {
    pub pos: [f32; 3],
    pub range: f32,
    pub color: [f32; 3],
    pub channel: u32,
    pub dir: [f32; 3],
    pub kind: u32,
    pub cone: [f32; 2],
}

pub struct HdrLight {
    pub w: u32,
    pub h: u32,
    pub irradiance: Vec<u32>,
    pub shadows: Option<Image>,
    pub sun: [f32; 3],
    pub lights: Vec<PointLight>,
    pub curve: [f32; 3],
}

pub fn skip_class(cls: &str) -> bool {
    cls.starts_with("trigger_") || SKIP_CLASSES.contains(&cls)
}

fn parse_f(s: Option<&str>, d: f64) -> f64 {
    s.and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(d)
}

struct Atlas {
    blocks: Vec<(u32, u32, Vec<u8>)>,
    pos: Vec<(u32, u32)>,
}

impl Atlas {
    fn add(&mut self, w: u32, h: u32, rgb: Vec<u8>) -> usize {
        self.blocks.push((w, h, rgb));
        self.blocks.len() - 1
    }

    fn pack(&mut self, max_dim: u32) -> Result<Image> {
        let mut order: Vec<usize> = (0..self.blocks.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.blocks[i].1));
        let widest = self.blocks.iter().map(|b| b.0 + 2).max().unwrap_or(0);
        let mut width = widest.next_power_of_two().max(2048);
        loop {
            let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
            let mut pos = vec![(0, 0); self.blocks.len()];
            for &i in &order {
                let (w, h, _) = self.blocks[i];
                if x + w + 2 > width {
                    x = 0;
                    y += shelf;
                    shelf = 0;
                }
                pos[i] = (x + 1, y + 1);
                x += w + 2;
                shelf = shelf.max(h + 2);
            }
            let height = (y + shelf).max(16).next_power_of_two();
            if height > max_dim && width < max_dim {
                width *= 2;
                continue;
            }
            if width > max_dim || height > max_dim {
                bail!("lightmap atlas needs {width}x{height}, over the GPU limit of {max_dim}");
            }
            let mut img = vec![0u8; (width * height * 4) as usize];
            for (i, &(px, py)) in pos.iter().enumerate() {
                let (w, h, ref b) = self.blocks[i];
                for yy in 0..h + 2 {
                    let sy = yy.saturating_sub(1).min(h - 1);
                    for xx in 0..w + 2 {
                        let sx = xx.saturating_sub(1).min(w - 1);
                        let s = ((sy * w + sx) * 3) as usize;
                        let d = (((py - 1 + yy) * width + px - 1 + xx) * 4) as usize;
                        img[d..d + 3].copy_from_slice(&b[s..s + 3]);
                        img[d + 3] = 255;
                    }
                }
            }
            self.pos = pos;
            return Ok(Image { w: width, h: height, rgba: img });
        }
    }
}

fn checker(w: u32, h: u32) -> Image {
    let (w, h) = (w.max(16), h.max(16));
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = ((x / 8 + y / 8) % 2 == 1) && x < w / 8 * 8 && y < h / 8 * 8;
            rgba[i..i + 4].copy_from_slice(if c { &[255, 0, 255, 255] } else { &[0, 0, 0, 255] });
        }
    }
    Image { w, h, rgba }
}

#[derive(Clone, Copy)]
struct LmMap {
    ax: DVec3,
    ay: DVec3,
    c: [f64; 2],
    sv: DVec3,
    tv: DVec3,
}

struct Record {
    tex: usize,
    mode: Mode,
    alpha: f32,
    p: Vec<DVec3>,
    u: Vec<f64>,
    v: Vec<f64>,
    block: usize,
    lm: Option<(Vec<f64>, Vec<f64>)>,
    lmap: Option<LmMap>,
    lstyles: [u8; 4],
    normal: DVec3,
    model: usize,
    warp: Option<[f32; 4]>,
}

fn apply_lut(im: &mut Image, lut: &[u8]) {
    for px in im.rgba.chunks_exact_mut(4) {
        for c in &mut px[..3] {
            *c = lut[*c as usize];
        }
    }
}

fn anim_sequences(bsp: &Bsp, textures: &mut TextureSource, images: &mut Vec<Option<Image>>, lut: &[u8]) -> Vec<Option<Vec<usize>>> {
    let lower: Vec<String> = bsp.miptex.iter().map(|m| m.name.to_lowercase()).collect();
    let mut seqs: HashMap<String, Vec<usize>> = HashMap::new();
    let mut anim = vec![None; images.len()];
    for (i, n) in lower.iter().enumerate() {
        let b = n.as_bytes();
        if b.len() < 3 || b[0] != b'+' || !b[1].is_ascii_digit() {
            continue;
        }
        let rest = n[2..].to_string();
        if !seqs.contains_key(&rest) {
            let mut frames = Vec::new();
            for d in 0..10 {
                let name = format!("+{d}{rest}");
                let found = lower.iter().position(|x| *x == name).filter(|&k| images[k].is_some());
                let k = match found {
                    Some(k) => k,
                    None => match textures.find(&name) {
                        Some((w, h, rgba)) => {
                            let mut im = Image { w, h, rgba };
                            apply_lut(&mut im, lut);
                            images.push(Some(im));
                            images.len() - 1
                        }
                        None => break,
                    },
                };
                frames.push(k);
            }
            seqs.insert(rest.clone(), frames);
        }
        let seq = &seqs[&rest];
        if seq.len() > 1 {
            anim[i] = Some(seq.clone());
        }
    }
    anim.resize(images.len(), None);
    anim
}

pub fn build_mesh(
    bsp: &Bsp,
    textures: &mut TextureSource,
    light: &LightParams,
    reach: Option<&Reach>,
    max_dim: u32,
) -> Result<Mesh> {
    let lut = light.tex_lut();
    let mut missing = BTreeSet::new();
    let mut tex_images: Vec<Option<Image>> = Vec::with_capacity(bsp.miptex.len());
    for mt in &bsp.miptex {
        let mut img = mt.rgba.as_ref().map(|r| Image { w: mt.width, h: mt.height, rgba: r.clone() });
        if img.is_none() && !mt.name.is_empty() {
            img = Some(match textures.find(&mt.name) {
                Some((w, h, rgba)) => Image { w, h, rgba },
                None => {
                    missing.insert(mt.name.clone());
                    checker(mt.width, mt.height)
                }
            });
        }
        if let Some(im) = img.as_mut() {
            apply_lut(im, &lut);
        }
        tex_images.push(img);
    }
    let anim = anim_sequences(bsp, textures, &mut tex_images, &lut);

    let mut draw: Vec<(usize, DVec3, Mode, f32)> = vec![(0, DVec3::ZERO, Mode::Opaque, 1.0)];
    for ent in bsp.entities.iter().skip(1) {
        let Some(mi) = ent.model() else { continue };
        if skip_class(ent.class()) || mi == 0 || mi >= bsp.models.len() {
            continue;
        }
        let rmode = parse_f(ent.get("rendermode"), 0.0) as i32;
        let amt = parse_f(ent.get("renderamt"), 255.0) / 255.0;
        if matches!(rmode, 1 | 2 | 5) && amt <= 0.01 {
            continue;
        }
        let origin = ent.origin().unwrap_or(DVec3::ZERO);
        if let Some(r) = reach {
            let m = &bsp.models[mi];
            if !r.keep_point(bsp, (m.mins + m.maxs) / 2.0 + origin) {
                continue;
            }
        }
        let mode = match rmode {
            0 => Mode::Opaque,
            4 => Mode::AlphaTest,
            5 => Mode::Additive,
            _ => Mode::Blend,
        };
        let alpha = if matches!(mode, Mode::Blend | Mode::Additive) { amt as f32 } else { 1.0 };
        draw.push((mi, origin, mode, alpha));
    }

    let mut atlas = Atlas { blocks: Vec::new(), pos: Vec::new() };
    let white = atlas.add(1, 1, vec![255, 255, 255]);
    let mut records = Vec::new();
    let lighting = &bsp.lighting;
    for &(mi, origin, mode, alpha) in &draw {
        let m = &bsp.models[mi];
        for fi in m.firstface as usize..(m.firstface + m.numfaces) as usize {
            let f = &bsp.faces[fi];
            if f.numedges < 3 {
                continue;
            }
            if mi == 0 && reach.is_some_and(|r| !r.keep_face(fi)) {
                continue;
            }
            let ti = &bsp.texinfo[f.texinfo as usize];
            if ti.miptex < 0 || ti.miptex as usize >= bsp.miptex.len() {
                continue;
            }
            let mt_i = ti.miptex as usize;
            let mt = &bsp.miptex[mt_i];
            let lname = mt.name.to_lowercase();
            if SKIP_TEXTURES.contains(&lname.as_str()) || lname.starts_with("sky") {
                continue;
            }
            let pts = bsp.face_points(fi);
            let sv = DVec3::new(ti.s[0] as f64, ti.s[1] as f64, ti.s[2] as f64);
            let tv = DVec3::new(ti.t[0] as f64, ti.t[1] as f64, ti.t[2] as f64);
            let mut s: Vec<f64> = pts.iter().map(|p| p.dot(sv) + ti.s[3] as f64).collect();
            let mut t: Vec<f64> = pts.iter().map(|p| p.dot(tv) + ti.t[3] as f64).collect();
            let mut fmode = mode;
            if fmode == Mode::Opaque && mt.name.starts_with('{') {
                fmode = Mode::AlphaTest;
            }
            let special = ti.flags & TEX_SPECIAL != 0 || mt.name.starts_with('!');
            let mut block = white;
            let mut lm = None;
            let mut lmap = None;
            if !special && f.lightofs >= 0 && mode != Mode::Additive && !lighting.is_empty() {
                let ext = |v: [f32; 4]| {
                    pts.iter()
                        .map(|p| (p.x as f32 * v[0] + p.y as f32 * v[1] + p.z as f32 * v[2] + v[3]) as f64)
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)))
                };
                let (smin, smax) = ext(ti.s);
                let (tmin, tmax) = ext(ti.t);
                let bmin = [(smin / 16.0).floor(), (tmin / 16.0).floor()];
                let bmax = [(smax / 16.0).ceil(), (tmax / 16.0).ceil()];
                let lw = (bmax[0] - bmin[0] + 1.0) as usize;
                let lh = (bmax[1] - bmin[1] + 1.0) as usize;
                if lw > 0 && lh > 0 && lw * lh < 1 << 20 {
                    let size = lw * lh * 3;
                    let mut acc = vec![0f64; size];
                    for (k, &st) in f.styles.iter().filter(|&&st| st != 255).enumerate() {
                        let ofs = f.lightofs as usize + k * size;
                        if ofs + size > lighting.len() {
                            break;
                        }
                        if st < 32 || light.all_styles {
                            for (a, &b) in acc.iter_mut().zip(&lighting[ofs..ofs + size]) {
                                *a += (b as f64 / 255.0).powf(light.lightgamma);
                            }
                        }
                    }
                    let rgb: Vec<u8> = acc.iter().map(|&v| light.light_to_screen(v)).collect();
                    block = atlas.add(lw as u32, lh as u32, rgb);
                    lmap = Some(LmMap {
                        ax: sv / 16.0,
                        ay: tv / 16.0,
                        c: [ti.s[3] as f64 / 16.0 - bmin[0] + 0.5, ti.t[3] as f64 / 16.0 - bmin[1] + 0.5],
                        sv,
                        tv,
                    });
                    lm = Some((
                        s.iter().map(|v| v / 16.0 - bmin[0] + 0.5).collect::<Vec<_>>(),
                        t.iter().map(|v| v / 16.0 - bmin[1] + 0.5).collect::<Vec<_>>(),
                    ));
                }
            }
            let normal = bsp.face_normal(fi);
            let mut p: Vec<DVec3> = pts.iter().map(|q| *q + origin).collect();
            let c = p.iter().copied().sum::<DVec3>() / p.len() as f64;
            let n = p.len();
            let newell: DVec3 = (0..n).map(|i| (p[i] - c).cross(p[(i + 1) % n] - c)).sum();
            if newell.dot(normal) < 0.0 {
                p.reverse();
                s.reverse();
                t.reverse();
                if let Some((a, b)) = lm.as_mut() {
                    a.reverse();
                    b.reverse();
                }
            }
            let (tw, th) = (mt.width.max(1) as f64, mt.height.max(1) as f64);
            records.push(Record {
                tex: mt_i,
                mode: fmode,
                alpha,
                p,
                u: s.iter().map(|v| v / tw).collect(),
                v: t.iter().map(|v| v / th).collect(),
                block,
                lm,
                lmap,
                lstyles: f.styles,
                normal,
                model: mi,
                warp: mt.name.starts_with('!').then(|| [ti.s[3], ti.t[3], tw as f32, th as f32]),
            });
        }
    }

    merge_lightmaps(&mut records, &mut atlas);
    equalize_edges(&records, &mut atlas);
    let levels = overlap_levels(&records);
    let atlas_img = atlas.pack(max_dim)?;
    let (aw, ah) = (atlas_img.w as f64, atlas_img.h as f64);
    let mut index: HashMap<(usize, Mode, u32, Option<[u32; 4]>), usize> = HashMap::new();
    let mut batches: Vec<Batch> = Vec::new();
    let mut points = Vec::new();
    for (r, &level) in records.iter().zip(&levels) {
        let (bx, by) = atlas.pos[r.block];
        let (bx, by) = (bx as f64, by as f64);
        let n = r.p.len();
        let lmuv = |i: usize| -> [f32; 2] {
            match &r.lm {
                None => [((bx + 0.5) / aw) as f32, ((by + 0.5) / ah) as f32],
                Some((a, b)) => [((bx + a[i]) / aw) as f32, ((by + b[i]) / ah) as f32],
            }
        };
        let vert = |i: usize| Vertex {
            pos: [r.p[i].x as f32, r.p[i].y as f32, r.p[i].z as f32],
            uv: [r.u[i] as f32, r.v[i] as f32],
            lm: lmuv(i),
            bias: level as f32,
            normal: [r.normal.x as f32, r.normal.y as f32, r.normal.z as f32],
            ..Vertex::BLANK
        };
        let key = (r.tex, r.mode, r.alpha.to_bits(), r.warp.map(|w| w.map(f32::to_bits)));
        let bi = *index.entry(key).or_insert_with(|| {
            batches.push(Batch {
                tex: r.tex,
                mode: r.mode,
                alpha: r.alpha,
                warp: r.warp,
                tint: [1.0; 3],
                layer2: None,
                layer3: None,
                four: None,
                tint_mask: None,
                verts: Vec::new(),
                idx: Vec::new(),
                chunks: Vec::new(),
            });
            batches.len() - 1
        });
        let b = &mut batches[bi];
        let base = b.verts.len() as u32;
        b.verts.extend((0..n).map(vert));
        for i in 1..n as u32 - 1 {
            b.idx.extend([base, base + i, base + i + 1]);
        }
    }
    for b in &batches {
        points.extend(b.verts.iter().map(|v| DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64)));
    }

    Ok(Mesh {
        batches,
        textures: tex_images,
        anim,
        atlas: atlas_img,
        faces: records.len(),
        missing: missing.into_iter().collect(),
        points: points.into(),
        nudged: levels.iter().filter(|&&l| l > 0).count(),
        names: Vec::new(),
        hdr: None,
        casters: Vec::new(),
        caster_idx: Vec::new(),
        point_cell: None,
        compressed: Vec::new(),
    })
}

const OVERLAP_MIN_AREA: f64 = 4.0;
const MAX_LEVEL: u8 = 4;

struct Flat {
    idx: usize,
    poly: Vec<DVec2>,
    lo: DVec2,
    hi: DVec2,
    area: f64,
    model: usize,
    translucent: bool,
}

fn signed_area(poly: &[DVec2]) -> f64 {
    let n = poly.len();
    (0..n).map(|i| poly[i].perp_dot(poly[(i + 1) % n])).sum::<f64>() / 2.0
}

fn clip_convex(subject: &[DVec2], clipper: &[DVec2]) -> Vec<DVec2> {
    let sign = signed_area(clipper).signum();
    let mut out = subject.to_vec();
    let n = clipper.len();
    for i in 0..n {
        if out.is_empty() {
            break;
        }
        let (a, b) = (clipper[i], clipper[(i + 1) % n]);
        let e = b - a;
        let side = |p: DVec2| sign * e.perp_dot(p - a);
        let input = std::mem::take(&mut out);
        let m = input.len();
        for j in 0..m {
            let (p, q) = (input[j], input[(j + 1) % m]);
            let (sp, sq) = (side(p), side(q));
            if sp >= 0.0 {
                out.push(p);
            }
            if (sp >= 0.0) != (sq >= 0.0) {
                out.push(p + (q - p) * (sp / (sp - sq)));
            }
        }
    }
    out
}

fn above(a: &Flat, b: &Flat) -> bool {
    if a.translucent != b.translucent {
        return a.translucent;
    }
    let tol = 1e-6 * a.area.max(b.area);
    if (a.area - b.area).abs() > tol {
        return a.area < b.area;
    }
    if a.model != b.model {
        return a.model > b.model;
    }
    a.idx > b.idx
}

fn bucket_levels(faces: &[Flat]) -> Vec<(usize, u8)> {
    let mut lv = vec![0u8; faces.len()];
    for i in 0..faces.len() {
        for j in i + 1..faces.len() {
            let (a, b) = (&faces[i], &faces[j]);
            if a.hi.x <= b.lo.x || b.hi.x <= a.lo.x || a.hi.y <= b.lo.y || b.hi.y <= a.lo.y {
                continue;
            }
            if signed_area(&clip_convex(&a.poly, &b.poly)).abs() <= OVERLAP_MIN_AREA {
                continue;
            }
            let k = if above(a, b) { i } else { j };
            lv[k] = (lv[k] + 1).min(MAX_LEVEL);
        }
    }
    faces.iter().zip(lv).map(|(f, l)| (f.idx, l)).collect()
}

fn sample_block(b: &(u32, u32, Vec<u8>), x: f64, y: f64) -> [f64; 3] {
    let (w, h) = (b.0 as usize, b.1 as usize);
    let x = (x - 0.5).clamp(0.0, (w - 1) as f64);
    let y = (y - 0.5).clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let g = |xx: usize, yy: usize| b.2[(yy * w + xx) * 3 + c] as f64;
        *o = (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy;
    }
    out
}

const TOUCH: f64 = 0.5;

fn seg_dist(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let d = b - a;
    let l2 = d.length_squared();
    let t = if l2 < 1e-12 { 0.0 } else { ((p - a).dot(d) / l2).clamp(0.0, 1.0) };
    (p - (a + d * t)).length()
}

fn polys_touch(a: &[DVec2], b: &[DVec2]) -> bool {
    let near = |p: &[DVec2], q: &[DVec2]| {
        p.iter().any(|&v| (0..q.len()).any(|e| seg_dist(v, q[e], q[(e + 1) % q.len()]) <= TOUCH))
    };
    near(a, b) || near(b, a)
}

fn uf_find(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

struct Merged {
    g: Vec<usize>,
    u: DVec3,
    w: DVec3,
    lo: DVec2,
    texel: f64,
    gw: usize,
    gh: usize,
    rgb: Vec<u8>,
}

fn plane_groups(records: &[Record], ids: &[usize]) -> Vec<Vec<usize>> {
    if ids.len() < 2 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..ids.len()).collect();
    let n0 = records[ids[0]].normal;
    let u0 = n0.any_orthonormal_vector();
    let w0 = n0.cross(u0);
    let shapes: Vec<(Vec<DVec2>, DVec2, DVec2)> = ids
        .iter()
        .map(|&ri| {
            let poly: Vec<DVec2> = records[ri].p.iter().map(|q| DVec2::new(q.dot(u0), q.dot(w0))).collect();
            let lo = poly.iter().fold(DVec2::INFINITY, |m, p| m.min(*p));
            let hi = poly.iter().fold(DVec2::NEG_INFINITY, |m, p| m.max(*p));
            (poly, lo, hi)
        })
        .collect();
    let ds: Vec<f64> = ids.iter().map(|&ri| n0.dot(records[ri].p[0])).collect();
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.sort_by(|&a, &b| ds[a].total_cmp(&ds[b]).then(a.cmp(&b)));
    for (k, &x) in order.iter().enumerate() {
        for &y in &order[k + 1..] {
            if ds[y] - ds[x] >= 0.2 {
                break;
            }
            let (a, b) = (&shapes[x], &shapes[y]);
            if a.1.x > b.2.x + TOUCH || b.1.x > a.2.x + TOUCH || a.1.y > b.2.y + TOUCH || b.1.y > a.2.y + TOUCH {
                continue;
            }
            if polys_touch(&a.0, &b.0) {
                let (p, q) = (uf_find(&mut parent, x), uf_find(&mut parent, y));
                parent[p] = q;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, &ri) in ids.iter().enumerate() {
        let root = uf_find(&mut parent, k);
        groups.entry(root).or_default().push(ri);
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

fn merge_group(records: &[Record], atlas: &Atlas, g: Vec<usize>) -> Option<Merged> {
    let r0 = &records[g[0]];
    let n = r0.normal;
    let m0 = r0.lmap.unwrap();
    let mut u = m0.sv - n * m0.sv.dot(n);
    if u.length() < 1e-9 {
        u = n.any_orthonormal_vector();
    }
    let u = u.normalize();
    let w = n.cross(u);
    let texel = g
        .iter()
        .flat_map(|&i| {
            let m = records[i].lmap.unwrap();
            [16.0 / m.sv.length().max(1e-9), 16.0 / m.tv.length().max(1e-9)]
        })
        .fold(16.0f64, f64::min)
        .max(1.0);
    let (mut lo, mut hi) = (DVec2::INFINITY, DVec2::NEG_INFINITY);
    for &i in &g {
        for p in &records[i].p {
            let q = DVec2::new(p.dot(u), p.dot(w));
            lo = lo.min(q);
            hi = hi.max(q);
        }
    }
    let gw = ((hi.x - lo.x) / texel).ceil() as usize + 1;
    let gh = ((hi.y - lo.y) / texel).ceil() as usize + 1;
    if gw * gh > 1 << 18 {
        return None;
    }
    let polys: Vec<(Vec<DVec2>, f64)> = g
        .iter()
        .map(|&i| {
            let poly: Vec<DVec2> = records[i].p.iter().map(|p| DVec2::new(p.dot(u), p.dot(w))).collect();
            let a = signed_area(&poly);
            (poly, if a < 0.0 { -1.0 } else { 1.0 })
        })
        .collect();
    let origin = n * n.dot(r0.p[0]);
    let mut rgb = vec![0u8; gw * gh * 3];
    for gy in 0..gh {
        for gx in 0..gw {
            let q2 = DVec2::new(lo.x + gx as f64 * texel, lo.y + gy as f64 * texel);
            let q = origin + u * q2.x + w * q2.y;
            let (mut acc, mut wsum) = ([0.0f64; 3], 0.0);
            let mut best = (f64::NEG_INFINITY, [0.0f64; 3]);
            for (k, &i) in g.iter().enumerate() {
                let (poly, sgn) = &polys[k];
                let mut sd = f64::INFINITY;
                for e in 0..poly.len() {
                    let (a, b) = (poly[e], poly[(e + 1) % poly.len()]);
                    let d = b - a;
                    let l = d.length().max(1e-9);
                    let dist = sgn * (d.x * (q2.y - a.y) - d.y * (q2.x - a.x)) / l;
                    sd = sd.min(dist);
                }
                let m = records[i].lmap.unwrap();
                let c = sample_block(&atlas.blocks[records[i].block], q.dot(m.ax) + m.c[0], q.dot(m.ay) + m.c[1]);
                if sd > best.0 {
                    best = (sd, c);
                }
                let wt = ((sd + texel) / (2.0 * texel)).clamp(0.0, 1.0);
                if wt > 0.0 {
                    for (a, v) in acc.iter_mut().zip(c) {
                        *a += v * wt;
                    }
                    wsum += wt;
                }
            }
            let c = if wsum > 0.0 { acc.map(|v| v / wsum) } else { best.1 };
            let d = (gy * gw + gx) * 3;
            for k in 0..3 {
                rgb[d + k] = c[k].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(Merged { g, u, w, lo, texel, gw, gh, rgb })
}

fn merge_lightmaps(records: &mut [Record], atlas: &mut Atlas) {
    let mut planes: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        if r.model != 0 || r.lmap.is_none() || r.mode != Mode::Opaque {
            continue;
        }
        let n = r.normal;
        let q = |v: f64| (v * 1e3).round() as i64;
        planes.entry([q(n.x), q(n.y), q(n.z)]).or_default().push(i);
    }
    let recs: &[Record] = records;
    let mut jobs: Vec<Vec<usize>> = planes.into_par_iter().flat_map_iter(|(_, ids)| plane_groups(recs, &ids)).collect();
    jobs.sort_unstable_by_key(|g| g[0]);
    let merged: Vec<Merged> = jobs.into_par_iter().filter_map(|g| merge_group(recs, atlas, g)).collect();
    for m in merged {
        let block = atlas.add(m.gw as u32, m.gh as u32, m.rgb);
        for &i in &m.g {
            atlas.blocks[records[i].block] = (1, 1, vec![0, 0, 0]);
            let r = &mut records[i];
            r.block = block;
            let (a, b): (Vec<f64>, Vec<f64>) = r
                .p
                .iter()
                .map(|p| ((p.dot(m.u) - m.lo.x) / m.texel + 0.5, (p.dot(m.w) - m.lo.y) / m.texel + 0.5))
                .unzip();
            r.lm = Some((a, b));
            r.lmap = None;
        }
    }
}

const EDGE_MAX_DIHEDRAL: f64 = 60.0;
const EDGE_FALL: f64 = 1.5;
const EDGE_STEP: f64 = 4.0;
const EDGE_ITERS: usize = 4;
const EDGE_GAIN: f64 = 0.7;

struct Affine {
    gu: DVec3,
    gv: DVec3,
    c: [f64; 2],
}

impl Affine {
    fn uv(&self, p: DVec3) -> [f64; 2] {
        [p.dot(self.gu) + self.c[0], p.dot(self.gv) + self.c[1]]
    }
}

fn record_affine(r: &Record) -> Option<Affine> {
    let (a, b) = r.lm.as_ref()?;
    let n = r.p.len();
    let mut best = (1e-6, 0, 0, 0);
    for i in 1..n - 1 {
        let ar = (r.p[i] - r.p[0]).cross(r.p[i + 1] - r.p[0]).length();
        if ar > best.0 {
            best = (ar, 0, i, i + 1);
        }
    }
    if best.0 <= 1e-6 {
        return None;
    }
    let (p0, p1, p2) = (r.p[best.1], r.p[best.2], r.p[best.3]);
    let (e1, e2) = (p1 - p0, p2 - p0);
    let nn = e1.cross(e2).normalize();
    let t = e1.dot(e2.cross(nn));
    let solve = |d1: f64, d2: f64| (e2.cross(nn) * d1 + nn.cross(e1) * d2) / t;
    let (u0, u1, u2) = (a[best.1], a[best.2], a[best.3]);
    let (v0, v1, v2) = (b[best.1], b[best.2], b[best.3]);
    let gu = solve(u1 - u0, u2 - u0);
    let gv = solve(v1 - v0, v2 - v0);
    Some(Affine { gu, gv, c: [u0 - p0.dot(gu), v0 - p0.dot(gv)] })
}

fn shared_segment(a: &[DVec3], b: &[DVec3]) -> Vec<(DVec3, DVec3)> {
    let mut out = Vec::new();
    for i in 0..a.len() {
        let (a0, a1) = (a[i], a[(i + 1) % a.len()]);
        let d = a1 - a0;
        let l = d.length();
        if l < 1.0 {
            continue;
        }
        let d = d / l;
        for j in 0..b.len() {
            let (b0, b1) = (b[j], b[(j + 1) % b.len()]);
            let off = |p: DVec3| (p - a0 - d * (p - a0).dot(d)).length();
            if off(b0) > 0.5 || off(b1) > 0.5 {
                continue;
            }
            let (s0, s1) = ((b0 - a0).dot(d), (b1 - a0).dot(d));
            let (lo, hi) = (s0.min(s1).max(0.0), s0.max(s1).min(l));
            if hi - lo > 1.0 {
                out.push((a0 + d * lo, a0 + d * hi));
            }
        }
    }
    out
}

fn equalize_edges(records: &[Record], atlas: &mut Atlas) {
    let cos_max = EDGE_MAX_DIHEDRAL.to_radians().cos();
    let aff: Vec<Option<Affine>> = records.iter().map(|r| if r.model == 0 && r.mode == Mode::Opaque { record_affine(r) } else { None }).collect();
    let bbox: Vec<(DVec3, DVec3)> = records
        .iter()
        .map(|r| {
            r.p.iter().fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
        })
        .collect();
    let cell = 256.0;
    let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (i, (lo, hi)) in bbox.iter().enumerate() {
        if aff[i].is_none() {
            continue;
        }
        let (l, h) = ((*lo - 1.0) / cell, (*hi + 1.0) / cell);
        for x in l.x.floor() as i64..=h.x.floor() as i64 {
            for y in l.y.floor() as i64..=h.y.floor() as i64 {
                for z in l.z.floor() as i64..=h.z.floor() as i64 {
                    grid.entry([x, y, z]).or_default().push(i);
                }
            }
        }
    }
    let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    for v in grid.values() {
        for (k, &i) in v.iter().enumerate() {
            for &j in &v[k + 1..] {
                let (a, b) = (i.min(j), i.max(j));
                let (ba, bb) = (&bbox[a], &bbox[b]);
                if ba.0.cmpgt(bb.1 + 1.0).any() || bb.0.cmpgt(ba.1 + 1.0).any() {
                    continue;
                }
                pairs.insert((a, b));
            }
        }
    }
    let mut work: Vec<(usize, usize, Vec<(DVec3, DVec3)>)> = Vec::new();
    for (a, b) in pairs {
        let (ra, rb) = (&records[a], &records[b]);
        if ra.block == rb.block || ra.lstyles != rb.lstyles {
            continue;
        }
        let dot = ra.normal.dot(rb.normal);
        if dot > 0.9999 || dot < cos_max {
            continue;
        }
        let segs = shared_segment(&ra.p, &rb.p);
        if !segs.is_empty() {
            work.push((a, b, segs));
        }
    }
    for _ in 0..EDGE_ITERS {
    let mut deltas: HashMap<usize, Vec<f32>> = HashMap::new();
    for (a, b, segs) in &work {
        let (ra, rb) = (&records[*a], &records[*b]);
        let (fa, fb) = (aff[*a].as_ref().unwrap(), aff[*b].as_ref().unwrap());
        for &(p0, p1) in segs {
            let len = (p1 - p0).length();
            let steps = ((len / EDGE_STEP).ceil() as usize).max(1);
            let diffs: Vec<[f64; 3]> = (0..=steps)
                .map(|k| {
                    let p = p0.lerp(p1, k as f64 / steps as f64);
                    let (ua, ub) = (fa.uv(p), fb.uv(p));
                    let ca = sample_block(&atlas.blocks[ra.block], ua[0], ua[1]);
                    let cb = sample_block(&atlas.blocks[rb.block], ub[0], ub[1]);
                    [cb[0] - ca[0], cb[1] - ca[1], cb[2] - ca[2]]
                })
                .collect();
            for (rec, f, sign) in [(ra, fa, 0.5), (rb, fb, -0.5)] {
                let (q0, q1) = (f.uv(p0), f.uv(p1));
                let (w, h) = (atlas.blocks[rec.block].0 as usize, atlas.blocks[rec.block].1 as usize);
                let (x0, x1) = (((q0[0].min(q1[0]) - EDGE_FALL - 1.0).floor().max(0.0)) as usize, ((q0[0].max(q1[0]) + EDGE_FALL + 1.0).ceil().min(w as f64)) as usize);
                let (y0, y1) = (((q0[1].min(q1[1]) - EDGE_FALL - 1.0).floor().max(0.0)) as usize, ((q0[1].max(q1[1]) + EDGE_FALL + 1.0).ceil().min(h as f64)) as usize);
                let seg = [q1[0] - q0[0], q1[1] - q0[1]];
                let l2 = (seg[0] * seg[0] + seg[1] * seg[1]).max(1e-9);
                let buf = deltas.entry(rec.block).or_insert_with(|| vec![0.0; w * h * 3]);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
                        let t = (((cx - q0[0]) * seg[0] + (cy - q0[1]) * seg[1]) / l2).clamp(0.0, 1.0);
                        let (dx, dy) = (cx - q0[0] - seg[0] * t, cy - q0[1] - seg[1] * t);
                        let wt = 1.0 - (dx * dx + dy * dy).sqrt() / EDGE_FALL;
                        if wt <= 0.0 {
                            continue;
                        }
                        let fpos = t * steps as f64;
                        let (i0, fr) = (fpos.floor() as usize, fpos.fract());
                        let i1 = (i0 + 1).min(steps);
                        for c in 0..3 {
                            let d = diffs[i0][c] * (1.0 - fr) + diffs[i1][c] * fr;
                            buf[(y * w + x) * 3 + c] += (d * sign * wt * EDGE_GAIN) as f32;
                        }
                    }
                }
            }
        }
    }
    for (bi, buf) in deltas {
        for (px, d) in atlas.blocks[bi].2.iter_mut().zip(buf) {
            *px = (*px as f64 + d as f64).round().clamp(0.0, 255.0) as u8;
        }
    }
    }
}

fn overlap_levels(records: &[Record]) -> Vec<u8> {
    let mut buckets: HashMap<[i64; 4], Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        let n = r.normal;
        let d = n.dot(r.p[0]);
        let q = |v: f64, s: f64| (v * s).round() as i64;
        let key = [q(n.x, 1e3), q(n.y, 1e3), q(n.z, 1e3), q(d, 4.0)];
        buckets.entry(key).or_default().push(i);
    }
    let found: Vec<Vec<(usize, u8)>> = buckets
        .into_par_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(_, v)| {
            let n = records[v[0]].normal;
            let u = n.any_orthonormal_vector();
            let w = n.cross(u);
            let faces: Vec<Flat> = v
                .iter()
                .map(|&i| {
                    let r = &records[i];
                    let poly: Vec<DVec2> = r.p.iter().map(|q| DVec2::new(q.dot(u), q.dot(w))).collect();
                    let lo = poly.iter().fold(DVec2::INFINITY, |m, p| m.min(*p));
                    let hi = poly.iter().fold(DVec2::NEG_INFINITY, |m, p| m.max(*p));
                    let area = signed_area(&poly).abs();
                    Flat {
                        idx: i,
                        poly,
                        lo,
                        hi,
                        area,
                        model: r.model,
                        translucent: r.mode == Mode::Additive || (r.mode == Mode::Blend && r.alpha < 1.0),
                    }
                })
                .collect();
            bucket_levels(&faces)
        })
        .collect();
    let mut levels = vec![0u8; records.len()];
    for (i, l) in found.into_iter().flatten() {
        levels[i] = l;
    }
    levels
}

pub fn roof_levels(mesh: &Mesh) -> Vec<(f64, f64, f64)> {
    let bin_size = 16.0;
    let min_frac = 0.02;
    let gap = 32.0;
    let zs: Vec<(f64, f64)> = mesh
        .batches
        .par_iter()
        .flat_map_iter(|b| {
            b.tris().filter_map(|tri| {
                let p = [0, 1, 2].map(|k| DVec3::new(tri[k].pos[0] as f64, tri[k].pos[1] as f64, tri[k].pos[2] as f64));
                let n = (p[1] - p[0]).cross(p[2] - p[0]);
                let ln = n.length();
                (ln > 1e-6 && n.z / ln > 0.7).then(|| ((p[0].z + p[1].z + p[2].z) / 3.0, n.z / 2.0))
            })
        })
        .collect();
    if zs.is_empty() {
        return Vec::new();
    }
    let bins: Vec<i64> = zs.iter().map(|(z, _)| (z / bin_size).floor() as i64).collect();
    let b0 = *bins.iter().min().unwrap();
    let nb = (*bins.iter().max().unwrap() - b0 + 1) as usize;
    let mut hist = vec![0f64; nb];
    let mut lo = vec![f64::INFINITY; nb];
    let mut hi = vec![f64::NEG_INFINITY; nb];
    for (&b, &(z, a)) in bins.iter().zip(&zs) {
        let i = (b - b0) as usize;
        hist[i] += a;
        lo[i] = lo[i].min(z);
        hi[i] = hi[i].max(z);
    }
    let total: f64 = hist.iter().sum();
    let mut levels: Vec<(f64, f64, f64)> = Vec::new();
    for i in 0..nb {
        if hist[i] < min_frac * total {
            continue;
        }
        match levels.last_mut() {
            Some(last) if lo[i] - last.1 <= gap => {
                last.1 = hi[i];
                last.2 += hist[i];
            }
            _ => levels.push((lo[i], hi[i], hist[i])),
        }
    }
    levels.reverse();
    levels
}

pub fn roof_zmax(levels: &[(f64, f64, f64)], n: usize, log: &mut dyn FnMut(String)) -> f64 {
    if n == 0 || levels.is_empty() {
        return 1e9;
    }
    let mut n = n;
    if n >= levels.len() {
        log(format!("  --roofs {n}: only {} levels, keeping the lowest", levels.len()));
        n = levels.len() - 1;
        if n == 0 {
            return 1e9;
        }
    }
    levels[n - 1].0 - 1.0
}
