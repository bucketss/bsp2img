use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::addon::Addon;
use crate::bsp::Bsp;
use crate::camera::{Framing, auto_persp, camera_basis, extents};
use crate::grid::grid_preview;
use crate::look::Look;
use crate::overview::{self, Overview, Radar};
use crate::radar::{Official, Split, icons, stem};
use crate::paths::{Partial, free_name, set_suffix};
use crate::render::{Cuts, Renderer};
use crate::scene::{Report, num};

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
    pub fit: bool,
    pub split: Split,
    pub tga: bool,
    pub official: bool,
    pub addon: String,
}

impl Default for OverviewOpts {
    fn default() -> Self {
        OverviewOpts {
            margin: 0.04,
            ss: 3,
            cull: true,
            from_txt: None,
            png: false,
            grid: false,
            fit: false,
            split: Split::Official,
            tga: false,
            official: false,
            addon: String::new(),
        }
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
    let pts = if o.framing.persp.is_some() { r.points_in(cuts) } else { Default::default() };
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadarSource {
    Official,
    Fitted,
    Txt,
}

impl RadarSource {
    pub fn label(self) -> &'static str {
        match self {
            RadarSource::Official => "official",
            RadarSource::Fitted => "fitted",
            RadarSource::Txt => "from txt",
        }
    }
}

pub fn official(bsp: &Bsp) -> Option<&Official> {
    bsp.extra.as_ref().and_then(|x| x.radar.as_ref())
}

pub fn radar_params(r: &Renderer, bsp: &Bsp, cuts: &Cuts, o: &OverviewOpts) -> Result<(Radar, RadarSource, Option<Official>)> {
    if let Some(t) = &o.from_txt {
        let t = Official::read(t)?;
        return Ok((t.frame, RadarSource::Txt, Some(t)));
    }
    let off = official(bsp).cloned();
    match &off {
        Some(x) if !o.fit => Ok((x.frame, RadarSource::Official, off)),
        _ => Ok((overview::fit_radar(&r.points_in(cuts), o.margin), RadarSource::Fitted, off)),
    }
}

pub fn radar_alignment(r: &Renderer, bsp: &Bsp, cuts: &Cuts, margin: f64) -> Option<String> {
    let off = official(bsp)?.frame;
    let pts = r.points_in(cuts);
    let fit = overview::fit_radar(&pts, margin);
    let ((fx, fy), (ox, oy)) = (fit.center(), off.center());
    let (dx, dy) = (fx - ox, fy - oy);
    Some(format!(
        "fit vs official: centre offset {dx:.0} {dy:.0} units ({:.0} px), scale {} vs {} ({:.2}x), {:.1}% of geometry inside the official frame",
        dx.hypot(dy) / off.scale,
        num(fit.scale),
        num(off.scale),
        fit.scale / off.scale,
        overview::coverage(&off, &pts) * 100.0
    ))
}

#[allow(clippy::too_many_arguments)]
fn export_radar(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    cuts: &Cuts,
    o: &OverviewOpts,
    out: &Path,
    mut files: Partial,
    addon: Option<Addon>,
    rep: &mut Report,
) -> Result<()> {
    let (frame, src, info) = radar_params(r, bsp, cuts, o)?;
    rep.log(format!(
        "  framing: {}  pos_x {}  pos_y {}  scale {}",
        src.label(),
        num(frame.pos_x),
        num(frame.pos_y),
        num(frame.scale)
    ));
    match radar_alignment(r, bsp, cuts, o.margin) {
        Some(a) => rep.log(format!("  {a}")),
        None => rep.log("  no official overview for this map".into()),
    }
    let (mut secs, why) = o.split.sections(info.as_ref(), bsp, cuts);
    if let Some(w) = why {
        rep.log(format!("  auto sections: {w}"));
    }
    if !matches!(o.split, Split::Official | Split::None) {
        secs.extend(info.iter().flat_map(|i| i.volumes().cloned()));
    }
    let stems: Vec<String> = secs.iter().map(|s| stem(name, &s.name)).collect();
    let plan = addon.as_ref().map(|a| a.plan(name, &stems));
    if let (Some(a), Some(p)) = (&addon, &plan) {
        rep.log(format!("  compile into addon {}, writing:", a.name));
        for f in p.writes() {
            rep.log(format!("    {}{}", f.display(), if f.exists() { " (replaces)" } else { "" }));
        }
    }
    let mut sent = Vec::new();
    let total = secs.len() as f32 + 0.5;
    for (i, s) in secs.iter().enumerate() {
        rep.step((i as f32 + 0.3) / total)?;
        let f = s.frame.unwrap_or(frame);
        let img = overview::render_radar(r, &f, o.ss, &s.cuts(cuts), o.cull)?;
        let st = &stems[i];
        if let Some(p) = &plan {
            if let Some(d) = p.tga[i].parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&p.tga[i], overview::tga_rgba_bytes(&img))?;
            std::fs::write(&p.vtex[i], crate::addon::vtex(st))?;
            sent.push(img.clone());
        }
        for e in overview::radar_exts(true, o.tga) {
            files.add(out.join(format!("{st}_radar{e}")));
        }
        let mut saved = overview::save_radar(out, st, &img, true, o.tga)?;
        if let crate::bsp::Engine::Source(v) = bsp.engine() {
            for (n, b) in crate::s1::radar::export(st, img.width() as usize, img.height() as usize, img.as_raw(), v) {
                let p = out.join(n);
                files.add(p.clone());
                std::fs::write(&p, b)?;
                saved.push(p);
            }
        }
        let what = if s.frame.is_some() { format!("pos_x {} pos_y {} scale {}", num(f.pos_x), num(f.pos_y), num(f.scale)) } else { s.range() };
        rep.log(format!("  {} ({what}): {}", s.name, saved.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("  ")));
        if o.official {
            match bsp.extra.as_ref().map(|x| x.radar_image(&bsp.name().to_lowercase(), &s.name, 4096)).transpose()?.flatten() {
                Some((n, im)) => {
                    let p = out.join(format!("{st}_radar_official.png"));
                    files.add(p.clone());
                    image::RgbaImage::from_raw(im.w as u32, im.h as u32, im.rgba).map(|x| x.save(&p)).transpose()?;
                    rep.log(format!("  {} official {}x{} from {n}: {}", s.name, im.w, im.h, p.display()));
                }
                None => rep.log(format!("  {} has no official radar image", s.name)),
            }
        }
    }
    let mut t = Official::new(name, frame);
    if src != RadarSource::Fitted
        && let Some(i) = &info
    {
        (t.rotate, t.zoom, t.insets) = (i.rotate, i.zoom, i.insets);
    }
    t.sections = secs;
    t.icons = icons(bsp, &frame, info.as_ref());
    t.autogen = (o.tga || addon.is_some()) && bsp.is_cs2();
    let txt = out.join(format!("{name}.txt"));
    files.add(txt.clone());
    std::fs::write(&txt, t.txt())?;
    files.keep();
    rep.log(format!("  {}", txt.display()));
    if let (Some(a), Some(p)) = (&addon, &plan) {
        if let Some(d) = p.txt.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p.txt, t.txt())?;
        let t0 = std::time::SystemTime::now();
        a.compile(&p.vtex, rep)?;
        let fresh = |f: &Path| std::fs::metadata(f).and_then(|m| m.modified()).is_ok_and(|m| m + std::time::Duration::from_secs(2) >= t0);
        let stale: Vec<String> = p.vtex_c.iter().filter(|f| !fresh(f)).map(|f| f.display().to_string()).collect();
        if !stale.is_empty() {
            anyhow::bail!("resourcecompiler finished but did not write {}", stale.join(", "));
        }
        for (f, img) in p.vtex_c.iter().zip(&sent) {
            rep.log(format!("  compiled {} ({})", f.display(), crate::addon::check(f, img)?));
        }
    }
    let _ = rep.step(1.0);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn export_overview(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    cuts: &Cuts,
    o: &OverviewOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<()> {
    if bsp.is_tf2() {
        bail!("overviews are not supported for TF2 maps");
    }
    let mut files = Partial::default();
    rep.step(0.0)?;
    let addon = match o.addon.trim() {
        "" => None,
        _ if !bsp.is_cs2() => anyhow::bail!("compiling into an addon is for CS2 maps only"),
        a => Some(Addon::find(bsp.extra.as_ref().and_then(|x| x.root.as_deref()), a)?),
    };
    if o.grid {
        let f = free_name(out, &format!("{name}_grid"), ".png");
        files.add(f.clone());
        grid_preview(r, bsp, &f, cuts)?;
        rep.log(format!("  {}", f.display()));
        rep.step(0.3)?;
    }
    if bsp.modern() {
        return export_radar(r, bsp, name, cuts, o, out, files, addon, rep);
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
    Ok(())
}
