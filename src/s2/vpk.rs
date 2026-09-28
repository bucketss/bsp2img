use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

use super::buf::{Rd, u32_};

const MAGIC: u32 = 0x55AA1234;
const SAME_FILE: u16 = 0x7fff;

pub struct Entry {
    preload: Vec<u8>,
    arc: u16,
    off: u32,
    len: u32,
}

impl Entry {
    pub fn size(&self) -> usize {
        self.preload.len() + self.len as usize
    }
}

pub struct Vpk {
    pub path: PathBuf,
    data_off: u64,
    entries: HashMap<String, Entry>,
    own: OnceLock<Option<File>>,
    files: Vec<OnceLock<Option<File>>>,
}

#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = f.seek_read(&mut buf[done..], off + done as u64)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        done += n;
    }
    Ok(())
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, off)
}

impl Vpk {
    pub fn open(path: &Path) -> Result<Vpk> {
        Self::read_tree(path, 0, None).with_context(|| format!("reading {}", path.display()))
    }

    pub fn open_at(path: &Path, offset: u64, len: u64) -> Result<Vpk> {
        Self::read_tree(path, offset, Some(len)).with_context(|| format!("reading {} at {offset}", path.display()))
    }

    fn read_tree(path: &Path, base: u64, limit: Option<u64>) -> Result<Vpk> {
        let f = File::open(path)?;
        let mut h = [0u8; 28];
        read_at(&f, &mut h[..12], base)?;
        if u32_(&h, 0)? != MAGIC {
            bail!("not a VPK");
        }
        let ver = u32_(&h, 4)?;
        let hdr = match ver {
            1 => 12,
            2 => 28,
            v => bail!("VPK version {v}"),
        };
        let tree = u32_(&h, 8)? as usize;
        if limit.is_some_and(|n| (hdr + tree) as u64 > n) {
            bail!("tree larger than the nested VPK");
        }
        let mut t = vec![0u8; tree];
        read_at(&f, &mut t, base + hdr as u64)?;
        let mut r = Rd::new(&t);
        let mut entries = HashMap::new();
        let mut max_arc = 0u16;
        let mut end = 0u64;
        loop {
            let ext = r.cstr()?;
            if ext.is_empty() {
                break;
            }
            loop {
                let dir = r.cstr()?;
                if dir.is_empty() {
                    break;
                }
                loop {
                    let name = r.cstr()?;
                    if name.is_empty() {
                        break;
                    }
                    let _crc = r.u32()?;
                    let pre = r.u16()? as usize;
                    let arc = r.u16()?;
                    let off = r.u32()?;
                    let len = r.u32()?;
                    r.u16()?;
                    let preload = r.take(pre)?.to_vec();
                    if arc != SAME_FILE {
                        max_arc = max_arc.max(arc + 1);
                    } else {
                        end = end.max(off as u64 + len as u64);
                    }
                    let full = if dir == " " { format!("{name}.{ext}") } else { format!("{dir}/{name}.{ext}") };
                    entries.insert(full.to_lowercase(), Entry { preload, arc, off, len });
                }
            }
        }
        if let Some(n) = limit {
            if max_arc > 0 {
                bail!("nested VPK refers to other archives");
            }
            if (hdr + tree) as u64 + end > n {
                bail!("entries run past the end of the nested VPK");
            }
        }
        let files = (0..max_arc).map(|_| OnceLock::new()).collect();
        Ok(Vpk { path: path.to_path_buf(), data_off: base + (hdr + tree) as u64, entries, own: OnceLock::new(), files })
    }

    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.get(&name.to_lowercase())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    fn arc_path(&self, arc: u16) -> PathBuf {
        let s = self.path.to_string_lossy();
        let base = s.strip_suffix("_dir.vpk").unwrap_or(&s);
        PathBuf::from(format!("{base}_{arc:03}.vpk"))
    }

    fn archive(&self, arc: u16) -> Result<&File> {
        let f = if arc == SAME_FILE {
            self.own.get_or_init(|| File::open(&self.path).ok())
        } else {
            self.files.get(arc as usize).context("archive index out of range")?.get_or_init(|| File::open(self.arc_path(arc)).ok())
        };
        f.as_ref().with_context(|| format!("missing archive {arc:03} of {}", self.path.display()))
    }

    pub fn nested(&self, name: &str) -> Result<Vpk> {
        let e = self.entry(name).with_context(|| format!("{name} not in {}", self.path.display()))?;
        if !e.preload.is_empty() {
            bail!("{name} in {} has preload data", self.path.display());
        }
        let (path, off) = if e.arc == SAME_FILE { (self.path.clone(), self.data_off + e.off as u64) } else { (self.arc_path(e.arc), e.off as u64) };
        Vpk::open_at(&path, off, e.len as u64)
    }

