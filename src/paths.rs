use std::path::{Path, PathBuf};

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Result, bail};
use rayon::prelude::*;

#[cfg(windows)]
fn cfg_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(not(windows))]
fn cfg_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

pub fn app_dir() -> Option<PathBuf> {
    cfg_dir().map(|d| d.join("bsp2img"))
}

pub fn resolve_map(arg: &str, game: Option<&Path>) -> Result<PathBuf> {
    let p = Path::new(arg);
    if p.is_file() {
        let world = p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("world.vwrld_c"));
        return Ok(p.parent().filter(|_| world).unwrap_or(p).to_path_buf());
    }
    if crate::s2::vpk::is_loose(p) || crate::s2::vpk::nested_addon(p).is_some() {
        return Ok(p.to_path_buf());
    }
    if let Some(game) = game {
        let lower = arg.to_lowercase();
        let fnames = if lower.ends_with(".bsp") || lower.ends_with(".vpk") {
            vec![arg.to_string()]
        } else {
            vec![format!("{arg}.bsp"), format!("{arg}.vpk")]
        };
        let roots = roots(game);
        let mut cands = Vec::new();
        for root in roots {
            cands.push(root.clone());
            if let Ok(rd) = std::fs::read_dir(&root) {
                let mut subs: Vec<String> = rd
                    .flatten()
                    .filter(|e| e.path().join("maps").is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                subs.sort();
                subs.sort_by_key(|d| {
                    let l = d.to_lowercase();
                    (!matches!(l.as_str(), "cstrike" | "csgo" | "tf" | "left4dead" | "left4dead2"), l.ends_with("_downloads") || l.ends_with("_addon"))
                });
                cands.extend(subs.iter().map(|d| root.join(d)));
            }
        }
        for c in cands {
            for f in &fnames {
                let p = c.join("maps").join(f);
                if p.is_file() {
                    return Ok(p);
                }
            }
        }
    }
    if let Some(p) = game.and_then(|g| addon_map(arg, g)) {
        return Ok(p);
    }
    bail!("map not found: {arg}")
}

fn sorted_dirs(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    v
}

fn in_addon(addon: &Path, map: &str) -> Option<PathBuf> {
    let maps = addon.join("maps");
    let v = maps.join(format!("{map}.vpk"));
    if crate::s2::vpk::is_map(&v) {
        return Some(v);
    }
    let d = maps.join(map);
    crate::s2::vpk::is_loose(&d).then_some(d)
}

fn in_workshop(item: &Path, map: &str) -> Option<PathBuf> {
    let id = item.file_name()?.to_string_lossy();
    let maps = crate::s2::vpk::addon_maps(&item.join(format!("{id}_dir.vpk")));
    let m = maps.iter().find(|m| m.eq_ignore_ascii_case(map))?;
    Some(item.join("maps").join(format!("{m}.vpk")))
}

fn addon_map(arg: &str, game: &Path) -> Option<PathBuf> {
    let root = crate::s2::vpk::find_root(game)?;
    let ws = crate::s2::vpk::workshop_dir(&root);
    let parts: Vec<&str> = arg.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match parts[..] {
        [w, id, m] if w.eq_ignore_ascii_case("workshop") && digits(id) => in_workshop(&ws?.join(id), m),
        [w, m] if w.eq_ignore_ascii_case("workshop") => sorted_dirs(&ws?).iter().find_map(|d| in_workshop(d, m)),
        [id, m] if digits(id) && ws.as_ref().is_some_and(|w| w.join(id).is_dir()) => in_workshop(&ws?.join(id), m),
        [a, m] => in_addon(&root.join("csgo_addons").join(a), m),
        [m] => std::iter::once(root.join("csgo_addons").join(m))
            .chain(sorted_dirs(&root.join("csgo_addons")))
            .find_map(|d| in_addon(&d, m))
            .or_else(|| sorted_dirs(ws.as_ref()?).iter().find_map(|d| in_workshop(d, m))),
        _ => None,
    }
}

