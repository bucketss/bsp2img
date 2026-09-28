pub mod buf;
pub mod entities;
pub mod kv3;
pub mod light;
pub mod material;
pub mod mesh;
pub mod model;
pub mod nav;
pub mod phys;
pub mod probe;
pub mod radar;
pub mod resource;
pub mod sky;
pub mod texture;
pub mod vpk;
pub mod world;

use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Context, Result};
use glam::DVec3;
use rayon::prelude::*;

use crate::bsp::{Bsp, Entity, Model as BModel};
use crate::mesh::Mesh;
use crate::scene::LoadOpts;

const LIGHTMAP_MAX: usize = 4096;
const SPAWN_REACH: f32 = 64.0;
const CROP_CELL: f64 = 16.0;
const CROP_PAD: f64 = 512.0;
const REACH_MIN: f64 = 0.5;

pub struct Extra {
    pub nav: Option<crate::navmesh::NavMesh>,
    pub fs: vpk::Fs,
    pub sky: Option<String>,
    pub phys_src: phys::Src,
    phys: OnceLock<Result<crate::phys::Phys, String>>,
    pub radar: Option<crate::radar::Official>,
    pub radar_err: Option<String>,
}

impl Extra {
    pub fn phys(&self) -> Result<&crate::phys::Phys> {
        self.phys
            .get_or_init(|| phys::load(&self.fs, &self.phys_src).map_err(|e| format!("{e:#}")))
            .as_ref()
            .map_err(|e| anyhow::anyhow!("collision: {e}"))
    }

    pub fn sky_material(&self, name: &str) -> Option<String> {
        sky::resolve(&self.fs, name).or_else(|| self.sky.clone())
    }
}

fn crop_box(bsp: &Bsp, log: &mut dyn FnMut(String)) -> Option<[f64; 4]> {
    let t = Instant::now();
    let nav = crate::nav::Nav::build(bsp, CROP_CELL, 250.0).ok()?;
    let Some(mut reach) = crate::reach::nav_reach(&nav, bsp, 250.0) else {
        log("auto crop: no spawn points on the nav mesh, using whole map".into());
        return None;
    };
    let hit = reach.iter().filter(|r| **r).count();
    if (hit as f64) < REACH_MIN * reach.len() as f64 {
        log(format!("auto crop: spawns reach only {hit} of {} nav cells, keeping the whole nav mesh", reach.len()));
        reach.fill(true);
    }
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut n = 0;
    for (u, _) in reach.iter().enumerate().filter(|(_, r)| **r) {
        let (x, y) = nav.cell_xy(nav.col[u] as usize);
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        n += 1;
    }
    if n == 0 {
        return None;
    }
    log(format!(
        "auto crop: {n} reachable nav cells, box {:.0} {:.0} .. {:.0} {:.0} + {CROP_PAD:.0} ({:.1}s)",
        b[0],
        b[1],
        b[2],
        b[3],
        t.elapsed().as_secs_f64()
    ));
    Some(b)
}

pub fn is_vpk(path: &Path) -> bool {
    path.extension().is_some_and(|x| x.eq_ignore_ascii_case("vpk"))
}

fn brush_model(m: &str, map: &str) -> bool {
    m.starts_with(&format!("maps/{map}/entities/"))
}

