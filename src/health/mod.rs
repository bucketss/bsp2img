use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use glam::DVec3;
use rayon::prelude::*;
use tiny_skia::{FillRule, PathBuilder, Pixmap, Stroke, Transform};

use crate::bsp::{Bsp, Game, LUMP_ENTITIES, LUMP_LIGHTING, LUMP_TEXTURES, LUMP_VISIBILITY, TEX_SPECIAL};
use crate::camera::top_down;
use crate::grid::{CT, T, Text, grid_frame, line, paint, rect};
use crate::nav::{NONE, Nav, STAND_OFS, STEP, root, unite};
use crate::overview::{RADAR, Radar};
use crate::paths::{Partial, free_name};
use crate::look::Look;
use crate::phys::{Mask, Phys, PhysStats};
use crate::radar::DEFAULT;
use crate::render::{Cuts, NO_CLIP, Renderer, View};
use crate::s2::radar as s2radar;
use crate::extra::{Extra, LoadStats, PhysSrc};
use crate::s1::bsp::{self as s1b, Map};
use crate::s1::sprp::StaticProps;
use crate::scene::{Report, Scene, num};
use crate::sky::find_sky;
use crate::wad::TextureSource;

mod card;
mod gold;
mod modern;
mod source;

use card::*;
use gold::*;
use modern::*;
use source::*;

const WARN_PCT: f64 = 75.0;
const BAD_PCT: f64 = 90.0;
const SPAWNS_FOR_32_SLOTS: usize = 16;
const M2_PER_UNIT2: f64 = 0.0254 * 0.0254;
const OPEN_SPOTS: usize = 3;
const COMP_MIN: usize = 5;
const PLAYER_HALF: f64 = 16.0;
const PLAYER_HEIGHT: f64 = 72.0;
const BOX_LIFT: f64 = 2.0;
const HOLE_BELOW: f32 = 24.0;
const HOLE_ABOVE: f32 = STEP;
const HOLE_CLUSTERS: usize = 5;
const RADAR_MARGIN_PX: f64 = 16.0;
const COVER_MIN: f64 = 0.99;
const ICON_FAR: f64 = 512.0;
const ALIGN_MARGIN: f64 = 0.04;
const SHORT_LIST: usize = 10;
const UNLIT_WARN_PCT: f64 = 1.0;
const CS2_COLS: [&str; 11] = [
    "failed_models",
    "missing_materials",
    "draws",
    "triangles",
    "comp_t",
    "comp_ct",
    "disabled_spawns",
    "min_spawn_gap",
    "spawns_in_collision",
    "collision_holes",
    "radar_img",
];
const HOLE: [u8; 3] = [230, 60, 230];
const GREEN: [u8; 3] = [70, 190, 90];
const AMBER: [u8; 3] = [240, 170, 40];
const RED: [u8; 3] = [230, 60, 50];

#[derive(Clone, Debug, PartialEq)]
pub struct HealthOpts {
    pub cell: f64,
    pub size: u32,
    pub pixelwalks: bool,
}