pub fn workshop_title(map: &Path) -> Option<String> {
    let item = crate::s2::vpk::nested_addon(map)?.parent()?.to_path_buf();
    let id = item.file_name()?.to_string_lossy().into_owned();
    let text = std::fs::read_to_string(item.join("publish_data.txt")).ok()?;
    let title = text.lines().find_map(|l| {
        let q: Vec<&str> = l.split('"').collect();
        (q.len() >= 4 && q[1].eq_ignore_ascii_case("title")).then(|| q[3].to_string())
    })?;
    Some(format!("{title}, workshop {id}"))
}

static CS2_MAPS: Mutex<Option<HashMap<PathBuf, (SystemTime, bool)>>> = Mutex::new(None);

fn cs2_maps(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let stamp = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    let known: HashMap<PathBuf, (SystemTime, bool)> = CS2_MAPS.lock().unwrap().clone().unwrap_or_default();
    let checked: Vec<(PathBuf, SystemTime, bool)> = files
        .into_par_iter()
        .map(|p| {
            let t = stamp(&p);
            let ok = match known.get(&p) {
                Some(&(kt, ok)) if kt == t => ok,
                _ => crate::s2::vpk::is_map(&p),
            };
            (p, t, ok)
        })
        .collect();
    let mut cache = CS2_MAPS.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    let mut out = Vec::new();
    for (p, t, ok) in checked {
        cache.insert(p.clone(), (t, ok));
        if ok {
            out.push(p);
        }
    }
    out
}

fn roots(game: &Path) -> Vec<PathBuf> {
    let sub = game.join("game");
    match (sub.is_dir(), crate::s2::vpk::is_root(&sub)) {
        (true, true) => vec![sub, game.to_path_buf()],
        (true, false) => vec![game.to_path_buf(), sub],
        _ => vec![game.to_path_buf()],
    }
}

pub fn list_maps(game: &Path) -> Vec<(String, PathBuf)> {
    let mut dirs = Vec::new();
    for root in roots(game) {
        dirs.push(root.clone());
        if let Ok(rd) = std::fs::read_dir(&root) {
            let mut subs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("maps").is_dir()).collect();
            subs.sort_by_key(|p| {
                let l = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                (!matches!(l.as_str(), "cstrike" | "csgo" | "tf" | "left4dead" | "left4dead2"), l.ends_with("_downloads") || l.ends_with("_addon"), l)
            });
            dirs.extend(subs);
        }
    }
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut bsps = Vec::new();
    let mut vpks = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d.join("maps")) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("bsp")) {
                bsps.push(p);
            } else if crate::s2::is_vpk(&p) {
                vpks.push(p);
            }
        }
    }
    let cs2_first = roots(game).first().is_some_and(|r| crate::s2::vpk::is_root(r));
    let cs2 = cs2_maps(vpks);
    let (first, second) = if cs2_first { (cs2, bsps) } else { (bsps, cs2) };
    for p in first.into_iter().chain(second) {
        let n = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        if !out.iter().any(|(k, _)| k.eq_ignore_ascii_case(&n)) {
            out.push((n, p));
        }
    }
    if let Some(root) = crate::s2::vpk::find_root(game) {
        addon_list(&root, &mut out);
    }
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

type WsCache = HashMap<PathBuf, (SystemTime, Vec<String>)>;

static WS_MAPS: Mutex<Option<WsCache>> = Mutex::new(None);

fn workshop_maps(items: Vec<PathBuf>) -> Vec<(PathBuf, Vec<String>)> {
    let dir_vpk = |d: &Path| d.join(format!("{}_dir.vpk", d.file_name().unwrap_or_default().to_string_lossy()));
    let stamp = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    let known = WS_MAPS.lock().unwrap().clone().unwrap_or_default();
    let found: Vec<(PathBuf, SystemTime, Vec<String>)> = items
        .into_par_iter()
        .filter(|d| dir_vpk(d).is_file())
        .map(|d| {
            let v = dir_vpk(&d);
            let t = stamp(&v);
            let maps = match known.get(&v) {
                Some((kt, m)) if *kt == t => m.clone(),
                _ => crate::s2::vpk::addon_maps(&v),
            };
            (d, t, maps)
        })
        .collect();
    let mut cache = WS_MAPS.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    found
        .into_iter()
        .map(|(d, t, maps)| {
            cache.insert(dir_vpk(&d), (t, maps.clone()));
            (d, maps)
        })
        .collect()
}

