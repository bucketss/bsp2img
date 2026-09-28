use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::Result;
use glam::DVec3;

use crate::bsp::Bsp;
use crate::navmesh::NavMesh;
use crate::s2::vpk::Fs;

const SPAWN_REACH: f32 = 64.0;
const CROP_CELL: f64 = 16.0;
pub const CROP_PAD: f64 = 512.0;
const SKY_GAP: f64 = 1024.0;
const REACH_MIN: f64 = 0.5;

pub enum PhysSrc {
    Cs2(crate::s2::phys::Src),
    Source(Arc<crate::s1::bsp::Map>),
}

pub struct Extra {
    pub nav: Option<NavMesh>,
    pub places: Option<crate::navmesh::Places>,
    pub fs: Fs,
    pub sky: Option<String>,
    pub phys_src: PhysSrc,
    pub(crate) phys: OnceLock<Result<crate::phys::Phys, String>>,
    pub radar: Option<crate::radar::Official>,
    pub radar_err: Option<String>,
    pub root: Option<PathBuf>,
    pub stats: LoadStats,
}

#[derive(Default)]
pub struct LoadStats {
    pub bad_models: Vec<String>,
    pub missing: Vec<String>,
    pub draws: usize,
    pub dropped: usize,
    pub materials: usize,
    pub models: usize,
    pub tris: usize,
    pub lightmap: Option<(u32, u32)>,
    pub probes: usize,
    pub legacy_ents: usize,
    pub disabled: Vec<(String, DVec3)>,
    pub raised: Vec<usize>,
}

impl Extra {
    pub fn phys(&self) -> Result<&crate::phys::Phys> {
        self.phys
            .get_or_init(|| {
                let r = match &self.phys_src {
                    PhysSrc::Cs2(s) => crate::s2::phys::load(&self.fs, s),
                    PhysSrc::Source(m) => crate::s1::collide::load(&self.fs, m),
                };
                r.map_err(|e| format!("{e:#}"))
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!("collision: {e}"))
    }

    fn material(&self) -> Option<&str> {
        self.radar.as_ref().and_then(|o| o.material.as_deref())
    }

    pub fn radar_image_name(&self, map: &str, section: &str) -> Option<String> {
        match &self.phys_src {
            PhysSrc::Cs2(_) => crate::s2::radar::image_name(&self.fs, map, section),
            PhysSrc::Source(_) => crate::s1::radar::image_name(|n| self.fs.has(n), map, section, self.material()),
        }
    }

    pub fn radar_image(&self, map: &str, section: &str, max_dim: usize) -> Result<Option<(String, crate::s2::texture::Image)>> {
        match &self.phys_src {
            PhysSrc::Cs2(_) => crate::s2::radar::image(&self.fs, map, section, max_dim),
            PhysSrc::Source(_) => crate::s1::radar::image(&self.fs, map, section, self.material(), max_dim),
        }
    }

    pub fn sky_material(&self, name: &str) -> Option<String> {
        crate::s2::sky::resolve(&self.fs, name).or_else(|| self.sky.clone())
    }
}

pub fn raise_spawns(bsp: &mut Bsp, m: &NavMesh, log: &mut dyn FnMut(String)) -> Vec<usize> {
    raise_spawns_to(bsp, &|x, y, z| m.floor_below(x, y, z), "nav floor", log)
}

pub fn raise_spawns_phys(bsp: &mut Bsp, p: &crate::phys::Phys, log: &mut dyn FnMut(String)) -> Vec<usize> {
    let floor = |x: f32, y: f32, z: f32| {
        let mut out = Vec::new();
        p.column(x as f64, y as f64, crate::phys::Mask::PLAYER, &mut out);
        out.iter().map(|iv| iv.1).filter(|&t| t.is_finite() && t <= z).reduce(f32::max)
    };
    raise_spawns_to(bsp, &floor, "collision floor", log)
}

fn raise_spawns_to(bsp: &mut Bsp, floor_below: &dyn Fn(f32, f32, f32) -> Option<f32>, what: &str, log: &mut dyn FnMut(String)) -> Vec<usize> {
    let mut raised = Vec::new();
    for (i, e) in bsp.entities.iter_mut().enumerate().filter(|(_, e)| crate::reach::SEED_CLASSES.contains(&e.class())) {
        let Some(o) = e.origin() else { continue };
        let ring = [(0.0, 0.0), (24.0, 0.0), (-24.0, 0.0), (0.0, 24.0), (0.0, -24.0), (48.0, 0.0), (-48.0, 0.0), (0.0, 48.0), (0.0, -48.0)];
        let found = ring.iter().find_map(|&(dx, dy)| floor_below(o.x as f32 + dx, o.y as f32 + dy, o.z as f32 + SPAWN_REACH));
        if let Some(z) = found {
            e.0.insert("origin".into(), format!("{} {} {}", o.x, o.y, z as f64 + crate::nav::STAND_OFS));
            raised.push(i);
        }
    }
    log(format!("{} spawn points raised to {what} + {}", raised.len(), crate::nav::STAND_OFS));
    raised
}

pub fn crop_box(bsp: &Bsp, log: &mut dyn FnMut(String)) -> Option<[f64; 4]> {
    crop_region(bsp, log).map(|r| r.0)
}

pub fn skybox_band(ents: &[crate::bsp::Entity], xy: [f64; 4], z: [f64; 2], log: &mut dyn FnMut(String)) -> Option<[f64; 2]> {
    let o = ents.iter().find(|e| e.class() == "sky_camera")?.origin()?;
    if o.x < xy[0] || o.x > xy[2] || o.y < xy[1] || o.y > xy[3] {
        return None;
    }
    let band = if o.z < z[0] - 2.0 * SKY_GAP {
        [z[0] - SKY_GAP, f64::INFINITY]
    } else if o.z > z[1] + 2.0 * SKY_GAP {
        [f64::NEG_INFINITY, z[1] + SKY_GAP]
    } else {
        return None;
    };
    log(format!("auto crop: 3D skybox (sky_camera at z {:.0}) cut at z {:.0}", o.z, if band[0].is_finite() { band[0] } else { band[1] }));
    Some(band)
}

pub fn crop_region(bsp: &Bsp, log: &mut dyn FnMut(String)) -> Option<([f64; 4], [f64; 2])> {
    let t = Instant::now();
    let nav = crate::nav::Nav::build_with(bsp, CROP_CELL, 250.0, false).ok()?;
    let Some(mut reach) = crate::reach::nav_reach(&nav, bsp, 250.0) else {
        log("auto crop: no spawn points on the nav mesh, using whole map".into());
        return None;
    };
    let hit = reach.iter().filter(|r| **r).count();
    if (hit as f64) < REACH_MIN * reach.len() as f64 && !nav.walked {
        log(format!("auto crop: spawns reach only {hit} of {} nav cells, keeping the whole nav mesh", reach.len()));
        reach.fill(true);
    }
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut z = [f64::INFINITY, f64::NEG_INFINITY];
    let mut n = 0;
    for (u, _) in reach.iter().enumerate().filter(|(_, r)| **r) {
        let (x, y) = nav.cell_xy(nav.col[u] as usize);
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        let h = nav.nodes[u].z as f64;
        z = [z[0].min(h), z[1].max(h)];
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
    Some(([b[0] - CROP_PAD, b[1] - CROP_PAD, b[2] + CROP_PAD, b[3] + CROP_PAD], z))
}
