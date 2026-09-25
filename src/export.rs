use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::bsp::Bsp;
use crate::camera::{Framing, auto_persp, camera_basis, extents};
use crate::grid::grid_preview;
use crate::look::Look;
use crate::overview::{self, Overview};
use crate::paths::{Partial, free_name, set_suffix};
use crate::render::{Cuts, Renderer};
use crate::scene::Report;

#[derive(Clone, Debug, PartialEq)]
pub struct IsoOpts {
    pub size: u32,
    pub ss: u32,
    pub pitch: f64,
    pub yaws: Vec<f64>,
    pub grid: bool,
    pub look: Look,
    pub framing: Framing,
}

impl Default for IsoOpts {
    fn default() -> Self {
        IsoOpts {
            size: 2048,
            ss: 3,
            pitch: 35.264,
            yaws: vec![45.0, 135.0, 225.0, 315.0],
            grid: false,
            look: Look::default(),
            framing: Framing::default(),
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
    rep: &mut Report,
) -> Result<Vec<PathBuf>> {
    let mut files = Partial::default();
    let yaws = if o.framing.camera.is_some() { &[0.0][..] } else { &o.yaws[..] };
    let total = (yaws.len() + o.grid as usize).max(1) as f32;
    if o.grid {
        rep.step(0.0)?;
        let f = free_name(out, &format!("{name}{cut_tag}_grid"), ".png");
        files.add(f.clone());
        grid_preview(r, bsp, &f, cuts)?;
        rep.log(format!("  {}", f.display()));
    }
    let ftag = o.framing.tag();
    let stems: Vec<String> = match &o.framing.camera {
        Some(_) => vec![format!("{name}{sky_tag}{cut_tag}{ftag}")],
        None => yaws.iter().map(|y| format!("{name}{sky_tag}{cut_tag}{ftag}_{:03}", (*y as i64).rem_euclid(360))).collect(),
    };
    let suffix = set_suffix(out, &stems, ".png");
    let upp = if o.framing == Framing::default() { iso_upp(r, cuts, &o.yaws, o.pitch, o.size) } else { 1.0 };
    let pts = if o.framing.persp.is_some() { r.points_in(cuts) } else { Vec::new() };
    for (i, (&yaw, stem)) in yaws.iter().zip(&stems).enumerate() {
        rep.step((i + o.grid as usize) as f32 / total)?;
        let (view, w, h) = match (&o.framing.camera, o.framing.persp) {
            (Some(c), _) => {
                let (w, h) = c.size(o.size);
                (c.view(w, h), w, h)
            }
            (None, Some(fov)) => auto_persp(&pts, yaw, o.pitch, fov, o.size, 16),
            (None, None) => r.iso_view(yaw, o.pitch, upp, 16, cuts),
        };
        let img = r.render_view(&view, w, h, o.ss, cuts, &o.look, 0.0)?;
        let f = out.join(format!("{stem}{suffix}.png"));
        files.add(f.clone());
        img.save(&f)?;
        rep.log(format!("  {} {}x{}", f.display(), w, h));
    }
    let _ = rep.step(1.0);
    Ok(files.keep())
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
    rep: &mut Report,
) -> Result<Overview> {
    let mut files = Partial::default();
    rep.step(0.0)?;
    if o.grid {
        let f = free_name(out, &format!("{name}_grid"), ".png");
        files.add(f.clone());
        grid_preview(r, bsp, &f, cuts)?;
        rep.log(format!("  {}", f.display()));
        rep.step(0.3)?;
    }
    let ov = overview_params(r, cuts, o)?;
    let img = overview::render(r, &ov, o.ss, cuts, o.cull)?;
    rep.step(0.8)?;
    for ext in overview::exts(o.png) {
        files.add(out.join(format!("{name}{ext}")));
    }
    overview::save(out, name, &ov, &img, o.png)?;
    files.keep();
    let _ = rep.step(1.0);
    rep.log(format!(
        "  ZOOM {:.2}  ORIGIN {:.0} {:.0} {:.0}  ROTATED {}",
        ov.zoom, ov.origin[0], ov.origin[1], ov.origin[2], ov.rotated
    ));
    rep.log(format!("  {}.bmp/.tga/.txt", out.join(name).display()));
    Ok(ov)
}
