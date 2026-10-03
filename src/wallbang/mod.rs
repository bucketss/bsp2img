pub mod draw2d;
pub mod draw3d;
pub mod export;
pub mod hull0;
pub mod shots;
pub mod sim;
pub mod texture;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::bsp::Bsp;
use crate::grid::Backdrop;
use crate::radar::{Section, Split};
use crate::scene::Report;
use draw2d::Frame;
use hull0::{Hull0, Trace, V3};
use shots::ShotOpts;
use texture::{Materials, Textures};

pub fn overview_bases(bsp: &Bsp, dirs: &[PathBuf], size: u32, secs: &[Section], rep: &mut Report) -> Result<Vec<DrawBase>> {
    let name = bsp.name();
    let mut search: Vec<PathBuf> = dirs.to_vec();
    if let Some(g) = std::path::absolute(&bsp.path).ok().and_then(|p| p.parent()?.parent().map(|g| g.to_path_buf())) {
        search.push(g);
    }
    let find = |ext: &str| search.iter().map(|d| d.join("overviews").join(format!("{name}.{ext}"))).find(|p| p.is_file());
    let Some(txt) = find("txt") else { return Ok(Vec::new()) };
    let ov = crate::overview::read_txt(&txt)?;
    let Some(img_path) = find("bmp").or_else(|| find("tga")) else { anyhow::bail!("{} has no .bmp or .tga image", txt.display()) };
    let mut img = image::open(&img_path).with_context(|| format!("reading {}", img_path.display()))?.to_rgba8();
    for p in img.pixels_mut() {
        if p[0] < 40 && p[1] > 215 && p[2] < 40 {
            *p = image::Rgba([0x25, 0x25, 0x25, 255]);
        }
    }
    let s = size as f64 / img.width().max(img.height()) as f64;
    let (w, h) = ((img.width() as f64 * s).round() as u32, (img.height() as f64 * s).round() as u32);
    let img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
    let mut px = img.into_raw();
    for p in px.chunks_exact_mut(4) {
        let l = 0.3 * p[0] as f64 + 0.59 * p[1] as f64 + 0.11 * p[2] as f64;
        for k in 0..3 {
            p[k] = ((p[k] as f64 * 0.4 + l * 0.6) * 0.75) as u8;
        }
        p[3] = 255;
    }
    let k = ov.zoom / 8.0 * (w as f64 / 1024.0);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let (ox, oy) = (ov.origin[0], ov.origin[1]);
    let m = if ov.rotated != 0 { [k, 0.0, cx - ox * k, 0.0, -k, cy + oy * k] } else { [0.0, -k, cx + oy * k, -k, 0.0, cy + ox * k] };
    rep.log(format!("  backdrop: {} (zoom {}, rotated {})", img_path.display(), ov.zoom, ov.rotated));
    let one = secs.len() == 1;
    Ok(secs
        .iter()
        .map(|sec| DrawBase {
            tag: if one || sec.name.eq_ignore_ascii_case(crate::radar::DEFAULT) { String::new() } else { format!("_{}", sec.name) },
            title: if one { String::new() } else { format!(" {} ({})", sec.name, sec.range()) },
            frame: Frame { w, h, m, upp: 1.0 / k },
            px: px.clone(),
            section: sec.clone(),
        })
        .collect())
}

pub const NAV_CELL: f64 = 8.0;

#[derive(Clone, Debug)]
pub struct WallbangOpts {
    pub shots: ShotOpts,
    pub ents: EntOpts,
    pub materials: Option<PathBuf>,
    pub areas: bool,
    pub lines: bool,
    pub sigma: f64,
    pub base: Backdrop,
    pub size: u32,
    pub split: Split,
    pub others_faint: bool,
    pub csv: bool,
    pub pixelwalks: bool,
    pub damage: bool,
}

