use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glam::DVec3;

pub const TEX_SPECIAL: i32 = 1;
pub const CONTENTS_SOLID: i32 = -2;
pub const CONTENTS_SKY: i32 = -6;

const LUMP_ENTITIES: usize = 0;
const LUMP_PLANES: usize = 1;
const LUMP_TEXTURES: usize = 2;
const LUMP_VERTICES: usize = 3;
const LUMP_NODES: usize = 5;
const LUMP_TEXINFO: usize = 6;
const LUMP_FACES: usize = 7;
const LUMP_LIGHTING: usize = 8;
const LUMP_CLIPNODES: usize = 9;
const LUMP_LEAVES: usize = 10;
const LUMP_MARKSURFACES: usize = 11;
const LUMP_EDGES: usize = 12;
const LUMP_SURFEDGES: usize = 13;
const LUMP_MODELS: usize = 14;

pub struct Rd<'a> {
    pub data: &'a [u8],
}

impl<'a> Rd<'a> {
    pub fn bytes(&self, ofs: usize, n: usize) -> Option<&'a [u8]> {
        self.data.get(ofs..ofs.checked_add(n)?)
    }
    pub fn u8(&self, o: usize) -> u8 {
        self.data[o]
    }
    pub fn u16(&self, o: usize) -> u16 {
        u16::from_le_bytes([self.data[o], self.data[o + 1]])
    }
    pub fn i16(&self, o: usize) -> i16 {
        self.u16(o) as i16
    }
    pub fn u32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.data[o..o + 4].try_into().unwrap())
    }
    pub fn i32(&self, o: usize) -> i32 {
        self.u32(o) as i32
    }
    pub fn f32(&self, o: usize) -> f32 {
        f32::from_bits(self.u32(o))
    }
    pub fn vec3(&self, o: usize) -> [f32; 3] {
        [self.f32(o), self.f32(o + 4), self.f32(o + 8)]
    }
}

#[derive(Clone, Debug, Default)]
pub struct Entity(pub HashMap<String, String>);

impl Entity {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|s| s.as_str())
    }
    pub fn class(&self) -> &str {
        self.get("classname").unwrap_or("")
    }
    pub fn origin(&self) -> Option<DVec3> {
        let v: Vec<f64> = self.get("origin")?.split_whitespace().map(|s| s.parse().ok()).collect::<Option<_>>()?;
        (v.len() >= 3).then(|| DVec3::new(v[0], v[1], v[2]))
    }
    pub fn model(&self) -> Option<usize> {
        self.get("model")?.strip_prefix('*')?.parse().ok()
    }
}

pub fn parse_entities(text: &str) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut cur: Option<HashMap<String, String>> = None;
    let mut strings: Vec<String> = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                cur = Some(HashMap::new());
                strings.clear();
            }
            '}' => {
                if let Some(mut m) = cur.take() {
                    for kv in strings.chunks_exact(2) {
                        m.insert(kv[0].clone(), kv[1].clone());
                    }
                    out.push(Entity(m));
                }
                strings.clear();
            }
            '"' => {
                let mut s = String::new();
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    s.push(c);
                }
                if cur.is_some() {
                    strings.push(s);
                }
            }
            _ => {}
        }
    }
    out
}

pub fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

pub fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    latin1(&b[..end])
}

