use std::collections::{BTreeSet, HashMap};

use bytemuck::{Pod, Zeroable};
use glam::DVec3;

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
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub lm: [f32; 2],
}

pub struct Batch {
    pub tex: usize,
    pub mode: Mode,
    pub alpha: f32,
    pub verts: Vec<Vertex>,
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
    pub atlas: Image,
    pub faces: usize,
    pub missing: Vec<String>,
    pub points: Vec<DVec3>,
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

    fn pack(&mut self, max_dim: u32) -> Image {
        let mut order: Vec<usize> = (0..self.blocks.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.blocks[i].1));
        let mut width = 2048;
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
            return Image { w: width, h: height, rgba: img };
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

struct Record {
    tex: usize,
    mode: Mode,
    alpha: f32,
    p: Vec<DVec3>,
    u: Vec<f64>,
    v: Vec<f64>,
    block: usize,
    lm: Option<(Vec<f64>, Vec<f64>)>,
}

pub fn build_mesh(
    bsp: &Bsp,
    textures: &mut TextureSource,
    light: &LightParams,
    reach: Option<&Reach>,
    max_dim: u32,
) -> Mesh {
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
            for px in im.rgba.chunks_exact_mut(4) {
                for c in &mut px[..3] {
                    *c = lut[*c as usize];
                }
            }
        }
        tex_images.push(img);
    }

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
            if !special && f.lightofs >= 0 && mode != Mode::Additive && !lighting.is_empty() {
                let smin = s.iter().cloned().fold(f64::INFINITY, f64::min);
                let smax = s.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let tmin = t.iter().cloned().fold(f64::INFINITY, f64::min);
                let tmax = t.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
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
            });
        }
    }

    let atlas_img = atlas.pack(max_dim);
    let (aw, ah) = (atlas_img.w as f64, atlas_img.h as f64);
    let mut index: HashMap<(usize, Mode, u32), usize> = HashMap::new();
    let mut batches: Vec<Batch> = Vec::new();
    let mut points = Vec::new();
    for r in &records {
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
        };
        let key = (r.tex, r.mode, r.alpha.to_bits());
        let bi = *index.entry(key).or_insert_with(|| {
            batches.push(Batch { tex: r.tex, mode: r.mode, alpha: r.alpha, verts: Vec::new() });
            batches.len() - 1
        });
        let out = &mut batches[bi].verts;
        for i in 1..n - 1 {
            out.push(vert(0));
            out.push(vert(i));
            out.push(vert(i + 1));
        }
    }
    for b in &batches {
        points.extend(b.verts.iter().map(|v| DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64)));
    }

    Mesh {
        batches,
        textures: tex_images,
        atlas: atlas_img,
        faces: records.len(),
        missing: missing.into_iter().collect(),
        points,
    }
}

pub fn roof_levels(mesh: &Mesh) -> Vec<(f64, f64, f64)> {
    let bin_size = 16.0;
    let min_frac = 0.02;
    let gap = 32.0;
    let mut zs = Vec::new();
    for b in &mesh.batches {
        for tri in b.verts.chunks_exact(3) {
            let p: Vec<DVec3> =
                tri.iter().map(|v| DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64)).collect();
            let n = (p[1] - p[0]).cross(p[2] - p[0]);
            let ln = n.length();
            if ln <= 1e-6 || n.z / ln <= 0.7 {
                continue;
            }
            zs.push(((p[0].z + p[1].z + p[2].z) / 3.0, n.z / 2.0));
        }
    }
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
