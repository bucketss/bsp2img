use std::borrow::Cow;

use anyhow::Result;
use rayon::prelude::*;

use super::bsp::{Face, LUMP_LIGHTING_HDR, Map, SURF_BUMPLIGHT, World};
use crate::light::LightParams;
use crate::mesh::{HdrLight, Image, pack_rgb9e5};
use crate::s2::light::{EXPOSURE, curve};

pub const LIGHTMAP_MAX: usize = 4096;
pub const RGBE_SCALE: f32 = 1.5;
pub const FULL: f32 = 1.0 / EXPOSURE;
const MIN_WIDTH: u32 = 1024;
const SHRINK: f32 = 0.75;

pub struct Lighting<'a> {
    pub data: Cow<'a, [u8]>,
    pub faces: Vec<Face>,
    pub hdr: bool,
    pub extra: usize,
    pub all_styles: bool,
}

pub struct Block {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<[f32; 3]>,
}

impl Block {
    pub fn solid(c: [f32; 3]) -> Block {
        Block { w: 1, h: 1, rgb: vec![c] }
    }
}

pub fn rgbe(b: &[u8]) -> [f32; 3] {
    let s = 2f32.powi(b[3] as i8 as i32) / 255.0 * RGBE_SCALE;
    [b[0] as f32 * s, b[1] as f32 * s, b[2] as f32 * s]
}

fn bump(w: &World, f: &Face) -> bool {
    usize::try_from(f.texinfo).ok().and_then(|t| w.texinfo.get(t)).is_some_and(|t| t.flags & SURF_BUMPLIGHT != 0)
}

impl<'a> Lighting<'a> {
    pub fn open(map: &'a Map, w: &World, all_styles: bool) -> Result<Option<Lighting<'a>>> {
        let Some((fl, ll)) = map.light_lumps() else { return Ok(None) };
        let faces = map.faces_lump(fl)?;
        if faces.len() != w.faces.len() {
            anyhow::bail!("lighting face lump has {} faces, the world {}", faces.len(), w.faces.len());
        }
        let mut l = Lighting { data: map.lump(ll)?, faces, hdr: ll == LUMP_LIGHTING_HDR, extra: 0, all_styles };
        if map.version >= 21 {
            let best = [4, 1, 0].into_iter().max_by_key(|&e| {
                l.extra = e;
                l.check(w).0
            });
            l.extra = best.unwrap_or(4);
        }
        if cfg!(debug_assertions) {
            let (ok, n) = l.check(w);
            debug_assert_eq!(ok, n, "lightmap layout: {ok} of {n} face spans match");
        }
        Ok(Some(l))
    }

    pub fn stride(&self, bump: bool) -> usize {
        (if bump { 16 } else { 4 }) + self.extra
    }

    fn styles(f: &Face) -> usize {
        f.styles.iter().take_while(|&&s| s != 255).count()
    }

    pub fn check(&self, w: &World) -> (usize, usize) {
        let mut lit: Vec<(usize, usize, usize)> = self
            .faces
            .iter()
            .filter(|f| f.lightofs >= 0 && Self::styles(f) > 0)
            .map(|f| {
                let lux = ((f.lm_size[0] + 1) * (f.lm_size[1] + 1)).max(0) as usize;
                let ns = Self::styles(f);
                (f.lightofs as usize, ns * lux * self.stride(bump(w, f)), ns)
            })
            .collect();
        lit.sort();
        lit.dedup_by_key(|x| x.0);
        let mut ok = 0;
        for (k, &(o, span, _)) in lit.iter().enumerate() {
            let next = lit.get(k + 1).map_or(self.data.len(), |n| n.0 - n.2 * 4);
            ok += (o + span == next) as usize;
        }
        (ok, lit.len())
    }

    pub fn sample(&self, w: &World, fi: usize) -> Option<Block> {
        let f = &self.faces[fi];
        let ns = Self::styles(f);
        if f.lightofs < 0 || ns == 0 || f.lm_size[0] < 0 || f.lm_size[1] < 0 {
            return None;
        }
        let (bw, bh) = (f.lm_size[0] as u32 + 1, f.lm_size[1] as u32 + 1);
        let lux = (bw * bh) as usize;
        if lux > 1 << 20 {
            return None;
        }
        let k = self.stride(bump(w, f));
        let mut rgb = vec![[0f32; 3]; lux];
        for (s, &st) in f.styles[..ns].iter().enumerate() {
            if st >= 32 && !self.all_styles {
                continue;
            }
            let o = f.lightofs as usize + s * lux * k;
            let d = self.data.get(o..o + lux * 4)?;
            for (a, px) in rgb.iter_mut().zip(d.chunks_exact(4)) {
                let c = rgbe(px);
                for i in 0..3 {
                    a[i] += c[i];
                }
            }
        }
        Some(Block { w: bw, h: bh, rgb })
    }
}

pub struct Atlas {
    pub w: u32,
    pub h: u32,
    pub px: Vec<[f32; 3]>,
    pub place: Vec<[f32; 4]>,
    pub shrunk: bool,
}

impl Atlas {
    pub fn uv(&self, block: usize, lux: [f32; 2]) -> [f32; 2] {
        let p = self.place[block];
        [(p[0] + (lux[0] + 0.5) * p[2]) / self.w as f32, (p[1] + (lux[1] + 0.5) * p[3]) / self.h as f32]
    }
}

fn layout(dims: &[(u32, u32)], width: u32) -> (Vec<(u32, u32)>, u32) {
    let mut order: Vec<usize> = (0..dims.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(dims[i].1), std::cmp::Reverse(dims[i].0)));
    let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
    let mut pos = vec![(0, 0); dims.len()];
    for &i in &order {
        let (w, h) = dims[i];
        if x + w + 2 > width {
            x = 0;
            y += shelf;
            shelf = 0;
        }
        pos[i] = (x + 1, y + 1);
        x += w + 2;
        shelf = shelf.max(h + 2);
    }
    (pos, (y + shelf).max(4).next_multiple_of(4))
}