#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub normal: DVec3,
    pub dist: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Face {
    pub plane: u16,
    pub side: u16,
    pub firstedge: i32,
    pub numedges: u16,
    pub texinfo: u16,
    pub styles: [u8; 4],
    pub lightofs: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct TexInfo {
    pub s: [f32; 4],
    pub t: [f32; 4],
    pub miptex: i32,
    pub flags: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub plane: i32,
    pub children: [i16; 2],
}

#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub contents: i32,
    pub firstmarksurface: u16,
    pub nummarksurfaces: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct Model {
    pub mins: DVec3,
    pub maxs: DVec3,
    pub headnode: [i32; 4],
    pub firstface: i32,
    pub numfaces: i32,
}

#[derive(Clone, Debug)]
pub struct MipTex {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Option<Vec<u8>>,
}

pub fn read_miptex(data: &[u8], ofs: usize) -> Option<MipTex> {
    let r = Rd { data };
    r.bytes(ofs, 40)?;
    let name = cstr(&data[ofs..ofs + 16]);
    let w = r.u32(ofs + 16);
    let h = r.u32(ofs + 20);
    let o0 = r.u32(ofs + 24) as usize;
    let o3 = r.u32(ofs + 36) as usize;
    let mut rgba = None;
    if o0 != 0 && w > 0 && h > 0 && w <= 4096 && h <= 4096 {
        let n = (w * h) as usize;
        let pal_ofs = ofs + o3 + (w as usize / 8) * (h as usize / 8) + 2;
        if let (Some(pix), Some(pal)) = (r.bytes(ofs + o0, n), r.bytes(pal_ofs, 768)) {
            let masked = name.starts_with('{');
            let mut out = vec![0u8; n * 4];
            for (i, &p) in pix.iter().enumerate() {
                if masked && p == 255 {
                    continue;
                }
                let c = &pal[p as usize * 3..p as usize * 3 + 3];
                out[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
            rgba = Some(out);
        }
    }
    Some(MipTex { name, width: w, height: h, rgba })
}

pub struct Bsp {
    pub path: PathBuf,
    pub entities: Vec<Entity>,
    pub planes: Vec<Plane>,
    pub vertices: Vec<DVec3>,
    pub texinfo: Vec<TexInfo>,
    pub faces: Vec<Face>,
    pub edges: Vec<[u16; 2]>,
    pub surfedges: Vec<i32>,
    pub models: Vec<Model>,
    pub nodes: Vec<Node>,
    pub leaves: Vec<Leaf>,
    pub marksurfaces: Vec<u16>,
    pub clipnodes: Vec<Node>,
    pub lighting: Vec<u8>,
    pub miptex: Vec<MipTex>,
}

fn lump<'a, T>(r: &Rd<'a>, l: (usize, usize), size: usize, f: impl Fn(&Rd<'a>, usize) -> T) -> Result<Vec<T>> {
    let (o, n) = l;
    if r.bytes(o, n).is_none() {
        bail!("lump out of range");
    }
    Ok((0..n / size).map(|i| f(r, o + i * size)).collect())
}

impl Bsp {
    pub fn load(path: &Path) -> Result<Bsp> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(path, &data).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(path: &Path, data: &[u8]) -> Result<Bsp> {
        if data.len() < 124 {
            bail!("file too small");
        }
        let r = Rd { data };
        let version = r.i32(0);
        if version != 30 {
            bail!("unsupported BSP version {version} (GoldSrc is 30)");
        }
        let mut lumps: Vec<(usize, usize)> =
            (0..15).map(|i| (r.u32(4 + i * 8) as usize, r.u32(8 + i * 8) as usize)).collect();
        let first = |l: (usize, usize)| data.get(l.0).copied();
        if first(lumps[0]) != Some(b'{') && first(lumps[1]) == Some(b'{') {
            lumps.swap(0, 1);
        }
        let el = lumps[LUMP_ENTITIES];
        let ent_bytes = r.bytes(el.0, el.1).context("entity lump out of range")?;
        let entities = parse_entities(&latin1(ent_bytes));

        let planes = lump(&r, lumps[LUMP_PLANES], 20, |r, o| {
            let n = r.vec3(o);
            Plane { normal: DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64), dist: r.f32(o + 12) as f64 }
        })?;
        let vertices = lump(&r, lumps[LUMP_VERTICES], 12, |r, o| {
            let v = r.vec3(o);
            DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
        })?;
        let texinfo = lump(&r, lumps[LUMP_TEXINFO], 40, |r, o| TexInfo {
            s: [r.f32(o), r.f32(o + 4), r.f32(o + 8), r.f32(o + 12)],
            t: [r.f32(o + 16), r.f32(o + 20), r.f32(o + 24), r.f32(o + 28)],
            miptex: r.i32(o + 32),
            flags: r.i32(o + 36),
        })?;
        let faces = lump(&r, lumps[LUMP_FACES], 20, |r, o| Face {
            plane: r.u16(o),
            side: r.u16(o + 2),
            firstedge: r.i32(o + 4),
            numedges: r.u16(o + 8),
            texinfo: r.u16(o + 10),
            styles: [r.u8(o + 12), r.u8(o + 13), r.u8(o + 14), r.u8(o + 15)],
            lightofs: r.i32(o + 16),
        })?;
        let edges = lump(&r, lumps[LUMP_EDGES], 4, |r, o| [r.u16(o), r.u16(o + 2)])?;
        let surfedges = lump(&r, lumps[LUMP_SURFEDGES], 4, |r, o| r.i32(o))?;
        let models = lump(&r, lumps[LUMP_MODELS], 64, |r, o| {
            let a = r.vec3(o);
            let b = r.vec3(o + 12);
            Model {
                mins: DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64),
                maxs: DVec3::new(b[0] as f64, b[1] as f64, b[2] as f64),
                headnode: [r.i32(o + 36), r.i32(o + 40), r.i32(o + 44), r.i32(o + 48)],
                firstface: r.i32(o + 56),
                numfaces: r.i32(o + 60),
            }
        })?;
        let nodes = lump(&r, lumps[LUMP_NODES], 24, |r, o| Node {
            plane: r.i32(o),
            children: [r.i16(o + 4), r.i16(o + 6)],
        })?;
        let leaves = lump(&r, lumps[LUMP_LEAVES], 28, |r, o| Leaf {
            contents: r.i32(o),
            firstmarksurface: r.u16(o + 20),
            nummarksurfaces: r.u16(o + 22),
        })?;
        let marksurfaces = lump(&r, lumps[LUMP_MARKSURFACES], 2, |r, o| r.u16(o))?;
        let clipnodes = lump(&r, lumps[LUMP_CLIPNODES], 8, |r, o| Node {
            plane: r.i32(o),
            children: [r.i16(o + 4), r.i16(o + 6)],
        })?;
        let ll = lumps[LUMP_LIGHTING];
        let lighting = r.bytes(ll.0, ll.1).context("lighting lump out of range")?.to_vec();

        let tl = lumps[LUMP_TEXTURES];
        let mut miptex = Vec::new();
        if tl.1 >= 4 {
            let o = tl.0;
            let n = r.i32(o).max(0) as usize;
            if r.bytes(o + 4, n * 4).is_none() {
                bail!("texture lump out of range");
            }
            for i in 0..n {
                let mo = r.i32(o + 4 + i * 4);
                let mt = if mo < 0 { None } else { read_miptex(data, o + mo as usize) };
                miptex.push(mt.unwrap_or(MipTex { name: String::new(), width: 16, height: 16, rgba: None }));
            }
        }

        let bsp = Bsp {
            path: path.to_path_buf(),
            entities,
            planes,
            vertices,
            texinfo,
            faces,
            edges,
            surfedges,
            models,
            nodes,
            leaves,
            marksurfaces,
            clipnodes,
            lighting,
            miptex,
        };
        bsp.validate()?;
        Ok(bsp)
    }

    fn validate(&self) -> Result<()> {
        if self.models.is_empty() {
            bail!("no models");
        }
        for f in &self.faces {
            if f.plane as usize >= self.planes.len() || f.texinfo as usize >= self.texinfo.len() {
                bail!("face references missing plane or texinfo");
            }
            let a = f.firstedge as usize;
            if f.firstedge < 0 || a + f.numedges as usize > self.surfedges.len() {
                bail!("face edges out of range");
            }
            for &se in &self.surfedges[a..a + f.numedges as usize] {
                let e = self.edges.get(se.unsigned_abs() as usize).context("surfedge out of range")?;
                if e[0] as usize >= self.vertices.len() || e[1] as usize >= self.vertices.len() {
                    bail!("edge vertex out of range");
                }
            }
        }
        for m in &self.models {
            if m.firstface < 0 || m.numfaces < 0 || (m.firstface + m.numfaces) as usize > self.faces.len() {
                bail!("model faces out of range");
            }
        }
        let h = self.models[0].headnode;
        if self.nodes.is_empty() || self.leaves.is_empty() || h[0] < 0 || h[0] as usize >= self.nodes.len() {
            bail!("no BSP tree");
        }
        if h[1..].iter().any(|&c| c >= 0 && c as usize >= self.clipnodes.len()) {
            bail!("clipnode headnode out of range");
        }
        for n in self.nodes.iter().chain(&self.clipnodes) {
            if n.plane < 0 || n.plane as usize >= self.planes.len() {
                bail!("node plane out of range");
            }
        }
        for n in &self.nodes {
            for &c in &n.children {
                if c >= 0 && c as usize >= self.nodes.len() || c < 0 && (-(c as i32 + 1)) as usize >= self.leaves.len() {
                    bail!("node child out of range");
                }
            }
        }
        for n in &self.clipnodes {
            for &c in &n.children {
                if c >= 0 && c as usize >= self.clipnodes.len() {
                    bail!("clipnode child out of range");
                }
            }
        }
        for l in &self.leaves {
            if l.firstmarksurface as usize + l.nummarksurfaces as usize > self.marksurfaces.len() {
                bail!("leaf marksurfaces out of range");
            }
        }
        if self.marksurfaces.iter().any(|&m| m as usize >= self.faces.len()) {
            bail!("marksurface out of range");
        }
        Ok(())
    }

    pub fn name(&self) -> String {
        self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn worldspawn(&self) -> Entity {
        self.entities.first().cloned().unwrap_or_default()
    }

    pub fn face_points(&self, fi: usize) -> Vec<DVec3> {
        let f = &self.faces[fi];
        let a = f.firstedge as usize;
        self.surfedges[a..a + f.numedges as usize]
            .iter()
            .map(|&se| {
                let e = self.edges[se.unsigned_abs() as usize];
                self.vertices[if se >= 0 { e[0] } else { e[1] } as usize]
            })
            .collect()
    }

    pub fn face_normal(&self, fi: usize) -> DVec3 {
        let f = &self.faces[fi];
        let n = self.planes[f.plane as usize].normal;
        if f.side != 0 { -n } else { n }
    }

    pub fn world_bounds(&self) -> (DVec3, DVec3) {
        (self.models[0].mins, self.models[0].maxs)
    }

    pub fn point_leaf(&self, p: DVec3) -> usize {
        let mut n = self.models[0].headnode[0];
        for _ in 0..1 << 16 {
            let node = &self.nodes[n as usize];
            let pl = &self.planes[node.plane as usize];
            let c = node.children[if p.dot(pl.normal) - pl.dist >= 0.0 { 0 } else { 1 }] as i32;
            if c < 0 {
                return (-(c + 1)) as usize;
            }
            n = c;
        }
        0
    }

    pub fn leaf_open(&self, leaf: usize) -> bool {
        let c = self.leaves[leaf].contents;
        c != CONTENTS_SOLID && c != CONTENTS_SKY
    }

    pub fn hull_contents(&self, p: DVec3, hull: usize) -> i32 {
        self.model_contents(0, p, hull)
    }

    pub fn model_contents(&self, model: usize, p: DVec3, hull: usize) -> i32 {
        let mut n = self.models[model].headnode[hull];
        if n < 0 {
            return n;
        }
        let mut steps = 0;
        loop {
            let node = &self.clipnodes[n as usize];
            let pl = &self.planes[node.plane as usize];
            let c = node.children[if p.dot(pl.normal) - pl.dist >= 0.0 { 0 } else { 1 }] as i32;
            if c < 0 {
                return c;
            }
            n = c;
            steps += 1;
            if steps > 1 << 16 {
                return CONTENTS_SOLID;
            }
        }
    }
}