    pub fn read_limit(&self, e: &Entry, limit: usize) -> Result<Vec<u8>> {
        let n = e.size().min(limit);
        let mut out = Vec::with_capacity(n);
        out.extend_from_slice(&e.preload[..e.preload.len().min(n)]);
        let rest = n - out.len();
        if rest > 0 {
            let start = out.len();
            out.resize(n, 0);
            let off = if e.arc == SAME_FILE { self.data_off + e.off as u64 } else { e.off as u64 };
            read_at(self.archive(e.arc)?, &mut out[start..], off)?;
        }
        Ok(out)
    }
}

pub struct Dir {
    pub root: PathBuf,
    #[cfg(not(windows))]
    index: OnceLock<HashMap<String, PathBuf>>,
}

#[cfg(not(windows))]
fn walk(root: &Path, dir: &Path, out: &mut HashMap<String, PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            let k = rel.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>().join("/");
            out.insert(k, p);
        }
    }
}

impl Dir {
    pub fn new(root: &Path) -> Dir {
        Dir {
            root: root.to_path_buf(),
            #[cfg(not(windows))]
            index: OnceLock::new(),
        }
    }

    #[cfg(windows)]
    fn path(&self, name: &str) -> Option<PathBuf> {
        if name.split('/').any(|c| c == "..") {
            return None;
        }
        Some(self.root.join(name)).filter(|p| p.is_file())
    }

    #[cfg(not(windows))]
    fn path(&self, name: &str) -> Option<PathBuf> {
        if name.split('/').any(|c| c == "..") {
            return None;
        }
        let p = self.root.join(name);
        if p.is_file() {
            return Some(p);
        }
        let index = self.index.get_or_init(|| {
            let mut m = HashMap::new();
            walk(&self.root, &self.root, &mut m);
            m
        });
        index.get(name).cloned()
    }
}

fn read_file(p: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    File::open(p)?.take(limit as u64).read_to_end(&mut out)?;
    Ok(out)
}

pub enum Src {
    Vpk(Vpk),
    Dir(Dir),
}

impl Src {
    pub fn path(&self) -> &Path {
        match self {
            Src::Vpk(v) => &v.path,
            Src::Dir(d) => &d.root,
        }
    }

    fn has(&self, k: &str) -> bool {
        match self {
            Src::Vpk(v) => v.entries.contains_key(k),
            Src::Dir(d) => d.path(k).is_some(),
        }
    }

    fn read_limit(&self, k: &str, limit: usize) -> Option<Result<Vec<u8>>> {
        match self {
            Src::Vpk(v) => v.entries.get(k).map(|e| v.read_limit(e, limit)),
            Src::Dir(d) => d.path(k).map(|p| read_file(&p, limit)),
        }
    }

    fn names_with(&self, p: &str, out: &mut Vec<String>) {
        match self {
            Src::Vpk(v) => out.extend(v.entries.keys().filter(|n| n.starts_with(p)).cloned()),
            Src::Dir(d) => {
                let (dir, _) = p.rsplit_once('/').unwrap_or(("", p));
                if dir.split('/').any(|c| c == "..") {
                    return;
                }
                let Ok(rd) = std::fs::read_dir(d.root.join(dir)) else { return };
                for e in rd.flatten().filter(|e| e.path().is_file()) {
                    let n = e.file_name().to_string_lossy().to_lowercase();
                    let n = if dir.is_empty() { n } else { format!("{dir}/{n}") };
                    if n.starts_with(p) {
                        out.push(n);
                    }
                }
            }
        }
    }
}

pub struct Fs {
    pub srcs: Vec<Src>,
}

impl Fs {
    pub fn has(&self, name: &str) -> bool {
        let k = name.to_lowercase();
        self.srcs.iter().any(|s| s.has(&k))
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.read_limit(name, usize::MAX)
    }

    pub fn names_with(&self, prefix: &str) -> Vec<String> {
        let p = prefix.to_lowercase();
        let mut v = Vec::new();
        for s in &self.srcs {
            s.names_with(&p, &mut v);
        }
        v.sort();
        v.dedup();
        v
    }

    pub fn read_limit(&self, name: &str, limit: usize) -> Result<Vec<u8>> {
        let k = name.to_lowercase();
        let Some(r) = self.srcs.iter().find_map(|s| s.read_limit(&k, limit)) else { bail!("not found: {name}") };
        r.with_context(|| format!("reading {name}"))
    }
}

pub const PAKS: [&str; 4] = ["csgo", "csgo_imported", "csgo_core", "core"];