fn resample(b: &Block, w: u32, h: u32) -> Vec<[f32; 3]> {
    if (w, h) == (b.w, b.h) {
        return b.rgb.clone();
    }
    let get = |x: i64, y: i64| b.rgb[(y.clamp(0, b.h as i64 - 1) * b.w as i64 + x.clamp(0, b.w as i64 - 1)) as usize];
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let sy = ((y as f32 + 0.5) * b.h as f32 / h as f32 - 0.5).max(0.0);
        for x in 0..w {
            let sx = ((x as f32 + 0.5) * b.w as f32 / w as f32 - 0.5).max(0.0);
            let (x0, y0) = (sx.floor() as i64, sy.floor() as i64);
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let (a, c, d, e) = (get(x0, y0), get(x0 + 1, y0), get(x0, y0 + 1), get(x0 + 1, y0 + 1));
            out.push(std::array::from_fn(|k| (a[k] * (1.0 - fx) + c[k] * fx) * (1.0 - fy) + (d[k] * (1.0 - fx) + e[k] * fx) * fy));
        }
    }
    out
}

pub fn pack(blocks: &[Block], cap: u32) -> Atlas {
    let cap = cap.max(64);
    let mut scale = 1.0f32;
    loop {
        let dims: Vec<(u32, u32)> = blocks
            .iter()
            .map(|b| if scale >= 1.0 { (b.w, b.h) } else { (((b.w as f32 * scale).ceil() as u32).max(1), ((b.h as f32 * scale).ceil() as u32).max(1)) })
            .collect();
        let mut width = MIN_WIDTH.min(cap);
        let (pos, height) = loop {
            let (pos, height) = layout(&dims, width);
            if height > cap && width < cap {
                width = (width * 2).min(cap);
                continue;
            }
            break (pos, height);
        };
        if height > cap && dims.iter().any(|d| d.0 > 1 || d.1 > 1) {
            scale *= SHRINK;
            continue;
        }
        let mut px = vec![[0f32; 3]; (width * height) as usize];
        let mut place = Vec::with_capacity(blocks.len());
        for (i, b) in blocks.iter().enumerate() {
            let (w, h) = dims[i];
            let (bx, by) = pos[i];
            let src = resample(b, w, h);
            for yy in 0..h + 2 {
                let sy = yy.saturating_sub(1).min(h - 1);
                let row = ((by - 1 + yy) * width + bx - 1) as usize;
                for xx in 0..w + 2 {
                    let sx = xx.saturating_sub(1).min(w - 1);
                    px[row + xx as usize] = src[(sy * w + sx) as usize];
                }
            }
            place.push([bx as f32, by as f32, w as f32 / b.w as f32, h as f32 / b.h as f32]);
        }
        return Atlas { w: width, h: height, px, place, shrunk: scale < 1.0 };
    }
}

pub fn finish(a: &Atlas, light: &LightParams) -> (Image, HdrLight) {
    let lut: Vec<u8> = (0..4096).map(|i| light.light_to_screen(i as f64 / 1024.0)).collect();
    let to_byte = |v: f32| lut[((v.max(0.0) * EXPOSURE * 1024.0) as usize).min(4095)];
    let rgba: Vec<u8> = a.px.par_iter().flat_map_iter(|c| [to_byte(c[0]), to_byte(c[1]), to_byte(c[2]), 255]).collect();
    let irradiance: Vec<u32> = a.px.par_iter().map(|&c| pack_rgb9e5(c)).collect();
    (
        Image { w: a.w, h: a.h, rgba },
        HdrLight { w: a.w, h: a.h, irradiance, shadows: None, sun: [0.0; 3], lights: Vec::new(), curve: curve(light) },
    )
}

