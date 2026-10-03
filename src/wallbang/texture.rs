use std::path::{Path, PathBuf};

use crate::bsp::{Bsp, TEX_SPECIAL};

use super::hull0::{HPlane, V3, dot};

const MAX_TEXTURES: usize = 1024;
const NAME_LEN: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct Surf {
    pub mins: [i16; 2],
    pub ext: [i16; 2],
    pub texinfo: u32,
}

#[derive(Clone, Copy, Debug)]
struct TNode {
    plane: u32,
    children: [i32; 2],
    first: u16,
    num: u16,
}

pub struct Textures {
    nodes: Vec<TNode>,
    pub surfs: Vec<Surf>,
    vecs: Vec<[[f32; 4]; 2]>,
    names: Vec<[u8; NAME_LEN]>,
}

fn engine_name(name: &str) -> [u8; NAME_LEN] {
    let mut out = [0u8; NAME_LEN];
    for (o, c) in out.iter_mut().zip(name.chars()) {
        *o = c as u32 as u8;
    }
    if out.contains(&b'~') {
        out[2] = b' ';
    }
    out
}

pub fn cname(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

impl Textures {
    pub fn new(bsp: &Bsp) -> Textures {
        let nodes = bsp
            .nodes
            .iter()
            .zip(&bsp.node_faces)
            .map(|(n, fc)| TNode { plane: n.plane as u32, children: n.children.map(i32::from), first: fc[0], num: fc[1] })
            .collect();
        let mut tex: Vec<([u8; NAME_LEN], u32, u32, i32)> = bsp
            .texinfo
            .iter()
            .map(|ti| {
                let mt = usize::try_from(ti.miptex).ok().and_then(|i| bsp.miptex.get(i)).filter(|m| !m.name.is_empty());
                match mt {
                    Some(m) => (engine_name(&m.name), m.width, m.height, ti.flags),
                    None => ([0; NAME_LEN], 16, 16, 0),
                }
            })
            .collect();
        let vecs = bsp.texinfo.iter().map(|ti| [ti.s, ti.t]).collect();
        let mut surfs = Vec::with_capacity(bsp.faces.len());
        for (fi, face) in bsp.faces.iter().enumerate() {
            let ti = face.texinfo as usize;
            let t = &bsp.texinfo[ti];
            let mut mins = [999999.0f32; 2];
            let mut maxs = [-99999.0f32; 2];
            for v in bsp.face_points(fi) {
                let p = [v.x as f32, v.y as f32, v.z as f32];
                for (j, vec) in [t.s, t.t].iter().enumerate() {
                    let val = (f64::from(p[0]) * f64::from(vec[0])
                        + f64::from(p[1]) * f64::from(vec[1])
                        + f64::from(p[2]) * f64::from(vec[2])
                        + f64::from(vec[3])) as f32;
                    if val < mins[j] {
                        mins[j] = val;
                    }
                    if val > maxs[j] {
                        maxs[j] = val;
                    }
                }
            }
            let mut s = Surf { mins: [0; 2], ext: [0; 2], texinfo: ti as u32 };
            for i in 0..2 {
                let bmin = (f64::from(mins[i]) / 16.0).floor() as i32;
                let bmax = (f64::from(maxs[i]) / 16.0).ceil() as i32;
                s.mins[i] = (bmin * 16) as i16;
                s.ext[i] = ((bmax - bmin) * 16) as i16;
            }
            let (name, w, h, flags) = tex[ti];
            let n = cname(&name);
            let lower = |k: usize| n.iter().take(k).map(|c| c.to_ascii_lowercase()).collect::<Vec<u8>>();
            if n.starts_with(b"sky") {
            } else if n.starts_with(b"scroll") {
                s.ext = [w as i16, h as i16];
            } else if n.first() == Some(&b'!') || lower(5) == b"laser" || lower(5) == b"water" {
                s.ext = [16384, 16384];
                s.mins = [-8192, -8192];
                tex[ti].3 |= TEX_SPECIAL;
            } else if flags & TEX_SPECIAL != 0 {
                s.ext = [w as i16, h as i16];
            }
            surfs.push(s);
        }
        let names = tex.iter().map(|t| t.0).collect();
        Textures { nodes, surfs, vecs, names }
    }

    pub fn name(&self, surf: u32) -> &[u8] {
        cname(&self.names[self.surfs[surf as usize].texinfo as usize])
    }

    pub fn surface_at(&self, planes: &[HPlane], node: i32, start: &V3, end: &V3) -> Option<u32> {
        if node < 0 {
            return None;
        }
        let nd = &self.nodes[node as usize];
        let pl = &planes[nd.plane as usize];
        let front = (f64::from(dot(start, &pl.n)) - f64::from(pl.d)) as f32;
        let back = (f64::from(dot(end, &pl.n)) - f64::from(pl.d)) as f32;
        let s = usize::from(front < 0.0);
        let t = usize::from(back < 0.0);
        if s == t {
            return self.surface_at(planes, nd.children[s], start, end);
        }
        let frac = (f64::from(front) / (f64::from(front) - f64::from(back))) as f32;
        let mid: V3 = std::array::from_fn(|i| {
            ((f64::from(end[i]) - f64::from(start[i])) * f64::from(frac) + f64::from(start[i])) as f32
        });
        if let Some(r) = self.surface_at(planes, nd.children[s], start, &mid) {
            return Some(r);
        }
        for i in 0..nd.num as u32 {
            let si = nd.first as u32 + i;
            let surf = &self.surfs[si as usize];
            let v = &self.vecs[surf.texinfo as usize];
            let ds = (f64::from(dot(&mid, &[v[0][0], v[0][1], v[0][2]])) + f64::from(v[0][3])) as i32;
            let dt = (f64::from(dot(&mid, &[v[1][0], v[1][1], v[1][2]])) + f64::from(v[1][3])) as i32;
            if ds >= surf.mins[0] as i32
                && dt >= surf.mins[1] as i32
                && ds - surf.mins[0] as i32 <= surf.ext[0] as i32
                && dt - surf.mins[1] as i32 <= surf.ext[1] as i32
            {
                return Some(si);
            }
        }
        self.surface_at(planes, nd.children[s ^ 1], &mid, end)
    }
}

pub struct Materials {
    names: Vec<Vec<u8>>,
    kinds: Vec<u8>,
    pub source: Option<PathBuf>,
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

impl Materials {
    pub fn empty() -> Materials {
        Materials { names: Vec::new(), kinds: Vec::new(), source: None }
    }

    pub fn parse(data: &[u8]) -> Materials {
        let mut m = Materials::empty();
        let mut pos = 0usize;
        let buf_size = 511usize;
        while pos < data.len() && m.names.len() < MAX_TEXTURES {
            let last = data.len().min(pos + buf_size - 1);
            let mut i = pos;
            let mut stop = false;
            while i < last && !stop {
                if data[i] == b'\n' {
                    stop = true;
                }
                i += 1;
            }
            if i == pos {
                break;
            }
            let line = &data[pos..i];
            pos = i;
            let at = |k: usize| line.get(k).copied().unwrap_or(0);
            let mut i = 0;
            while at(i) != 0 && is_space(at(i)) {
                i += 1;
            }
            if at(i) == 0 || at(i) == b'/' || !at(i).is_ascii_alphabetic() {
                continue;
            }
            let kind = at(i).to_ascii_uppercase();
            i += 1;
            while at(i) != 0 && is_space(at(i)) {
                i += 1;
            }
            if at(i) == 0 {
                continue;
            }
            let mut j = i;
            while at(j) != 0 && !is_space(at(j)) {
                j += 1;
            }
            if at(j) == 0 {
                continue;
            }
            let j = j.min(NAME_LEN + i);
            m.kinds.push(kind);
            m.names.push(line[i..j].to_vec());
        }
        m
    }

    pub fn load(path: &Path) -> std::io::Result<Materials> {
        let mut m = Materials::parse(&std::fs::read(path)?);
        m.source = Some(path.to_path_buf());
        Ok(m)
    }

    pub fn locate(bsp: &Path) -> Option<PathBuf> {
        let game = std::path::absolute(bsp).ok()?.parent()?.parent()?.to_path_buf();
        let own = game.join("sound").join("materials.txt");
        if own.exists() {
            return Some(own);
        }
        let lib = std::fs::read(game.join("liblist.gam")).ok()?;
        for line in crate::bsp::latin1(&lib).lines() {
            let parts: Vec<&str> = line.split('"').collect();
            if line.trim().to_ascii_lowercase().starts_with("fallback_dir") && parts.len() > 1 {
                let fb = game.parent()?.join(parts[1]).join("sound").join("materials.txt");
                if fb.exists() {
                    return Some(fb);
                }
            }
        }
        None
    }

    fn find(&self, name: &[u8]) -> u8 {
        for (n, &k) in self.names.iter().zip(&self.kinds) {
            let mut eq = true;
            for c in 0..NAME_LEN {
                let a = name.get(c).copied().unwrap_or(0).to_ascii_lowercase();
                let b = n.get(c).copied().unwrap_or(0).to_ascii_lowercase();
                if a != b {
                    eq = false;
                    break;
                }
                if a == 0 {
                    break;
                }
            }
            if eq {
                return k;
            }
        }
        b'C'
    }

    pub fn material(&self, name: &[u8]) -> u8 {
        let mut n = name;
        if matches!(n.first(), Some(b'-' | b'+')) {
            n = n.get(2..).unwrap_or(&[]);
        }
        if matches!(n.first(), Some(b'{' | b'!' | b'~' | b' ')) {
            n = &n[1..];
        }
        self.find(&n[..n.len().min(NAME_LEN)])
    }
}
