use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::bsp::{Rd, cstr, read_miptex};

pub struct Wad {
    pub path: PathBuf,
    entries: HashMap<String, usize>,
    data: OnceCell<Vec<u8>>,
}

impl Wad {
    pub fn open(path: &Path) -> Result<Wad> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; 12];
        f.read_exact(&mut head)?;
        if &head[..4] != b"WAD3" && &head[..4] != b"WAD2" {
            bail!("not a wad: {}", path.display());
        }
        let r = Rd { data: &head };
        let n = r.i32(4).max(0) as usize;
        let dirofs = r.i32(8).max(0) as u64;
        f.seek(SeekFrom::Start(dirofs))?;
        let mut d = vec![0u8; n * 32];
        f.read_exact(&mut d)?;
        let r = Rd { data: &d };
        let mut entries = HashMap::new();
        for i in 0..n {
            let o = i * 32;
            let filepos = r.i32(o).max(0) as usize;
            let typ = r.u8(o + 12);
            let comp = r.u8(o + 13);
            if typ == 0x43 && comp == 0 {
                entries.insert(cstr(&d[o + 16..o + 32]).to_lowercase(), filepos);
            }
        }
        Ok(Wad { path: path.to_path_buf(), entries, data: OnceCell::new() })
    }

    pub fn get(&self, name: &str) -> Option<(u32, u32, Vec<u8>)> {
        let ofs = *self.entries.get(&name.to_lowercase())?;
        let data = self.data.get_or_init(|| std::fs::read(&self.path).unwrap_or_default());
        let mt = read_miptex(data, ofs)?;
        Some((mt.width, mt.height, mt.rgba?))
    }
}

pub struct TextureSource {
    listing: Vec<(String, PathBuf)>,
    cache: HashMap<PathBuf, Option<usize>>,
    wads: Vec<Wad>,
    primary: Vec<usize>,
    fallback: Option<Vec<usize>>,
}

impl TextureSource {
    pub fn new(search_dirs: &[PathBuf]) -> TextureSource {
        let mut listing: Vec<(String, PathBuf)> = Vec::new();
        for d in search_dirs {
            let Ok(rd) = std::fs::read_dir(d) else { continue };
            let mut found: Vec<(String, PathBuf)> = rd
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_lowercase();
                    n.ends_with(".wad").then(|| (n, e.path()))
                })
                .collect();
            found.sort();
            for (n, p) in found {
                if !listing.iter().any(|(k, _)| *k == n) {
                    listing.push((n, p));
                }
            }
        }
        TextureSource { listing, cache: HashMap::new(), wads: Vec::new(), primary: Vec::new(), fallback: None }
    }

    fn open(&mut self, fname: &str) -> Option<usize> {
        let key = fname.to_lowercase();
        let path = self.listing.iter().find(|(k, _)| *k == key)?.1.clone();
        if let Some(&i) = self.cache.get(&path) {
            return i;
        }
        let i = Wad::open(&path).ok().map(|w| {
            self.wads.push(w);
            self.wads.len() - 1
        });
        self.cache.insert(path, i);
        i
    }

    pub fn use_worldspawn(&mut self, wadkey: &str) -> Vec<String> {
        let mut missing = Vec::new();
        for p in wadkey.split(';') {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            let fname = p.rsplit(['\\', '/']).next().unwrap_or(p).to_string();
            match self.open(&fname) {
                Some(i) => {
                    if !self.primary.contains(&i) {
                        self.primary.push(i);
                    }
                }
                None => missing.push(fname),
            }
        }
        missing
    }

    pub fn find(&mut self, name: &str) -> Option<(u32, u32, Vec<u8>)> {
        for &i in &self.primary {
            if let Some(r) = self.wads[i].get(name) {
                return Some(r);
            }
        }
        if self.fallback.is_none() {
            let mut names: Vec<String> = self.listing.iter().map(|(k, _)| k.clone()).collect();
            names.sort();
            let mut fb = Vec::new();
            for n in names {
                if let Some(i) = self.open(&n) {
                    if !self.primary.contains(&i) && !fb.contains(&i) {
                        fb.push(i);
                    }
                }
            }
            self.fallback = Some(fb);
        }
        for &i in self.fallback.as_ref().unwrap() {
            if let Some(r) = self.wads[i].get(name) {
                return Some(r);
            }
        }
        None
    }
}
