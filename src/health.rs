use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glam::DVec3;
use tiny_skia::{FillRule, PathBuilder, Pixmap, Stroke, Transform};

use crate::bsp::{Bsp, LUMP_ENTITIES, LUMP_LIGHTING, LUMP_TEXTURES, LUMP_VISIBILITY, TEX_SPECIAL};
use crate::camera::top_down;
use crate::grid::{CT, T, Text, grid_frame, line, paint, rect};
use crate::nav::{Nav, STAND_OFS};
use crate::paths::{Partial, free_name};
use crate::look::Look;
use crate::render::{Cuts, NO_CLIP, Renderer, View};
use crate::scene::{Report, Scene};
use crate::sky::find_sky;
use crate::wad::TextureSource;

const HLSDK_MAX_MAP_MODELS: usize = 400;
const HLSDK_MAX_MAP_ENTITIES: usize = 1024;
const HLSDK_MAX_MAP_ENTSTRING: usize = 128 * 1024;
const HLSDK_MAX_MAP_PLANES: usize = 32767;
const HLSDK_MAX_MAP_NODES: usize = 32767;
const HLSDK_MAX_MAP_CLIPNODES: usize = 32767;
const HLSDK_MAX_MAP_LEAFS: usize = 8192;
const HLSDK_MAX_MAP_VERTS: usize = 65535;
const HLSDK_MAX_MAP_FACES: usize = 65535;
const HLSDK_MAX_MAP_MARKSURFACES: usize = 65535;
const HLSDK_MAX_MAP_TEXINFO: usize = 8192;
const HLSDK_MAX_MAP_EDGES: usize = 256000;
const HLSDK_MAX_MAP_SURFEDGES: usize = 512000;
const HLSDK_MAX_MAP_TEXTURES: usize = 512;
const HLSDK_MAX_MAP_MIPTEX: usize = 0x200000;
const HLSDK_MAX_MAP_LIGHTING: usize = 0x200000;
const HLSDK_MAX_MAP_VISIBILITY: usize = 0x200000;

const VHLT34_MAX_MAP_MODELS: usize = 512;
const VHLT34_MAX_MAP_ENTITIES: usize = 16384;
const VHLT34_MAX_MAP_ENTSTRING: usize = 2048 * 1024;
const VHLT34_MAX_MAP_PLANES: usize = 32768;
const VHLT34_MAX_MAP_NODES: usize = 32767;
const VHLT34_MAX_MAP_CLIPNODES: usize = 32767;
const VHLT34_MAX_MAP_LEAFS: usize = 32760;
const VHLT34_MAX_MAP_LEAFS_ENGINE: usize = 8192;
const VHLT34_MAX_MAP_VERTS: usize = 65535;
const VHLT34_MAX_MAP_FACES: usize = 65535;
const VHLT34_MAX_MAP_WORLDFACES: usize = 32768;
const VHLT34_MAX_MAP_MARKSURFACES: usize = 65535;
const VHLT34_MAX_MAP_TEXINFO: usize = 32767;
const VHLT34_MAX_MAP_EDGES: usize = 256000;
const VHLT34_MAX_MAP_SURFEDGES: usize = 512000;
const VHLT34_MAX_MAP_TEXTURES: usize = 4096;
const VHLT34_DEFAULT_MAX_MAP_MIPTEX: usize = 0x2000000;
const VHLT34_DEFAULT_MAX_MAP_LIGHTDATA: usize = 0x3000000;
const VHLT34_MAX_MAP_VISIBILITY: usize = 0x800000;

const REHLDS_QLIMITS_MAX_MODELS: usize = 512;

const WARN_PCT: f64 = 75.0;
const BAD_PCT: f64 = 90.0;
const SPAWNS_FOR_32_SLOTS: usize = 16;
const M2_PER_UNIT2: f64 = 0.0254 * 0.0254;
const OPEN_SPOTS: usize = 3;
const GREEN: [u8; 3] = [70, 190, 90];
const AMBER: [u8; 3] = [240, 170, 40];
const RED: [u8; 3] = [230, 60, 50];

#[derive(Clone, Debug, PartialEq)]
pub struct HealthOpts {
    pub cell: f64,
    pub size: u32,
}

impl Default for HealthOpts {
    fn default() -> Self {
        HealthOpts { cell: 8.0, size: 600 }
    }
}

pub struct Limit {
    pub label: &'static str,
    pub count: usize,
    pub engine: Option<usize>,
    pub hlsdk: Option<usize>,
    pub vhlt: usize,
}

impl Limit {
    fn cap(&self) -> usize {
        self.engine.map_or(self.vhlt, |e| e.min(self.vhlt))
    }

    pub fn pct(&self) -> f64 {
        self.count as f64 * 100.0 / self.cap() as f64
    }