impl Default for HealthOpts {
    fn default() -> Self {
        HealthOpts { cell: 8.0, size: 600, pixelwalks: false }
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

pub struct Bmp8 {
    pub w: i32,
    pub h: i32,
    pub n255: usize,
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
    pub game: Game,
    pub game_checks: Vec<String>,
    pub engine: Engine,
}

pub enum Engine {
    Gold(Box<GoldHealth>),
    Cs2(Box<ModernHealth>),
    Source(Box<SrcHealth>),
}

pub struct ModernHealth {
    pub paks: Vec<PathBuf>,
    pub legacy: usize,
    pub bad_models: Vec<String>,
    pub missing_materials: Vec<String>,
    pub missing_textures: Vec<String>,
    pub draws: usize,
    pub dropped: usize,
    pub materials: usize,
    pub models: usize,
    pub tris: usize,
    pub lightmap: Option<(u32, u32)>,
    pub probes: usize,
    pub comp: [usize; 2],
    pub disabled: [usize; 2],
    pub gaps: [Option<(f64, DVec3)>; 2],
    pub stuck: Result<Vec<(String, DVec3)>, String>,
    pub areas: usize,
    pub mesh_ladders: usize,
    pub graph: Option<Graph>,
    pub collision: Result<Collision, String>,
    pub radar: RadarCheck,
}

pub struct Graph {
    pub cells: usize,
    pub reach: usize,
    pub components: usize,
    pub largest: usize,
    pub ladders: usize,
}

pub struct Collision {
    pub stats: PhysStats,
    pub checked: usize,
    pub holes: usize,
    pub clusters: Vec<(DVec3, usize)>,
    pub nclusters: usize,
    pub secs: f64,
}

pub type RadarImage = (String, Option<String>, Option<(usize, usize)>);

#[derive(Default)]
pub struct RadarCheck {
    pub txt: bool,
    pub err: Option<String>,
    pub frame: Option<(Radar, Option<(usize, usize)>)>,
    pub images: Vec<RadarImage>,
    pub cover: Option<(f64, f64)>,
    pub icons: Vec<(String, Option<f64>)>,
    pub align: Option<String>,
}

impl RadarCheck {
    fn images_ok(&self) -> bool {
        !self.images.is_empty() && self.images.iter().all(|i| i.1.is_some())
    }
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
    pub pixelwalks: Option<Result<PwInfo, String>>,
}

pub struct PwInfo {
    pub stand: usize,
    pub duck: usize,
    pub worst: Option<String>,
}

fn pixelwalk_info(bsp: &Bsp) -> Result<PwInfo, String> {
    use crate::pixelwalk::present::{PixelwalkOpts, Stance, find_pixelwalks, setpos};
    let o = PixelwalkOpts {
        finder: Default::default(),
        merge_gap: 64.0,
        split: crate::radar::Split::None,
        base: Default::default(),
        size: 0,
        labels: false,
    };
    let pws = find_pixelwalks(bsp, &o).map_err(|e| format!("{e:#}"))?;
    let stand = pws.iter().filter(|p| p.stance == Stance::Stand).count();
    let worst = pws
        .iter()
        .max_by(|a, b| a.span().total_cmp(&b.span()).then((b.stance as u8).cmp(&(a.stance as u8))))
        .map(|p| format!("{} {:.0}u span, {}", p.stance.label(), p.span(), setpos(p.from, p.yaw)));
    Ok(PwInfo { stand, duck: pws.len() - stand, worst })
}

pub struct SrcLimit {
    pub key: &'static str,
    pub label: &'static str,
    pub count: usize,
    pub cap: Option<usize>,
}

impl SrcLimit {
    pub fn pct(&self) -> Option<f64> {
        self.cap.map(|c| self.count as f64 * 100.0 / c as f64)
    }
}

pub struct Lit {
    pub bytes: usize,
    pub lightable: usize,
    pub unlit: usize,
}

pub enum NavFile {
    Used { file: String, version: u32, sub: u32, analyzed: bool, areas: usize, places: usize, named: usize, built_for: Option<usize> },
    Ignored { file: String, why: String },
    Missing { file: String },
}

pub struct SrcHealth {
    pub base: ModernHealth,
    pub version: u32,
    pub revision: i32,
    pub game: &'static str,
    pub sdk: &'static str,
    pub limits: Vec<SrcLimit>,
    pub ldr: Option<Lit>,
    pub hdr: Option<Lit>,
    pub vis_bytes: usize,
    pub displacements: usize,
    pub static_props: usize,
    pub failed_models: Vec<String>,
    pub missing_models: Vec<String>,
    pub nav: NavFile,
    pub pak: Option<(usize, usize)>,
}

const OV_EXTS: [&str; 3] = ["txt", "bmp", "tga"];

pub(super) fn teams(game: Game) -> [&'static str; 2] {
    match game {
        Game::Tf2 => ["RED", "BLU"],
        Game::L4d | Game::L4d2 => ["SAFE", "START"],
        Game::Cs => ["T", "CT"],
    }
}

pub(super) fn game_checks(bsp: &Bsp) -> Vec<String> {
    let mut w = Vec::new();
    if bsp.is_l4d() {
        if count(bsp, &["info_l4d_saferoom"]) == 0 {
            w.push("no saferoom (nav checkpoint areas)".to_string());
        }
        if count(bsp, &["info_changelevel", "trigger_finale"]) == 0 {
            w.push("no info_changelevel or trigger_finale".to_string());
        }
        if count(bsp, &["info_director"]) == 0 {
            w.push("no info_director".to_string());
        }
        return w;
    }
    if !bsp.is_tf2() {
        return w;
    }
    let neutral = count(bsp, &["info_player_teamspawn"]);
    for (team, cls) in [("RED", "info_player_deathmatch"), ("BLU", "info_player_start")] {
        if neutral == 0 && count(bsp, &[cls]) == 0 {
            w.push(format!("no {team} spawns"));
        }
    }
    if count(bsp, &["team_control_point"]) > 0 && count(bsp, &["team_control_point_master"]) == 0 {
        w.push("control points without a team_control_point_master".to_string());
    }
    if count(bsp, &["func_respawnroom"]) == 0 {
        w.push("no func_respawnroom".to_string());
    }
    w
}

fn count(bsp: &Bsp, classes: &[&str]) -> usize {
    bsp.entities.iter().filter(|e| classes.contains(&e.class())).count()
}

struct Walk {
    info: NavInfo,
    nav: std::sync::Arc<Nav>,
    reach: Vec<usize>,
}

fn nav_info(bsp: &Bsp, o: &HealthOpts, t: &[DVec3], ct: &[DVec3]) -> Result<NavInfo> {
    walk(bsp, o, t, ct).map(|w| w.info)
}

fn walk(bsp: &Bsp, o: &HealthOpts, t: &[DVec3], ct: &[DVec3]) -> Result<Walk> {
    let nav = Nav::build(bsp, o.cell, 250.0)?;
    let mut off_ground = Vec::new();
    let mut seeds = Vec::new();
    let [tn, ctn] = teams(bsp.game);
    for (team, pts) in [(tn, t), (ctn, ct)] {
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
    let info = NavInfo { off_ground, walkable_m2: reach.len() as f64 * o.cell * o.cell * M2_PER_UNIT2, cells: reach.len(), spots };
    Ok(Walk { info, nav, reach })
}

pub fn gather(scene: &Scene, name: &str, o: &HealthOpts) -> Health {
    if let Some(x) = &scene.bsp.extra {
        if let PhysSrc::Source(map) = &x.phys_src {
            return gather_src(scene, x, map, name, o);
        }
        return gather_cs2(scene, x, name, o);
    }
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

    let (t, ct, objectives, buyzones) = basics(bsp);

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
        game: Game::Cs,
        game_checks: Vec::new(),
        engine: Engine::Gold(Box::new(GoldHealth {
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
            pixelwalks: o.pixelwalks.then(|| pixelwalk_info(bsp)),
        })),
    }
}

type Basics = (Vec<DVec3>, Vec<DVec3>, Vec<(&'static str, usize)>, usize);

fn basics(bsp: &Bsp) -> Basics {
    let origins = |cls: &str| -> Vec<DVec3> { bsp.entities.iter().filter(|e| e.class() == cls).filter_map(|e| e.origin()).collect() };
    let (t, ct) = if bsp.is_l4d() {
        (["info_l4d_saferoom", "info_landmark", "info_survivor_position_seed"].iter().flat_map(|c| origins(c)).collect(), origins("info_player_start"))
    } else {
        (origins("info_player_deathmatch"), origins("info_player_start"))
    };
    let objectives = vec![
        ("bombsites", count(bsp, &["func_bomb_target", "info_bomb_target"])),
        ("hostages", count(bsp, &["hostage_entity"])),
        ("rescue zones", count(bsp, &["func_hostage_rescue", "info_hostage_rescue"])),
        ("VIP starts", count(bsp, &["info_vip_start"])),
        ("VIP escapes", count(bsp, &["func_vip_safetyzone"])),
        ("escape zones", count(bsp, &["func_escapezone"])),
        ("control points", count(bsp, &["team_control_point"])),
        ("capture areas", count(bsp, &["trigger_capture_area"])),
        ("flags", count(bsp, &["item_teamflag"])),
        ("flag capture zones", count(bsp, &["func_capturezone"])),
        ("payload tracks", count(bsp, &["team_train_watcher"])),
        ("saferooms", count(bsp, &["info_l4d_saferoom"])),
        ("saferoom exits", count(bsp, &["info_changelevel"])),
        ("finales", count(bsp, &["trigger_finale"])),
        ("rescue closets", count(bsp, &["info_survivor_rescue"])),
    ];
    let buyzones = count(bsp, &["func_buyzone"]);
    (t, ct, objectives, buyzones)
}

fn min_gap(pts: &[DVec3]) -> Option<(f64, DVec3)> {
    let mut best: Option<(f64, DVec3)> = None;
    for (i, a) in pts.iter().enumerate() {
        for b in &pts[i + 1..] {
            let d = a.distance(*b);
            if best.is_none_or(|x| d < x.0) {
                best = Some((d, *a));
            }
        }
    }
    best
}

fn short(v: &[String]) -> String {
    if v.len() <= SHORT_LIST {
        return list(v);
    }
    format!("{} and {} more", v[..SHORT_LIST].join(", "), v.len() - SHORT_LIST)
}

fn list(v: &[String]) -> String {
    if v.is_empty() { "none".into() } else { v.join(", ") }
}

fn opt(v: Option<usize>) -> String {
    v.map_or("-".into(), |v| v.to_string())
}

impl Health {
    fn modern(&self) -> Option<&ModernHealth> {
        match &self.engine {
            Engine::Gold(_) => None,
            Engine::Cs2(c) => Some(c),
            Engine::Source(s) => Some(&s.base),
        }
    }

    fn modern_mut(&mut self) -> Option<&mut ModernHealth> {
        match &mut self.engine {
            Engine::Gold(_) => None,
            Engine::Cs2(c) => Some(c),
            Engine::Source(s) => Some(&mut s.base),
        }
    }

    pub fn warnings(&self) -> Vec<String> {
        match &self.engine {
            Engine::Gold(g) => self.gold_warnings(g),
            Engine::Cs2(c) => self.cs2_warnings(c),
            Engine::Source(h) => self.src_warnings(h),
        }
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        let _ = self.write_text(&mut s);
        s
    }

    fn write_text(&self, s: &mut String) -> std::fmt::Result {
        match &self.engine {
            Engine::Gold(g) => self.write_gold(g, s),
            Engine::Cs2(c) => self.write_cs2(c, s),
            Engine::Source(h) => self.write_src(h, s),
        }
    }

}

impl Health {
    pub fn csv_header() -> String {
        let mut cols: Vec<String> = [
            "map", "engine", "error", "warnings", "vis_bytes", "lighting_bytes", "unlit_faces", "lightable_faces", "t_spawns",
            "ct_spawns", "spawns_off_ground", "bombsites", "hostages", "rescue_zones", "vip_starts", "vip_escapes",
            "escape_zones", "control_points", "capture_areas", "flags", "flag_zones", "payload_tracks", "saferooms", "saferoom_exits", "finales", "rescue_closets", "buyzones", "missing_wads", "missing_textures", "sky_missing", "missing_files", "ov_txt",
            "ov_bmp", "ov_tga", "bmp_index255_px", "res", "res_txt", "res_bmp", "res_tga", "walkable_m2", "open_radius",
        ]
        .map(String::from)
        .to_vec();
        for (key, ..) in LIMITS {
            cols.push(key.to_string());
            cols.push(format!("{key}_pct"));
        }
        cols.extend(CS2_COLS.map(String::from));
        cols.join(",")
    }

    pub fn csv_row(&self) -> String {
        match &self.engine {
            Engine::Gold(g) => self.gold_row(g),
            Engine::Cs2(c) => self.cs2_row(c),
            Engine::Source(h) => self.src_row(h),
        }
    }

    fn nav_cols(&self) -> (String, String, String) {
        match &self.nav {
            Ok(n) => (
                n.off_ground.len().to_string(),
                format!("{:.0}", n.walkable_m2),
                n.spots.first().map_or(String::new(), |s| format!("{:.0}", s.radius)),
            ),
            Err(_) => (String::new(), String::new(), String::new()),
        }
    }

}

pub fn csv_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
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
    rep.step(0.0)?;
    let mut h = gather(scene, name, o);
    if let Some(c) = h.modern_mut() {
        c.radar.align = crate::export::radar_alignment(r, &scene.bsp, cuts, ALIGN_MARGIN);
    }
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