pub struct DrawBase {
    pub tag: String,
    pub title: String,
    pub frame: Frame,
    pub px: Vec<u8>,
    pub section: Section,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EntOpts {
    pub skip_translucent: Option<u8>,
    pub skip_doors: bool,
}

#[derive(Clone, Debug)]
pub struct Placed {
    pub model: usize,
    pub head: i32,
    pub origin: V3,
    pub mins: V3,
    pub maxs: V3,
}

pub struct World {
    pub hull: Hull0,
    pub tex: Textures,
    pub mats: Materials,
    pub world_head: i32,
    pub ents: Vec<Placed>,
    pub rotated: BTreeMap<String, usize>,
}

fn flag(e: &crate::bsp::Entity, k: &str) -> i64 {
    e.get(k).and_then(|v| v.trim().parse::<f64>().ok()).map_or(0, |v| v as i64)
}

fn solid_class(e: &crate::bsp::Entity) -> Option<bool> {
    let sf = flag(e, "spawnflags");
    let (solid, keeps_angles) = match e.class().to_ascii_lowercase().as_str() {
        "func_wall" | "momentary_door" | "func_button" | "func_plat" => (true, false),
        "func_wall_toggle" => (sf & 1 == 0 && sf & 8 == 0, false),
        "func_conveyor" => (sf & 2 == 0, false),
        "func_door" | "func_water" => (flag(e, "skin") == 0 && sf & 8 == 0, false),
        "func_door_rotating" | "func_pendulum" | "func_train" | "func_tracktrain" | "func_vehicle" => (sf & 8 == 0, true),
        "func_rot_button" => (sf & 1 == 0, true),
        "momentary_rot_button" => (sf & 1 != 0, true),
        "func_rotating" => (sf & 64 == 0, true),
        "func_platrot" | "func_guntarget" | "func_tank" | "func_tanklaser" | "func_tankrocket" | "func_tankmortar"
        | "func_pushable" => (true, true),
        _ => (false, false),
    };
    solid.then_some(keeps_angles)
}

fn parse_v3(s: Option<&str>) -> V3 {
    let mut out = [0.0f32; 3];
    if let Some(s) = s {
        for (o, t) in out.iter_mut().zip(s.split_whitespace()) {
            *o = t.parse().unwrap_or(0.0);
        }
    }
    out
}

impl World {
    pub fn new(bsp: &Bsp, mats: Materials, eo: Option<EntOpts>) -> World {
        let mut w = World {
            hull: Hull0::new(bsp),
            tex: Textures::new(bsp),
            mats,
            world_head: bsp.models[0].headnode[0],
            ents: Vec::new(),
            rotated: BTreeMap::new(),
        };
        let Some(eo) = eo else { return w };
        for e in &bsp.entities {
            let Some(m) = e.model() else { continue };
            if m == 0 || m >= bsp.models.len() {
                continue;
            }
            let Some(keeps) = solid_class(e) else { continue };
            let class = e.class().to_ascii_lowercase();
            if eo.skip_doors && matches!(class.as_str(), "func_door" | "func_door_rotating" | "momentary_door") {
                continue;
            }
            if let Some(a) = eo.skip_translucent
                && flag(e, "rendermode") != 0
                && flag(e, "renderamt") < a as i64
            {
                continue;
            }
            if keeps && parse_v3(e.get("angles")).iter().any(|&v| v != 0.0) {
                *w.rotated.entry(class).or_default() += 1;
                continue;
            }
            let head = bsp.models[m].headnode[0];
            if head < 0 || head as usize >= bsp.nodes.len() {
                continue;
            }
            let origin = parse_v3(e.get("origin"));
            let (lo, hi) = (bsp.models[m].mins, bsp.models[m].maxs);
            w.ents.push(Placed {
                model: m,
                head,
                origin,
                mins: [lo.x as f32 + origin[0], lo.y as f32 + origin[1], lo.z as f32 + origin[2]],
                maxs: [hi.x as f32 + origin[0], hi.y as f32 + origin[1], hi.z as f32 + origin[2]],
            });
        }
        w.ents.sort_by_key(|p| p.model);
        w
    }

    pub fn trace(&self, src: &V3, end: &V3, ents: bool) -> (Trace, usize) {
        let (t, m, _) = self.trace_own(src, end, ents, 0);
        (t, m)
    }

    pub fn trace_own(&self, src: &V3, end: &V3, ents: bool, own: usize) -> (Trace, usize, Trace) {
        let mut best = self.hull.trace(self.world_head, src, end);
        let mut own_tr = best;
        let mut model = 0;
        if ents {
            for e in &self.ents {
                if e.model != own && (0..3).any(|i| src[i].min(end[i]) > e.maxs[i] + 1.0 || src[i].max(end[i]) < e.mins[i] - 1.0) {
                    continue;
                }
                let mut t = self.hull.trace(e.head, &sub(src, &e.origin), &sub(end, &e.origin));
                t.endpos = add(&t.endpos, &e.origin);
                t.abortpos = add(&t.abortpos, &e.origin);
                if e.model == own {
                    own_tr = t;
                }
                if t.fraction < best.fraction {
                    best = t;
                    model = e.model;
                }
            }
        }
        (best, model, own_tr)
    }