    fn over_hlsdk(&self) -> bool {
        self.hlsdk.is_some_and(|h| self.count > h)
    }
}

fn level_color(pct: f64) -> [u8; 3] {
    if pct > BAD_PCT {
        RED
    } else if pct >= WARN_PCT {
        AMBER
    } else {
        GREEN
    }
}

type LimitRow = (&'static str, &'static str, Option<usize>, Option<usize>, usize);

const LIMITS: [LimitRow; 19] = [
    ("models", "models", Some(REHLDS_QLIMITS_MAX_MODELS), Some(HLSDK_MAX_MAP_MODELS), VHLT34_MAX_MAP_MODELS),
    ("planes", "planes", None, Some(HLSDK_MAX_MAP_PLANES), VHLT34_MAX_MAP_PLANES),
    ("vertices", "vertices", None, Some(HLSDK_MAX_MAP_VERTS), VHLT34_MAX_MAP_VERTS),
    ("nodes", "nodes", None, Some(HLSDK_MAX_MAP_NODES), VHLT34_MAX_MAP_NODES),
    ("texinfo", "texinfo", None, Some(HLSDK_MAX_MAP_TEXINFO), VHLT34_MAX_MAP_TEXINFO),
    ("faces", "faces", None, Some(HLSDK_MAX_MAP_FACES), VHLT34_MAX_MAP_FACES),
    ("world_faces", "world faces", None, None, VHLT34_MAX_MAP_WORLDFACES),
    ("clipnodes", "clipnodes", None, Some(HLSDK_MAX_MAP_CLIPNODES), VHLT34_MAX_MAP_CLIPNODES),
    ("leaves", "leaves", None, Some(HLSDK_MAX_MAP_LEAFS), VHLT34_MAX_MAP_LEAFS),
    ("world_leaves", "world leaves", Some(VHLT34_MAX_MAP_LEAFS_ENGINE), None, VHLT34_MAX_MAP_LEAFS_ENGINE),
    ("marksurfaces", "marksurfaces", None, Some(HLSDK_MAX_MAP_MARKSURFACES), VHLT34_MAX_MAP_MARKSURFACES),
    ("edges", "edges", None, Some(HLSDK_MAX_MAP_EDGES), VHLT34_MAX_MAP_EDGES),
    ("surfedges", "surfedges", None, Some(HLSDK_MAX_MAP_SURFEDGES), VHLT34_MAX_MAP_SURFEDGES),
    ("textures", "textures", None, Some(HLSDK_MAX_MAP_TEXTURES), VHLT34_MAX_MAP_TEXTURES),
    ("texture_bytes", "texture data", None, Some(HLSDK_MAX_MAP_MIPTEX), VHLT34_DEFAULT_MAX_MAP_MIPTEX),
    ("lighting_bytes", "lighting data", None, Some(HLSDK_MAX_MAP_LIGHTING), VHLT34_DEFAULT_MAX_MAP_LIGHTDATA),
    ("vis_bytes", "vis data", None, Some(HLSDK_MAX_MAP_VISIBILITY), VHLT34_MAX_MAP_VISIBILITY),
    ("entities", "entities", None, Some(HLSDK_MAX_MAP_ENTITIES), VHLT34_MAX_MAP_ENTITIES),
    ("entity_bytes", "entity data", None, Some(HLSDK_MAX_MAP_ENTSTRING), VHLT34_MAX_MAP_ENTSTRING),
];

fn limits(bsp: &Bsp) -> Vec<Limit> {
    let counts = [
        bsp.models.len(),
        bsp.planes.len(),
        bsp.vertices.len(),
        bsp.nodes.len(),
        bsp.texinfo.len(),
        bsp.faces.len(),
        bsp.models[0].numfaces.max(0) as usize,
        bsp.clipnodes.len(),
        bsp.leaves.len(),
        bsp.models[0].visleafs.max(0) as usize,
        bsp.marksurfaces.len(),
        bsp.edges.len(),
        bsp.surfedges.len(),
        bsp.miptex.len(),
        bsp.lump_len[LUMP_TEXTURES],
        bsp.lump_len[LUMP_LIGHTING],
        bsp.lump_len[LUMP_VISIBILITY],
        bsp.entities.len(),
        bsp.lump_len[LUMP_ENTITIES],
    ];
    LIMITS.iter().zip(counts).map(|(&(_, label, engine, hlsdk, vhlt), count)| Limit { label, count, engine, hlsdk, vhlt }).collect()
}

fn find_file(dirs: &[PathBuf], rel: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| rel.split('/').fold(d.clone(), |p, s| p.join(s))).find(|p| p.is_file())
}

fn game_dirs(scene: &Scene) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for d in &scene.dirs {
        if !out.contains(d) {
            out.push(d.clone());
        }
    }
    out
}

fn norm(s: &str) -> String {
    s.trim().replace('\\', "/").trim_start_matches('/').to_string()
}

