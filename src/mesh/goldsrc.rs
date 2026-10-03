use std::collections::{BTreeSet, HashMap};

use anyhow::{Result, bail};
use glam::{DVec2, DVec3};
use rayon::prelude::*;

use super::lightmap::{equalize_edges, merge_lightmaps};
use super::{Batch, FaceSpan, Image, Mesh, Mode, SKIP_TEXTURES, Vertex, skip_class};
use crate::bsp::{Bsp, TEX_SPECIAL};
use crate::light::LightParams;
use crate::reach::Reach;
use crate::wad::TextureSource;

fn parse_f(s: Option<&str>, d: f64) -> f64 {
    s.and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(d)
}

pub(super) struct Atlas {
    pub(super) blocks: Vec<(u32, u32, Vec<u8>)>,
    pub(super) pos: Vec<(u32, u32)>,
}

impl Atlas {
    pub(super) fn add(&mut self, w: u32, h: u32, rgb: Vec<u8>) -> usize {
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
pub(super) struct LmMap {
    pub(super) ax: DVec3,
    pub(super) ay: DVec3,
    pub(super) c: [f64; 2],
    pub(super) sv: DVec3,
    pub(super) tv: DVec3,
}

pub(super) struct Record {
    pub(super) tex: usize,
    pub(super) mode: Mode,
    pub(super) alpha: f32,
    pub(super) p: Vec<DVec3>,
    pub(super) u: Vec<f64>,
    pub(super) v: Vec<f64>,
    pub(super) block: usize,
    pub(super) lm: Option<(Vec<f64>, Vec<f64>)>,
    pub(super) lmap: Option<LmMap>,
    pub(super) lstyles: [u8; 4],
    pub(super) normal: DVec3,
    pub(super) model: usize,
    pub(super) warp: Option<[f32; 4]>,
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
    let mut face_spans = Vec::with_capacity(records.len());
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
        face_spans.push(FaceSpan { batch: bi as u32, first: base, count: n as u32, model: r.model as u32 });
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
        face_spans,
        overlay: None,
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

pub(super) fn signed_area(poly: &[DVec2]) -> f64 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_blocks_do_not_overlap() {
        let mut a = Atlas { blocks: Vec::new(), pos: Vec::new() };
        let sizes = [(3000, 2), (16, 16), (1, 1), (200, 40), (40, 200), (2046, 3)];
        for &(w, h) in &sizes {
            a.add(w, h, vec![7; (w * h * 3) as usize]);
        }
        let img = a.pack(8192).unwrap();
        let rects: Vec<(u32, u32, u32, u32)> = a.pos.iter().zip(&sizes).map(|(&(x, y), &(w, h))| (x - 1, y - 1, x + w + 1, y + h + 1)).collect();
        for (i, r) in rects.iter().enumerate() {
            assert!(r.2 <= img.w && r.3 <= img.h, "block {i} outside {}x{}", img.w, img.h);
            for q in &rects[i + 1..] {
                assert!(r.2 <= q.0 || q.2 <= r.0 || r.3 <= q.1 || q.3 <= r.1);
            }
        }
    }

    #[test]
    fn atlas_over_the_limit_is_an_error() {
        let mut a = Atlas { blocks: Vec::new(), pos: Vec::new() };
        a.add(5000, 1, vec![0; 15000]);
        assert!(a.pack(4096).is_err());
    }
}
