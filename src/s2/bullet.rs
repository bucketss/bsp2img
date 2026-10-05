use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result};
use glam::{DAffine3, DVec3, Vec3};
use rayon::prelude::*;

use super::entities::{self, Ent};
use super::kv3::Value;
use super::phys::{affine, model_phys};
use super::resource::Res;
use super::surfprop::{Surf, Surfaces};
use super::vpk::Fs;
use super::world::{self, Xf, flatten};

pub const MASK: u32 = 0x1c_300b;
pub const PASSBULLETS: u32 = 1 << 13;
pub const SKIN: f64 = 1.0 / 32.0;
const LEAF: usize = 4;
const BOX_PAD: f32 = 0.01;

const LAYERS: [(&str, u32); 21] = [
    ("solid", 0),
    ("hitboxes", 1),
    ("trigger", 2),
    ("sky", 3),
    ("playerclip", 4),
    ("npcclip", 5),
    ("blocklos", 6),
    ("blocklight", 7),
    ("ladder", 8),
    ("pickup", 9),
    ("blocksound", 10),
    ("window", 12),
    ("passbullets", 13),
    ("worldgeometry", 14),
    ("water", 15),
    ("slime", 16),
    ("touchall", 17),
    ("player", 18),
    ("npc", 19),
    ("debris", 20),
    ("physics_prop", 21),
];

const SOLID_ENTS: &[&str] = &[
    "func_door",
    "func_door_rotating",
    "prop_door_rotating",
    "func_brush",
    "func_breakable",
    "prop_dynamic",
    "prop_physics_multiplayer",
    "prop_physics",
    "prop_physics_override",
];

#[derive(Clone, Copy, Debug)]
pub struct Crossing {
    pub t: f64,
    pub exit: bool,
    pub normal: DVec3,
    pub surf: u32,
    pub layers: u32,
    pub ent: u32,
    pub hull: bool,
}

#[derive(Clone, Copy)]
struct Shape {
    layers: u32,
    ent: u32,
}

struct Tri {
    p: [Vec3; 3],
    surf: u32,
    shape: u32,
}

struct Hull {
    first: u32,
    count: u32,
    lo: DVec3,
    hi: DVec3,
    surf: u32,
    shape: u32,
}

#[derive(Clone, Copy)]
struct Node {
    lo: Vec3,
    hi: Vec3,
    first: u32,
    count: u32,
}

struct Bvh {
    nodes: Vec<Node>,
}

impl Bvh {
    fn build(boxes: &[(Vec3, Vec3)]) -> (Bvh, Vec<u32>) {
        let mut idx: Vec<u32> = (0..boxes.len() as u32).collect();
        let cent: Vec<Vec3> = boxes.iter().map(|(a, b)| (*a + *b) * 0.5).collect();
        let mut nodes = vec![Node { lo: Vec3::ZERO, hi: Vec3::ZERO, first: 0, count: 0 }];
        let mut stack = vec![(0usize, 0usize, boxes.len())];
        while let Some((ni, s, e)) = stack.pop() {
            let (mut lo, mut hi) = (Vec3::INFINITY, Vec3::NEG_INFINITY);
            let (mut clo, mut chi) = (Vec3::INFINITY, Vec3::NEG_INFINITY);
            for &i in &idx[s..e] {
                let (a, b) = boxes[i as usize];
                lo = lo.min(a);
                hi = hi.max(b);
                clo = clo.min(cent[i as usize]);
                chi = chi.max(cent[i as usize]);
            }
            nodes[ni].lo = lo - Vec3::splat(BOX_PAD);
            nodes[ni].hi = hi + Vec3::splat(BOX_PAD);
            if e - s <= LEAF {
                nodes[ni].first = s as u32;
                nodes[ni].count = (e - s) as u32;
                continue;
            }
            let ext = chi - clo;
            let ax = if ext.x >= ext.y && ext.x >= ext.z { 0 } else if ext.y >= ext.z { 1 } else { 2 };
            let mid = (s + e) / 2;
            idx[s..e].select_nth_unstable_by(mid - s, |&a, &b| cent[a as usize][ax].total_cmp(&cent[b as usize][ax]));
            let l = nodes.len();
            nodes.push(Node { lo: Vec3::ZERO, hi: Vec3::ZERO, first: 0, count: 0 });
            nodes.push(Node { lo: Vec3::ZERO, hi: Vec3::ZERO, first: 0, count: 0 });
            nodes[ni].first = l as u32;
            stack.push((l + 1, mid, e));
            stack.push((l, s, mid));
        }
        (Bvh { nodes }, idx)
    }