fn referenced_files(bsp: &Bsp) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in &bsp.entities {
        for (k, v) in &e.0 {
            let v = norm(v);
            let lv = v.to_lowercase();
            if v.starts_with('*') || v.starts_with('!') || v.is_empty() {
                continue;
            }
            let sound = lv.ends_with(".wav") || lv.ends_with(".mp3");
            let is_sound_key = k.starts_with("noise") || (k == "message" && e.class() == "ambient_generic");
            if k == "model" && (lv.ends_with(".mdl") || lv.ends_with(".spr")) {
                out.insert(v);
            } else if is_sound_key && sound {
                out.insert(format!("sound/{v}"));
            }
        }
    }
    out
}

pub struct Bmp8 {
    pub w: i32,
    pub h: i32,
    pub n255: usize,
}

fn read_bmp8(p: &Path) -> Result<Bmp8, String> {
    let d = std::fs::read(p).map_err(|e| e.to_string())?;
    if d.len() < 54 || &d[..2] != b"BM" {
        return Err("not a BMP".into());
    }
    let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let off = u32_at(10) as usize;
    let w = u32_at(18) as i32;
    let h = u32_at(22) as i32;
    let bpp = u16_at(28);
    let comp = u32_at(30);
    if bpp != 8 {
        return Err(format!("{bpp}-bit, not 8-bit"));
    }
    if comp != 0 {
        return Err("compressed".into());
    }
    let row = (w.max(0) as usize).div_ceil(4) * 4;
    let rows = h.unsigned_abs() as usize;
    let mut n255 = 0;
    for y in 0..rows {
        let s = off + y * row;
        let Some(r) = d.get(s..s + w.max(0) as usize) else { return Err("truncated".into()) };
        n255 += r.iter().filter(|&&c| c == 255).count();
    }
    Ok(Bmp8 { w, h: h.abs(), n255 })
}

