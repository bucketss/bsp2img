use std::collections::HashMap;
use std::fs::File;
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
        Self::read_tree(path).with_context(|| format!("reading {}", path.display()))
    }

    fn read_tree(path: &Path) -> Result<Vpk> {
        let f = File::open(path)?;
        let mut h = [0u8; 28];
        read_at(&f, &mut h[..12], 0)?;
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
        let mut t = vec![0u8; tree];
        read_at(&f, &mut t, hdr as u64)?;
        let mut r = Rd::new(&t);
        let mut entries = HashMap::new();
        let mut max_arc = 0u16;
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
                    }
                    let full = if dir == " " { format!("{name}.{ext}") } else { format!("{dir}/{name}.{ext}") };
                    entries.insert(full.to_lowercase(), Entry { preload, arc, off, len });
                }
            }
        }
        let files = (0..max_arc).map(|_| OnceLock::new()).collect();
        Ok(Vpk { path: path.to_path_buf(), data_off: (hdr + tree) as u64, entries, own: OnceLock::new(), files })
    }

    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.get(&name.to_lowercase())
    }

    fn archive(&self, arc: u16) -> Result<&File> {
        let f = if arc == SAME_FILE {
            self.own.get_or_init(|| File::open(&self.path).ok())
        } else {
            self.files.get(arc as usize).context("archive index out of range")?.get_or_init(|| {
                let s = self.path.to_string_lossy();
                let base = s.strip_suffix("_dir.vpk").unwrap_or(&s);
                File::open(format!("{base}_{arc:03}.vpk")).ok()
            })
        };
        f.as_ref().with_context(|| format!("missing archive {arc:03} of {}", self.path.display()))
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

pub struct Fs {
    pub paks: Vec<Vpk>,
}

impl Fs {
    fn find(&self, name: &str) -> Option<(&Vpk, &Entry)> {
        let k = name.to_lowercase();
        self.paks.iter().find_map(|p| p.entries.get(&k).map(|e| (p, e)))
    }

    pub fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.read_limit(name, usize::MAX)
    }

    pub fn read_limit(&self, name: &str, limit: usize) -> Result<Vec<u8>> {
        let Some((p, e)) = self.find(name) else { bail!("not found: {name}") };
        p.read_limit(e, limit).with_context(|| format!("reading {name}"))
    }
}

pub const PAKS: [&str; 4] = ["csgo", "csgo_imported", "csgo_core", "core"];

fn is_root(p: &Path) -> bool {
    p.join("csgo").join("pak01_dir.vpk").is_file()
}

pub fn game_root(vpk: &Path, game: Option<&Path>) -> Option<PathBuf> {
    let vpk = std::path::absolute(vpk).unwrap_or_else(|_| vpk.to_path_buf());
    let mut cands: Vec<PathBuf> = vpk.ancestors().skip(1).take(4).map(Path::to_path_buf).collect();
    if let Some(g) = game {
        cands.extend([g.to_path_buf(), g.join("game")]);
        cands.extend(g.parent().map(Path::to_path_buf));
    }
    cands.into_iter().find(|p| is_root(p))
}

pub fn map_fs(vpk: &Path, root: Option<&Path>) -> Result<Fs> {
    let mut paks = vec![Vpk::open(vpk)?];
    if let Some(root) = root {
        let n = vpk.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
        let addon = root.join("csgo_community_addons").join(&n).join(format!("{n}_dir.vpk"));
        if addon.is_file() {
            paks.push(Vpk::open(&addon)?);
        }
        for m in PAKS {
            let p = root.join(m).join("pak01_dir.vpk");
            if p.is_file() {
                paks.push(Vpk::open(&p)?);
            }
        }
    }
    Ok(Fs { paks })
}

pub fn is_map(path: &Path) -> bool {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
    let menu = stem.ends_with("_vanity") || stem == "graphics_settings" || stem == "lobby_mapveto";
    if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("vpk")) || stem.ends_with("_dir") || menu {
        return false;
    }
    Vpk::open(path).is_ok_and(|v| v.entry(&format!("maps/{stem}/world.vwrld_c")).is_some())
}
