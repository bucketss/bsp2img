#![allow(dead_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Result, anyhow, bail};
use vmdl::mdl::ModelFlags;
use vmdl::vtx::StripFlags;
use vmdl::{Mdl, Vtx, Vvd};

pub const VTX_EXTS: [&str; 4] = [".dx90.vtx", ".vtx", ".dx80.vtx", ".sw.vtx"];

pub struct Mesh {
    pub material: usize,
    pub pos: Vec<[f32; 3]>,
    pub normal: Vec<[f32; 3]>,
    pub uv: Vec<[f32; 2]>,
    pub idx: Vec<u32>,
}

impl Mesh {
    pub fn tris(&self) -> usize {
        self.idx.len() / 3
    }
}

pub struct Model {
    pub version: i32,
    pub checksum: u32,
    pub static_prop: bool,
    pub meshes: Vec<Mesh>,
    pub textures: Vec<String>,
    pub cdtextures: Vec<String>,
    pub skins: Vec<Vec<u16>>,
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    pub groups: Vec<Group>,
}

#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub mesh: Option<usize>,
    pub first: usize,
    pub verts: usize,
}

pub fn norm_name(name: &str) -> String {
    let s = name.trim().replace('\\', "/").to_lowercase();
    let mut s = s.as_str();
    while let Some(r) = s.strip_prefix("./").or_else(|| s.strip_prefix('/')) {
        s = r;
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !(c == '/' && out.ends_with('/')) {
            out.push(c);
        }
    }
    out
}

fn i32_at(d: &[u8], o: usize) -> Result<i32> {
    d.get(o..o + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| anyhow!("MDL header truncated"))
}

impl Model {
    pub fn load(name: &str, read: impl Fn(&str) -> Option<Vec<u8>>) -> Result<Model> {
        let name = norm_name(name);
        let base = name.strip_suffix(".mdl").unwrap_or(&name);
        let mdl = read(&format!("{base}.mdl")).ok_or_else(|| anyhow!("{name}: not found"))?;
        let vvd = read(&format!("{base}.vvd")).ok_or_else(|| anyhow!("{name}: no .vvd"))?;
        let vtx = VTX_EXTS.iter().find_map(|e| read(&format!("{base}{e}"))).ok_or_else(|| anyhow!("{name}: no .vtx"))?;
        Model::parse(&mdl, &vvd, &vtx).map_err(|e| anyhow!("{name}: {e}"))
    }

    pub fn parse(mdl: &[u8], vvd: &[u8], vtx: &[u8]) -> Result<Model> {
        match catch_unwind(AssertUnwindSafe(|| build(mdl, vvd, vtx))) {
            Ok(r) => r,
            Err(_) => bail!("model parser panicked"),
        }
    }

    pub fn tris(&self) -> usize {
        self.meshes.iter().map(Mesh::tris).sum()
    }

    pub fn verts(&self) -> usize {
        self.meshes.iter().map(|m| m.pos.len()).sum()
    }

    pub fn texture(&self, skin: usize, material: usize) -> Option<usize> {
        let fam = self.skins.get(skin).or(self.skins.first())?;
        fam.get(material).map(|&t| t as usize).or(Some(material)).filter(|&t| t < self.textures.len())
    }

    pub fn vmt_paths(&self, tex: usize) -> Vec<String> {
        let Some(t) = self.textures.get(tex) else { return Vec::new() };
        let t = t.replace('\\', "/");
        let t = t.strip_suffix(".vmt").unwrap_or(&t);
        let mut out: Vec<String> = Vec::new();
        for cd in &self.cdtextures {
            let cd = cd.replace('\\', "/");
            let p = norm_name(&format!("materials/{}/{}.vmt", cd.trim_matches('/'), t.trim_matches('/')));
            if !out.contains(&p) {
                out.push(p);
            }
        }
        if self.cdtextures.is_empty() {
            out.push(norm_name(&format!("materials/{}.vmt", t.trim_matches('/'))));
        }
        out
    }
}