fn res_entries(p: &Path) -> Vec<String> {
    let text: String = std::fs::read(p).unwrap_or_default().iter().map(|&c| c as char).collect();
    text.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .flat_map(|l| l.split_whitespace().map(|t| t.trim_matches('"').to_string()).collect::<Vec<_>>())
        .map(|t| norm(&t).to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

pub struct Spot {
    pub pos: DVec3,
    pub radius: f64,
}

pub struct NavInfo {
    pub off_ground: Vec<(String, DVec3)>,
    pub walkable_m2: f64,
    pub cells: usize,
    pub spots: Vec<Spot>,
}

pub struct Health {
    pub name: String,
    pub t: Vec<DVec3>,
    pub ct: Vec<DVec3>,
    pub objectives: Vec<(&'static str, usize)>,
    pub buyzones: usize,
    pub sky: String,
    pub sky_found: bool,
    pub nav: Result<NavInfo, String>,
    pub engine: Engine,
}

pub enum Engine {
    Gold(GoldHealth),
}

pub struct GoldHealth {
    pub limits: Vec<Limit>,
    pub vis_bytes: usize,
    pub light_bytes: usize,
    pub unlit: usize,
    pub lightable: usize,
    pub missing_wads: Vec<String>,
    pub missing_textures: Vec<String>,
    pub missing_files: Vec<String>,
    pub ov: [Option<PathBuf>; 3],
    pub bmp: Option<Result<Bmp8, String>>,
    pub res: Option<PathBuf>,
    pub res_lists: [bool; 3],
}

const OV_EXTS: [&str; 3] = ["txt", "bmp", "tga"];

fn count(bsp: &Bsp, classes: &[&str]) -> usize {
    bsp.entities.iter().filter(|e| classes.contains(&e.class())).count()
}

fn nav_info(bsp: &Bsp, o: &HealthOpts, t: &[DVec3], ct: &[DVec3]) -> Result<NavInfo> {
    let nav = Nav::build(bsp, o.cell, 250.0)?;
    let mut off_ground = Vec::new();
    let mut seeds = Vec::new();
    for (team, pts) in [("T", t), ("CT", ct)] {
        for &p in pts {
            match nav.nearest(p, STAND_OFS) {
                Some(n) => seeds.push(n),
                None => off_ground.push((team.to_string(), p)),
            }
        }
    }
    let dist = nav.flood(&seeds, 250.0);
    let mut reach: Vec<usize> = (0..nav.len()).filter(|&n| dist[n].is_finite()).collect();
    if reach.is_empty() {
        reach = (0..nav.len()).collect();
    }
    let clear = nav.clearance();
    let pos = |n: usize| {
        let (x, y) = nav.cell_xy(nav.col[n] as usize);
        DVec3::new(x, y, nav.nodes[n].z as f64)
    };
    let mut order = reach.clone();
    order.sort_by(|&a, &b| clear[b].total_cmp(&clear[a]));
    let mut spots: Vec<Spot> = Vec::new();
    for n in order {
        if spots.len() >= OPEN_SPOTS {
            break;
        }
        let (p, r) = (pos(n), clear[n] as f64);
        if !r.is_finite() {
            continue;
        }
        if spots.iter().all(|s| (s.pos.x - p.x).hypot(s.pos.y - p.y) >= s.radius + r) {
            spots.push(Spot { pos: p, radius: r });
        }
    }
    Ok(NavInfo {
        off_ground,
        walkable_m2: reach.len() as f64 * o.cell * o.cell * M2_PER_UNIT2,
        cells: reach.len(),
        spots,
    })
}

pub fn gather(scene: &Scene, name: &str, o: &HealthOpts) -> Health {
    let bsp = &scene.bsp;
    let dirs = game_dirs(scene);
    let mut tex = TextureSource::new(&dirs);
    let missing_wads = tex.use_worldspawn(bsp.worldspawn().get("wad").unwrap_or(""));
    let sky = scene.sky_name(None);
    let sky_found = find_sky(&sky, &dirs).is_some();
    let missing_files: Vec<String> =
        referenced_files(bsp).into_iter().filter(|f| find_file(&dirs, f).is_none()).collect();

    let (mut unlit, mut lightable) = (0, 0);
    for f in &bsp.faces {
        let ti = &bsp.texinfo[f.texinfo as usize];
        let special = ti.flags & TEX_SPECIAL != 0
            || usize::try_from(ti.miptex).ok().and_then(|m| bsp.miptex.get(m)).is_some_and(|m| m.name.starts_with('!'));
        if special {
            continue;
        }
        lightable += 1;
        if f.lightofs < 0 {
            unlit += 1;
        }
    }

    let origins = |cls: &str| -> Vec<DVec3> { bsp.entities.iter().filter(|e| e.class() == cls).filter_map(|e| e.origin()).collect() };
    let t = origins("info_player_deathmatch");
    let ct = origins("info_player_start");
    let objectives = vec![
        ("bombsites", count(bsp, &["func_bomb_target", "info_bomb_target"])),
        ("hostages", count(bsp, &["hostage_entity"])),
        ("rescue zones", count(bsp, &["func_hostage_rescue", "info_hostage_rescue"])),
        ("VIP starts", count(bsp, &["info_vip_start"])),
        ("VIP escapes", count(bsp, &["func_vip_safetyzone"])),
        ("escape zones", count(bsp, &["func_escapezone"])),
    ];
    let buyzones = count(bsp, &["func_buyzone"]);

    let ov = OV_EXTS.map(|e| find_file(&dirs, &format!("overviews/{name}.{e}")));
    let bmp = ov[1].as_ref().map(|p| read_bmp8(p));
    let res = dirs
        .iter()
        .flat_map(|d| [d.join("maps").join(format!("{name}.res")), d.join(format!("{name}.res"))])
        .find(|p| p.is_file() && p.parent().is_some_and(|q| q.file_name().is_some_and(|n| n.eq_ignore_ascii_case("maps"))));
    let entries = res.as_deref().map(res_entries).unwrap_or_default();
    let lname = name.to_lowercase();
    let res_lists = OV_EXTS.map(|e| entries.iter().any(|x| *x == format!("overviews/{lname}.{e}")));

    let nav = nav_info(bsp, o, &t, &ct).map_err(|e| format!("{e:#}"));
    Health {
        name: name.to_string(),
        t,
        ct,
        objectives,
        buyzones,
        sky,
        sky_found,
        nav,
        engine: Engine::Gold(GoldHealth {
            limits: limits(bsp),
            vis_bytes: bsp.lump_len[LUMP_VISIBILITY],
            light_bytes: bsp.lump_len[LUMP_LIGHTING],
            unlit,
            lightable,
            missing_wads,
            missing_textures: scene.mesh.missing.clone(),
            missing_files,
            ov,
            bmp,
            res,
            res_lists,
        }),
    }
}

fn list(v: &[String]) -> String {
    if v.is_empty() { "none".into() } else { v.join(", ") }
}

fn opt(v: Option<usize>) -> String {
    v.map_or("-".into(), |v| v.to_string())
}

impl Health {
    pub fn warnings(&self) -> Vec<String> {
        match &self.engine {
            Engine::Gold(g) => self.gold_warnings(g),
        }
    }

    fn gold_warnings(&self, g: &GoldHealth) -> Vec<String> {
        let mut w = Vec::new();
        if g.vis_bytes == 0 {
            w.push("VIS was not run (usually a leak, or -novis)".to_string());
        }
        if g.light_bytes == 0 {
            w.push("no lighting data (RAD not run)".to_string());
        } else if g.unlit > 0 {
            w.push(format!("{} of {} lightable faces have no lightmap (-nolight or partial RAD)", g.unlit, g.lightable));
        }
        for l in &g.limits {
            if l.pct() > BAD_PCT {
                w.push(format!("{} at {:.0}% of the limit", l.label, l.pct()));
            }
        }
        for (team, n) in [("T", self.t.len()), ("CT", self.ct.len())] {
            if n < SPAWNS_FOR_32_SLOTS {
                w.push(format!("{n} {team} spawns (fewer than {SPAWNS_FOR_32_SLOTS} for a 32-slot server)"));
            }
        }
        match &self.nav {
            Ok(n) if !n.off_ground.is_empty() => w.push(format!("{} spawns not on walkable ground", n.off_ground.len())),
            Err(e) => w.push(format!("walk grid failed: {e}")),
            _ => {}
        }
        if self.objectives.iter().all(|(_, n)| *n == 0) {
            w.push("no objectives".to_string());
        }
        if !g.missing_wads.is_empty() {
            w.push(format!("{} WADs missing", g.missing_wads.len()));
        }
        if !g.missing_textures.is_empty() {
            w.push(format!("{} textures missing", g.missing_textures.len()));
        }
        if !self.sky_found {
            w.push(format!("sky {} missing", self.sky));
        }
        if !g.missing_files.is_empty() {
            w.push(format!("{} models/sprites/sounds missing", g.missing_files.len()));
        }
        if g.ov[0].is_none() || g.ov[1].is_none() {
            w.push("no overview".to_string());
        }
        if let Some(Ok(b)) = &g.bmp {
            if b.n255 > 0 {
                w.push(format!("overview BMP uses palette index 255 in {} pixels", b.n255));
            }
        }
        if let Some(Err(e)) = &g.bmp {
            w.push(format!("overview BMP: {e}"));
        }
        if g.ov[1].is_some() && !g.res_lists[1] {
            w.push("overview BMP not listed in the .res".to_string());
        }
        w
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        let _ = self.write_text(&mut s);
        s
    }

    fn write_text(&self, s: &mut String) -> std::fmt::Result {
        match &self.engine {
            Engine::Gold(g) => self.write_gold(g, s),
        }
    }

    fn write_gold(&self, g: &GoldHealth, s: &mut String) -> std::fmt::Result {
        writeln!(s, "{} health", self.name)?;
        writeln!(s)?;
        writeln!(s, "Compile")?;
        if g.vis_bytes == 0 {
            writeln!(s, "  VIS            not run")?;
        } else {
            writeln!(s, "  VIS            {} bytes", g.vis_bytes)?;
        }
        writeln!(s, "  lighting       {} bytes, {} of {} lightable faces unlit", g.light_bytes, g.unlit, g.lightable)?;
        writeln!(s)?;
        writeln!(s, "Limits (% of the engine limit where known, else VHLT 34)")?;
        writeln!(s, "  {:<15}{:>10}{:>10}{:>10}{:>10}{:>7}", "", "count", "engine", "HLSDK", "VHLT", "%")?;
        for l in &g.limits {
            writeln!(
                s,
                "  {:<15}{:>10}{:>10}{:>10}{:>10}{:>6.0}%{}",
                l.label,
                l.count,
                opt(l.engine),
                opt(l.hlsdk),
                l.vhlt,
                l.pct(),
                if l.over_hlsdk() { "  over HLSDK" } else { "" }
            )?;
        }
        writeln!(s)?;
        writeln!(s, "Gameplay")?;
        writeln!(s, "  spawns         T {}, CT {}", self.t.len(), self.ct.len())?;
        match &self.nav {
            Ok(n) => {
                if n.off_ground.is_empty() {
                    writeln!(s, "  off ground     none")?;
                }
                for (team, p) in &n.off_ground {
                    writeln!(s, "  off ground     {team} at {:.0} {:.0} {:.0}", p.x, p.y, p.z)?;
                }
            }
            Err(e) => writeln!(s, "  off ground     unknown ({e})")?,
        }
        let obj: Vec<String> = self.objectives.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {k}")).collect();
        writeln!(s, "  objectives     {}", list(&obj))?;
        if self.buyzones == 0 {
            writeln!(s, "  buy zones      none (CS adds default zones around spawns)")?;
        } else {
            writeln!(s, "  buy zones      {}", self.buyzones)?;
        }
        writeln!(s)?;
        writeln!(s, "Missing assets")?;
        writeln!(s, "  WADs           {}", list(&g.missing_wads))?;
        writeln!(s, "  textures       {}", list(&g.missing_textures))?;
        writeln!(s, "  sky            {}", if self.sky_found { format!("{} found", self.sky) } else { format!("{} missing", self.sky) })?;
        writeln!(s, "  files          {}", list(&g.missing_files))?;
        writeln!(s)?;
        writeln!(s, "Overview")?;
        for (i, e) in OV_EXTS.iter().enumerate() {
            let where_ = g.ov[i].as_ref().map_or("missing".to_string(), |p| p.display().to_string());
            writeln!(s, "  .{e:<13} {where_}")?;
        }
        match &g.bmp {
            Some(Ok(b)) => writeln!(
                s,
                "  BMP            {}x{}, palette index 255 in {} pixels",
                b.w,
                b.h,
                b.n255
            )?,
            Some(Err(e)) => writeln!(s, "  BMP            {e}")?,
            None => {}
        }
        match &g.res {
            Some(p) => {
                let listed: Vec<String> =
                    OV_EXTS.iter().zip(g.res_lists).filter(|(_, b)| *b).map(|(e, _)| format!(".{e}")).collect();
                writeln!(s, "  .res           {} lists {}", p.display(), if listed.is_empty() { "no overview files".into() } else { listed.join(" ") })?;
            }
            None => writeln!(s, "  .res           missing")?,
        }
        writeln!(s)?;
        match &self.nav {
            Ok(n) => {
                writeln!(s, "Open areas (walkable {:.0} m2 in {} cells)", n.walkable_m2, n.cells)?;
                for (i, sp) in n.spots.iter().enumerate() {
                    writeln!(s, "  {}. x {:.0} y {:.0} z {:.0}, radius {:.0} units", i + 1, sp.pos.x, sp.pos.y, sp.pos.z, sp.radius)?;
                }
            }
            Err(e) => writeln!(s, "Open areas: unknown ({e})")?,
        }
        writeln!(s)?;
        let w = self.warnings();
        writeln!(s, "Warnings ({})", w.len())?;
        for x in &w {
            writeln!(s, "  - {x}")?;
        }
        Ok(())
    }

    pub fn csv_header() -> String {
        let mut cols: Vec<String> = [
            "map", "error", "warnings", "vis_bytes", "lighting_bytes", "unlit_faces", "lightable_faces", "t_spawns",
            "ct_spawns", "spawns_off_ground", "bombsites", "hostages", "rescue_zones", "vip_starts", "vip_escapes",
            "escape_zones", "buyzones", "missing_wads", "missing_textures", "sky_missing", "missing_files", "ov_txt",
            "ov_bmp", "ov_tga", "bmp_index255_px", "res", "res_txt", "res_bmp", "res_tga", "walkable_m2", "open_radius",
        ]
        .map(String::from)
        .to_vec();
        for (key, ..) in LIMITS {
            cols.push(key.to_string());
            cols.push(format!("{key}_pct"));
        }
        cols.join(",")
    }

    pub fn csv_row(&self) -> String {
        match &self.engine {
            Engine::Gold(g) => self.gold_row(g),
        }
    }

    fn gold_row(&self, g: &GoldHealth) -> String {
        let b = |v: bool| if v { "1" } else { "0" }.to_string();
        let (off, m2, rad) = match &self.nav {
            Ok(n) => (
                n.off_ground.len().to_string(),
                format!("{:.0}", n.walkable_m2),
                n.spots.first().map_or(String::new(), |s| format!("{:.0}", s.radius)),
            ),
            Err(_) => (String::new(), String::new(), String::new()),
        };
        let mut cols = vec![
            self.name.clone(),
            match &self.nav {
                Err(e) => csv_quote(&format!("walk grid: {e}")),
                _ => String::new(),
            },
            self.warnings().len().to_string(),
            g.vis_bytes.to_string(),
            g.light_bytes.to_string(),
            g.unlit.to_string(),
            g.lightable.to_string(),
            self.t.len().to_string(),
            self.ct.len().to_string(),
            off,
        ];
        cols.extend(self.objectives.iter().map(|(_, n)| n.to_string()));
        cols.extend([
            self.buyzones.to_string(),
            g.missing_wads.len().to_string(),
            g.missing_textures.len().to_string(),
            b(!self.sky_found),
            g.missing_files.len().to_string(),
            b(g.ov[0].is_some()),
            b(g.ov[1].is_some()),
            b(g.ov[2].is_some()),
            match &g.bmp {
                Some(Ok(x)) => x.n255.to_string(),
                Some(Err(_)) => "?".into(),
                None => String::new(),
            },
            b(g.res.is_some()),
            b(g.res_lists[0]),
            b(g.res_lists[1]),
            b(g.res_lists[2]),
            m2,
            rad,
        ]);
        for l in &g.limits {
            cols.push(l.count.to_string());
            cols.push(format!("{:.1}", l.pct()));
        }
        cols.join(",")
    }
}

pub fn csv_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn card(r: &mut Renderer, h: &Health, cuts: &Cuts, size: u32) -> Result<image::RgbImage> {
    let g = grid_frame(r, cuts, size);
    let view = View {
        basis: top_down(DVec3::X, DVec3::Y),
        cx: (g.x0 + g.x1) / 2.0,
        cy: (g.y0 + g.y1) / 2.0,
        w: g.x1 - g.x0,
        h: g.y1 - g.y0,
        sky_yaw: None,
        persp: None,
    };
    let rc = Cuts { clip: NO_CLIP, use_mask: false, ..*cuts };
    let thumb = r.render_view(&view, g.wpx, g.hpx, 2, &rc, &Look::plain(true, Some([0x1c, 0x1c, 0x1c])), 0.0)?;
    let title = Text::new(18.0);
    let text = Text::new(13.0);
    let pw = 560u32;
    let row = 18.0f32;
    let facts = card_facts(h);
    let limits: &[Limit] = match &h.engine {
        Engine::Gold(g) => &g.limits,
    };
    let panel_h = 44.0 + row * (limits.len() as f32 + 1.5) + 16.0 * (facts.len() as f32 + 1.0) + 12.0;
    let (w, ht) = (g.wpx + pw, g.hpx.max(panel_h.ceil() as u32).max(size / 2));
    let mut pm = Pixmap::new(w, ht).context("pixmap")?;
    pm.fill(tiny_skia::Color::from_rgba8(0x1c, 0x1c, 0x1c, 255));
    {
        let data = pm.data_mut();
        for y in 0..g.hpx as usize {
            let src = &thumb.as_raw()[y * g.wpx as usize * 4..(y + 1) * g.wpx as usize * 4];
            data[y * w as usize * 4..y * w as usize * 4 + src.len()].copy_from_slice(src);
        }
    }
    let sx = |x: f64| ((x - g.x0) / g.upp) as f32;
    let sy = |y: f64| ((g.y1 - y) / g.upp) as f32;
    let off: Vec<DVec3> = h.nav.as_ref().map(|n| n.off_ground.iter().map(|(_, p)| *p).collect()).unwrap_or_default();
    for (pts, col) in [(&h.t, T), (&h.ct, CT)] {
        for p in pts {
            if let Some(c) = PathBuilder::from_circle(sx(p.x), sy(p.y), 3.0) {
                pm.fill_path(&c, &paint(col, 255), FillRule::Winding, Transform::identity(), None);
                pm.stroke_path(&c, &paint([0, 0, 0], 255), &Stroke::default(), Transform::identity(), None);
            }
            if off.contains(p) {
                if let Some(c) = PathBuilder::from_circle(sx(p.x), sy(p.y), 8.0) {
                    pm.stroke_path(&c, &paint(RED, 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
                }
            }
        }
    }
    if let Ok(n) = &h.nav {
        for (i, s) in n.spots.iter().enumerate() {
            let (cx, cy, rr) = (sx(s.pos.x), sy(s.pos.y), (s.radius / g.upp) as f32);
            if let Some(c) = PathBuilder::from_circle(cx, cy, rr.max(2.0)) {
                pm.fill_path(&c, &paint([255, 230, 0], 40), FillRule::Winding, Transform::identity(), None);
                pm.stroke_path(&c, &paint([255, 230, 0], 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
            }
            let l = format!("{}", i + 1);
            let (tw, th) = text.size(&l);
            text.draw(&mut pm, cx - tw / 2.0, cy - th / 2.0, &l, [255, 230, 0]);
        }
    }

    let x0 = g.wpx as f32 + 16.0;
    let mut y = 12.0;
    title.draw(&mut pm, x0, y, &format!("{} health", h.name), [255, 255, 255]);
    y += 32.0;
    let (lw, bw) = (110.0, 170.0);
    for l in limits {
        let pct = l.pct();
        let col = level_color(pct);
        text.draw(&mut pm, x0, y, l.label, [220, 220, 220]);
        let bx = x0 + lw;
        rect(&mut pm, bx, y + 3.0, bx + bw, y + 14.0, [60, 60, 60], 255);
        rect(&mut pm, bx, y + 3.0, bx + bw * (pct / 100.0).min(1.0) as f32, y + 14.0, col, 255);
        let hl = if l.over_hlsdk() { "  >HLSDK" } else { "" };
        let s = format!("{:>3.0}%  {} / {}{hl}", pct, l.count, l.cap());
        text.draw(&mut pm, bx + bw + 8.0, y, &s, col);
        y += row;
    }
    y += row * 0.5;
    for (s, col) in &facts {
        text.draw(&mut pm, x0, y, s, *col);
        y += 16.0;
    }
    line(&mut pm, (g.wpx as f32 + 0.5, 0.0), (g.wpx as f32 + 0.5, ht as f32), [70, 70, 70], 255, 1.0);
    let rgb: Vec<u8> = pm.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    image::RgbImage::from_raw(w, ht, rgb).context("image")
}

fn card_facts(h: &Health) -> Vec<(String, [u8; 3])> {
    match &h.engine {
        Engine::Gold(g) => gold_facts(h, g),
    }
}

fn gold_facts(h: &Health, g: &GoldHealth) -> Vec<(String, [u8; 3])> {
    let white = [220, 220, 220];
    let ok = |b: bool| if b { GREEN } else { RED };
    let mut v = Vec::new();
    v.push((
        if g.vis_bytes == 0 { "VIS: not run".to_string() } else { format!("VIS: {} KiB", g.vis_bytes / 1024) },
        ok(g.vis_bytes > 0),
    ));
    v.push((format!("lighting: {} KiB, {} unlit faces", g.light_bytes / 1024, g.unlit), ok(g.light_bytes > 0 && g.unlit == 0)));
    let spawns_ok = h.t.len() >= SPAWNS_FOR_32_SLOTS && h.ct.len() >= SPAWNS_FOR_32_SLOTS;
    v.push((format!("spawns: T {}, CT {}", h.t.len(), h.ct.len()), if spawns_ok { GREEN } else { AMBER }));
    match &h.nav {
        Ok(n) => v.push((format!("spawns off ground: {}", n.off_ground.len()), ok(n.off_ground.is_empty()))),
        Err(_) => v.push(("spawns off ground: unknown".into(), AMBER)),
    }
    let obj: Vec<String> = h.objectives.iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("{n} {k}")).collect();
    v.push((format!("objectives: {}", list(&obj)), if obj.is_empty() { AMBER } else { white }));
    v.push((
        if h.buyzones == 0 { "buy zones: default".to_string() } else { format!("buy zones: {}", h.buyzones) },
        white,
    ));
    let miss = g.missing_wads.len() + g.missing_textures.len() + g.missing_files.len() + usize::from(!h.sky_found);
    v.push((
        format!(
            "missing: {} WADs, {} textures, {} files{}",
            g.missing_wads.len(),
            g.missing_textures.len(),
            g.missing_files.len(),
            if h.sky_found { "" } else { ", sky" }
        ),
        ok(miss == 0),
    ));
    let have: Vec<&str> = OV_EXTS.iter().zip(&g.ov).filter(|(_, p)| p.is_some()).map(|(e, _)| *e).collect();
    let bmp_ok = !matches!(&g.bmp, Some(Ok(b)) if b.n255 > 0) && !matches!(&g.bmp, Some(Err(_)));
    v.push((
        format!(
            "overview: {}{}",
            if have.is_empty() { "none".to_string() } else { have.join(" ") },
            match &g.bmp {
                Some(Ok(b)) if b.n255 > 0 => ", BMP uses index 255",
                Some(Err(_)) => ", BMP not 8-bit",
                _ => "",
            }
        ),
        if g.ov[0].is_some() && g.ov[1].is_some() && bmp_ok { GREEN } else { AMBER },
    ));
    let listed: Vec<&str> = OV_EXTS.iter().zip(g.res_lists).filter(|(_, b)| *b).map(|(e, _)| *e).collect();
    v.push((
        match &g.res {
            Some(_) => format!(".res lists overview: {}", if listed.is_empty() { "nothing".into() } else { listed.join(" ") }),
            None => ".res: none".to_string(),
        },
        if g.ov[1].is_none() || g.res_lists[1] { white } else { AMBER },
    ));
    match &h.nav {
        Ok(n) => {
            v.push((format!("walkable area: {:.0} m2", n.walkable_m2), white));
            for (i, s) in n.spots.iter().enumerate() {
                v.push((
                    format!("open area {}: {:.0} {:.0}, radius {:.0}", i + 1, s.pos.x, s.pos.y, s.radius),
                    [255, 230, 0],
                ));
            }
        }
        Err(e) => v.push((format!("walk grid: {e}"), AMBER)),
    }
    v
}

pub fn export_health(
    r: &mut Renderer,
    scene: &Scene,
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &HealthOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<(Vec<PathBuf>, Health)> {
    if scene.bsp.is_cs2() {
        anyhow::bail!("health reports are for GoldSrc maps only");
    }
    rep.step(0.0)?;
    let h = gather(scene, name, o);
    rep.step(0.6)?;
    let txt = h.text();
    for w in h.warnings() {
        rep.log(format!("  {w}"));
    }
    let mut files = Partial::default();
    let img = card(r, &h, cuts, o.size)?;
    rep.step(0.9)?;
    let f = free_name(out, &format!("{name}{cut_tag}_health"), ".png");
    files.add(f.clone());
    img.save(&f)?;
    rep.log(format!("  {}", f.display()));
    let f = free_name(out, &format!("{name}{cut_tag}_health"), ".txt");
    files.add(f.clone());
    std::fs::write(&f, &txt)?;
    rep.log(format!("  {}", f.display()));
    rep.step(1.0)?;
    Ok((files.keep(), h))
}
