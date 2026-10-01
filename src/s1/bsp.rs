
use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use glam::DVec3;
use memmap2::Mmap;

use crate::bsp::Rd;

pub const LUMPS: usize = 64;
pub const HEADER: usize = 8 + LUMPS * 16 + 4;

pub const LUMP_ENTITIES: usize = 0;
pub const LUMP_PLANES: usize = 1;
pub const LUMP_TEXDATA: usize = 2;
pub const LUMP_VERTEXES: usize = 3;
pub const LUMP_VISIBILITY: usize = 4;
pub const LUMP_NODES: usize = 5;
pub const LUMP_TEXINFO: usize = 6;
pub const LUMP_FACES: usize = 7;
pub const LUMP_LIGHTING: usize = 8;
pub const LUMP_LEAFS: usize = 10;
pub const LUMP_EDGES: usize = 12;
pub const LUMP_SURFEDGES: usize = 13;
pub const LUMP_MODELS: usize = 14;
pub const LUMP_LEAFFACES: usize = 16;
pub const LUMP_LEAFBRUSHES: usize = 17;
pub const LUMP_BRUSHES: usize = 18;
pub const LUMP_AREAS: usize = 20;
pub const LUMP_BRUSHSIDES: usize = 19;
pub const LUMP_DISPINFO: usize = 26;
pub const LUMP_DISP_VERTS: usize = 33;
pub const LUMP_GAME_LUMP: usize = 35;
pub const LUMP_PAKFILE: usize = 40;
pub const LUMP_CUBEMAPS: usize = 42;
pub const LUMP_TEXDATA_STRING_DATA: usize = 43;
pub const LUMP_TEXDATA_STRING_TABLE: usize = 44;
pub const LUMP_OVERLAYS: usize = 45;
pub const LUMP_LIGHTING_HDR: usize = 53;
pub const LUMP_LEAF_AMBIENT_HDR: usize = 55;
pub const LUMP_LEAF_AMBIENT: usize = 56;
pub const LUMP_FACES_HDR: usize = 58;
pub const LUMP_DISP_MULTIBLEND: usize = 63;

pub const SURF_SKY2D: i32 = 0x2;
pub const SURF_SKY: i32 = 0x4;
pub const SURF_WARP: i32 = 0x8;
pub const SURF_TRIGGER: i32 = 0x40;
pub const SURF_NODRAW: i32 = 0x80;
pub const SURF_HINT: i32 = 0x100;
pub const SURF_SKIP: i32 = 0x200;
pub const SURF_NOLIGHT: i32 = 0x400;
pub const SURF_BUMPLIGHT: i32 = 0x800;
pub const SURF_HIDDEN: i32 = SURF_SKY2D | SURF_SKY | SURF_TRIGGER | SURF_NODRAW | SURF_HINT | SURF_SKIP;

