use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};
use glam::{DMat4, DQuat, DVec3, EulerRot};

use crate::actors::{self, Show};
use crate::bsp::{Bsp, CONTENTS_SOLID, Rd, TEX_SPECIAL, cstr};
use crate::light::LightParams;
use crate::mesh::{Batch, Chunk, Image, Mesh, Mode, Vertex, pack_rgb9e5};
use crate::reach::Reach;

const FLAT: u32 = 1;
const FULLBRIGHT: u32 = 4;
const ADDITIVE: u32 = 0x20;
const MASKED: u32 = 0x40;
const ACT_IDLE: i32 = 1;
const DROP_MAX: f64 = 512.0;
const LIGHT_DOWN: f64 = 2048.0;
pub const SUN: [f64; 3] = [0.3, 0.2, 0.93];
pub const SHADE_MIN: f64 = 0.6;

pub struct Part {
    pub tex: usize,
    pub pos: Vec<DVec3>,
    pub normal: Vec<DVec3>,
    pub uv: Vec<[f32; 2]>,
}

pub struct Texture {
    pub name: String,
    pub flags: u32,
    pub image: Image,
}

pub struct Model {
    pub parts: Vec<Part>,
    pub textures: Vec<Texture>,
}

fn need(d: &[u8], o: usize, n: usize) -> Result<()> {
    ensure!(o.checked_add(n).is_some_and(|e| e <= d.len()), "offset {o}+{n} past end ({})", d.len());
    Ok(())
}