fn addon_list(root: &Path, out: &mut Vec<(String, PathBuf)>) {
    let sky = |n: &str| n.to_lowercase().ends_with("_3dsky");
    let fname = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let push = |out: &mut Vec<(String, PathBuf)>, n: String, p: PathBuf| {
        if !out.iter().any(|(k, _)| k.eq_ignore_ascii_case(&n)) {
            out.push((n, p));
        }
    };
    let mut vpks = Vec::new();
    for a in sorted_dirs(&root.join("csgo_addons")) {
        let Ok(rd) = std::fs::read_dir(a.join("maps")) else { continue };
        let mut ps: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        ps.sort();
        for p in ps {
            if crate::s2::vpk::is_loose(&p) {
                if !sky(&fname(&p)) {
                    push(out, format!("{}/{}", fname(&a), fname(&p)), p);
                }
            } else if crate::s2::is_vpk(&p) {
                vpks.push(p);
            }
        }
    }
    for p in cs2_maps(vpks) {
        let n = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let a = p.parent().and_then(Path::parent).map(fname).unwrap_or_default();
        if !sky(&n) {
            push(out, format!("{a}/{n}"), p);
        }
    }
    let Some(ws) = crate::s2::vpk::workshop_dir(root) else { return };
    for (d, maps) in workshop_maps(sorted_dirs(&ws)) {
        let id = fname(&d);
        for m in maps.iter().filter(|m| !sky(m)) {
            let label = format!("workshop/{m}");
            let label = if out.iter().any(|(k, _)| k.eq_ignore_ascii_case(&label)) { format!("{id}/{m}") } else { label };
            push(out, label, d.join("maps").join(format!("{m}.vpk")));
        }
    }
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

pub fn mod_tag(engine: crate::bsp::Engine) -> &'static str {
    match engine {
        crate::bsp::Engine::GoldSrc => "cs",
        crate::bsp::Engine::Source(v) if v >= 21 => "csgo",
        crate::bsp::Engine::Source(_) => "css",
        crate::bsp::Engine::Cs2 => "cs2",
    }
}

pub fn game_tag(bsp: &crate::bsp::Bsp) -> &'static str {
    match bsp.game {
        crate::bsp::Game::Tf2 => "tf2",
        crate::bsp::Game::L4d => "l4d",
        crate::bsp::Game::L4d2 => "l4d2",
        crate::bsp::Game::Cs => mod_tag(bsp.engine),
    }
}

pub fn run_name(bsp: &crate::bsp::Bsp) -> String {
    format!("{}-{}", game_tag(bsp), bsp.name())
}

pub fn run_dir(base: &Path, name: &str) -> Result<PathBuf> {
    let [y, mo, d, h, mi, s] = crate::poster::now_parts();
    let stem = format!("{name}-{y:04}-{mo:02}{d:02}-{h:02}{mi:02}");
    let mut p = base.join(&stem);
    if p.exists() {
        p = base.join(format!("{stem}-{s:02}"));
        let mut i = 2;
        while p.exists() {
            p = base.join(format!("{stem}-{s:02}-{i}"));
            i += 1;
        }
    }
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
        let dir = p.parent().filter(|d| !d.as_os_str().is_empty() && d.is_dir());
        let pat = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let stem_pat = pat.strip_suffix(".bsp").or(pat.strip_suffix(".vpk")).unwrap_or(&pat).to_string();
        let label = a.replace('\\', "/");
        let label_pat = label.strip_suffix(".bsp").or(label.strip_suffix(".vpk")).unwrap_or(&label).to_string();
        let mut found: Vec<String> = match dir {
            Some(d) => std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("bsp")) || crate::s2::vpk::is_map(f) || crate::s2::vpk::is_loose(f))
                .filter(|f| glob_match(stem_pat.as_bytes(), f.file_stem().unwrap_or_default().to_string_lossy().as_bytes()))
                .map(|f| f.to_string_lossy().into_owned())
                .collect(),
            None => game
                .map(list_maps)
                .unwrap_or_default()
                .into_iter()
                .filter(|(n, _)| glob_match(label_pat.as_bytes(), n.as_bytes()))
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
