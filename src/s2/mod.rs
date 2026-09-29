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
use crate::extra::{Extra, LoadStats};
use crate::mesh::Mesh;
use crate::scene::LoadOpts;

const LIGHTMAP_MAX: usize = 4096;

pub fn is_vpk(path: &Path) -> bool {
    path.extension().is_some_and(|x| x.eq_ignore_ascii_case("vpk")) || vpk::is_loose(path)
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

    let mut bsp = Bsp::empty(path, crate::bsp::Engine::Cs2);
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
    let raised = match &navmesh {
        Some(m) => crate::extra::raise_spawns(&mut bsp, m, log),
        None => Vec::new(),
    };
    let sky = lumps
        .ents
        .iter()
        .filter(|e| e.kv.get("classname").is_some_and(|c| c == "env_sky"))
        .find_map(|e| e.kv.get("skyname").and_then(|n| sky::resolve(&fs, n)));
    let phys_src = crate::extra::PhysSrc::Cs2(phys::Src::new(&map, &lumps.ents));
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
    let stats = LoadStats { legacy_ents: lumps.legacy, disabled: lumps.disabled.clone(), raised, ..Default::default() };
    bsp.extra = Some(Box::new(Extra { nav: navmesh, places: None, fs, sky, phys_src, phys: OnceLock::new(), radar, radar_err, root, stats }));
    let fs = &bsp.extra.as_ref().unwrap().fs;
    let t1 = t0.elapsed().as_secs_f64();
    tm.lap("world");
    opts.cancel.check()?;

    let lightmap = match light::bake(fs, &world, &lumps.ents, &opts.light, LIGHTMAP_MAX.min(max_dim as usize)) {
        Ok(Some(b)) => {
            log(b.info);
            Some((b.lightmap, b.hdr, b.size))
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
    opts.cancel.check()?;
    let crop = if opts.auto_crop { crate::extra::crop_box(&bsp, log) } else { None };
    tm.lap("crop");
    let lm_size = lightmap.as_ref().map(|l| l.2);
    let (lightmap, hdr) = lightmap.map(|l| (l.0, l.1)).unzip();
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
    opts.cancel.check()?;
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
    if let Some(x) = bsp.extra.as_mut() {
        x.stats = LoadStats {
            bad_models: built.bad_models,
            missing: built.mesh.missing.clone(),
            draws: built.draws,
            dropped: built.dropped,
            materials: built.materials,
            models: built.models,
            tris: built.mesh.faces,
            lightmap: lm_size,
            probes: nprobe,
            ..std::mem::take(&mut x.stats)
        };
    }
    Ok((bsp, built.mesh))
}