fn read_textures(d: &[u8]) -> Result<(Vec<Texture>, Vec<u16>, usize)> {
    let r = Rd { data: d };
    let (nt, ti) = (r.i32(180).max(0) as usize, r.i32(184).max(0) as usize);
    let (nref, nfam, si) = (r.i32(192).max(0) as usize, r.i32(196).max(0) as usize, r.i32(200).max(0) as usize);
    need(d, ti, nt * 80)?;
    let mut out = Vec::with_capacity(nt);
    for i in 0..nt {
        let o = ti + i * 80;
        let (flags, w, h, idx) = (r.u32(o + 64), r.i32(o + 68).max(0) as usize, r.i32(o + 72).max(0) as usize, r.i32(o + 76).max(0) as usize);
        ensure!(w > 0 && h > 0 && w <= 4096 && h <= 4096, "texture {i} size {w}x{h}");
        need(d, idx, w * h + 768)?;
        let pal = &d[idx + w * h..idx + w * h + 768];
        let mut rgba = vec![0u8; w * h * 4];
        for (k, &p) in d[idx..idx + w * h].iter().enumerate() {
            if flags & MASKED != 0 && p == 255 {
                continue;
            }
            let c = &pal[p as usize * 3..p as usize * 3 + 3];
            rgba[k * 4..k * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
        out.push(Texture { name: cstr(&d[o..o + 64]), flags, image: Image { w: w as u32, h: h as u32, rgba } });
    }
    need(d, si, nref * nfam.max(1) * 2)?;
    let skins = (0..nref).map(|k| r.u16(si + k * 2)).collect();
    Ok((out, skins, nref))
}

fn pick_seq(r: &Rd, ns: usize, si: usize) -> usize {
    let label = |i: usize| cstr(&r.data[si + i * 176..si + i * 176 + 32]).to_lowercase();
    (0..ns).find(|&i| r.i32(si + i * 176 + 40) == ACT_IDLE).or_else(|| (0..ns).find(|&i| label(i).contains("idle"))).unwrap_or(0)
}

fn pose(d: &[u8], path: &Path) -> Result<Vec<DMat4>> {
    let r = Rd { data: d };
    let (nb, bi) = (r.i32(140).max(0) as usize, r.i32(144).max(0) as usize);
    let (ns, si) = (r.i32(164).max(0) as usize, r.i32(168).max(0) as usize);
    ensure!(nb > 0 && nb <= 256, "{nb} bones");
    need(d, bi, nb * 112)?;
    need(d, si, ns * 176)?;
    let seq = (ns > 0).then(|| si + pick_seq(&r, ns, si) * 176);
    let group = seq.map_or(0, |s| r.i32(s + 156).max(0) as usize);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = (group > 0).then(|| std::fs::read(path.with_file_name(format!("{stem}{group:02}.mdl"))).ok()).flatten();
    let (anim, ad): (Option<usize>, &[u8]) = match (seq, &ext) {
        (None, _) => (None, d),
        (Some(s), _) if group == 0 => (Some(r.i32(s + 124).max(0) as usize), d),
        (Some(s), Some(b)) => (Some(r.i32(s + 124).max(0) as usize), b.as_slice()),
        (Some(_), None) => (None, d),
    };
    let ar = Rd { data: ad };
    let mut world: Vec<DMat4> = Vec::with_capacity(nb);
    for i in 0..nb {
        let o = bi + i * 112;
        let parent = r.i32(o + 32);
        let mut v = [0f64; 6];
        for (j, x) in v.iter_mut().enumerate() {
            *x = r.f32(o + 64 + j * 4) as f64;
            let Some(a) = anim else { continue };
            let base = a + i * 12;
            if base + 12 > ad.len() {
                continue;
            }
            let off = ar.u16(base + j * 2) as usize;
            if off == 0 || base + off + 4 > ad.len() {
                continue;
            }
            let p = base + off;
            let raw = if ar.u8(p) > 0 { ar.i16(p + 2) } else { ar.i16(p) };
            *x += raw as f64 * r.f32(o + 88 + j * 4) as f64;
        }
        let q = DQuat::from_euler(EulerRot::ZYX, v[5], v[4], v[3]);
        let local = DMat4::from_rotation_translation(q, DVec3::new(v[0], v[1], v[2]));
        let m = if parent >= 0 && (parent as usize) < i { world[parent as usize] * local } else { local };
        world.push(m);
    }
    Ok(world)
}

pub fn load(path: &Path) -> Result<Model> {
    let d = std::fs::read(path)?;
    ensure!(d.len() >= 244 && &d[0..4] == b"IDST", "not a studio model");
    let r = Rd { data: &d };
    ensure!(r.i32(4) == 10, "studio version {} (want 10)", r.i32(4));
    let tfile;
    let (textures, skins, _) = if r.i32(180) > 0 {
        read_textures(&d)?
    } else {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        tfile = std::fs::read(path.with_file_name(format!("{stem}t.mdl"))).or_else(|_| std::fs::read(path.with_file_name(format!("{stem}T.mdl"))))?;
        ensure!(tfile.len() >= 244 && &tfile[0..4] == b"IDST", "bad texture file");
        read_textures(&tfile)?
    };
    let bones = pose(&d, path)?;
    let bone_info = |o: usize, n: usize| -> Result<Vec<u8>> {
        need(&d, o, n)?;
        Ok(d[o..o + n].to_vec())
    };
    let (nbp, bpi) = (r.i32(204).max(0) as usize, r.i32(208).max(0) as usize);
    need(&d, bpi, nbp * 76)?;
    let mut parts: HashMap<usize, Part> = HashMap::new();
    for b in 0..nbp {
        let bp = bpi + b * 76;
        if r.i32(bp + 64) <= 0 {
            continue;
        }
        let m = r.i32(bp + 72).max(0) as usize;
        need(&d, m, 112)?;
        let (nmesh, mi) = (r.i32(m + 72).max(0) as usize, r.i32(m + 76).max(0) as usize);
        let (nv, vbi, vi) = (r.i32(m + 80).max(0) as usize, r.i32(m + 84).max(0) as usize, r.i32(m + 88).max(0) as usize);
        let (nn, nbi, ni) = (r.i32(m + 92).max(0) as usize, r.i32(m + 96).max(0) as usize, r.i32(m + 100).max(0) as usize);
        need(&d, vi, nv * 12)?;
        need(&d, ni, nn * 12)?;
        need(&d, mi, nmesh * 20)?;
        let vb = bone_info(vbi, nv)?;
        let nbn = bone_info(nbi, nn)?;
        let xf = |b: u8| bones.get(b as usize).copied().unwrap_or(DMat4::IDENTITY);
        let verts: Vec<DVec3> = (0..nv).map(|k| xf(vb[k]).transform_point3(DVec3::from(r.vec3(vi + k * 12).map(f64::from)))).collect();
        let norms: Vec<DVec3> =
            (0..nn).map(|k| xf(nbn[k]).transform_vector3(DVec3::from(r.vec3(ni + k * 12).map(f64::from))).normalize_or_zero()).collect();
        for me in 0..nmesh {
            let mo = mi + me * 20;
            let skinref = r.i32(mo + 8).max(0) as usize;
            let tex = skins.get(skinref).map_or(skinref, |&t| t as usize);
            if tex >= textures.len() {
                continue;
            }
            let (tw, th) = (textures[tex].image.w as f32, textures[tex].image.h as f32);
            let part = parts.entry(tex).or_insert_with(|| Part { tex, pos: Vec::new(), normal: Vec::new(), uv: Vec::new() });
            let mut p = r.i32(mo + 4).max(0) as usize;
            loop {
                need(&d, p, 2)?;
                let n = r.i16(p);
                p += 2;
                if n == 0 {
                    break;
                }
                let fan = n < 0;
                let n = n.unsigned_abs() as usize;
                need(&d, p, n * 8)?;
                let pts: Vec<(usize, usize, [f32; 2])> = (0..n)
                    .map(|k| {
                        let o = p + k * 8;
                        (r.i16(o) as usize, r.i16(o + 2) as usize, [r.i16(o + 4) as f32 / tw, r.i16(o + 6) as f32 / th])
                    })
                    .collect();
                p += n * 8;
                for k in 2..n {
                    let t = if fan { [0, k - 1, k] } else if k % 2 == 1 { [k - 1, k - 2, k] } else { [k - 2, k - 1, k] };
                    let tri = t.map(|i| pts[i]);
                    if tri.iter().any(|v| v.0 >= verts.len() || v.1 >= norms.len()) {
                        bail!("triangle index out of range");
                    }
                    let pp = tri.map(|v| verts[v.0]);
                    let nn = tri.map(|v| norms[v.1]);
                    let face = (pp[1] - pp[0]).cross(pp[2] - pp[0]);
                    let order = if face.dot(nn[0] + nn[1] + nn[2]) < 0.0 { [0, 2, 1] } else { [0, 1, 2] };
                    for o in order {
                        part.pos.push(pp[o]);
                        part.normal.push(nn[o]);
                        part.uv.push(tri[o].2);
                    }
                }
            }
        }
    }
    let mut parts: Vec<Part> = parts.into_values().filter(|p| !p.pos.is_empty()).collect();
    parts.sort_by_key(|p| p.tex);
    Ok(Model { parts, textures })
}

fn face_extents(bsp: &Bsp, fi: usize) -> ([f64; 2], [f64; 2]) {
    let f = &bsp.faces[fi];
    let ti = &bsp.texinfo[f.texinfo as usize];
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in bsp.face_points(fi) {
        for (k, v) in [ti.s, ti.t].iter().enumerate() {
            let x = (p.x as f32 * v[0] + p.y as f32 * v[1] + p.z as f32 * v[2] + v[3]) as f64;
            lo[k] = lo[k].min(x);
            hi[k] = hi[k].max(x);
        }
    }
    let mins = lo.map(|v| (v / 16.0).floor() * 16.0);
    let maxs = hi.map(|v| (v / 16.0).ceil() * 16.0);
    (mins, [maxs[0] - mins[0], maxs[1] - mins[1]])
}

fn recurse(bsp: &Bsp, node: i32, a: DVec3, b: DVec3, light: &LightParams, depth: usize) -> Option<[f64; 3]> {
    if node < 0 || depth > 4096 {
        return None;
    }
    let n = bsp.nodes.get(node as usize)?;
    let pl = &bsp.planes[n.plane as usize];
    let fa = a.dot(pl.normal) - pl.dist;
    let fb = b.dot(pl.normal) - pl.dist;
    let side = (fa < 0.0) as usize;
    if (fb < 0.0) == (fa < 0.0) {
        return recurse(bsp, n.children[side] as i32, a, b, light, depth + 1);
    }
    let mid = a + (b - a) * (fa / (fa - fb));
    if let Some(c) = recurse(bsp, n.children[side] as i32, a, mid, light, depth + 1) {
        return Some(c);
    }
    let [first, count] = *bsp.node_faces.get(node as usize)?;
    for fi in first as usize..first as usize + count as usize {
        let Some(f) = bsp.faces.get(fi) else { break };
        let ti = &bsp.texinfo[f.texinfo as usize];
        if ti.flags & TEX_SPECIAL != 0 || f.lightofs < 0 {
            continue;
        }
        let ds = (mid.x as f32 * ti.s[0] + mid.y as f32 * ti.s[1] + mid.z as f32 * ti.s[2] + ti.s[3]) as f64;
        let dt = (mid.x as f32 * ti.t[0] + mid.y as f32 * ti.t[1] + mid.z as f32 * ti.t[2] + ti.t[3]) as f64;
        let (mins, ext) = face_extents(bsp, fi);
        let (ds, dt) = (ds - mins[0], dt - mins[1]);
        if ds < 0.0 || dt < 0.0 || ds > ext[0] || dt > ext[1] {
            continue;
        }
        let lw = (ext[0] / 16.0) as usize + 1;
        let lh = (ext[1] / 16.0) as usize + 1;
        let (x, y) = (((ds / 16.0) as usize).min(lw - 1), ((dt / 16.0) as usize).min(lh - 1));
        let mut acc = [0f64; 3];
        for (k, &st) in f.styles.iter().filter(|&&s| s != 255).enumerate() {
            let o = f.lightofs as usize + k * lw * lh * 3 + (y * lw + x) * 3;
            let Some(px) = bsp.lighting.get(o..o + 3) else { break };
            if st < 32 || light.all_styles {
                for c in 0..3 {
                    acc[c] += (px[c] as f64 / 255.0).powf(light.lightgamma);
                }
            }
        }
        return Some(acc);
    }
    recurse(bsp, n.children[1 - side] as i32, mid, b, light, depth + 1)
}

pub fn light_point(bsp: &Bsp, p: DVec3, light: &LightParams) -> [f64; 3] {
    if bsp.lighting.is_empty() || bsp.nodes.is_empty() {
        return [1.0; 3];
    }
    recurse(bsp, bsp.models[0].headnode[0], p, p - DVec3::Z * LIGHT_DOWN, light, 0).unwrap_or([0.0; 3])
}

fn drop_point(bsp: &Bsp, p: DVec3) -> DVec3 {
    if bsp.leaf_contents(0, p) == CONTENTS_SOLID {
        return p;
    }
    let mut z = 0.0;
    while z < DROP_MAX && bsp.leaf_contents(0, p - DVec3::Z * (z + 1.0)) != CONTENTS_SOLID {
        z += 1.0;
    }
    if z >= DROP_MAX { p } else { p - DVec3::Z * z }
}

fn drop_player(bsp: &Bsp, p: DVec3) -> DVec3 {
    if bsp.hull_contents(p, 1) == CONTENTS_SOLID {
        return p;
    }
    let mut z = 0.0;
    while z < DROP_MAX && bsp.hull_contents(p - DVec3::Z * (z + 1.0), 1) != CONTENTS_SOLID {
        z += 1.0;
    }
    if z >= DROP_MAX { p } else { p - DVec3::Z * z }
}

fn angles(e: &crate::bsp::Entity) -> DVec3 {
    let v: Vec<f64> = e.get("angles").map(|s| s.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    match v.len() {
        0 => DVec3::new(0.0, e.get("angle").and_then(|a| a.trim().parse().ok()).unwrap_or(0.0), 0.0),
        _ => DVec3::new(v[0], v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0)),
    }
}

pub fn shade(n: DVec3) -> f64 {
    SHADE_MIN + (1.0 - SHADE_MIN) * n.dot(DVec3::from(SUN).normalize()).max(0.0)
}

pub fn push_texture(mesh: &mut Mesh, im: Image, name: String) -> usize {
    let i = mesh.textures.len();
    mesh.compressed.resize(i, None);
    mesh.anim.resize(i, None);
    mesh.names.resize(i, String::new());
    std::sync::Arc::make_mut(&mut mesh.textures).push(Some(im));
    mesh.compressed.push(None);
    mesh.anim.push(None);
    mesh.names.push(name);
    i
}

#[derive(Default, Debug)]
pub struct Added {
    pub items: usize,
    pub players: usize,
    pub missing: Vec<String>,
    pub failed: Vec<String>,
}

impl Added {
    pub fn summary(&self) -> String {
        let mut s = format!("shown: {} items, {} players", self.items, self.players);
        if !self.missing.is_empty() {
            s += &format!("; models not found: {}", self.missing.join(", "));
        }
        for f in &self.failed {
            s += &format!("; {f}");
        }
        s
    }
}

pub fn add(bsp: &Bsp, dirs: &[PathBuf], mesh: &mut Mesh, light: &LightParams, reach: Option<&Reach>, show: Show) -> Added {
    let mut out = Added::default();
    let find = |rel: &str| dirs.iter().map(|d| d.join(rel)).find(|p| p.is_file());
    let mut wanted: Vec<(String, DVec3, DVec3, bool)> = Vec::new();
    for e in bsp.entities.iter().skip(1) {
        if !show.items || !actors::is_item_class(&e.class().to_lowercase()) || e.model().is_some() {
            continue;
        }
        let (Some(m), Some(o)) = (actors::gold_item(e), e.origin()) else { continue };
        wanted.push((m, drop_point(bsp, o), angles(e), false));
    }
    if show.spawns {
        for s in actors::spawns(&bsp.entities) {
            let e = &bsp.entities[s.index];
            let list = actors::GOLD_PLAYERS[s.team];
            let n = list[s.nth % list.len()];
            wanted.push((format!("models/player/{n}/{n}.mdl"), drop_player(bsp, e.origin().unwrap()), angles(e), true));
        }
    }
    if let Some(r) = reach {
        wanted.retain(|w| r.keep_point(bsp, w.1 + DVec3::Z * 8.0));
    }
    let lut = light.tex_lut();
    let mut models: HashMap<String, Option<(Model, Vec<usize>)>> = HashMap::new();
    let mut groups: HashMap<(usize, Mode), (Vec<Vertex>, Vec<u32>)> = HashMap::new();
    for (name, origin, ang, player) in wanted {
        let entry = models.entry(name.to_lowercase()).or_insert_with(|| {
            let Some(p) = find(&name) else {
                out.missing.push(name.clone());
                return None;
            };
            match load(&p) {
                Ok(m) => {
                    let ids = m
                        .textures
                        .iter()
                        .map(|t| {
                            let mut im = t.image.clone();
                            for (k, c) in im.rgba.iter_mut().enumerate() {
                                if k % 4 != 3 {
                                    *c = lut[*c as usize];
                                }
                            }
                            push_texture(mesh, im, format!("{}/{}", name.trim_end_matches(".mdl"), t.name))
                        })
                        .collect();
                    Some((m, ids))
                }
                Err(e) => {
                    out.failed.push(format!("{name}: {e:#}"));
                    None
                }
            }
        });
        let Some((m, ids)) = entry else { continue };
        let rot = crate::s2::entities::angle_matrix(DVec3::new(-ang.x, ang.y, ang.z));
        let lin = light_point(bsp, origin + DVec3::Z * 4.0, light);
        for part in &m.parts {
            let t = &m.textures[part.tex];
            let mode = if t.flags & ADDITIVE != 0 {
                Mode::Additive
            } else if t.flags & MASKED != 0 {
                Mode::AlphaTest
            } else {
                Mode::Opaque
            };
            let (verts, idx) = groups.entry((ids[part.tex], mode)).or_default();
            for k in 0..part.pos.len() {
                let p = origin + rot * part.pos[k];
                let n = (rot * part.normal[k]).normalize_or_zero();
                let f = if t.flags & (FULLBRIGHT | FLAT) != 0 { 1.0 } else { shade(n) };
                let c = if t.flags & FULLBRIGHT != 0 { [1.0; 3] } else { lin.map(|v| light.light_to_screen(v * f) as f32 / 255.0) };
                idx.push(verts.len() as u32);
                verts.push(Vertex {
                    pos: p.as_vec3().to_array(),
                    uv: part.uv[k],
                    lm: [0.5, 0.5],
                    normal: n.as_vec3().to_array(),
                    light: pack_rgb9e5(c).max(1),
                    ..Vertex::BLANK
                });
            }
        }
        if player { out.players += 1 } else { out.items += 1 }
    }
    let mut keys: Vec<(usize, Mode)> = groups.keys().copied().collect();
    keys.sort_by_key(|k| (k.0, k.1 as u8));
    for k in keys {
        let (verts, idx) = groups.remove(&k).unwrap();
        let chunk = Chunk::new(&verts, 0, idx.len());
        mesh.faces += idx.len() / 3;
        mesh.batches.push(Batch {
            tex: k.0,
            mode: k.1,
            alpha: 1.0,
            warp: None,
            tint: [1.0; 3],
            layer2: None,
            layer3: None,
            four: None,
            tint_mask: None,
            verts,
            idx,
            chunks: vec![chunk],
            actor: true,
        });
    }
    out.missing.sort();
    out.missing.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn cs_models() {
        let Ok(dir) = std::env::var("CS_DIR") else { return };
        for n in ["models/w_ak47.mdl", "models/w_awp.mdl", "models/player/terror/terror.mdl", "models/player/gign/gign.mdl"] {
            let m = load(&Path::new(&dir).join(n)).unwrap();
            let (lo, hi) = m.parts.iter().flat_map(|p| &p.pos).fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(a, b), p| (a.min(*p), b.max(*p)));
            let tris: usize = m.parts.iter().map(|p| p.pos.len() / 3).sum();
            println!("{n}: {} textures, {tris} tris, bounds {lo:.1} .. {hi:.1}", m.textures.len());
        }
    }
}
