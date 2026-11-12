pub mod bsp;
pub mod collide;
pub mod cube;
pub mod dds;
pub mod disp;
pub mod entities;
pub mod fs;
pub mod light;
pub mod model;
pub mod nav;
pub mod phy;
pub mod pose;
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

use crate::bsp::{Bsp, Engine, Game, Model as BModel};
use crate::extra::{Extra, LoadStats, PhysSrc};
use crate::mesh::Mesh;
use crate::navmesh::NavMesh;
use crate::scene::LoadOpts;

pub fn is_source(path: &Path) -> bool {
    bsp::is_source(path)
}

pub fn load(path: &Path, opts: &LoadOpts, max_dim: u32, log: &mut dyn FnMut(String)) -> Result<(Bsp, Mesh)> {
    let t0 = Instant::now();
    let mut map = bsp::Map::open(path)?;
    let name = path.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
    if map.l4d2 {
        for f in fs::lump_files(path, opts.game.as_deref(), &name) {
            match std::fs::read(&f).map_err(anyhow::Error::from).and_then(|b| map.apply_lump_file(&b)) {
                Ok(i) => log(format!("lump {i} replaced by {}", f.display())),
                Err(e) => log(format!("{}: {e:#}", f.display())),
            }
        }
    }
    let map = Arc::new(map);
    let opened = fs::open(&map, opts.game.as_deref())?;
    for n in &opened.notes {
        log(n.clone());
    }
    let w = bsp::World::read(&map).with_context(|| format!("reading {}", path.display()))?;
    opts.cancel.check()?;
    let root_name = opened.root.as_deref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ents = entities::parse_with(&map.entities()?, map.l4d2 || root_name.starts_with("left 4 dead"));
    if !ents.disabled.is_empty() {
        log(format!("{} disabled spawn points dropped", ents.disabled.len()));
    }

    let mut b = Bsp::empty(path, Engine::Source(map.version));
    if map.version == 20 && (root_name == "tf" || ents.tf2) {
        b.game = Game::Tf2;
    }
    if ents.l4d {
        b.game = if map.version >= 21 { Game::L4d2 } else { Game::L4d };
    }
    b.models = w
        .models
        .iter()
        .map(|m| BModel { mins: m.mins, maxs: m.maxs, headnode: [-1; 4], visleafs: 0, firstface: 0, numfaces: 0 })
        .collect();
    b.entities = ents.list;
    let (mut navmesh, mut places, spawn_attrs): (Option<NavMesh>, _, _) = nav::load(&opened.fs, &name, map.bytes().len(), log);
    if b.is_l4d() {
        l4d_seeds(&mut b, navmesh.as_ref(), &spawn_attrs, log);
    }
    let (radar, radar_err) = if !b.is_l4d() { radar::load(&opened.fs, &name, log) } else { (None, None) };
    let raised = match &navmesh {
        Some(m) => crate::extra::raise_spawns(&mut b, m, log),
        None => Vec::new(),
    };
    let classes = crate::reach::seed_classes(&b);
    let seeds = b.entities.iter().filter(|e| classes.contains(&e.class())).count();
    if navmesh.is_some() && raised.is_empty() && seeds > 0 {
        log(format!("{} covers none of the {seeds} spawn points: timing, svg, auto crop and --hull walk the brush collision instead", nav::name(&name)));
        navmesh = None;
        places = None;
    }
    let stats = LoadStats { disabled: ents.disabled, raised, ..Default::default() };
    b.extra = Some(Box::new(Extra {
        nav: navmesh,
        places,
        fs: opened.fs,
        sky: ents.sky,
        phys_src: PhysSrc::Source(map.clone()),
        phys: OnceLock::new(),
        radar,
        radar_err,
        root: opened.root,
        stats,
    }));
    if let Some(mut x) = b.extra.take() {
        if x.nav.is_none() {
            x.stats.raised = match x.phys() {
                Ok(p) => crate::extra::raise_spawns_phys(&mut b, p, log),
                Err(e) => {
                    log(format!("{e:#}"));
                    Vec::new()
                }
            };
        }
        b.extra = Some(x);
    }
    let t1 = t0.elapsed().as_secs_f64();
    let region = if opts.auto_crop { crate::extra::crop_region(&b, log) } else { None };
    let crop = region.map(|r| r.0);
    let band = region.and_then(|(xy, z)| crate::extra::skybox_band(&b.entities, xy, z, log));
    let mut light = opts.light;
    if b.is_l4d()
        && let Some(m) = light::floor_light(&map, &w, crop)
    {
        let k = (L4D_TARGET / m as f64).clamp(1.0, L4D_EXPOSURE_MAX);
        log(format!("median floor light {m:.4}, exposure x{k:.2}"));
        light.scale *= k;
    }
    opts.cancel.check()?;
    let x = b.extra.as_ref().unwrap();
    let mut built = world::build(&x.fs, &w, &b.entities, world::Opts { tex_max: opts.tex_max, crop, band, map: &map, light: &light, lm_cap: light::LIGHTMAP_MAX.min(max_dim as usize) });
    log(built.materials.summary());
    for m in built.materials.bad.iter().take(10) {
        log(format!("texture failed: {m}"));
    }
    for n in &built.notes {
        log(n.clone());
    }
    opts.cancel.check()?;
    let placed = if opts.show.any() {
        let (p, missing) = props::placed(&b, &x.fs, x.phys().ok(), &x.stats.raised, opts.show);
        if !missing.is_empty() {
            log(format!("no model known for: {}", missing.join(", ")));
        }
        p
    } else {
        Vec::new()
    };
    let pr = props::add(&map, &x.fs, &b.entities, &mut built.mesh, &mut built.tex_ids, props::Opts { tex_max: opts.tex_max, crop, band, light: &light, placed });
    log(pr.summary());
    if opts.show.any() {
        log(format!("shown: {} items, {} players", pr.items, pr.players));
        if !pr.unposed.is_empty() {
            log(format!("no idle pose found, bind pose used: {}", pr.unposed.join(", ")));
        }
    }
    if !pr.bad_models.is_empty() {
        log(format!("{} models failed ({} missing)", pr.bad_models.len(), pr.missing_models.len()));
    }
    for m in pr.bad_models.iter().take(10) {
        log(format!("model failed: {m}"));
    }
    log(format!(
        "Source v{} {name}: {} entities, {} brush entities, {} faces ({} cropped), {} displacements, world {t1:.1}s, total {:.1}s",
        map.version,
        b.entities.len(),
        built.brush_ents,
        built.faces,
        built.dropped,
        built.displacements,
        t0.elapsed().as_secs_f64()
    ));
    if let Some(x) = b.extra.as_mut() {
        x.stats.missing = built.mesh.missing.clone();
        x.stats.bad_models = pr.bad_models;
        x.stats.models = pr.models;
        x.stats.draws = built.faces;
        x.stats.dropped = built.dropped;
        x.stats.materials = w.texdata.len();
        x.stats.lightmap = built.lightmap;
        x.stats.tris = built.mesh.batches.iter().map(|b| b.idx.len() / 3).sum();
    }
    Ok((b, built.mesh))
}