#[derive(Clone, Copy, Debug, Default)]
pub struct Lump {
    pub ofs: usize,
    pub len: usize,
    pub version: i32,
    pub fourcc: [u8; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub normal: DVec3,
    pub dist: f64,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct Face {
    pub plane: u16,
    pub side: u8,
    pub on_node: u8,
    pub first_edge: i32,
    pub num_edges: u16,
    pub texinfo: i16,
    pub dispinfo: i16,
    pub fog_volume: i16,
    pub styles: [u8; 4],
    pub lightofs: i32,
    pub area: f32,
    pub lm_mins: [i32; 2],
    pub lm_size: [i32; 2],
    pub orig_face: i32,
    pub num_prims: u16,
    pub first_prim: u16,
    pub smoothing: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct TexInfo {
    pub s: [f32; 4],
    pub t: [f32; 4],
    pub lm_s: [f32; 4],
    pub lm_t: [f32; 4],
    pub flags: i32,
    pub texdata: i32,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TexData {
    pub reflectivity: [f32; 3],
    pub name: String,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct Model {
    pub mins: DVec3,
    pub maxs: DVec3,
    pub origin: DVec3,
    pub headnode: i32,
    pub first_face: i32,
    pub num_faces: i32,
}

pub struct Map {
    pub path: PathBuf,
    pub version: u32,
    pub revision: i32,
    pub lumps: [Lump; LUMPS],
    pub l4d2: bool,
    patched: Vec<Option<Vec<u8>>>,
    data: Mmap,
}

fn dvec(v: [f32; 3]) -> DVec3 {
    DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
}

fn f4(r: &Rd, o: usize) -> [f32; 4] {
    [r.f32(o), r.f32(o + 4), r.f32(o + 8), r.f32(o + 12)]
}

pub fn lzma(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(data.len() >= 17 && data.starts_with(b"LZMA"), "not an LZMA block");
    let r = Rd { data };
    let (actual, packed) = (r.u32(4) as usize, r.u32(8) as usize);
    let body = data.get(12..17 + packed).context("LZMA block truncated")?;
    let mut out = Vec::with_capacity(actual.min(packed.saturating_mul(64)));
    let o = lzma_rs::decompress::Options {
        unpacked_size: lzma_rs::decompress::UnpackedSize::UseProvided(Some(actual as u64)),
        ..Default::default()
    };
    lzma_rs::lzma_decompress_with_options(&mut &body[..], &mut out, &o).map_err(|e| anyhow::anyhow!("LZMA: {e}"))?;
    ensure!(out.len() == actual, "LZMA block gave {} of {actual} bytes", out.len());
    Ok(out)
}

pub fn is_source(path: &Path) -> bool {
    let mut b = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).is_ok() && &b == b"VBSP"
}

impl Map {
    pub fn open(path: &Path) -> Result<Map> {
        let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let data = unsafe { Mmap::map(&f) }.with_context(|| format!("mapping {}", path.display()))?;
        ensure!(data.len() >= HEADER && data.starts_with(b"VBSP"), "{} is not a Source BSP", path.display());
        let r = Rd { data: &data };
        let version = r.u32(4);
        if !(19..=21).contains(&version) {
            bail!("Source BSP version {version} is not supported (CS:S, TF2 and L4D are 19/20, CS:GO and L4D2 21)");
        }
        let l4d2 = version == 21 && (0..LUMPS).any(|i| r.u32(8 + i * 16 + 4) != 0 && (r.u32(8 + i * 16) as usize) < HEADER);
        let mut lumps = [Lump::default(); LUMPS];
        for (i, l) in lumps.iter_mut().enumerate() {
            let o = 8 + i * 16;
            let fourcc = data[o + 12..o + 16].try_into().unwrap();
            *l = if l4d2 {
                Lump { ofs: r.u32(o + 4) as usize, len: r.u32(o + 8) as usize, version: r.i32(o), fourcc }
            } else {
                Lump { ofs: r.u32(o) as usize, len: r.u32(o + 4) as usize, version: r.i32(o + 8), fourcc }
            };
            if l.len > 0 && l.ofs.checked_add(l.len).is_none_or(|e| e > data.len()) {
                bail!("lump {i} runs past the end of the file");
            }
        }
        let revision = r.i32(8 + LUMPS * 16);
        Ok(Map { path: path.to_path_buf(), version, revision, lumps, l4d2, patched: vec![None; LUMPS], data })
    }

    pub fn apply_lump_file(&mut self, b: &[u8]) -> Result<usize> {
        let r = Rd { data: b };
        ensure!(b.len() >= 20, "lump file too short");
        let (ofs, id, version, len) = (r.u32(0) as usize, r.u32(4) as usize, r.i32(8), r.u32(12) as usize);
        ensure!(id < LUMPS, "lump file patches lump {id}");
        ensure!(id != LUMP_PAKFILE && id != LUMP_GAME_LUMP, "lump file patches lump {id}, which is not supported");
        let body = ofs.checked_add(len).and_then(|e| b.get(ofs..e)).context("lump file data runs past the end")?;
        self.lumps[id].len = body.len();
        self.lumps[id].version = version;
        self.lumps[id].fourcc = [0; 4];
        self.patched[id] = Some(body.to_vec());
        Ok(id)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn raw(&self, i: usize) -> &[u8] {
        if let Some(p) = &self.patched[i] {
            return p;
        }
        let l = &self.lumps[i];
        &self.data[l.ofs..l.ofs + l.len]
    }

    pub fn compressed(&self, i: usize) -> bool {
        self.raw(i).starts_with(b"LZMA")
    }

    pub fn lump(&self, i: usize) -> Result<Cow<'_, [u8]>> {
        let raw = self.raw(i);
        if raw.starts_with(b"LZMA") {
            return Ok(Cow::Owned(lzma(raw).with_context(|| format!("lump {i}"))?));
        }
        Ok(Cow::Borrowed(raw))
    }

    pub fn has(&self, i: usize) -> bool {
        self.lumps[i].len > 0
    }

    pub fn read<T>(&self, i: usize, size: usize, f: impl Fn(&Rd, usize) -> T) -> Result<Vec<T>> {
        let d = self.lump(i)?;
        if d.len() % size != 0 {
            bail!("lump {i} is {} bytes, not a multiple of {size}", d.len());
        }
        let r = Rd { data: &d };
        Ok((0..d.len() / size).map(|k| f(&r, k * size)).collect())
    }

    pub fn entities(&self) -> Result<String> {
        let d = self.lump(LUMP_ENTITIES)?;
        let end = d.iter().position(|&c| c == 0).unwrap_or(d.len());
        Ok(crate::bsp::latin1(&d[..end]))
    }

    pub fn planes(&self) -> Result<Vec<Plane>> {
        self.read(LUMP_PLANES, 20, |r, o| Plane { normal: dvec(r.vec3(o)), dist: r.f32(o + 12) as f64 })
    }

    pub fn vertices(&self) -> Result<Vec<DVec3>> {
        self.read(LUMP_VERTEXES, 12, |r, o| dvec(r.vec3(o)))
    }

    pub fn edges(&self) -> Result<Vec<[u16; 2]>> {
        self.read(LUMP_EDGES, 4, |r, o| [r.u16(o), r.u16(o + 2)])
    }

    pub fn surfedges(&self) -> Result<Vec<i32>> {
        self.read(LUMP_SURFEDGES, 4, |r, o| r.i32(o))
    }

    pub fn faces_lump(&self, i: usize) -> Result<Vec<Face>> {
        self.read(i, 56, |r, o| Face {
            plane: r.u16(o),
            side: r.u8(o + 2),
            on_node: r.u8(o + 3),
            first_edge: r.i32(o + 4),
            num_edges: r.u16(o + 8),
            texinfo: r.i16(o + 10),
            dispinfo: r.i16(o + 12),
            fog_volume: r.i16(o + 14),
            styles: [r.u8(o + 16), r.u8(o + 17), r.u8(o + 18), r.u8(o + 19)],
            lightofs: r.i32(o + 20),
            area: r.f32(o + 24),
            lm_mins: [r.i32(o + 28), r.i32(o + 32)],
            lm_size: [r.i32(o + 36), r.i32(o + 40)],
            orig_face: r.i32(o + 44),
            num_prims: r.u16(o + 48),
            first_prim: r.u16(o + 50),
            smoothing: r.u32(o + 52),
        })
    }

    pub fn faces(&self) -> Result<Vec<Face>> {
        if self.has(LUMP_FACES) {
            return self.faces_lump(LUMP_FACES);
        }
        self.faces_lump(LUMP_FACES_HDR)
    }

    pub fn light_lumps(&self) -> Option<(usize, usize)> {
        if self.has(LUMP_LIGHTING_HDR) && self.has(LUMP_FACES_HDR) {
            Some((LUMP_FACES_HDR, LUMP_LIGHTING_HDR))
        } else if self.has(LUMP_LIGHTING) {
            Some((LUMP_FACES, LUMP_LIGHTING))
        } else {
            None
        }
    }

    pub fn texinfo(&self) -> Result<Vec<TexInfo>> {
        self.read(LUMP_TEXINFO, 72, |r, o| TexInfo {
            s: f4(r, o),
            t: f4(r, o + 16),
            lm_s: f4(r, o + 32),
            lm_t: f4(r, o + 48),
            flags: r.i32(o + 64),
            texdata: r.i32(o + 68),
        })
    }

    pub fn texdata(&self) -> Result<Vec<TexData>> {
        let table = self.read(LUMP_TEXDATA_STRING_TABLE, 4, |r, o| r.i32(o))?;
        let strings = self.lump(LUMP_TEXDATA_STRING_DATA)?;
        let name = |id: i32| -> String {
            let at = usize::try_from(id).ok().and_then(|i| table.get(i)).and_then(|&o| usize::try_from(o).ok());
            at.and_then(|o| strings.get(o..)).map(crate::bsp::cstr).unwrap_or_default()
        };
        self.read(LUMP_TEXDATA, 32, |r, o| TexData {
            reflectivity: r.vec3(o),
            name: name(r.i32(o + 12)),
            width: r.i32(o + 16),
            height: r.i32(o + 20),
        })
    }

    pub fn models(&self) -> Result<Vec<Model>> {
        self.read(LUMP_MODELS, 48, |r, o| Model {
            mins: dvec(r.vec3(o)),
            maxs: dvec(r.vec3(o + 12)),
            origin: dvec(r.vec3(o + 24)),
            headnode: r.i32(o + 36),
            first_face: r.i32(o + 40),
            num_faces: r.i32(o + 44),
        })
    }

    pub fn pakfile(&self) -> Option<(usize, usize)> {
        let l = &self.lumps[LUMP_PAKFILE];
        (l.len > 0).then_some((l.ofs, l.len))
    }
}

pub struct World {
    pub planes: Vec<Plane>,
    pub vertices: Vec<DVec3>,
    pub edges: Vec<[u16; 2]>,
    pub surfedges: Vec<i32>,
    pub faces: Vec<Face>,
    pub texinfo: Vec<TexInfo>,
    pub texdata: Vec<TexData>,
    pub models: Vec<Model>,
}

impl World {
    pub fn read(map: &Map) -> Result<World> {
        let w = World {
            planes: map.planes()?,
            vertices: map.vertices()?,
            edges: map.edges()?,
            surfedges: map.surfedges()?,
            faces: map.faces()?,
            texinfo: map.texinfo()?,
            texdata: map.texdata()?,
            models: map.models()?,
        };
        w.validate()?;
        Ok(w)
    }

    fn validate(&self) -> Result<()> {
        ensure!(!self.models.is_empty(), "no models");
        for (i, f) in self.faces.iter().enumerate() {
            ensure!((f.plane as usize) < self.planes.len(), "face {i}: plane {} out of range", f.plane);
            ensure!(f.texinfo < 0 || (f.texinfo as usize) < self.texinfo.len(), "face {i}: texinfo {} out of range", f.texinfo);
            let a = usize::try_from(f.first_edge).ok().filter(|a| a + f.num_edges as usize <= self.surfedges.len());
            let a = a.with_context(|| format!("face {i}: edges out of range"))?;
            for &se in &self.surfedges[a..a + f.num_edges as usize] {
                let e = self.edges.get(se.unsigned_abs() as usize).with_context(|| format!("face {i}: surfedge {se} out of range"))?;
                ensure!((e[0] as usize) < self.vertices.len() && (e[1] as usize) < self.vertices.len(), "face {i}: edge vertex out of range");
            }
        }
        for (i, t) in self.texinfo.iter().enumerate() {
            ensure!(t.texdata < 0 || (t.texdata as usize) < self.texdata.len(), "texinfo {i}: texdata {} out of range", t.texdata);
        }
        for (i, m) in self.models.iter().enumerate() {
            let ok = m.first_face >= 0 && m.num_faces >= 0 && (m.first_face as usize + m.num_faces as usize) <= self.faces.len();
            ensure!(ok, "model {i}: faces out of range");
        }
        Ok(())
    }

    pub fn face_points(&self, fi: usize) -> Vec<DVec3> {
        let f = &self.faces[fi];
        let a = f.first_edge as usize;
        self.surfedges[a..a + f.num_edges as usize]
            .iter()
            .map(|&se| {
                let e = self.edges[se.unsigned_abs() as usize];
                self.vertices[if se >= 0 { e[0] } else { e[1] } as usize]
            })
            .collect()
    }

    pub fn face_normal(&self, fi: usize) -> DVec3 {
        self.planes[self.faces[fi].plane as usize].normal
    }

    pub fn material(&self, fi: usize) -> Option<(&TexInfo, &TexData)> {
        let t = self.texinfo.get(usize::try_from(self.faces[fi].texinfo).ok()?)?;
        Some((t, self.texdata.get(usize::try_from(t.texdata).ok()?)?))
    }
}