fn build(mdl_b: &[u8], vvd_b: &[u8], vtx_b: &[u8]) -> Result<Model> {
    let mdl = Mdl::read(mdl_b).map_err(|e| anyhow!("mdl: {e}"))?;
    let vvd = Vvd::read(vvd_b).map_err(|e| anyhow!("vvd: {e}"))?;
    let vtx = Vtx::read(vtx_b).map_err(|e| anyhow!("vtx: {e}"))?;
    let version = mdl.header.version;
    let checksum = i32_at(mdl_b, 8)? as u32;
    let refs = i32_at(mdl_b, 220)?.max(0) as usize;
    let fams = i32_at(mdl_b, 224)?.max(0) as usize;
    let skins: Vec<Vec<u16>> = if refs == 0 {
        Vec::new()
    } else {
        mdl.skin_table.chunks(refs).take(fams.max(1)).map(|c| c.to_vec()).collect()
    };
    if vtx.body_parts.len() != mdl.body_parts.len() {
        bail!("vtx has {} body parts, mdl {}", vtx.body_parts.len(), mdl.body_parts.len());
    }
    let verts = &vvd.vertices;
    let mut meshes = Vec::new();
    let mut groups = Vec::new();
    for (bp, vbp) in mdl.body_parts.iter().zip(&vtx.body_parts) {
        if vbp.models.len() != bp.models.len() {
            bail!("vtx has {} models in a body part, mdl {}", vbp.models.len(), bp.models.len());
        }
        for (mi, (m, vm)) in bp.models.iter().zip(&vbp.models).enumerate() {
            let Some(lod) = vm.lods.first() else { continue };
            if lod.meshes.len() != m.meshes.len() {
                bail!("vtx lod 0 has {} meshes, mdl {}", lod.meshes.len(), m.meshes.len());
            }
            for (me, vme) in m.meshes.iter().zip(&lod.meshes) {
                let base = m.vertex_offset as i64 + me.vertex_offset as i64;
                let mut out = Mesh { material: me.material.max(0) as usize, pos: Vec::new(), normal: Vec::new(), uv: Vec::new(), idx: Vec::new() };
                let mesh_index = meshes.len();
                for sg in &vme.strip_groups {
                    let first = out.pos.len();
                    groups.push(Group { mesh: (mi == 0).then_some(mesh_index), first, verts: sg.vertices.len() });
                    if mi != 0 {
                        continue;
                    }
                    for v in &sg.vertices {
                        let id = base + v.original_mesh_vertex_id as i64;
                        let Some(src) = usize::try_from(id).ok().and_then(|i| verts.get(i)) else {
                            bail!("vertex {id} out of range ({} in vvd)", verts.len());
                        };
                        out.pos.push(src.position.into());
                        out.normal.push(src.normal.into());
                        out.uv.push(src.texture_coordinates);
                    }
                    for st in &sg.strips {
                        if st.flags.contains(StripFlags::IS_TRI_STRIP) {
                            bail!("triangle strips are not supported");
                        }
                        let tri: Vec<usize> = st.indices().collect();
                        if !tri.len().is_multiple_of(3) {
                            bail!("strip index count {} is not a multiple of 3", tri.len());
                        }
                        for t in tri.chunks_exact(3) {
                            let mut k = [0u32; 3];
                            for (o, &p) in k.iter_mut().zip(t) {
                                let gi = *sg.indices.get(p).ok_or_else(|| anyhow!("strip index {p} out of range"))? as usize;
                                if gi >= sg.vertices.len() {
                                    bail!("group index {gi} out of range ({})", sg.vertices.len());
                                }
                                *o = (first + gi) as u32;
                            }
                            out.idx.extend_from_slice(&k);
                        }
                    }
                }
                if mi == 0 {
                    meshes.push(out);
                }
            }
        }
    }
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for p in meshes.iter().flat_map(|m| &m.pos) {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    if lo[0] > hi[0] {
        lo = mdl.header.view_bounding_box[0].into();
        hi = mdl.header.view_bounding_box[1].into();
    }
    Ok(Model {
        version,
        checksum,
        static_prop: mdl.header.flags.contains(ModelFlags::STATIC_PROP),
        meshes,
        textures: mdl.textures.iter().map(|t| t.name.clone()).collect(),
        cdtextures: mdl.texture_paths.clone(),
        skins,
        lo,
        hi,
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mdl")) {
                out.push(p);
            }
        }
    }

    #[test]
    fn names() {
        assert_eq!(norm_name("./Models\\Props//x.MDL"), "models/props/x.mdl");
        assert_eq!(norm_name("/models/a.mdl"), "models/a.mdl");
    }

    #[test]
    #[ignore]
    fn all_props() {
        let Ok(roots) = std::env::var("S1_PROPS") else { return };
        for root in roots.split(';') {
            let root = Path::new(root);
            let mut files = Vec::new();
            walk(root, &mut files);
            files.sort();
            let t = std::time::Instant::now();
            let (mut ok, mut tris, mut verts, mut agree, mut against, mut flat, mut nonstatic) = (0, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
            let mut vers: BTreeMap<i32, usize> = BTreeMap::new();
            let mut errs: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut fams: BTreeMap<usize, usize> = BTreeMap::new();
            let mut bad_tex = 0;
            let (mut read_t, mut parse_t, mut multi, mut groups) = (0.0, 0.0, 0usize, 0usize);
            for f in &files {
                let rel = f.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                let t0 = std::time::Instant::now();
                let base = rel.strip_suffix(".mdl").unwrap();
                let parts: Vec<Option<Vec<u8>>> = [".mdl", ".vvd", ".dx90.vtx"].iter().map(|e| std::fs::read(root.join(format!("{base}{e}"))).ok()).collect();
                read_t += t0.elapsed().as_secs_f64();
                let t0 = std::time::Instant::now();
                let r = match &parts[..] {
                    [Some(a), Some(b), Some(c)] => Model::parse(a, b, c),
                    _ => Err(anyhow!("missing files")),
                };
                parse_t += t0.elapsed().as_secs_f64();
                match r {
                    Ok(m) => {
                        ok += 1;
                        *vers.entry(m.version).or_default() += 1;
                        *fams.entry(m.skins.len()).or_default() += 1;
                        tris += m.tris();
                        verts += m.verts();
                        nonstatic += !m.static_prop as usize;
                        multi += m.groups.iter().any(|g| g.mesh.is_none()) as usize;
                        groups += m.groups.len();
                        for me in &m.meshes {
                            if m.texture(0, me.material).is_none() {
                                bad_tex += 1;
                            }
                            for t in me.idx.chunks_exact(3) {
                                let [a, b, c] = [t[0], t[1], t[2]].map(|i| me.pos[i as usize]);
                                let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                                let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                                let g = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                                let n: [f32; 3] = std::array::from_fn(|k| t.iter().map(|&i| me.normal[i as usize][k]).sum());
                                let d = g[0] * n[0] + g[1] * n[1] + g[2] * n[2];
                                if d > 0.0 {
                                    agree += 1;
                                } else if d < 0.0 {
                                    against += 1;
                                } else {
                                    flat += 1;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let s = e.to_string();
                        let k = s.split_once(": ").map_or(s.clone(), |(_, r)| r.to_string());
                        errs.entry(k).or_default().push(rel);
                    }
                }
            }
            println!("{}: {ok} of {} loaded in {:.2}s, {tris} tris, {verts} verts", root.display(), files.len(), t.elapsed().as_secs_f64());
            println!("  read {read_t:.2}s parse {parse_t:.2}s; {groups} lod 0 strip groups; {multi} models with extra body part models");
            println!("  versions {vers:?} skin families {fams:?} not $staticprop {nonstatic} meshes without texture {bad_tex}");
            println!("  winding vs normals: agree {agree} against {against} degenerate {flat}");
            for (k, v) in &errs {
                println!("  {} x {k} e.g. {}", v.len(), v[0]);
            }
        }
    }
}
