use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub fn resolve_map(arg: &str, game: Option<&Path>) -> Result<PathBuf> {
    let p = Path::new(arg);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    if let Some(game) = game {
        let fname = if arg.to_lowercase().ends_with(".bsp") { arg.to_string() } else { format!("{arg}.bsp") };
        let mut cands = vec![game.to_path_buf()];
        if let Ok(rd) = std::fs::read_dir(game) {
            let mut subs: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().join("maps").is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            subs.sort();
            subs.sort_by_key(|d| {
                let l = d.to_lowercase();
                (l != "cstrike", l.ends_with("_downloads") || l.ends_with("_addon"))
            });
            cands.extend(subs.iter().map(|d| game.join(d)));
        }
        for c in cands {
            let p = c.join("maps").join(&fname);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    bail!("map not found: {arg}")
}

pub fn list_maps(game: &Path) -> Vec<(String, PathBuf)> {
    let mut dirs = vec![game.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(game) {
        let mut subs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("maps").is_dir()).collect();
        subs.sort_by_key(|p| {
            let l = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            (l != "cstrike", l.ends_with("_downloads") || l.ends_with("_addon"), l)
        });
        dirs.extend(subs);
    }
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d.join("maps")) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("bsp")) {
                let n = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                if !out.iter().any(|(k, _)| k.eq_ignore_ascii_case(&n)) {
                    out.push((n, p));
                }
            }
        }
    }
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

fn strip_suffix(game: &str) -> String {
    let g = game.trim_end_matches(['\\', '/']);
    for s in ["_downloads", "_addon"] {
        if g.to_lowercase().ends_with(s) {
            return g[..g.len() - s.len()].to_string();
        }
    }
    g.to_string()
}

pub fn search_dirs(bsp_path: &Path, game: Option<&Path>, extra: &[PathBuf]) -> Vec<PathBuf> {
    let bsp_path = std::path::absolute(bsp_path).unwrap_or_else(|_| bsp_path.to_path_buf());
    let bsp_dir = bsp_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let is_maps = bsp_dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("maps"));
    let game = if is_maps {
        bsp_dir.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        game.map(Path::to_path_buf).unwrap_or_else(|| bsp_dir.parent().map(Path::to_path_buf).unwrap_or_default())
    };
    let game = PathBuf::from(strip_suffix(&game.to_string_lossy()));
    let root = game.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = game.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let gs = game.to_string_lossy().into_owned();
    let mut dirs: Vec<PathBuf> = extra.to_vec();
    dirs.extend([
        bsp_dir.clone(),
        game.clone(),
        PathBuf::from(format!("{gs}_addon")),
        PathBuf::from(format!("{gs}_downloads")),
        root.join("valve"),
        root.join("valve_downloads"),
    ]);
    if name != "cstrike" && root.join("cstrike").is_dir() {
        dirs.push(root.join("cstrike"));
    }
    dirs
}

pub fn run_dir(base: &Path, name: &str) -> Result<PathBuf> {
    let mut i = 0;
    while base.join(format!("{name}{i:02}")).exists() {
        i += 1;
    }
    let p = base.join(format!("{name}{i:02}"));
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

pub fn free_name(base: &Path, stem: &str, ext: &str) -> PathBuf {
    let p = base.join(format!("{stem}{ext}"));
    if !p.exists() {
        return p;
    }
    let mut i = 0;
    while base.join(format!("{stem}_{i:02}{ext}")).exists() {
        i += 1;
    }
    base.join(format!("{stem}_{i:02}{ext}"))
}

pub fn set_suffix(dir: &Path, stems: &[String], ext: &str) -> String {
    if !stems.iter().any(|s| dir.join(format!("{s}{ext}")).exists()) {
        return String::new();
    }
    let mut i = 0;
    while stems.iter().any(|s| dir.join(format!("{s}_{i:02}{ext}")).exists()) {
        i += 1;
    }
    format!("_{i:02}")
}

#[derive(Default)]
pub struct Partial {
    files: Vec<PathBuf>,
    keep: bool,
}

impl Partial {
    pub fn add(&mut self, p: PathBuf) {
        self.files.push(p);
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn keep(mut self) -> Vec<PathBuf> {
        self.keep = true;
        std::mem::take(&mut self.files)
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.keep {
            for f in &self.files {
                let _ = std::fs::remove_file(f);
            }
        }
    }
}

pub fn glob_match(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob_match(&p[1..], s) || (!s.is_empty() && glob_match(p, &s[1..])),
        (Some(b'?'), Some(_)) => glob_match(&p[1..], &s[1..]),
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && glob_match(&p[1..], &s[1..]),
        _ => false,
    }
}

pub fn expand_maps(args: &[String], game: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        if !a.contains(['*', '?']) {
            out.push(a.clone());
            continue;
        }
        let p = Path::new(a);
        let dir = p.parent().filter(|d| !d.as_os_str().is_empty());
        let pat = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let stem_pat = pat.strip_suffix(".bsp").unwrap_or(&pat).to_string();
        let mut found: Vec<String> = match dir {
            Some(d) => std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("bsp")))
                .filter(|f| glob_match(stem_pat.as_bytes(), f.file_stem().unwrap_or_default().to_string_lossy().as_bytes()))
                .map(|f| f.to_string_lossy().into_owned())
                .collect(),
            None => game
                .map(list_maps)
                .unwrap_or_default()
                .into_iter()
                .filter(|(n, _)| glob_match(stem_pat.as_bytes(), n.as_bytes()))
                .map(|(n, _)| n)
                .collect(),
        };
        found.sort_by_key(|n| n.to_lowercase());
        out.extend(found);
    }
    out
}

pub fn expand_demos(args: &[String]) -> Vec<PathBuf> {
    let is_dem = |p: &Path| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dem"));
    let mut out = Vec::new();
    for a in args {
        let p = Path::new(a);
        if p.is_dir() {
            let mut found: Vec<PathBuf> =
                std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|f| is_dem(f)).collect();
            found.sort();
            out.extend(found);
        } else if a.contains(['*', '?']) {
            let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let pat = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|f| is_dem(f) && glob_match(pat.as_bytes(), f.file_name().unwrap_or_default().to_string_lossy().as_bytes()))
                .collect();
            found.sort();
            out.extend(found);
        } else {
            out.push(p.to_path_buf());
        }
    }
    out.dedup();
    out
}
