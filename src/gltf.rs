use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use glam::{DVec2, DVec3};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::clip::clip_tris_cuts;
use crate::mesh::{Image, Mode, Vertex};
use crate::paths::{Partial, free_name};
use crate::render::Cuts;
use crate::scene::{Report, Scene};

const METRES: f64 = 0.0254;
const NUDGE: f64 = 0.5;
const MAX_ATLAS: u32 = 8192;
const PAD: u32 = 2;
const SUPER: usize = 2;
const CHART_WASTE: f64 = 2.0;
const CHART_SLACK: f64 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lighting {
    Baked,
    Separate,
    None,
}

impl Lighting {
    pub const ALL: [Lighting; 3] = [Lighting::Baked, Lighting::Separate, Lighting::None];

    pub fn key(self) -> &'static str {
        match self {
            Lighting::Baked => "baked",
            Lighting::Separate => "separate",
            Lighting::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Lighting> {
        Lighting::ALL.into_iter().find(|l| l.key() == s.trim())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GltfOpts {
    pub lighting: Lighting,
    pub texel: f64,
    pub nearest: bool,
}

impl Default for GltfOpts {
    fn default() -> Self {
        GltfOpts { lighting: Lighting::Baked, texel: 2.0, nearest: false }
    }
}

struct Part<'a> {
    tex: usize,
    mode: Mode,
    alpha: f32,
    verts: Vec<Vertex>,
    image: Option<&'a Image>,
}

struct Chart {
    part: usize,
    tris: Vec<usize>,
    u: DVec3,
    w: DVec3,
    lo: DVec2,
    hi: DVec2,
    size: (u32, u32),
    at: (u32, u32),
}

fn dv(p: [f32; 3]) -> DVec3 {
    DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
}

fn plane_basis(n: DVec3) -> (DVec3, DVec3) {
    let a = if n.z.abs() > 0.7 { DVec3::Y } else { DVec3::Z };
    let u = a.cross(n).normalize_or(n.any_orthonormal_vector());
    (u, n.cross(u))
}

fn tri_area(t: &[Vertex]) -> f64 {
    (dv(t[1].pos) - dv(t[0].pos)).cross(dv(t[2].pos) - dv(t[0].pos)).length() / 2.0
}

fn make_charts(parts: &[Part], texel: f64) -> Vec<Chart> {
    let mut charts: Vec<Chart> = Vec::new();
    for (pi, p) in parts.iter().enumerate() {
        let mut key = None;
        let mut area = 0.0;
        for (ti, t) in p.verts.chunks_exact(3).enumerate() {
            let n = dv(t[0].normal);
            let k = (t[0].normal.map(f32::to_bits), (n.dot(dv(t[0].pos)) * 4.0).round() as i64);
            let same = key == Some(k);
            let (u, w) = charts.last().filter(|_| same).map(|c| (c.u, c.w)).unwrap_or_else(|| plane_basis(n));
            let (tlo, thi) = t.iter().fold((DVec2::INFINITY, DVec2::NEG_INFINITY), |(lo, hi), v| {
                let q = DVec2::new(dv(v.pos).dot(u), dv(v.pos).dot(w));
                (lo.min(q), hi.max(q))
            });
            let a = tri_area(t);
            let fits = same
                && charts.last().is_some_and(|c| {
                    let d = c.hi.max(thi) - c.lo.min(tlo);
                    let limit = ((MAX_ATLAS - 2 * PAD) as f64 - 1.0) * texel;
                    d.x <= limit
                        && d.y <= limit
                        && d.x * d.y <= CHART_WASTE * (area + a) + (CHART_SLACK * texel) * (CHART_SLACK * texel)
                });
            if fits {
                let c = charts.last_mut().unwrap();
                c.tris.push(ti);
                c.lo = c.lo.min(tlo);
                c.hi = c.hi.max(thi);
                area += a;
            } else {
                charts.push(Chart { part: pi, tris: vec![ti], u, w, lo: tlo, hi: thi, size: (0, 0), at: (0, 0) });
                key = Some(k);
                area = a;
            }
        }
    }
    for c in &mut charts {
        let d = (c.hi - c.lo) / texel;
        c.size = ((d.x.ceil() as u32).max(1) + 2 * PAD, (d.y.ceil() as u32).max(1) + 2 * PAD);
    }
    charts
}

fn shelf(charts: &mut [Chart], width: u32) -> u32 {
    let mut order: Vec<usize> = (0..charts.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(charts[i].size.1), std::cmp::Reverse(charts[i].size.0)));
    let (mut x, mut y, mut row) = (0u32, 0u32, 0u32);
    for i in order {
        let (w, h) = charts[i].size;
        if x + w > width {
            x = 0;
            y += row;
            row = 0;
        }
        charts[i].at = (x, y);
        x += w;
        row = row.max(h);
    }
    (y + row).div_ceil(4) * 4
}

fn layout(parts: &[Part], texel: f64, log: &mut dyn FnMut(String)) -> (Vec<Chart>, u32, u32, f64) {
    let mut texel = texel;
    loop {
        let mut charts = make_charts(parts, texel);
        let area: f64 = charts.iter().map(|c| c.size.0 as f64 * c.size.1 as f64).sum();
        let widest = charts.iter().map(|c| c.size.0).max().unwrap_or(1);
        let mut width = ((area * 1.1).sqrt().ceil() as u32).max(widest).next_power_of_two().clamp(64, MAX_ATLAS);
        let mut best: Option<(u64, u32)> = None;
        let mut h;
        loop {
            h = shelf(&mut charts, width);
            if h <= MAX_ATLAS && best.is_none_or(|(a, _)| (width as u64 * h as u64) < a) {
                best = Some((width as u64 * h as u64, width));
            }
            if width >= MAX_ATLAS {
                break;
            }
            width *= 2;
        }
        if let Some((_, w)) = best {
            let h = shelf(&mut charts, w);
            return (charts, w, h.max(4), texel);
        }
        texel *= (h as f64 / MAX_ATLAS as f64).sqrt().max(1.05);
        log(format!("  atlas would be {MAX_ATLAS}x{h}; raising texel to {texel:.2} units"));
    }
}

fn fetch(img: &Image, x: i64, y: i64) -> [f64; 4] {
    let i = ((y as usize) * img.w as usize + x as usize) * 4;
    let p = &img.rgba[i..i + 4];
    [p[0] as f64, p[1] as f64, p[2] as f64, p[3] as f64]
}

fn sample(img: &Image, uv: DVec2, wrap: bool, nearest: bool) -> [f64; 4] {
    let (w, h) = (img.w as i64, img.h as i64);
    let fix = |v: i64, n: i64| if wrap { v.rem_euclid(n) } else { v.clamp(0, n - 1) };
    if nearest {
        let (x, y) = ((uv.x * w as f64).floor() as i64, (uv.y * h as f64).floor() as i64);
        return fetch(img, fix(x, w), fix(y, h));
    }
    let (fx, fy) = (uv.x * w as f64 - 0.5, uv.y * h as f64 - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (xa, xb, ya, yb) = (fix(x0, w), fix(x0 + 1, w), fix(y0, h), fix(y0 + 1, h));
    let (a, b, c, d) = (fetch(img, xa, ya), fetch(img, xb, ya), fetch(img, xa, yb), fetch(img, xb, yb));
    let mut o = [0.0; 4];
    for k in 0..4 {
        o[k] = (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty;
    }
    o
}

struct Bary {
    a: DVec2,
    e1: DVec2,
    e2: DVec2,
    det: f64,
}

impl Bary {
    fn new(q: [DVec2; 3]) -> Bary {
        let (e1, e2) = (q[1] - q[0], q[2] - q[0]);
        Bary { a: q[0], e1, e2, det: e1.perp_dot(e2) }
    }

    fn at(&self, p: DVec2) -> [f64; 3] {
        if self.det.abs() < 1e-12 {
            return [1.0, 0.0, 0.0];
        }
        let d = p - self.a;
        let b1 = d.perp_dot(self.e2) / self.det;
        let b2 = self.e1.perp_dot(d) / self.det;
        [1.0 - b1 - b2, b1, b2]
    }
}

fn bake_chart(c: &Chart, part: &Part, lmap: &Image, texel: f64, nearest: bool) -> Vec<u8> {
    let (cw, ch) = (c.size.0 as usize, c.size.1 as usize);
    let local = |v: &Vertex| {
        let p = dv(v.pos);
        (DVec2::new(p.dot(c.u), p.dot(c.w)) - c.lo) / texel + DVec2::splat(PAD as f64)
    };
    let tris: Vec<(Bary, &[Vertex])> = c
        .tris
        .iter()
        .map(|&t| {
            let v = &part.verts[t * 3..t * 3 + 3];
            (Bary::new([local(&v[0]), local(&v[1]), local(&v[2])]), v)
        })
        .collect();
    let mut owner = vec![u32::MAX; cw * ch];
    let mut queue = VecDeque::new();
    for (ti, (b, _)) in tris.iter().enumerate() {
        if b.det.abs() < 1e-12 {
            continue;
        }
        let q = [b.a, b.a + b.e1, b.a + b.e2];
        let lo = q.iter().fold(DVec2::INFINITY, |m, v| m.min(*v));
        let hi = q.iter().fold(DVec2::NEG_INFINITY, |m, v| m.max(*v));
        let (x0, y0) = ((lo.x.floor() as i64).max(0) as usize, (lo.y.floor() as i64).max(0) as usize);
        let (x1, y1) = ((hi.x.ceil() as usize).min(cw), (hi.y.ceil() as usize).min(ch));
        for y in y0..y1 {
            for x in x0..x1 {
                let k = y * cw + x;
                if owner[k] != u32::MAX {
                    continue;
                }
                let w = b.at(DVec2::new(x as f64 + 0.5, y as f64 + 0.5));
                if w.iter().all(|&v| v >= -1e-9) {
                    owner[k] = ti as u32;
                    queue.push_back(k);
                }
            }
        }
    }
    if queue.is_empty() {
        owner.fill(0);
    }
    while let Some(k) = queue.pop_front() {
        let (x, y) = (k % cw, k / cw);
        let o = owner[k];
        let mut go = |n: usize| {
            if owner[n] == u32::MAX {
                owner[n] = o;
                queue.push_back(n);
            }
        };
        if x > 0 {
            go(k - 1);
        }
        if x + 1 < cw {
            go(k + 1);
        }
        if y > 0 {
            go(k - cw);
        }
        if y + 1 < ch {
            go(k + cw);
        }
    }
    let mut out = vec![0u8; cw * ch * 4];
    let n = (SUPER * SUPER) as f64;
    for (k, px) in out.chunks_exact_mut(4).enumerate() {
        let (b, v) = &tris[owner[k] as usize];
        let (x, y) = ((k % cw) as f64, (k / cw) as f64);
        let mut acc = [0.0f64; 4];
        for sy in 0..SUPER {
            for sx in 0..SUPER {
                let p = DVec2::new(x + (sx as f64 + 0.5) / SUPER as f64, y + (sy as f64 + 0.5) / SUPER as f64);
                let w = b.at(p);
                let mix = |f: fn(&Vertex) -> [f32; 2]| {
                    DVec2::new(
                        (0..3).map(|i| w[i] * f(&v[i])[0] as f64).sum(),
                        (0..3).map(|i| w[i] * f(&v[i])[1] as f64).sum(),
                    )
                };
                let t = match part.image {
                    Some(img) => sample(img, mix(|v| v.uv), true, nearest),
                    None => [255.0; 4],
                };
                let l = sample(lmap, mix(|v| v.lm), false, false);
                for i in 0..3 {
                    acc[i] += t[i] * l[i] / 255.0;
                }
                acc[3] += t[3];
            }
        }
        let mut c = acc.map(|v| v / n / 255.0);
        if part.mode == Mode::Additive {
            let m = c[0].max(c[1]).max(c[2]);
            if m > 1e-6 {
                c = [c[0] / m, c[1] / m, c[2] / m, c[3] * m];
            } else {
                c[3] = 0.0;
            }
        }
        for i in 0..4 {
            px[i] = (c[i] * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn png(img: &Image) -> Result<Vec<u8>> {
    let opaque = img.rgba.chunks_exact(4).all(|p| p[3] == 255);
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, img.w, img.h);
        enc.set_depth(png::BitDepth::Eight);
        if opaque {
            enc.set_color(png::ColorType::Rgb);
            let rgb: Vec<u8> = img.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            enc.write_header()?.write_image_data(&rgb)?;
        } else {
            enc.set_color(png::ColorType::Rgba);
            enc.write_header()?.write_image_data(&img.rgba)?;
        }
    }
    Ok(buf)
}

#[derive(Default)]
struct Doc {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    images: Vec<Value>,
    textures: Vec<Value>,
    samplers: Vec<Value>,
    materials: Vec<Value>,
    prims: Vec<Value>,
}

impl Doc {
    fn view(&mut self, data: &[u8], target: Option<u32>) -> usize {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let mut v = json!({"buffer": 0, "byteOffset": self.bin.len(), "byteLength": data.len()});
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.bin.extend_from_slice(data);
        self.views.push(v);
        self.views.len() - 1
    }

    fn floats(&mut self, data: &[f32], comps: usize, bounds: bool) -> usize {
        let view = self.view(bytemuck::cast_slice(data), Some(34962));
        let ty = ["", "SCALAR", "VEC2", "VEC3", "VEC4"][comps];
        let mut a = json!({"bufferView": view, "componentType": 5126, "count": data.len() / comps, "type": ty});
        if bounds {
            let mut lo = vec![f32::INFINITY; comps];
            let mut hi = vec![f32::NEG_INFINITY; comps];
            for v in data.chunks_exact(comps) {
                for i in 0..comps {
                    lo[i] = lo[i].min(v[i]);
                    hi[i] = hi[i].max(v[i]);
                }
            }
            a["min"] = json!(lo);
            a["max"] = json!(hi);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn indices(&mut self, data: &[u32]) -> usize {
        let view = self.view(bytemuck::cast_slice(data), Some(34963));
        self.accessors.push(json!({"bufferView": view, "componentType": 5125, "count": data.len(), "type": "SCALAR"}));
        self.accessors.len() - 1
    }

    fn texture(&mut self, img: &Image, name: &str, sampler: usize) -> Result<usize> {
        let data = png(img)?;
        let view = self.view(&data, None);
        self.images.push(json!({"name": name, "bufferView": view, "mimeType": "image/png"}));
        self.textures.push(json!({"name": name, "sampler": sampler, "source": self.images.len() - 1}));
        Ok(self.textures.len() - 1)
    }

    fn sampler(&mut self, repeat: bool, nearest: bool) -> usize {
        let wrap = if repeat { 10497 } else { 33071 };
        let (mag, min) = if nearest { (9728, 9986) } else { (9729, 9987) };
        self.samplers.push(json!({"magFilter": mag, "minFilter": min, "wrapS": wrap, "wrapT": wrap}));
        self.samplers.len() - 1
    }

    fn glb(self, name: &str, unlit: bool) -> Result<Vec<u8>> {
        let mut root = json!({
            "asset": {"version": "2.0", "generator": "bsp2img"},
            "scene": 0,
            "scenes": [{"name": name, "nodes": [0]}],
            "nodes": [{"name": name, "mesh": 0}],
            "meshes": [{"name": name, "primitives": self.prims}],
            "materials": self.materials,
            "accessors": self.accessors,
            "bufferViews": self.views,
            "buffers": [{"byteLength": self.bin.len()}],
        });
        if unlit {
            root["extensionsUsed"] = json!(["KHR_materials_unlit"]);
        }
        for (k, v) in [("images", self.images), ("textures", self.textures), ("samplers", self.samplers)] {
            if !v.is_empty() {
                root[k] = Value::Array(v);
            }
        }
        let mut js = serde_json::to_vec(&root)?;
        while js.len() % 4 != 0 {
            js.push(b' ');
        }
        let mut bin = self.bin;
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let total = 12 + 8 + js.len() + 8 + bin.len();
        if total > u32::MAX as usize {
            bail!("GLB over 4 GB");
        }
        let mut out = Vec::with_capacity(total);
        for v in [0x4654_6C67u32, 2, total as u32, js.len() as u32, 0x4E4F_534A] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&js);
        for v in [bin.len() as u32, 0x004E_4942] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&bin);
        Ok(out)
    }
}

fn to_gltf(p: DVec3) -> [f32; 3] {
    [(p.x * METRES) as f32, (p.z * METRES) as f32, (-p.y * METRES) as f32]
}

fn add_prim(doc: &mut Doc, verts: &[Vertex], uv0: &dyn Fn(usize, &Vertex) -> [f32; 2], uv1: bool, material: usize) {
    let mut map: HashMap<[u32; 10], u32> = HashMap::new();
    let (mut pos, mut nrm, mut t0, mut t1, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (ti, tri) in verts.chunks_exact(3).enumerate() {
        let conv: Vec<([f32; 3], [f32; 3], [f32; 2], [f32; 2])> = tri
            .iter()
            .enumerate()
            .map(|(k, v)| {
                let n = dv(v.normal);
                let p = dv(v.pos) + n * (v.bias as f64 * NUDGE);
                (to_gltf(p), [n.x as f32, n.z as f32, -n.y as f32], uv0(ti * 3 + k, v), if uv1 { v.lm } else { [0.0; 2] })
            })
            .collect();
        let d = |i: usize| DVec3::new(conv[i].0[0] as f64, conv[i].0[1] as f64, conv[i].0[2] as f64);
        if (d(1) - d(0)).cross(d(2) - d(0)).length_squared() <= 1e-16 {
            continue;
        }
        for (p, n, a, b) in conv {
            let key = [p[0], p[1], p[2], n[0], n[1], n[2], a[0], a[1], b[0], b[1]].map(f32::to_bits);
            let i = *map.entry(key).or_insert_with(|| {
                pos.extend(p);
                nrm.extend(n);
                t0.extend(a);
                t1.extend(b);
                (pos.len() / 3 - 1) as u32
            });
            idx.push(i);
        }
    }
    if idx.is_empty() {
        return;
    }
    let mut attrs = json!({
        "POSITION": doc.floats(&pos, 3, true),
        "NORMAL": doc.floats(&nrm, 3, false),
        "TEXCOORD_0": doc.floats(&t0, 2, false),
    });
    if uv1 {
        attrs["TEXCOORD_1"] = json!(doc.floats(&t1, 2, false));
    }
    let indices = doc.indices(&idx);
    doc.prims.push(json!({"attributes": attrs, "indices": indices, "material": material, "mode": 4}));
}

fn material(name: &str, mode: Mode, alpha: f32, tex: Option<usize>, unlit: bool) -> Value {
    let mut pbr = json!({"metallicFactor": 0.0, "roughnessFactor": 1.0});
    if let Some(t) = tex {
        pbr["baseColorTexture"] = json!({"index": t});
    }
    if matches!(mode, Mode::Blend | Mode::Additive) && alpha < 1.0 {
        pbr["baseColorFactor"] = json!([1.0, 1.0, 1.0, alpha]);
    }
    let mut m = json!({"name": name, "pbrMetallicRoughness": pbr});
    match mode {
        Mode::Opaque => {}
        Mode::AlphaTest => {
            m["alphaMode"] = json!("MASK");
            m["alphaCutoff"] = json!(0.5);
        }
        Mode::Blend | Mode::Additive => m["alphaMode"] = json!("BLEND"),
    }
    if unlit {
        m["extensions"] = json!({"KHR_materials_unlit": {}});
    }
    m
}

fn mode_key(m: Mode) -> &'static str {
    match m {
        Mode::Opaque => "opaque",
        Mode::AlphaTest => "mask",
        Mode::Blend => "blend",
        Mode::Additive => "additive",
    }
}

pub fn export_gltf(
    scene: &Scene,
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &GltfOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<Vec<PathBuf>> {
    if !(o.texel > 0.0) {
        bail!("texel must be above 0");
    }
    let t0 = Instant::now();
    rep.step(0.0)?;
    let mesh = &scene.mesh;
    let parts: Vec<Part> = mesh
        .batches
        .par_iter()
        .map(|b| Part {
            tex: b.tex,
            mode: b.mode,
            alpha: b.alpha,
            verts: clip_tris_cuts(&b.verts, cuts, scene.mask.as_ref()),
            image: mesh.textures.get(b.tex).and_then(|t| t.as_ref()),
        })
        .filter(|p| !p.verts.is_empty())
        .collect();
    let tris: usize = parts.iter().map(|p| p.verts.len() / 3).sum();
    if tris == 0 {
        bail!("nothing left to export after the cuts");
    }
    rep.log(format!("  {tris} triangles in {} batches", parts.len()));
    rep.step(0.1)?;
    let tex_name = |t: usize| {
        scene.bsp.miptex.get(t).map(|m| m.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("tex{t}"))
    };
    let mut doc = Doc::default();
    let unlit = o.lighting == Lighting::Baked;
    match o.lighting {
        Lighting::Baked => {
            let mut log = |s: String| rep.log(s);
            let (mut charts, aw, ah, texel) = layout(&parts, o.texel, &mut log);
            let used: f64 = charts.iter().map(|c| c.size.0 as f64 * c.size.1 as f64).sum();
            rep.log(format!(
                "  atlas {aw}x{ah} at {texel:.2} units per texel, {} charts, {:.0}% used",
                charts.len(),
                100.0 * used / (aw as f64 * ah as f64)
            ));
            rep.step(0.2)?;
            let baked: Vec<Vec<u8>> =
                charts.par_iter().map(|c| bake_chart(c, &parts[c.part], &mesh.atlas, texel, o.nearest)).collect();
            rep.step(0.6)?;
            let mut atlas = Image { w: aw, h: ah, rgba: vec![0u8; (aw * ah * 4) as usize] };
            let mut uvs: Vec<Vec<[f32; 2]>> = parts.iter().map(|p| vec![[0.0; 2]; p.verts.len()]).collect();
            for (c, px) in charts.iter_mut().zip(&baked) {
                let (cw, ch) = c.size;
                for y in 0..ch {
                    let d = (((c.at.1 + y) * aw + c.at.0) * 4) as usize;
                    let s = (y * cw * 4) as usize;
                    atlas.rgba[d..d + (cw * 4) as usize].copy_from_slice(&px[s..s + (cw * 4) as usize]);
                }
                let part = &parts[c.part];
                for &t in &c.tris {
                    for i in t * 3..t * 3 + 3 {
                        let p = dv(part.verts[i].pos);
                        let q = (DVec2::new(p.dot(c.u), p.dot(c.w)) - c.lo) / texel
                            + DVec2::new((c.at.0 + PAD) as f64, (c.at.1 + PAD) as f64);
                        uvs[c.part][i] = [(q.x / aw as f64) as f32, (q.y / ah as f64) as f32];
                    }
                }
            }
            drop(baked);
            let s = doc.sampler(false, o.nearest);
            let tex = doc.texture(&atlas, "atlas", s)?;
            drop(atlas);
            rep.step(0.8)?;
            let mut mats: HashMap<(Mode, u32), usize> = HashMap::new();
            for (pi, p) in parts.iter().enumerate() {
                let m = *mats.entry((p.mode, p.alpha.to_bits())).or_insert_with(|| {
                    let label = format!("{}_{:.0}", mode_key(p.mode), p.alpha * 255.0);
                    doc.materials.push(material(&label, p.mode, p.alpha, Some(tex), true));
                    doc.materials.len() - 1
                });
                let uv = &uvs[pi];
                add_prim(&mut doc, &p.verts, &|i, _| uv[i], false, m);
            }
        }
        Lighting::Separate | Lighting::None => {
            let rs = doc.sampler(true, o.nearest);
            let lm_tex = if o.lighting == Lighting::Separate {
                let ls = doc.sampler(false, false);
                Some(doc.texture(&mesh.atlas, "lightmap", ls)?)
            } else {
                None
            };
            let mut texs: HashMap<usize, usize> = HashMap::new();
            let mut mats: HashMap<(usize, Mode, u32), usize> = HashMap::new();
            for (i, p) in parts.iter().enumerate() {
                rep.step(0.1 + 0.7 * i as f32 / parts.len() as f32)?;
                let t = match p.image {
                    Some(img) => Some(match texs.get(&p.tex) {
                        Some(&t) => t,
                        None => {
                            let t = doc.texture(img, &tex_name(p.tex), rs)?;
                            texs.insert(p.tex, t);
                            t
                        }
                    }),
                    None => None,
                };
                let m = *mats.entry((p.tex, p.mode, p.alpha.to_bits())).or_insert_with(|| {
                    let mut m = material(&tex_name(p.tex), p.mode, p.alpha, t, false);
                    if let Some(l) = lm_tex {
                        m["occlusionTexture"] = json!({"index": l, "texCoord": 1});
                    }
                    doc.materials.push(m);
                    doc.materials.len() - 1
                });
                add_prim(&mut doc, &p.verts, &|_, v: &Vertex| v.uv, lm_tex.is_some(), m);
            }
        }
    }
    rep.step(0.9)?;
    let (prims, mats, imgs) = (doc.prims.len(), doc.materials.len(), doc.images.len());
    let data = doc.glb(name, unlit)?;
    let mut files = Partial::default();
    let f = free_name(out, &format!("{name}{cut_tag}_{}", o.lighting.key()), ".glb");
    files.add(f.clone());
    std::fs::write(&f, &data)?;
    let _ = rep.step(1.0);
    rep.log(format!(
        "  {} {:.1} MB, {prims} primitives, {mats} materials, {imgs} images ({:.1}s)",
        f.display(),
        data.len() as f64 / 1048576.0,
        t0.elapsed().as_secs_f64()
    ));
    Ok(files.keep())
}
