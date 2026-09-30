
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};

use super::bsp::Map;
use crate::bsp::Rd;
use crate::s2::vpk::{Dir, Fs, Src, Vpk};

const EOCD: u32 = 0x06054b50;
const CENTRAL: u32 = 0x02014b50;
const LOCAL: u32 = 0x04034b50;

struct Entry {
    local: usize,
    method: u16,
    packed: usize,
    size: usize,
}

pub struct Zip {
    map: Arc<Map>,
    base: usize,
    len: usize,
    entries: HashMap<String, Entry>,
}

impl Zip {
    pub fn open(map: Arc<Map>) -> Result<Option<Zip>> {
        let Some((base, len)) = map.pakfile() else { return Ok(None) };
        let d = &map.bytes()[base..base + len];
        let r = Rd { data: d };
        let lo = len.saturating_sub(22 + 65535);
        let Some(e) = (lo..len.saturating_sub(21)).rev().find(|&o| r.u32(o) == EOCD) else { bail!("pakfile: no zip directory") };
        let (n, cd) = (r.u16(e + 10) as usize, r.u32(e + 16) as usize);
        let mut entries = HashMap::with_capacity(n);
        let mut p = cd;
        for _ in 0..n {
            ensure!(r.bytes(p, 46).is_some() && r.u32(p) == CENTRAL, "pakfile: bad directory entry at {p}");
            let (nl, xl, cl) = (r.u16(p + 28) as usize, r.u16(p + 30) as usize, r.u16(p + 32) as usize);
            let name = r.bytes(p + 46, nl).context("pakfile: name past the end")?;
            let name = String::from_utf8_lossy(name).to_lowercase().replace('\\', "/");
            let e = Entry { local: r.u32(p + 42) as usize, method: r.u16(p + 10), packed: r.u32(p + 20) as usize, size: r.u32(p + 24) as usize };
            if !name.ends_with('/') {
                entries.insert(name, e);
            }
            p += 46 + nl + xl + cl;
        }
        Ok(Some(Zip { map, base, len, entries }))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn bytes(&self) -> usize {
        self.len
    }

    pub fn path(&self) -> &Path {
        &self.map.path
    }

    pub fn has(&self, k: &str) -> bool {
        self.entries.contains_key(k)
    }

    pub fn names_with(&self, p: &str, out: &mut Vec<String>) {
        out.extend(self.entries.keys().filter(|n| n.starts_with(p)).cloned());
    }

    fn data(&self, e: &Entry) -> Result<&[u8]> {
        let d = &self.map.bytes()[self.base..self.base + self.len];
        let r = Rd { data: d };
        ensure!(r.bytes(e.local, 30).is_some() && r.u32(e.local) == LOCAL, "bad local header");
        let at = e.local + 30 + r.u16(e.local + 26) as usize + r.u16(e.local + 28) as usize;
        r.bytes(at, e.packed).context("entry runs past the pakfile")
    }

    pub fn read_limit(&self, k: &str, limit: usize) -> Option<Result<Vec<u8>>> {
        let e = self.entries.get(k)?;
        Some(self.data(e).and_then(|d| match e.method {
            0 => Ok(d[..d.len().min(limit)].to_vec()),
            14 => {
                ensure!(d.len() >= 9, "LZMA entry too short");
                let props = u16::from_le_bytes([d[2], d[3]]) as usize;
                let body = d.get(4..).context("LZMA entry too short")?;
                ensure!(props == 5, "LZMA entry with {props} property bytes");
                let mut out = Vec::with_capacity(e.size);
                let o = lzma_rs::decompress::Options {
                    unpacked_size: lzma_rs::decompress::UnpackedSize::UseProvided(Some(e.size as u64)),
                    ..Default::default()
                };
                lzma_rs::lzma_decompress_with_options(&mut &body[..], &mut out, &o).map_err(|x| anyhow::anyhow!("LZMA: {x}"))?;
                out.truncate(limit);
                Ok(out)
            }
            m => bail!("zip method {m} not supported"),
        }))
    }
}

pub fn norm(name: &str) -> String {
    let mut s = name.to_lowercase().replace('\\', "/");
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    s.trim_start_matches('/').to_string()
}

fn gameinfo_dir(map: &Path, game: Option<&Path>) -> Option<PathBuf> {
    let has = |p: &Path| p.join("gameinfo.txt").is_file();
    let map = std::path::absolute(map).unwrap_or_else(|_| map.to_path_buf());
    let maps = map.parent().filter(|d| d.file_name().is_some_and(|n| n.eq_ignore_ascii_case("maps"))).and_then(Path::parent);
    if let Some(d) = maps.filter(|d| has(d)) {
        return Some(d.to_path_buf());
    }
    let g = game?;
    if has(g) {
        return Some(g.to_path_buf());
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(g).ok()?.flatten().map(|e| e.path()).filter(|p| has(p)).collect();
    subs.sort_by_key(|p| {
        let l = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        (l != "cstrike" && l != "csgo" && l != "tf", l)
    });
    subs.into_iter().next()
}

fn dir_vpk(p: &Path) -> Option<PathBuf> {
    let s = p.to_string_lossy();
    let base = s.strip_suffix(".vpk").or_else(|| s.strip_suffix(".VPK"))?;
    [PathBuf::from(format!("{base}_dir.vpk")), p.to_path_buf()].into_iter().find(|c| c.is_file())
}

pub fn search_paths(gameinfo: &Path) -> Result<Vec<PathBuf>> {
    let text = std::fs::read(gameinfo.join("gameinfo.txt"))?;
    let kv = crate::kv1::parse_bytes(&text).context("parsing gameinfo.txt")?;
    let root = kv.first().and_then(|(_, v)| v.block()).context("gameinfo.txt has no GameInfo block")?;
    let sp = crate::kv1::get(root, "FileSystem")
        .and_then(|v| v.block())
        .and_then(|b| crate::kv1::get(b, "SearchPaths"))
        .and_then(|v| v.block())
        .context("gameinfo.txt has no SearchPaths")?;
    let base = gameinfo.parent().unwrap_or(gameinfo).to_path_buf();
    let mut out = Vec::new();
    for (k, v) in sp {
        if !k.split('+').any(|p| p.eq_ignore_ascii_case("game") || p.eq_ignore_ascii_case("mod")) {
            continue;
        }
        let Some(v) = v.str() else { continue };
        let lower = v.to_lowercase();
        let (dir, rest) = if let Some(r) = lower.strip_prefix("|gameinfo_path|") {
            (gameinfo.to_path_buf(), &v[v.len() - r.len()..])
        } else if let Some(r) = lower.strip_prefix("|all_source_engine_paths|") {
            (base.clone(), &v[v.len() - r.len()..])
        } else {
            (base.clone(), v)
        };
        let rest = rest.trim_start_matches(['/', '\\']);
        if let Some(parent) = rest.strip_suffix("/*").or_else(|| rest.strip_suffix("\\*")) {
            let mut subs: Vec<PathBuf> = std::fs::read_dir(dir.join(parent)).into_iter().flatten().flatten().map(|e| e.path()).collect();
            subs.sort_by_key(|p| p.to_string_lossy().to_lowercase());
            out.extend(subs.into_iter().filter(|p| p.is_dir() || p.to_string_lossy().to_lowercase().ends_with("_dir.vpk")));
            continue;
        }
        let p = if rest == "." || rest.is_empty() { dir } else { dir.join(rest) };
        if rest.to_lowercase().ends_with(".vpk") {
            out.extend(dir_vpk(&p));
        } else if p.is_dir() {
            out.push(p);
        }
    }
    Ok(out)
}

fn same(a: &Path, b: &Path) -> bool {
    let c = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    c(a) == c(b)
}

pub struct Opened {
    pub fs: Fs,
    pub root: Option<PathBuf>,
    pub notes: Vec<String>,
}

pub fn open(map: &Arc<Map>, game: Option<&Path>) -> Result<Opened> {
    let mut srcs = Vec::new();
    let mut notes = Vec::new();
    match Zip::open(map.clone()) {
        Ok(Some(z)) => srcs.push(Src::Zip(z)),
        Ok(None) => {}
        Err(e) => notes.push(format!("map pakfile unreadable: {e:#}")),
    }
    let info = gameinfo_dir(&map.path, game);
    let mut paths: Vec<PathBuf> = Vec::new();
    match &info {
        Some(g) => match search_paths(g) {
            Ok(p) => paths = p,
            Err(e) => {
                notes.push(format!("{}: {e:#}; searching {} only", g.join("gameinfo.txt").display(), g.display()));
                paths.push(g.clone());
            }
        },
        None => {
            notes.push("gameinfo.txt not found next to the map or via --game; only the map's own pakfile and folder are searched".into());
            let maps = std::path::absolute(&map.path).ok().and_then(|p| p.parent().and_then(Path::parent).map(Path::to_path_buf));
            paths.extend(maps);
        }
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    for p in paths {
        if seen.iter().any(|s| same(s, &p)) {
            continue;
        }
        seen.push(p.clone());
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        if p.is_file() {
            if name.contains("_sound") {
                continue;
            }
            match Vpk::open(&p) {
                Ok(v) => srcs.push(Src::Vpk(v)),
                Err(e) => notes.push(format!("{e:#}")),
            }
            continue;
        }
        let pak = p.join("pak01_dir.vpk");
        if pak.is_file() && !seen.iter().any(|s| same(s, &pak)) {
            seen.push(pak.clone());
            match Vpk::open(&pak) {
                Ok(v) => srcs.push(Src::Vpk(v)),
                Err(e) => notes.push(format!("{e:#}")),
            }
        }
        srcs.push(Src::Dir(Dir::new(&p)));
    }
    let root = info.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    Ok(Opened { fs: Fs { srcs }, root, notes })
}