    fn visit(&self, o: DVec3, d: DVec3, mut f: impl FnMut(usize)) {
        if self.nodes.is_empty() || self.nodes[0].lo.x > self.nodes[0].hi.x {
            return;
        }
        let inv = d.recip();
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let n = &self.nodes[stack[sp] as usize];
            if !slab(o, d, inv, n.lo.as_dvec3(), n.hi.as_dvec3()) {
                continue;
            }
            if n.count > 0 {
                for i in n.first..n.first + n.count {
                    f(i as usize);
                }
            } else {
                stack[sp] = n.first + 1;
                stack[sp + 1] = n.first;
                sp += 2;
            }
        }
    }
}

fn slab(o: DVec3, d: DVec3, inv: DVec3, lo: DVec3, hi: DVec3) -> bool {
    let (mut tn, mut tf) = (0.0f64, 1.0f64);
    for k in 0..3 {
        if d[k] == 0.0 {
            if o[k] < lo[k] || o[k] > hi[k] {
                return false;
            }
            continue;
        }
        let a = (lo[k] - o[k]) * inv[k];
        let b = (hi[k] - o[k]) * inv[k];
        tn = tn.max(a.min(b));
        tf = tf.min(a.max(b));
    }
    tn <= tf + 1e-6
}

pub struct Bullet {
    tris: Vec<Tri>,
    tri_bvh: Bvh,
    planes: Vec<[f64; 4]>,
    hulls: Vec<Hull>,
    hull_bvh: Bvh,
    shapes: Vec<Shape>,
    pub surfs: Vec<Surf>,
    pub ents: Vec<String>,
    pub stats: Stats,
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub tris: usize,
    pub hulls: usize,
    pub skipped_tris: usize,
    pub skipped_hulls: usize,
    pub ents: usize,
    pub secs: f64,
}

fn layers_of(attr: &Value) -> u32 {
    let names = attr.get("m_InteractAsStrings").arr();
    if names.is_empty() {
        return 1;
    }
    names.iter().filter_map(|n| LAYERS.iter().find(|(l, _)| n.s().eq_ignore_ascii_case(l))).fold(0, |b, (_, i)| b | 1 << i)
}

fn f32s(b: &[u8]) -> impl Iterator<Item = f32> + '_ {
    b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap()))
}

fn bytes_or_ints(v: &Value) -> Vec<u32> {
    match v {
        Value::Blob(b) => b.iter().map(|&x| u32::from(x)).collect(),
        v => v.arr().iter().map(|x| x.i() as u32).collect(),
    }
}