    fn placed(&self, model: usize) -> Option<&Placed> {
        self.ents.iter().find(|e| e.model == model)
    }

    pub fn surface_at(&self, model: usize, src: &V3, end: &V3) -> Option<u32> {
        match self.placed(model).filter(|_| model != 0) {
            Some(e) => self.tex.surface_at(&self.hull.planes, e.head, &sub(src, &e.origin), &sub(end, &e.origin)),
            None => self.tex.surface_at(&self.hull.planes, self.world_head, src, end),
        }
    }

    pub fn texture_hit(&self, model: usize, src: &V3, end: &V3) -> (Option<u32>, u8) {
        match self.surface_at(model, src, end) {
            Some(s) => (Some(s), self.mats.material(self.tex.name(s))),
            None => (None, 0),
        }
    }
}

pub fn sub(a: &V3, b: &V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: &V3, b: &V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[cfg(test)]
mod tests {
    use super::sim::{self, Fired, WEAPONS};
    use super::*;
    use std::path::Path;

    pub fn default_opts() -> WallbangOpts {
        WallbangOpts {
            shots: ShotOpts {
                weapon: sim::weapon("ak47").unwrap(),
                stand: true,
                duck: true,
                yaw_step: 2.0,
                pitch: (-20.0, 20.0, 4.0),
                spot_cell: 64.0,
                max_hit: 2000.0,
                min_thickness: 0.0,
                slab: 256.0,
            },
            ents: EntOpts::default(),
            materials: None,
            areas: true,
            lines: true,
            sigma: 2.0,
            base: Backdrop::Render,
            size: 1600,
            split: Split::None,
            others_faint: false,
            csv: false,
            pixelwalks: false,
            damage: false,
        }
    }

    fn sim_map(path: &str) -> Option<(Bsp, export::Sim)> {
        let path = Path::new(path);
        if !path.exists() {
            return None;
        }
        let bsp = Bsp::load(path).unwrap();
        let mut log = |s: String| println!("{s}");
        let mut prog = |_: f32| true;
        let mut rep = Report { log: &mut log, progress: &mut prog };
        let sim = export::simulate(&bsp, &default_opts(), None, &mut rep).unwrap();
        Some((bsp, sim))
    }

    #[test]
    #[ignore]
    fn inferno_pocket() {
        let path = Path::new(r"F:\servers\cs-coftest\cstrike\maps\de_inferno.bsp");
        if !path.exists() {
            return;
        }
        let bsp = Bsp::load(path).unwrap();
        let nav = crate::nav::Nav::build(&bsp, NAV_CELL, 250.0).unwrap();
        let reach = shots::reachable(&bsp, &nav);
        let n = nav
            .nodes_in(glam::DVec3::new(96.0, 1824.0, -130.0), glam::DVec3::new(928.0, 2592.0, -60.0))
            .into_iter()
            .filter(|&n| reach[n])
            .count();
        println!("reachable nodes in the pocket: {n}");
        assert_eq!(n, 0);
    }

    #[test]
    #[ignore]
    fn truths() {
        for path in [r"F:\servers\cs-coftest\cstrike\maps\de_dust2.bsp", r"F:\servers\cs-cz\czero\maps\de_dust2_cz.bsp"] {
            let Some((bsp, sim)) = sim_map(path) else { continue };
            println!("== {}", bsp.name());
            let ts: Vec<glam::DVec3> = bsp.entities.iter().filter(|e| e.class() == "info_player_deathmatch").filter_map(|e| e.origin()).collect();
            let tc = ts.iter().copied().sum::<glam::DVec3>() / ts.len().max(1) as f64;
            println!("T spawn centre {tc:?}");
            let mut thick: Vec<&shots::Shot> = sim
                .shots
                .iter()
                .filter(|s| s.made() && s.crossings[0].path > 300.0 && sim.spots[s.shooter as usize].feet.distance(tc) < 1200.0)
                .collect();
            thick.sort_by(|a, b| b.path().total_cmp(&a.path()));
            println!("thick made shots near T spawn: {}", thick.len());
            for s in thick.iter().take(8) {
                let c = &s.crossings[0];
                println!("  path {:.0} entry {:.0?} exit {:.0?} shooter {:.0?}", c.path, c.entry, c.exit, sim.spots[s.shooter as usize].feet);
            }
            for e in bsp.entities.iter().filter(|e| e.class() == "func_bomb_target") {
                let Some(m) = e.model().and_then(|m| bsp.models.get(m)) else { continue };
                let (lo, hi) = (m.mins - glam::DVec3::splat(64.0), m.maxs + glam::DVec3::splat(64.0));
                let pitches = sim.spots.len();
                let _ = pitches;
                let from: Vec<&shots::Shot> = sim
                    .shots
                    .iter()
                    .filter(|s| {
                        let f = sim.spots[s.shooter as usize].feet;
                        f.x >= lo.x && f.y >= lo.y && f.x <= hi.x && f.y <= hi.y && f.z >= lo.z - 64.0 && f.z <= hi.z
                    })
                    .collect();
                let made_down: Vec<&&shots::Shot> = from.iter().filter(|s| s.made() && s.dir.z < -0.12 && s.dir.z > -0.28).collect();
                println!("bomb target {:.0?}..{:.0?}: {} shots, {} made, {} made at 8-16 deg down", m.mins, m.maxs, from.len(), from.iter().filter(|s| s.made()).count(), made_down.len());
                let mut cells: BTreeMap<(i64, i64, i64), usize> = BTreeMap::new();
                for s in &made_down {
                    let x = s.crossings.last().unwrap().exit;
                    *cells.entry(((x.x / 128.0).floor() as i64 * 128, (x.y / 128.0).floor() as i64 * 128, (x.z / 64.0).floor() as i64 * 64)).or_default() += 1;
                }
                println!("  exit cells (x, y, z) -> count: {cells:?}");
                if m.mins.x >= 0.0 {
                    continue;
                }
                let nav = crate::nav::Nav::build(&bsp, NAV_CELL, 250.0).unwrap();
                let (lo, hi) = bsp.world_bounds();
                let sc = 0.25;
                let (w, h) = (((hi.x - lo.x) * sc) as u32 + 1, ((hi.y - lo.y) * sc) as u32 + 1);
                let mut img = image::RgbImage::new(w, h);
                for n in 0..nav.len() {
                    let (x, y) = nav.cell_xy(nav.col[n] as usize);
                    let z = nav.nodes[n].z as f64;
                    let px = ((x - lo.x) * sc) as u32;
                    let py = ((hi.y - y) * sc) as u32;
                    let g = (((z + 200.0) / 500.0).clamp(0.0, 1.0) * 180.0) as u8 + 40;
                    if px < w && py < h {
                        img.put_pixel(px, py, image::Rgb([g, g, g]));
                    }
                }
                for s in &made_down {
                    let (a, b) = (s.start(), s.end());
                    for i in 0..=200 {
                        let p = a + (b - a) * (i as f64 / 200.0);
                        let px = ((p.x - lo.x) * sc) as u32;
                        let py = ((hi.y - p.y) * sc) as u32;
                        if px < w && py < h {
                            img.put_pixel(px, py, image::Rgb([60, 240, 70]));
                        }
                    }
                }
                let dir = std::env::var("WALLBANG_DEBUG_DIR").unwrap_or_else(|_| ".".into());
                img.save(Path::new(&dir).join(format!("b_{}.png", bsp.name()))).unwrap();
            }
        }
    }

    const MAPS: [(&str, &str); 5] = [
        ("de_dust2", r"F:\servers\cs-coftest\cstrike\maps\de_dust2.bsp"),
        ("de_dust2_cz", r"F:\servers\cs-cz\czero\maps\de_dust2_cz.bsp"),
        ("de_inferno", r"F:\servers\cs-coftest\cstrike\maps\de_inferno.bsp"),
        ("de_inferno_cz", r"F:\servers\cs-cz\czero\maps\de_inferno_cz.bsp"),
        ("de_oc2", r"F:\servers\cs-coftest\cstrike\maps\de_oc2.bsp"),
    ];

    fn rd_f32(b: &[u8], o: usize) -> f32 {
        f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }

    fn rd_v3(b: &[u8], o: usize) -> V3 {
        [rd_f32(b, o), rd_f32(b, o + 4), rd_f32(b, o + 8)]
    }

    fn bits(v: &V3) -> [u32; 3] {
        v.map(f32::to_bits)
    }

    fn compare(f: &Fired, rec: &[u8], maxit: usize) -> Option<String> {
        let niter = rec[0] as usize;
        let outcome = rec[1];
        if f.n != niter {
            return Some(format!("niter {} vs {}", f.n, niter));
        }
        let o = match f.outcome() {
            sim::Outcome::Missed => 0,
            sim::Outcome::Made => 1,
            sim::Outcome::Died => 2,
            sim::Outcome::Stopped => 3,
        };
        if o != outcome {
            return Some(format!("outcome {o} vs {outcome}"));
        }
        for k in 0..niter.min(maxit) {
            let b = &rec[4 + k * 54..4 + (k + 1) * 54];
            let it = &f.it[k];
            let checks: [(&str, bool); 10] = [
                ("src", bits(&it.src) == bits(&rd_v3(b, 0))),
                ("dist", it.dist.to_bits() == rd_f32(b, 12).to_bits()),
                ("fraction", it.fraction.to_bits() == rd_f32(b, 16).to_bits()),
                ("endpos", bits(&it.endpos) == bits(&rd_v3(b, 20))),
                ("abortpos", bits(&it.abortpos) == bits(&rd_v3(b, 32))),
                ("face", it.face == i32::from_le_bytes(b[44..48].try_into().unwrap())),
                ("power", it.power == i16::from_le_bytes([b[48], b[49]])),
                ("damage", it.damage == i16::from_le_bytes([b[50], b[51]])),
                ("flags", it.flags == b[52]),
                ("tex", it.tex == b[53]),
            ];
            if let Some((name, _)) = checks.iter().find(|c| !c.1) {
                return Some(format!("iter {k} field {name}: {:?}", it));
            }
        }
        None
    }

    #[test]
    #[ignore]
    fn golden() {
        let Ok(dir) = std::env::var("WALLBANG_GOLDEN") else { return };
        let dir = Path::new(&dir);
        let only = std::env::var("WALLBANG_MAPS").ok();
        let mut total = 0usize;
        let mut bad = 0usize;
        for (name, path) in MAPS {
            if only.as_ref().is_some_and(|o| !o.split(',').any(|m| m == name)) {
                continue;
            }
            let path = Path::new(path);
            if !path.exists() {
                continue;
            }
            let bsp = Bsp::load(path).unwrap();
            let mats = Materials::locate(path).map(|p| Materials::load(&p).unwrap()).unwrap_or_else(Materials::empty);
            let world = World::new(&bsp, mats, None);
            for wname in ["ak47", "awp", "m4a1", "deagle", "scout"] {
                let Ok(shots) = std::fs::read(dir.join(format!("{name}_{wname}_shots.bin"))) else { continue };
                let res = std::fs::read(dir.join(format!("{name}_{wname}.bin"))).unwrap();
                assert_eq!(&shots[..4], b"WBS1");
                assert_eq!(&res[..4], b"WBR1");
                let count = u32::from_le_bytes(shots[4..8].try_into().unwrap()) as usize;
                let maxit = u32::from_le_bytes(res[8..12].try_into().unwrap()) as usize;
                let rsize = 4 + maxit * 54;
                let mut mism = 0usize;
                let mut first = None;
                for i in 0..count {
                    let s = &shots[8 + i * 25..8 + (i + 1) * 25];
                    let w = &WEAPONS[s[24] as usize];
                    let f = sim::fire(&world, w, &rd_v3(s, 0), &rd_v3(s, 12), false);
                    let rec = &res[16 + i * rsize..16 + (i + 1) * rsize];
                    if let Some(m) = compare(&f, rec, maxit) {
                        mism += 1;
                        if first.is_none() {
                            first = Some(format!("shot {i}: {m}"));
                        }
                    }
                }
                println!("{name} {wname}: {count} shots, {mism} mismatches {}", first.unwrap_or_default());
                total += count;
                bad += mism;
            }
        }
        println!("total {total} shots, {bad} mismatches");
        assert_eq!(bad, 0);
    }
}