const L4D_TARGET: f64 = 0.2;
const L4D_EXPOSURE_MAX: f64 = 24.0;
const SAFEROOM_MIN_AREAS: usize = 3;

fn l4d_seeds(b: &mut Bsp, nav: Option<&NavMesh>, attrs: &[u32], log: &mut dyn FnMut(String)) {
    use std::collections::HashMap;
    let mut rooms = 0;
    if let Some(m) = nav.filter(|m| m.areas.len() == attrs.len()) {
        let keep: Vec<bool> = attrs.iter().map(|a| a & (nav::SPAWN_CHECKPOINT | nav::SPAWN_PLAYER_START) != 0).collect();
        let mut up: Vec<u32> = (0..m.areas.len() as u32).collect();
        for (i, a) in m.areas.iter().enumerate().filter(|(i, _)| keep[*i]) {
            for &c in a.conns.iter().filter(|&&c| keep[c as usize]) {
                crate::nav::unite(&mut up, i as u32, c);
            }
        }
        let centre = |i: usize| {
            let p = &m.areas[i].poly;
            let s = p.iter().fold([0f64; 3], |s, v| [s[0] + v[0] as f64, s[1] + v[1] as f64, s[2] + v[2] as f64]);
            glam::DVec3::from(s) / p.len() as f64
        };
        let mut groups: HashMap<u32, Vec<usize>> = HashMap::new();
        for i in (0..m.areas.len()).filter(|&i| keep[i]) {
            groups.entry(crate::nav::root(&mut up, i as u32)).or_default().push(i);
        }
        let mut groups: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() >= SAFEROOM_MIN_AREAS).collect();
        groups.sort_by_key(|g| g[0]);
        for g in groups {
            let mid = g.iter().map(|&i| centre(i)).sum::<glam::DVec3>() / g.len() as f64;
            let best = g.iter().map(|&i| centre(i)).min_by(|a, c| a.distance_squared(mid).total_cmp(&c.distance_squared(mid))).unwrap();
            let start = g.iter().any(|&i| attrs[i] & nav::SPAWN_PLAYER_START != 0);
            let mut kv = std::collections::HashMap::from([
                ("classname".to_string(), "info_l4d_saferoom".to_string()),
                ("origin".to_string(), format!("{} {} {}", best.x, best.y, best.z + crate::nav::STAND_OFS)),
            ]);
            if start {
                kv.insert("start".into(), "1".into());
            }
            b.entities.push(crate::bsp::Entity(kv));
            rooms += 1;
        }
    }
    let primary = b.entities.iter().filter(|e| matches!(e.class(), "info_player_start" | "info_landmark" | "info_l4d_saferoom")).count();
    if primary == 0 {
        let mut n = 0;
        for e in b.entities.iter_mut().filter(|e| e.class() == "info_survivor_position") {
            e.0.insert("classname".into(), "info_survivor_position_seed".into());
            n += 1;
        }
        log(format!("no saferoom, player start or landmark: {n} survivor positions used as seeds"));
    } else {
        log(format!("{rooms} saferooms from nav checkpoint areas, {primary} survivor seeds"));
    }
}
