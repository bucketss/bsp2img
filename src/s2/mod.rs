pub mod buf;
pub mod entities;
pub mod kv3;
pub mod light;
pub mod material;
pub mod mesh;
pub mod model;
pub mod resource;
pub mod texture;
pub mod vpk;
pub mod world;

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use glam::DVec3;
use rayon::prelude::*;

use crate::bsp::{Bsp, Entity, Model as BModel};
use crate::mesh::Mesh;
use crate::scene::LoadOpts;

const LIGHTMAP_MAX: usize = 4096;

pub struct Extra {
    pub map: String,
    pub root: Option<PathBuf>,
}

pub fn is_vpk(path: &Path) -> bool {
    path.extension().is_some_and(|x| x.eq_ignore_ascii_case("vpk"))
}

fn brush_model(m: &str, map: &str) -> bool {
    m.starts_with(&format!("maps/{map}/entities/"))
}

pub fn load(path: &Path, opts: &LoadOpts, max_dim: u32, log: &mut dyn FnMut(String)) -> Result<(Bsp, Mesh)> {
    let t0 = Instant::now();
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
    let t1 = t0.elapsed().as_secs_f64();

    let lightmap = match light::bake(&fs, &world, &lumps.ents, &opts.light, LIGHTMAP_MAX.min(max_dim as usize)) {
        Ok(Some((l, info))) => {
            log(info);
            Some(l)
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
    let built = mesh::build(&fs, &world, &lumps.ents, mesh::Opts { tex_max: opts.tex_max, keep: None, lightmap });
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
    bsp.cs2 = Some(Box::new(Extra { map, root }));
    Ok((bsp, built.mesh))
}