fn truthy(kv: &HashMap<String, String>, k: &str) -> bool {
    kv.get(k).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

pub fn solid_ent(kv: &HashMap<String, String>) -> bool {
    let cls = kv.get("classname").map_or("", |c| c.as_str());
    let off = truthy(kv, "startdisabled");
    if cls == "func_brush" {
        let s = kv.get("solidity").map_or("0", |v| v.as_str());
        return s == "2" || (s != "1" && !off);
    }
    if cls.starts_with("prop_dynamic") {
        return kv.get("solid").map_or("6", |v| v.as_str()) != "0" && !off;
    }
    !off && (SOLID_ENTS.contains(&cls) || cls.starts_with("func_door"))
}

#[derive(Default)]
struct Builder {
    tris: Vec<Tri>,
    planes: Vec<[f64; 4]>,
    hulls: Vec<Hull>,
    shapes: Vec<Shape>,
    surfs: Vec<Surf>,
    surf_ids: HashMap<String, u32>,
    ents: Vec<String>,
    stats: Stats,
}

impl Builder {
    fn surf(&mut self, s: Surf) -> u32 {
        if let Some(&i) = self.surf_ids.get(&s.name) {
            return i;
        }
        let i = self.surfs.len() as u32;
        self.surf_ids.insert(s.name.clone(), i);
        self.surfs.push(s);
        i
    }

    fn add(&mut self, ph: &Value, xf: &DAffine3, sp: &Surfaces, ent: &str) {
        let layers: Vec<u32> = ph.get("m_collisionAttributes").arr().iter().map(layers_of).collect();
        let mut lut: Vec<u32> = Vec::new();
        for h in ph.get("m_surfacePropertyHashes").arr() {
            let s = sp.props(sp.name_of(h.i() as u32));
            lut.push(self.surf(s));
        }
        if lut.is_empty() {
            let s = sp.props("default");
            lut.push(self.surf(s));
        }
        let ei = self.ents.len() as u32;
        self.ents.push(ent.to_string());
        let poses = ph.get("m_bindPose").arr();
        for (pi, part) in ph.get("m_parts").arr().iter().enumerate() {
            let pose = poses.get(pi).map_or(DAffine3::IDENTITY, |p| affine(&Xf::from34(&flatten(p))));
            let m = *xf * pose;
            let r = m.matrix3;
            let rn = r.inverse().transpose();
            let flip = r.determinant() < 0.0;
            let sh = part.get("m_rnShape");
            for h in sh.get("m_hulls").arr() {
                let ly = layers.get(h.get("m_nCollisionAttributeIndex").i() as usize).copied().unwrap_or(1);
                if ly & MASK == 0 {
                    self.stats.skipped_hulls += 1;
                    continue;
                }
                let si = h.get("m_nSurfacePropertyIndex").i() as usize;
                let surf = lut[if si < lut.len() { si } else { 0 }];
                let hv = h.get("m_Hull");
                let first = self.planes.len() as u32;
                let pl: Vec<f32> = f32s(hv.get("m_Planes").blob()).collect();
                for p in pl.chunks_exact(4) {
                    let nl = DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
                    let n = (rn * nl).normalize();
                    let w = n.dot(m.transform_point3(nl * p[3] as f64));
                    self.planes.push([n.x, n.y, n.z, w]);
                }
                let mut vb = hv.get("m_VertexPositions").blob();
                if vb.is_empty() {
                    vb = hv.get("m_Vertices").blob();
                }
                let vs: Vec<f32> = f32s(vb).collect();
                let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
                for v in vs.chunks_exact(3) {
                    let q = m.transform_point3(DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64));
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
                let shape = self.shapes.len() as u32;
                self.shapes.push(Shape { layers: ly, ent: ei });
                self.hulls.push(Hull { first, count: self.planes.len() as u32 - first, lo, hi, surf, shape });
            }
            for me in sh.get("m_meshes").arr() {
                let mv = me.get("m_Mesh");
                let idx: Vec<u32> = mv.get("m_Triangles").blob().chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap()) as u32).collect();
                let ly = layers.get(me.get("m_nCollisionAttributeIndex").i() as usize).copied().unwrap_or(1);
                if ly & MASK == 0 {
                    self.stats.skipped_tris += idx.len() / 3;
                    continue;
                }
                let verts: Vec<Vec3> = f32s(mv.get("m_Vertices").blob())
                    .collect::<Vec<_>>()
                    .chunks_exact(3)
                    .map(|v| m.transform_point3(DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)).as_vec3())
                    .collect();
                let mats = bytes_or_ints(mv.get("m_Materials"));
                let def = me.get("m_nSurfacePropertyIndex").i() as u32;
                let shape = self.shapes.len() as u32;
                self.shapes.push(Shape { layers: ly, ent: ei });
                for (ti, t) in idx.chunks_exact(3).enumerate() {
                    let Some(p) = t.iter().map(|&i| verts.get(i as usize).copied()).collect::<Option<Vec<_>>>() else { continue };
                    let p = if flip { [p[0], p[2], p[1]] } else { [p[0], p[1], p[2]] };
                    let mi = if mats.is_empty() { def } else { mats.get(ti).copied().unwrap_or(0) };
                    let surf = lut[(mi as usize).min(lut.len() - 1)];
                    self.tris.push(Tri { p, surf, shape });
                }
            }
        }
    }

    fn finish(mut self) -> Bullet {
        let boxes: Vec<(Vec3, Vec3)> = self.tris.iter().map(|t| (t.p[0].min(t.p[1]).min(t.p[2]), t.p[0].max(t.p[1]).max(t.p[2]))).collect();
        let (tri_bvh, order) = Bvh::build(&boxes);
        let mut tris: Vec<Option<Tri>> = std::mem::take(&mut self.tris).into_iter().map(Some).collect();
        let tris: Vec<Tri> = order.iter().map(|&i| tris[i as usize].take().unwrap()).collect();
        let pad = DVec3::splat(2.0 * SKIN);
        let boxes: Vec<(Vec3, Vec3)> = self.hulls.iter().map(|h| ((h.lo - pad).as_vec3(), (h.hi + pad).as_vec3())).collect();
        let (hull_bvh, order) = Bvh::build(&boxes);
        let mut hulls: Vec<Option<Hull>> = std::mem::take(&mut self.hulls).into_iter().map(Some).collect();
        let hulls: Vec<Hull> = order.iter().map(|&i| hulls[i as usize].take().unwrap()).collect();
        self.stats.tris = tris.len();
        self.stats.hulls = hulls.len();
        Bullet {
            tris,
            tri_bvh,
            planes: self.planes,
            hulls,
            hull_bvh,
            shapes: self.shapes,
            surfs: self.surfs,
            ents: self.ents,
            stats: self.stats,
        }
    }
}

