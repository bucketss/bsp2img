#![allow(dead_code)]

use anyhow::{Context, Result, bail};

use super::model::Model;

pub struct VhvMesh {
    pub lod: u32,
    pub verts: usize,
    pub ofs: usize,
}

pub struct Vhv<'a> {
    pub version: i32,
    pub checksum: u32,
    pub flags: u32,
    pub size: usize,
    pub verts: usize,
    pub meshes: Vec<VhvMesh>,
    data: &'a [u8],
}

fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).context("vhv truncated")
}

pub fn average(c: &[[u8; 4]; 3]) -> [u8; 3] {
    std::array::from_fn(|k| ((c[0][k] as u32 + c[1][k] as u32 + c[2][k] as u32 + 1) / 3) as u8)
}

pub fn prop_name(index: usize, hdr: bool) -> String {
    if hdr { format!("sp_hdr_{index}.vhv") } else { format!("sp_{index}.vhv") }
}

impl<'a> Vhv<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Vhv<'a>> {
        let version = u32_at(data, 0)? as i32;
        if version != 2 {
            bail!("vhv version {version}");
        }
        let checksum = u32_at(data, 4)?;
        let flags = u32_at(data, 8)?;
        let size = u32_at(data, 12)? as usize;
        let verts = u32_at(data, 16)? as usize;
        let n = u32_at(data, 20)? as usize;
        if size == 0 || !size.is_multiple_of(4) {
            bail!("vhv vertex size {size}");
        }
        let mut meshes = Vec::with_capacity(n.min(4096));
        for i in 0..n {
            let o = 40 + i * 28;
            let m = VhvMesh { lod: u32_at(data, o)?, verts: u32_at(data, o + 4)? as usize, ofs: u32_at(data, o + 8)? as usize };
            if m.ofs + m.verts * size > data.len() {
                bail!("vhv mesh {i} runs past the end ({} + {} x {size} > {})", m.ofs, m.verts, data.len());
            }
            meshes.push(m);
        }
        Ok(Vhv { version, checksum, flags, size, verts, meshes, data })
    }

    pub fn streams(&self) -> usize {
        self.size / 4
    }

    pub fn raw(&self, mesh: usize, vert: usize) -> &'a [u8] {
        let m = &self.meshes[mesh];
        let o = m.ofs + vert * self.size;
        &self.data[o..o + self.size]
    }

    pub fn rgba(&self, mesh: usize, vert: usize, stream: usize) -> [u8; 4] {
        let b = &self.raw(mesh, vert)[stream * 4..stream * 4 + 4];
        [b[2], b[1], b[0], b[3]]
    }

    pub fn lod0(&self) -> impl Iterator<Item = usize> + '_ {
        self.meshes.iter().enumerate().filter(|(_, m)| m.lod == 0).map(|(i, _)| i)
    }

    pub fn for_model(&self, model: &Model) -> Result<Vec<Vec<[[u8; 4]; 3]>>> {
        if self.checksum != model.checksum {
            bail!("vhv checksum {:08x} does not match the model's {:08x}", self.checksum, model.checksum);
        }
        let lod0: Vec<usize> = self.lod0().collect();
        if lod0.len() != model.groups.len() {
            bail!("vhv has {} lod 0 meshes, the model {} strip groups", lod0.len(), model.groups.len());
        }
        let mut out: Vec<Vec<[[u8; 4]; 3]>> = model.meshes.iter().map(|m| vec![[[0; 4]; 3]; m.pos.len()]).collect();
        let streams = self.streams();
        for (g, &vm) in model.groups.iter().zip(&lod0) {
            let nv = self.meshes[vm].verts;
            if nv != g.verts {
                bail!("vhv mesh {vm} has {nv} vertices, the strip group {}", g.verts);
            }
            let Some(mesh) = g.mesh else { continue };
            let (first, dst) = (g.first, &mut out[mesh]);
            for v in 0..nv {
                dst[first + v] = std::array::from_fn(|s| self.rgba(vm, v, s.min(streams - 1)));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s1::sprp::{GAME_LUMP, StaticProps};
    use crate::s2::vpk::Vpk;
    use std::collections::HashMap;
    use std::path::Path;

    fn u32_le(d: &[u8], o: usize) -> usize {
        u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize
    }

    fn u16_le(d: &[u8], o: usize) -> usize {
        u16::from_le_bytes([d[o], d[o + 1]]) as usize
    }

    fn lump(f: &[u8], i: usize) -> &[u8] {
        let o = 8 + i * 16;
        &f[u32_le(f, o)..u32_le(f, o) + u32_le(f, o + 4)]
    }

    fn zip_dir(z: &[u8]) -> HashMap<String, (usize, usize)> {
        let mut out = HashMap::new();
        let Some(e) = (0..z.len().saturating_sub(21)).rev().take(70000).find(|&i| z[i..i + 4] == *b"PK\x05\x06") else { return out };
        let n = u16_le(z, e + 10);
        let mut o = u32_le(z, e + 16);
        for _ in 0..n {
            assert_eq!(&z[o..o + 4], b"PK\x01\x02");
            let method = u16_le(z, o + 10);
            let size = u32_le(z, o + 20);
            let (nl, xl, cl) = (u16_le(z, o + 28), u16_le(z, o + 30), u16_le(z, o + 32));
            let lh = u32_le(z, o + 42);
            let name = String::from_utf8_lossy(&z[o + 46..o + 46 + nl]).to_lowercase().replace('\\', "/");
            assert_eq!(method, 0, "{name} is compressed");
            let data = lh + 30 + u16_le(z, lh + 26) + u16_le(z, lh + 28);
            out.insert(name, (data, size));
            o += 46 + nl + xl + cl;
        }
        out
    }

    #[test]
    #[ignore]
    fn dust2() {
        let Ok(root) = std::env::var("S1_CSGO") else { return };
        let root = Path::new(&root);
        let f = std::fs::File::open(root.join("maps").join("de_dust2.bsp")).unwrap();
        let m = unsafe { memmap2::Mmap::map(&f) }.unwrap();
        let t = std::time::Instant::now();
        let sp = StaticProps::from_bsp(&m, lump(&m, GAME_LUMP)).unwrap().unwrap();
        let pak = lump(&m, 40);
        let dir = zip_dir(pak);
        let vpk = Vpk::open(&root.join("pak01_dir.vpk")).unwrap();
        println!("sprp + zip dir + vpk dir {:.2}s, {} props, {} pak entries", t.elapsed().as_secs_f64(), sp.props.len(), dir.len());
        let read = |n: &str| -> Option<Vec<u8>> {
            let n = n.to_lowercase();
            if let Some(&(o, l)) = dir.get(&n) {
                return Some(pak[o..o + l].to_vec());
            }
            vpk.entry(&n).and_then(|e| vpk.read_limit(e, usize::MAX).ok())
        };
        let t = std::time::Instant::now();
        let models: Vec<Option<Model>> = sp.names.iter().map(|n| Model::load(n, read).map_err(|e| println!("  {e:#}")).ok()).collect();
        let dt = t.elapsed().as_secs_f64();
        let loaded = models.iter().flatten().count();
        let distinct_tris: usize = models.iter().flatten().map(|m| m.tris()).sum();
        println!("models: {loaded} of {} loaded in {dt:.2}s, {distinct_tris} tris distinct", models.len());
        let mut placed = 0usize;
        let mut placed_verts = 0usize;
        for p in &sp.props {
            if let Some(Some(md)) = models.get(p.model as usize) {
                placed += md.tris();
                placed_verts += md.verts();
            }
        }
        println!("placed: {} props, {placed} tris, {placed_verts} verts", sp.props.len());

        let mut vmt_bump: HashMap<String, bool> = HashMap::new();
        let mut bumped = |md: &Model, mat: usize| -> Option<bool> {
            let t = md.texture(0, mat)?;
            for p in md.vmt_paths(t) {
                if let Some(b) = vmt_bump.get(&p) {
                    return Some(*b);
                }
                let Some(txt) = read(&p) else { continue };
                let mut s = String::from_utf8_lossy(&txt).to_lowercase();
                if let Some(i) = s.find("include") {
                    let rest = &s[i + 7..];
                    if let Some(inc) = rest.split('"').nth(2)
                        && let Some(t2) = read(inc)
                    {
                        s.push_str(&String::from_utf8_lossy(&t2).to_lowercase());
                    }
                }
                let b = s.contains("$bumpmap") || s.contains("$normalmap");
                vmt_bump.insert(p, b);
                return Some(b);
            }
            None
        };
        let (mut ok, mut missing, mut bad) = (0, 0, 0);
        let mut eq = [[0usize; 2]; 3];
        let mut sum = [[0f64; 4]; 3];
        let mut alpha_eq = [0usize; 2];
        let mut hdrs = HashMap::new();
        let sun = crate::s1::sprp::rotation([-50.0, 43.0, 0.0]) * glam::DVec3::X;
        let mut facing = [[0f64; 3]; 3];
        let t = std::time::Instant::now();
        for (i, p) in sp.props.iter().enumerate() {
            let Some(Some(md)) = models.get(p.model as usize) else { continue };
            let Some(bytes) = read(&prop_name(i, true)).or_else(|| read(&prop_name(i, false))) else {
                missing += 1;
                continue;
            };
            let v = match Vhv::parse(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    bad += 1;
                    println!("  prop {i}: {e:#}");
                    continue;
                }
            };
            *hdrs.entry((v.flags, v.size)).or_insert(0usize) += 1;
            match v.for_model(md) {
                Ok(cols) => {
                    ok += 1;
                    for (mesh, c) in md.meshes.iter().zip(&cols) {
                        let k = match bumped(md, mesh.material) {
                            Some(true) => 1,
                            Some(false) => 0,
                            None => 2,
                        };
                        let r = crate::s1::sprp::rotation(p.angles);
                        for (x, nrm) in c.iter().zip(&mesh.normal) {
                            let d = (r * glam::DVec3::from(nrm.map(|v| v as f64))).dot(sun);
                            let b = if d > 0.5 { 2 } else if d < -0.5 { 0 } else { 1 };
                            let rgb: f64 = x.iter().map(|y| (y[0] as f64 + y[1] as f64 + y[2] as f64) / 9.0).sum();
                            facing[b][0] += 1.0;
                            facing[b][1] += x[0][3] as f64;
                            facing[b][2] += rgb;
                            let same = x[0][..3] == x[1][..3] && x[1][..3] == x[2][..3];
                            eq[k][same as usize] += 1;
                            alpha_eq[(x[0][3] == x[1][3] && x[1][3] == x[2][3]) as usize] += 1;
                            for (s, y) in x.iter().enumerate() {
                                for ch in 0..4 {
                                    sum[s][ch] += y[ch] as f64;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    bad += 1;
                    if bad < 10 {
                        println!("  prop {i} {}: {e:#}", sp.name(p).unwrap_or("?"));
                    }
                }
            }
        }
        println!("vhv: {ok} match, {bad} mismatch, {missing} missing in {:.2}s; (flags, size) {hdrs:?}", t.elapsed().as_secs_f64());
        let n: usize = eq.iter().flatten().sum();
        for (k, name) in ["plain", "bumped", "no vmt"].iter().enumerate() {
            println!("  {name}: {} vertices with 3 equal rgb, {} different", eq[k][1], eq[k][0]);
        }
        println!("  alpha equal across 3: {} of {n}", alpha_eq[1]);
        for (k, f) in facing.iter().enumerate() {
            println!("  normal . sun {}: {} vertices, mean alpha {:.1}, mean rgb {:.1}", ["< -0.5", "-0.5..0.5", "> 0.5"][k], f[0], f[1] / f[0], f[2] / f[0]);
        }
        for (s, v) in sum.iter().enumerate() {
            println!("  stream {s} mean rgba {:.1} {:.1} {:.1} {:.1}", v[0] / n as f64, v[1] / n as f64, v[2] / n as f64, v[3] / n as f64);
        }
    }
}