pub fn is_root(p: &Path) -> bool {
    p.join("csgo").join("pak01_dir.vpk").is_file() && p.join("csgo").join("gameinfo.gi").is_file()
}

pub fn find_root(game: &Path) -> Option<PathBuf> {
    [Some(game.to_path_buf()), Some(game.join("game")), game.parent().map(Path::to_path_buf)].into_iter().flatten().find(|p| is_root(p))
}

fn steam_root(p: &Path) -> Option<PathBuf> {
    let ws = p.ancestors().find(|a| a.file_name().is_some_and(|n| n.eq_ignore_ascii_case("workshop")))?;
    Some(ws.parent()?.join("common").join("Counter-Strike Global Offensive").join("game"))
}

pub fn game_root(vpk: &Path, game: Option<&Path>) -> Option<PathBuf> {
    let vpk = std::path::absolute(vpk).unwrap_or_else(|_| vpk.to_path_buf());
    let mut cands: Vec<PathBuf> = vpk.ancestors().skip(1).take(4).map(Path::to_path_buf).collect();
    if let Some(g) = game {
        cands.extend([g.to_path_buf(), g.join("game")]);
        cands.extend(g.parent().map(Path::to_path_buf));
    }
    cands.extend(steam_root(&vpk));
    cands.into_iter().find(|p| is_root(p))
}

pub fn workshop_dir(root: &Path) -> Option<PathBuf> {
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let d = root.parent()?.parent()?.parent()?.join("workshop").join("content").join("730");
    d.is_dir().then_some(d)
}

fn in_maps(map: &Path) -> Option<&Path> {
    let maps = map.parent()?;
    maps.file_name()?.eq_ignore_ascii_case("maps").then_some(maps.parent()?)
}

pub fn nested_addon(map: &Path) -> Option<PathBuf> {
    let dir = in_maps(map)?;
    let id = dir.file_name()?.to_string_lossy();
    let a = dir.join(format!("{id}_dir.vpk"));
    a.is_file().then_some(a)
}

pub fn local_addon(map: &Path) -> Option<PathBuf> {
    let addon = in_maps(map)?;
    addon.parent()?.file_name()?.eq_ignore_ascii_case("csgo_addons").then(|| addon.to_path_buf())
}

pub fn is_loose(path: &Path) -> bool {
    path.join("world.vwrld_c").is_file()
}

pub fn map_fs(vpk: &Path, root: Option<&Path>) -> Result<Fs> {
    let n = vpk.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
    let mut srcs = Vec::new();
    if vpk.is_dir() {
        let base = in_maps(vpk).with_context(|| format!("{} is not inside a maps folder", vpk.display()))?;
        srcs.push(Src::Dir(Dir::new(base)));
    } else if vpk.is_file() {
        srcs.push(Src::Vpk(Vpk::open(vpk)?));
        if let Some(a) = local_addon(vpk) {
            srcs.push(Src::Dir(Dir::new(&a)));
        }
    } else if let Some(a) = nested_addon(vpk) {
        let a = Vpk::open(&a)?;
        srcs.push(Src::Vpk(a.nested(&format!("maps/{n}.vpk"))?));
        srcs.push(Src::Vpk(a));
    } else {
        bail!("map not found: {}", vpk.display());
    }
    if let Some(root) = root {
        let addon = root.join("csgo_community_addons").join(&n).join(format!("{n}_dir.vpk"));
        if addon.is_file() {
            srcs.push(Src::Vpk(Vpk::open(&addon)?));
        }
        for m in PAKS {
            let p = root.join(m).join("pak01_dir.vpk");
            if p.is_file() {
                srcs.push(Src::Vpk(Vpk::open(&p)?));
            }
        }
    }
    Ok(Fs { srcs })
}

pub fn addon_maps(dir_vpk: &Path) -> Vec<String> {
    let Ok(v) = Vpk::open(dir_vpk) else { return Vec::new() };
    let mut out: Vec<String> = v
        .names()
        .filter_map(|k| k.strip_prefix("maps/")?.strip_suffix(".vpk"))
        .filter(|n| !n.contains('/'))
        .filter(|n| v.nested(&format!("maps/{n}.vpk")).is_ok_and(|m| m.entry(&format!("maps/{n}/world.vwrld_c")).is_some()))
        .map(str::to_string)
        .collect();
    out.sort();
    out
}

pub fn is_map(path: &Path) -> bool {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
    let menu = stem.ends_with("_vanity") || stem == "graphics_settings" || stem == "lobby_mapveto";
    if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("vpk")) || stem.ends_with("_dir") || menu {
        return false;
    }
    Vpk::open(path).is_ok_and(|v| v.entry(&format!("maps/{stem}/world.vwrld_c")).is_some())
}
