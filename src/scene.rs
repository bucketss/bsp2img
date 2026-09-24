use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;

use crate::bsp::Bsp;
use crate::light::LightParams;
use crate::mesh::{Mesh, build_mesh, roof_levels, roof_zmax};
use crate::paths::search_dirs;
use crate::reach::{HullMask, analyze, hull_mask};
use crate::render::{Cuts, Gpu, NO_CLIP, Renderer};
use crate::sky::{find_sky, load_sky};
use crate::wad::TextureSource;

pub type Log<'a> = &'a mut dyn FnMut(String);

#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub struct Report<'a> {
    pub log: &'a mut dyn FnMut(String),
    pub progress: &'a mut dyn FnMut(f32) -> bool,
}

impl Report<'_> {
    pub fn log(&mut self, s: String) {
        (self.log)(s)
    }

    pub fn step(&mut self, f: f32) -> Result<()> {
        if (self.progress)(f.clamp(0.0, 1.0)) { Ok(()) } else { Err(Cancelled.into()) }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadOpts {
    pub game: Option<PathBuf>,
    pub wad_dirs: Vec<PathBuf>,
    pub light: LightParams,
    pub auto_crop: bool,
    pub hull: Option<usize>,
    pub hull_pad: f64,
}

impl Default for LoadOpts {
    fn default() -> Self {
        LoadOpts { game: None, wad_dirs: Vec::new(), light: LightParams::default(), auto_crop: true, hull: None, hull_pad: 64.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CutOpts {
    pub roofs: usize,
    pub zmin: Option<f64>,
    pub zmax: Option<f64>,
    pub crop: Option<[f64; 4]>,
    pub xmin: Option<f64>,
    pub xmax: Option<f64>,
    pub ymin: Option<f64>,
    pub ymax: Option<f64>,
}

impl CutOpts {
    pub fn clip_box(&self) -> [f64; 4] {
        let [mut x0, mut y0, mut x1, mut y1] = NO_CLIP;
        if let Some([a, b, c, d]) = self.crop {
            (x0, y0, x1, y1) = (a.min(c), b.min(d), a.max(c), b.max(d));
        }
        if let Some(v) = self.xmin {
            x0 = x0.max(v);
        }
        if let Some(v) = self.ymin {
            y0 = y0.max(v);
        }
        if let Some(v) = self.xmax {
            x1 = x1.min(v);
        }
        if let Some(v) = self.ymax {
            y1 = y1.min(v);
        }
        [x0, y0, x1, y1]
    }

    pub fn tag(&self, hull: Option<usize>) -> String {
        let mut parts = Vec::new();
        match hull {
            Some(3) => parts.push("hull".to_string()),
            Some(h) => parts.push(format!("hull{h}")),
            None => {}
        }
        if self.roofs > 0 {
            parts.push(format!("roof{:02}", self.roofs));
        }
        let [x0, y0, x1, y1] = self.clip_box();
        let z0 = self.zmin.unwrap_or(-1e9);
        let z1 = self.zmax.unwrap_or(1e9);
        for (axis, lo, hi) in [("x", x0, x1), ("y", y0, y1), ("z", z0, z1)] {
            if lo > -1e9 {
                parts.push(format!("{axis}min{}", num(lo)));
            }
            if hi < 1e9 {
                parts.push(format!("{axis}max{}", num(hi)));
            }
        }
        parts.iter().map(|p| format!("_{p}")).collect()
    }
}

pub fn num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") }
}

pub struct Scene {
    pub bsp: Bsp,
    pub mesh: Mesh,
    pub levels: Vec<(f64, f64, f64)>,
    pub mask: Option<HullMask>,
    pub dirs: Vec<PathBuf>,
    pub opts: LoadOpts,
}

impl Scene {
    pub fn load(path: &Path, opts: &LoadOpts, max_dim: u32, log: Log) -> Result<Scene> {
        let bsp = Bsp::load(path)?;
        let dirs = search_dirs(path, opts.game.as_deref(), &opts.wad_dirs);
        let mut tex = TextureSource::new(&dirs);
        let missing = tex.use_worldspawn(bsp.worldspawn().get("wad").unwrap_or(""));
        if !missing.is_empty() {
            log(format!("wads not found: {}", missing.join(", ")));
        }
        let rc = if opts.auto_crop {
            let t = Instant::now();
            let rc = analyze(&bsp);
            match &rc {
                Some(r) => log(format!(
                    "auto crop: {} reachable faces (grid {:.0}, {:.1}s)",
                    r.faces.len(),
                    r.spacing,
                    t.elapsed().as_secs_f64()
                )),
                None => log("auto crop: no spawn points found, using whole map".into()),
            }
            rc
        } else {
            None
        };
        let mesh = build_mesh(&bsp, &mut tex, &opts.light, rc.as_ref(), max_dim);
        if !mesh.missing.is_empty() {
            log(format!("missing textures ({}): {}", mesh.missing.len(), mesh.missing.join(", ")));
        }
        log(format!(
            "faces: {}, batches: {}, lightmap atlas: {}x{}",
            mesh.faces,
            mesh.batches.len(),
            mesh.atlas.w,
            mesh.atlas.h
        ));
        let levels = roof_levels(&mesh);
        log(format!(
            "roof levels: {}",
            levels
                .iter()
                .enumerate()
                .map(|(i, (lo, hi, _))| {
                    if hi - lo > 1.0 { format!("{}:{:.0}..{:.0}", i + 1, lo, hi) } else { format!("{}:{:.0}", i + 1, lo) }
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let mask = match opts.hull {
            Some(h) => {
                let t = Instant::now();
                let m = hull_mask(&bsp, h, opts.hull_pad);
                match &m {
                    Some(m) => log(format!(
                        "hull {h}: {} walkable cells (grid {:.0}, {:.1}s)",
                        m.cells,
                        m.spacing,
                        t.elapsed().as_secs_f64()
                    )),
                    None => log("hull: no clipnodes or spawn points, skipped".into()),
                }
                m
            }
            None => None,
        };
        Ok(Scene { bsp, mesh, levels, mask, dirs, opts: opts.clone() })
    }

    pub fn zmax(&self, cuts: &CutOpts, log: Log) -> f64 {
        let z = cuts.zmax.unwrap_or(1e9).min(roof_zmax(&self.levels, cuts.roofs, log));
        if cuts.roofs > 0 {
            log(format!("--roofs {}: zmax {:.0}", cuts.roofs, z));
        }
        z
    }

    pub fn cuts(&self, c: &CutOpts, log: Log) -> Cuts {
        Cuts { zmin: c.zmin.unwrap_or(-1e9), zmax: self.zmax(c, log), clip: c.clip_box(), use_mask: true }
    }

    pub fn renderer(&self, gpu: &Gpu, nearest: bool) -> Renderer {
        let mut r = Renderer::new(gpu, &self.mesh, nearest);
        r.set_mask(self.mask.clone());
        r
    }

    pub fn job_renderer(&self, gpu: &Gpu, nearest: bool, sky: Option<(String, f64, f64)>, log: Log) -> (Renderer, String) {
        let mut r = self.renderer(gpu, nearest);
        let Some((name, fov, pitch)) = sky else { return (r, String::new()) };
        let sname = self.sky_name(Some(&name));
        match self.load_sky(&sname) {
            Some(f) => {
                r.set_sky(Some(f), fov, pitch);
                log(format!("sky: {sname}"));
                let tag = if name.is_empty() { String::new() } else { format!("_{name}") };
                (r, tag)
            }
            None => {
                log(format!("sky: {sname} not found in gfx/env, using background"));
                (r, String::new())
            }
        }
    }

    pub fn sky_name(&self, name: Option<&str>) -> String {
        match name {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => self.bsp.worldspawn().get("skyname").unwrap_or("desert").to_string(),
        }
    }

    pub fn load_sky(&self, name: &str) -> Option<(u32, Vec<u8>)> {
        let faces = find_sky(name, &self.dirs)?;
        load_sky(&faces, &self.opts.light.tex_lut()).ok()
    }
}
