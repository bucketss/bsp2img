use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::bsp::Bsp;
use crate::camera::{camera_basis, extents};
use crate::grid::grid_preview;
use crate::overview::{self, Overview};
use crate::paths::{free_name, set_suffix};
use crate::render::{Cuts, Renderer};
use crate::scene::Log;

#[derive(Clone, Debug, PartialEq)]
pub struct IsoOpts {
    pub size: u32,
    pub ss: u32,
    pub pitch: f64,
    pub yaws: Vec<f64>,
    pub bg: Option<[u8; 3]>,
    pub cull: bool,
    pub grid: bool,
}

impl Default for IsoOpts {
    fn default() -> Self {
        IsoOpts {
            size: 2048,
            ss: 3,
            pitch: 35.264,
            yaws: vec![45.0, 135.0, 225.0, 315.0],
            bg: None,
            cull: true,
            grid: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OverviewOpts {
    pub margin: f64,
    pub ss: u32,
    pub cull: bool,
    pub from_txt: Option<PathBuf>,
    pub png: bool,
    pub grid: bool,
}

impl Default for OverviewOpts {
    fn default() -> Self {
        OverviewOpts { margin: 0.04, ss: 3, cull: true, from_txt: None, png: false, grid: false }
    }
}

pub fn iso_upp(r: &Renderer, cuts: &Cuts, yaws: &[f64], pitch: f64, size: u32) -> f64 {
    let pts = r.points_in(cuts);
    let mut span: f64 = 0.0;
    for &yaw in yaws {
        let e = extents(&pts, &camera_basis(yaw, pitch));
        span = span.max(e[0].1 - e[0].0).max(e[1].1 - e[1].0);
    }
    span / (size as f64 - 32.0).max(1.0)
}

pub fn export_iso(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    sky_tag: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &IsoOpts,
    out: &Path,
    log: Log,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if o.grid {
        let f = free_name(out, &format!("{name}{cut_tag}_grid"), ".png");
        grid_preview(r, bsp, &f, cuts)?;
        log(format!("  {}", f.display()));
        files.push(f);
    }
    let upp = iso_upp(r, cuts, &o.yaws, o.pitch, o.size);
    let stems: Vec<String> =
        o.yaws.iter().map(|y| format!("{name}{sky_tag}{cut_tag}_{:03}", (*y as i64).rem_euclid(360))).collect();
    let suffix = set_suffix(out, &stems, ".png");
    for (&yaw, stem) in o.yaws.iter().zip(&stems) {
        let (view, w, h) = r.iso_view(yaw, o.pitch, upp, 16, cuts);
        let img = r.render_view(&view, w, h, o.ss, cuts, o.cull, o.bg)?;
        let f = out.join(format!("{stem}{suffix}.png"));
        img.save(&f)?;
        log(format!("  {} {}x{}", f.display(), w, h));
        files.push(f);
    }
    Ok(files)
}

pub fn overview_params(r: &Renderer, cuts: &Cuts, o: &OverviewOpts) -> Result<Overview> {
    Ok(match &o.from_txt {
        Some(t) => overview::read_txt(t)?,
        None => overview::fit(&r.points_in(cuts), o.margin),
    })
}

pub fn export_overview(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    cuts: &Cuts,
    o: &OverviewOpts,
    out: &Path,
    log: Log,
) -> Result<Overview> {
    if o.grid {
        let f = free_name(out, &format!("{name}_grid"), ".png");
        grid_preview(r, bsp, &f, cuts)?;
        log(format!("  {}", f.display()));
    }
    let ov = overview_params(r, cuts, o)?;
    let img = overview::render(r, &ov, o.ss, cuts, o.cull)?;
    overview::save(out, name, &ov, &img, o.png)?;
    log(format!(
        "  ZOOM {:.2}  ORIGIN {:.0} {:.0} {:.0}  ROTATED {}",
        ov.zoom, ov.origin[0], ov.origin[1], ov.origin[2], ov.rotated
    ));
    log(format!("  {}.bmp/.tga/.txt", out.join(name).display()));
    Ok(ov)
}
