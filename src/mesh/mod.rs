mod goldsrc;
mod lightmap;
pub mod overlay;

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use rayon::prelude::*;

pub use goldsrc::build_mesh;

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

#[derive(Clone)]
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

#[derive(Clone)]
pub struct Mesh {
    pub batches: Vec<Batch>,
    pub textures: std::sync::Arc<Vec<Option<Image>>>,
    pub anim: Vec<Option<Vec<usize>>>,
    pub atlas: std::sync::Arc<Image>,
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
    pub face_spans: Vec<FaceSpan>,
    pub overlay: Option<Image>,
}

#[derive(Clone, Copy, Debug)]
pub struct FaceSpan {
    pub batch: u32,
    pub first: u32,
    pub count: u32,
    pub model: u32,
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

#[derive(Clone)]
pub struct PointLight {
    pub pos: [f32; 3],
    pub range: f32,
    pub color: [f32; 3],
    pub channel: u32,
    pub dir: [f32; 3],
    pub kind: u32,
    pub cone: [f32; 2],
}

#[derive(Clone)]
pub struct HdrLight {
    pub w: u32,
    pub h: u32,
    pub irradiance: Vec<u32>,
    pub shadows: Option<Image>,
    pub sun: [f32; 3],
    pub lights: Vec<PointLight>,
    pub curve: [f32; 3],
}

pub fn sample_points(batches: &[Batch], cell: f64) -> Vec<DVec3> {
    let key = |v: &Vertex| {
        let q = |x: f32| (x as f64 / cell).floor() as i32;
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
    let mut all: std::collections::BTreeMap<(i32, i32, i32), [f32; 3]> = std::collections::BTreeMap::new();
    for s in sets {
        for (k, v) in s {
            all.entry(k).or_insert(v);
        }
    }
    all.into_values().map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect()
}

pub fn skip_class(cls: &str) -> bool {
    cls.starts_with("trigger_") || SKIP_CLASSES.contains(&cls)
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