pub fn load(path: &Path, opts: &LoadOpts, max_dim: u32, log: &mut dyn FnMut(String)) -> Result<(Bsp, Mesh)> {
    let t0 = Instant::now();
    let mut tm = crate::prof::Timer::new("cs2");
    let root = vpk::game_root(path, opts.game.as_deref());
    if root.is_none() {
        log("CS2 install not found next to the map or via --game; only the map's own files are searched".into());
    }
    let fs = vpk::map_fs(path, root.as_deref())?;
    let map = path.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
    if !fs.has(&format!("maps/{map}/world.vwrld_c")) {
        anyhow::bail!("{} is not a CS2 map (no maps/{map}/world.vwrld)", path.display());
    }
    let world = world::load(&fs, &map).context("loading world")?;
    let lumps = entities::load(&fs, &world.lumps).context("loading entities")?;
    if lumps.legacy > 0 {
        log(format!("{} entities use the legacy key-value format and were skipped", lumps.legacy));
    }

    let mut bsp = Bsp::empty(path);
    let (lo, hi) = world.bounds;
    let blank = |mins: DVec3, maxs: DVec3| BModel { mins, maxs, headnode: [-1; 4], visleafs: 0, firstface: 0, numfaces: 0 };
    bsp.models.push(blank(lo, hi));
    let brushes: Vec<Option<(DVec3, DVec3)>> = lumps
        .ents
        .par_iter()
        .map(|e| {
            let v = e.vmdl.as_ref().filter(|v| brush_model(v, &map))?;
            let (a, b) = entities::model_bounds(&fs, v)?;
            let (a, b) = entities::world_bounds(&e.xf, a, b);
            Some((a - e.xf.t, b - e.xf.t))
        })
        .collect();
    for (e, b) in lumps.ents.iter().zip(brushes) {
        let mut kv = e.kv.clone();
        if let Some((a, b)) = b {
            kv.insert("model".into(), format!("*{}", bsp.models.len()));
            bsp.models.push(blank(a, b));
        }
        bsp.entities.push(Entity(kv));
    }
    let navmesh = match fs.read(&format!("maps/{map}.nav")).and_then(|b| nav::parse(&b)) {
        Ok(m) if !m.areas.is_empty() => Some(m),
        Ok(_) => {
            log("nav mesh is empty; timing, svg, auto crop and --hull are unavailable".into());
            None
        }
        Err(e) => {
            log(format!("nav mesh: {e:#}"));
            None
        }
    };
    if let Some(m) = &navmesh {
        let mut moved = 0;
        for e in bsp.entities.iter_mut().filter(|e| crate::reach::SEED_CLASSES.contains(&e.class())) {
            let Some(o) = e.origin() else { continue };
            let ring = [(0.0, 0.0), (24.0, 0.0), (-24.0, 0.0), (0.0, 24.0), (0.0, -24.0), (48.0, 0.0), (-48.0, 0.0), (0.0, 48.0), (0.0, -48.0)];
            let found = ring.iter().find_map(|&(dx, dy)| m.floor_below(o.x as f32 + dx, o.y as f32 + dy, o.z as f32 + SPAWN_REACH));
            if let Some(z) = found {
                e.0.insert("origin".into(), format!("{} {} {}", o.x, o.y, z as f64 + crate::nav::STAND_OFS));
                moved += 1;
            }
        }
        log(format!("{moved} spawn points raised to nav floor + {}", crate::nav::STAND_OFS));
    }
    let sky = lumps
        .ents
        .iter()
        .filter(|e| e.kv.get("classname").is_some_and(|c| c == "env_sky"))
        .find_map(|e| e.kv.get("skyname").and_then(|n| sky::resolve(&fs, n)));
    let phys_src = phys::Src::new(&map, &lumps.ents);
    let (radar, radar_err) = match radar::load(&fs, &map) {
        Ok(Some(o)) => {
            let secs: Vec<&str> = o.sections.iter().map(|s| s.name.as_str()).collect();
            let (f, num) = (o.frame, crate::scene::num);
            log(format!(
                "overview {}: pos_x {} pos_y {} scale {}, sections {}",
                radar::txt_name(&map),
                num(f.pos_x),
                num(f.pos_y),
                num(f.scale),
                secs.join(" ")
            ));
            (Some(o), None)
        }
        Ok(None) => {
            log(format!("no {}", radar::txt_name(&map)));
            (None, None)
        }
        Err(e) => {
            log(format!("overview: {e:#}"));
            (None, Some(format!("{e:#}")))
        }
    };
    bsp.cs2 = Some(Box::new(Extra { nav: navmesh, fs, sky, phys_src, phys: OnceLock::new(), radar, radar_err }));
    let fs = &bsp.cs2.as_ref().unwrap().fs;
    let t1 = t0.elapsed().as_secs_f64();
    tm.lap("world");

    let lightmap = match light::bake(fs, &world, &lumps.ents, &opts.light, LIGHTMAP_MAX.min(max_dim as usize)) {
        Ok(Some(b)) => {
            log(b.info);
            Some((b.lightmap, b.hdr))
        }
        Ok(None) => {
            log("no baked lighting; use --relight".into());
            None
        }
        Err(e) => {
            log(format!("baked lighting failed: {e:#}"));
            None
        }
    };
    tm.lap("lightmap");
    let crop = if opts.auto_crop { crop_box(&bsp, log) } else { None };
    tm.lap("crop");
    let crop = crop.map(|[x0, y0, x1, y1]| [x0 - CROP_PAD, y0 - CROP_PAD, x1 + CROP_PAD, y1 + CROP_PAD]);
    let (lightmap, hdr) = lightmap.unzip();
    let probes = if hdr.is_some() {
        match probe::Probes::load(fs, &lumps.ents) {
            Ok(p) => p,
            Err(e) => {
                log(format!("light probes: {e:#}"));
                None
            }
        }
    } else {
        None
    };
    let nprobe = probes.as_ref().map_or(0, |p| p.count());
    tm.lap("probes");
    let built = mesh::build(fs, &world, &lumps.ents, mesh::Opts { tex_max: opts.tex_max, crop, lightmap, hdr, probes });
    tm.lap("mesh");
    if nprobe > 0 {
        log(format!("light probes: {nprobe} volumes, {} draws probe-lit", built.probe_draws));
    }
    if built.casters > 0 {
        log(format!("{} light blockers cast shadows only", built.casters));
    }
    for b in built.bad_models.iter().take(10) {
        log(format!("model failed: {b}"));
    }
    if built.bad_models.len() > 10 {
        log(format!("... {} more models failed", built.bad_models.len() - 10));
    }
    log(format!(
        "CS2 {map}: {} entities, {} models, {} materials, {} draws ({} cropped), world {t1:.1}s, total {:.1}s",
        bsp.entities.len(),
        built.models,
        built.materials,
        built.draws,
        built.dropped,
        t0.elapsed().as_secs_f64()
    ));
    Ok((bsp, built.mesh))
}