pub fn load(fs: &Fs, map: &str, sp: &Surfaces) -> Result<Bullet> {
    let t = Instant::now();
    let mut b = Builder::default();
    let name = format!("maps/{map}/world_physics.vmdl_c");
    let world_phys = Res::new(fs.read(&name)?)?.kv("PHYS").with_context(|| name.clone())?;
    b.add(&world_phys, &DAffine3::IDENTITY, sp, "worldent");
    drop(world_phys);
    let w = world::load(fs, map).context("loading world")?;
    let lumps = entities::load(fs, &w.lumps).context("loading entities")?;
    let ents: Vec<&Ent> = lumps.ents.iter().filter(|e| e.vmdl.is_some() && solid_ent(&e.kv)).collect();
    let mut names: Vec<&str> = ents.iter().filter_map(|e| e.vmdl.as_deref()).collect();
    names.sort_unstable();
    names.dedup();
    let models: HashMap<&str, Value> = names.par_iter().filter_map(|&m| Some((m, model_phys(fs, m)?))).collect();
    for e in ents {
        let Some(p) = e.vmdl.as_deref().and_then(|m| models.get(m)) else { continue };
        b.add(p, &affine(&e.xf), sp, e.kv.get("classname").map_or("", |c| c.as_str()));
        b.stats.ents += 1;
    }
    b.stats.secs = t.elapsed().as_secs_f64();
    Ok(b.finish())
}

impl Bullet {
    pub fn surf(&self, i: u32) -> &Surf {
        &self.surfs[i as usize]
    }

    fn tri_hit(&self, i: usize, o: DVec3, d: DVec3) -> Option<Crossing> {
        let t = &self.tris[i];
        let v0 = t.p[0].as_dvec3();
        let e1 = t.p[1].as_dvec3() - v0;
        let e2 = t.p[2].as_dvec3() - v0;
        let p = d.cross(e2);
        let det = e1.dot(p);
        if det.abs() <= 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let s = o - v0;
        let u = s.dot(p) * inv;
        let q = s.cross(e1);
        let v = q.dot(d) * inv;
        let tt = e2.dot(q) * inv;
        if !(u >= 0.0 && v >= 0.0 && u + v <= 1.0 && (0.0..=1.0).contains(&tt)) {
            return None;
        }
        let c = e1.cross(e2);
        let n = c / c.length().max(1e-30);
        let sh = self.shapes[t.shape as usize];
        Some(Crossing { t: tt, exit: n.dot(d) > 0.0, normal: n, surf: t.surf, layers: sh.layers, ent: sh.ent, hull: false })
    }

    fn hull_hits(&self, i: usize, o: DVec3, d: DVec3, out: &mut Vec<Crossing>) {
        let h = &self.hulls[i];
        let pl = &self.planes[h.first as usize..(h.first + h.count) as usize];
        let (mut tin, mut ji) = (f64::NEG_INFINITY, usize::MAX);
        let (mut tout, mut jo) = (f64::INFINITY, usize::MAX);
        let mut den_o = 0.0;
        for (j, p) in pl.iter().enumerate() {
            let n = DVec3::new(p[0], p[1], p[2]);
            let den = n.dot(d);
            let dist = n.dot(o) - p[3] - SKIN;
            if den.abs() < 1e-12 {
                if dist > 0.0 {
                    return;
                }
                continue;
            }
            let t = -dist / den;
            if den < 0.0 {
                if ji == usize::MAX || t > tin {
                    tin = t;
                    ji = j;
                }
            } else if jo == usize::MAX || t < tout {
                tout = t;
                jo = j;
                den_o = den;
            }
        }
        if tin >= tout || tout < 0.0 || tin > 1.0 {
            return;
        }
        let to = if jo != usize::MAX { tout - 2.0 * SKIN / den_o.abs() } else { tout };
        if to <= tin {
            return;
        }
        let sh = self.shapes[h.shape as usize];
        let nrm = |j: usize| DVec3::new(pl[j][0], pl[j][1], pl[j][2]);
        let c = Crossing { t: 0.0, exit: false, normal: DVec3::ZERO, surf: h.surf, layers: sh.layers, ent: sh.ent, hull: true };
        if tin >= 0.0 {
            out.push(Crossing { t: tin, normal: nrm(ji), ..c });
        }
        if to <= 1.0 {
            out.push(Crossing { t: to, exit: true, normal: nrm(jo), ..c });
        }
    }

    pub fn query(&self, o: DVec3, d: DVec3, out: &mut Vec<Crossing>) {
        out.clear();
        self.tri_bvh.visit(o, d, |i| {
            if let Some(c) = self.tri_hit(i, o, d) {
                out.push(c);
            }
        });
        let pad = DVec3::splat(2.0 * SKIN);
        let inv = d.recip();
        self.hull_bvh.visit(o, d, |i| {
            let h = &self.hulls[i];
            if slab(o, d, inv, h.lo - pad, h.hi + pad) {
                self.hull_hits(i, o, d, out);
            }
        });
        out.sort_by(|a, b| a.t.total_cmp(&b.t));
    }

    pub fn first_entry(&self, o: DVec3, d: DVec3) -> Option<f64> {
        let mut buf = Vec::new();
        self.query(o, d, &mut buf);
        buf.iter().find(|c| !c.exit).map(|c| c.t)
    }
}
