pub mod bsp;
pub mod cube;
pub mod dds;
pub mod entities;
pub mod fs;
pub mod model;
pub mod nav;
pub mod props;
pub mod radar;
pub mod sky;
pub mod sprp;
pub mod vhv;
pub mod vmt;
pub mod vtf;
pub mod world;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result};

use crate::bsp::{Bsp, Engine, Model as BModel};
use crate::extra::{Extra, LoadStats, PhysSrc};
use crate::mesh::Mesh;
use crate::navmesh::NavMesh;
use crate::scene::LoadOpts;

pub fn is_source(path: &Path) -> bool {
    bsp::is_source(path)
}

pub fn load(path: &Path, opts: &LoadOpts, _max_dim: u32, log: &mut dyn FnMut(String)) -> Result<(Bsp, Mesh)> {
    let t0 = Instant::now();
    let map = Arc::new(bsp::Map::open(path)?);
    let name = path.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
    let opened = fs::open(&map, opts.game.as_deref())?;
    for n in &opened.notes {
        log(n.clone());
    }
    let w = bsp::World::read(&map).with_context(|| format!("reading {}", path.display()))?;
    let ents = entities::parse(&map.entities()?);
    if !ents.disabled.is_empty() {
        log(format!("{} disabled spawn points dropped", ents.disabled.len()));
    }

    let mut b = Bsp::empty(path, Engine::Source(map.version));
    b.models = w
        .models
        .iter()
        .map(|m| BModel { mins: m.mins, maxs: m.maxs, headnode: [-1; 4], visleafs: 0, firstface: 0, numfaces: 0 })
        .collect();
    b.entities = ents.list;
    let navmesh: Option<NavMesh> = None;
    let raised = match &navmesh {
        Some(m) => crate::extra::raise_spawns(&mut b, m, log),
        None => Vec::new(),
    };
    let stats = LoadStats { disabled: ents.disabled, raised, ..Default::default() };
    b.extra = Some(Box::new(Extra {
        nav: navmesh,
        fs: opened.fs,
        sky: ents.sky,
        phys_src: PhysSrc::Source(map.clone()),
        phys: OnceLock::new(),
        radar: None,
        radar_err: None,
        root: opened.root,
        stats,
    }));
    let t1 = t0.elapsed().as_secs_f64();
    let crop = if opts.auto_crop { crate::extra::crop_box(&b, log) } else { None };
    let x = b.extra.as_ref().unwrap();
    let mut built = world::build(&x.fs, &w, &b.entities, world::Opts { tex_max: opts.tex_max, crop });
    log(built.materials.summary());
    for m in built.materials.bad.iter().take(10) {
        log(format!("texture failed: {m}"));
    }
    let pr = props::add(&map, &x.fs, &b.entities, &mut built.mesh, &mut built.tex_ids, props::Opts { tex_max: opts.tex_max, crop, light: &opts.light });
    log(pr.summary());
    if !pr.bad_models.is_empty() {
        log(format!("{} models failed ({} missing)", pr.bad_models.len(), pr.missing_models.len()));
    }
    for m in pr.bad_models.iter().take(10) {
        log(format!("model failed: {m}"));
    }
    if built.displacements > 0 {
        log(format!("{} displacement faces not drawn yet", built.displacements));
    }
    log(format!(
        "Source v{} {name}: {} entities, {} brush entities, {} faces ({} cropped), no lightmaps yet, world {t1:.1}s, total {:.1}s",
        map.version,
        b.entities.len(),
        built.brush_ents,
        built.faces,
        built.dropped,
        t0.elapsed().as_secs_f64()
    ));
    if let Some(x) = b.extra.as_mut() {
        x.stats.missing = built.mesh.missing.clone();
        x.stats.bad_models = pr.bad_models;
        x.stats.models = pr.models;
        x.stats.draws = built.faces;
        x.stats.dropped = built.dropped;
        x.stats.materials = w.texdata.len();
        x.stats.tris = built.mesh.batches.iter().map(|b| b.idx.len() / 3).sum();
    }
    Ok((b, built.mesh))
}
