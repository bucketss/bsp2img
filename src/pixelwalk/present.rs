use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glam::DVec3;
use tiny_skia::{FillRule, PathBuilder, Pixmap, Stroke, Transform};

use super::finder::Find;
use super::output::yaw_of;
use super::{FinderOpts, find};
use crate::bsp::Bsp;
use crate::grid::{Backdrop, Text, paint, rect};
use crate::paths::{Partial, free_name};
use crate::radar::Split;
use crate::render::{Cuts, Renderer};
use crate::scene::Report;
use crate::wallbang::draw2d::{self, Frame};

const HUE_LOW: f64 = 300.0;
const HUE_HIGH: f64 = 120.0;
const YAW_TICK: f64 = 48.0;
const LINE_W: f32 = 4.0;
const DOT_R: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stance {
    Stand,
    Duck,
}

impl Stance {
    pub fn label(self) -> &'static str {
        match self {
            Stance::Stand => "stand",
            Stance::Duck => "duck",
        }
    }

    pub fn feet(self) -> f64 {
        match self {
            Stance::Stand => 36.0,
            Stance::Duck => 18.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pixelwalk {
    pub stance: Stance,
    pub yaw: f32,
    pub from: DVec3,
    pub to: DVec3,
    pub finds: Vec<Find>,
    pub floor_model: i32,
    pub wall_model: i32,
    pub catch_frames: u8,
}

impl Pixelwalk {
    pub fn span(&self) -> f64 {
        (self.to - self.from).truncate().length()
    }

    pub fn mid(&self) -> DVec3 {
        (self.from + self.to) / 2.0
    }
}

#[derive(Clone, Debug)]
pub struct PixelwalkOpts {
    pub finder: FinderOpts,
    pub merge_gap: f64,
    pub split: Split,
    pub base: Backdrop,
    pub size: u32,
    pub labels: bool,
}

fn dv(v: &[f32; 3]) -> DVec3 {
    DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
}

fn ends(f: &Find) -> (DVec3, DVec3) {
    let a = dv(&f.pos);
    (a, if f.length > 0.5 { dv(&f.to) } else { a })
}

pub fn merge(finds: &[Find], gap: f64) -> Vec<Pixelwalk> {
    let mut groups: BTreeMap<(u8, i64, i64, i64), Vec<&Find>> = BTreeMap::new();
    for f in finds {
        let yaw = yaw_of(&f.approach) as f64;
        let (a, _) = ends(f);
        let y = yaw.to_radians();
        let perp = ((a.x * y.cos() + a.y * y.sin()) / 2.0).round() as i64;
        groups.entry((f.usehull as u8, yaw.round() as i64, (a.z / 4.0).round() as i64, perp)).or_default().push(f);
    }
    let mut out = Vec::new();
    for g in groups.values() {
        let y = (yaw_of(&g[0].approach) as f64).to_radians();
        let u = DVec3::new(-y.sin(), y.cos(), 0.0);
        let along = |p: DVec3| p.dot(u);
        let mut g: Vec<(&Find, DVec3, DVec3)> = g
            .iter()
            .map(|f| {
                let (a, b) = ends(f);
                if along(a) <= along(b) { (*f, a, b) } else { (*f, b, a) }
            })
            .collect();
        g.sort_by(|p, q| along(p.1).total_cmp(&along(q.1)).then(p.1.x.total_cmp(&q.1.x)).then(p.1.y.total_cmp(&q.1.y)));
        let mut cur: Option<Pixelwalk> = None;
        for (f, lo, hi) in g {
            if let Some(c) = cur.as_mut()
                && along(lo) - along(c.to) <= gap
            {
                if along(hi) > along(c.to) {
                    c.to = hi;
                }
                c.catch_frames = c.catch_frames.max(f.catch_frames.clamp(0, 255) as u8);
                c.finds.push(f.clone());
                continue;
            }
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some(Pixelwalk {
                stance: if f.usehull == 0 { Stance::Stand } else { Stance::Duck },
                yaw: yaw_of(&f.approach),
                from: lo,
                to: hi,
                finds: vec![f.clone()],
                floor_model: f.floor_model,
                wall_model: f.wall_model,
                catch_frames: f.catch_frames.clamp(0, 255) as u8,
            });
        }
        out.extend(cur);
    }
    out.sort_by(|a, b| a.stance.cmp(&b.stance).then(b.from.z.total_cmp(&a.from.z)).then(a.from.x.total_cmp(&b.from.x)).then(a.from.y.total_cmp(&b.from.y)));
    out
}

pub fn setpos(p: DVec3, yaw: f32) -> String {
    format!("amx_setpos {:.7} {:.7} {:.7} 0 {:.7}", p.x as f32 as f64, p.y as f32 as f64, p.z as f32 as f64, yaw as f64)
}

pub fn txt(name: &str, pws: &[Pixelwalk], zr: (f64, f64)) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{name}: {} pixelwalks (z colour scale {:.0} .. {:.0})", pws.len(), zr.0, zr.1);
    let _ = writeln!(s, "amx_setpos x y z pitch yaw puts you on the spot; hold +forward along yaw.");
    let _ = writeln!(s);
    for (i, p) in pws.iter().enumerate() {
        let span = p.span();
        let mut head = format!("#{} {}, yaw {:.1}, z {:.1}", i + 1, p.stance.label(), p.yaw, p.from.z);
        if (p.to.z - p.from.z).abs() > 0.05 {
            let _ = write!(head, " .. {:.1}", p.to.z);
        }
        if p.finds.len() > 1 || span > 0.0 {
            let _ = write!(head, ", span {span:.0}u, {} find{}", p.finds.len(), if p.finds.len() > 1 { "s" } else { "" });
        }
        let _ = write!(head, ", floor*{} wall*{}, catch {}", p.floor_model, p.wall_model, p.catch_frames);
        let _ = writeln!(s, "{head}");
        let mut seen: Vec<[u32; 3]> = Vec::new();
        for f in &p.finds {
            let (a, b) = ends(f);
            for q in [a, b] {
                let k = [q.x as f32, q.y as f32, q.z as f32].map(f32::to_bits);
                if !seen.contains(&k) {
                    seen.push(k);
                    let _ = writeln!(s, "  {}", setpos(q, yaw_of(&f.approach)));
                }
            }
        }
        let _ = writeln!(s);
    }
    s
}

pub fn zcolor(z: f64, zr: (f64, f64)) -> [u8; 3] {
    let t = ((z - zr.0) / (zr.1 - zr.0).max(1e-6)).clamp(0.0, 1.0);
    let h = (HUE_LOW + (HUE_HIGH - HUE_LOW) * t) / 60.0;
    let x = 1.0 - ((h % 2.0) - 1.0).abs();
    let (r, g, b) = match h as i64 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

fn stroke(pm: &mut Pixmap, a: (f32, f32), b: (f32, f32), c: [u8; 3], w: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(a.0, a.1);
    pb.line_to(b.0, b.1);
    if let Some(p) = pb.finish() {
        let st = Stroke { width: w, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
        pm.stroke_path(&p, &paint(c, 255), &st, Transform::identity(), None);
    }
}

pub fn draw(pm: &mut Pixmap, f: &Frame, pws: &[(usize, &Pixelwalk)], zr: (f64, f64), labels: bool) {
    for (_, p) in pws {
        let m = p.mid();
        let y = (p.yaw as f64).to_radians();
        let tip = m + DVec3::new(y.cos(), y.sin(), 0.0) * YAW_TICK;
        stroke(pm, f.px(m.x, m.y), f.px(tip.x, tip.y), zcolor(m.z, zr), 2.0);
        let (a, b) = (f.px(p.from.x, p.from.y), f.px(p.to.x, p.to.y));
        let n = (((b.0 - a.0).hypot(b.1 - a.1) / 2.0) as usize).max(1);
        for i in 0..n {
            let t0 = i as f64 / n as f64;
            let t1 = (i + 1) as f64 / n as f64;
            let p0 = p.from + (p.to - p.from) * t0;
            let p1 = p.from + (p.to - p.from) * t1;
            stroke(pm, f.px(p0.x, p0.y), f.px(p1.x, p1.y), zcolor((p0.z + p1.z) / 2.0, zr), LINE_W);
        }
        for q in [p.from, p.to] {
            let (x, y) = f.px(q.x, q.y);
            let c = zcolor(q.z, zr);
            if let Some(path) = PathBuilder::from_circle(x, y, DOT_R) {
                if p.stance == Stance::Stand {
                    pm.fill_path(&path, &paint(c, 255), FillRule::Winding, Transform::identity(), None);
                } else {
                    pm.stroke_path(&path, &paint(c, 255), &Stroke { width: 2.5, ..Default::default() }, Transform::identity(), None);
                }
            }
        }
    }
    if labels {
        let small = Text::new(12.0);
        let mut placed: Vec<(f32, f32, f32, f32)> = Vec::new();
        for (i, p) in pws {
            let s = format!("#{}", i + 1);
            let (tw, th) = small.size(&s);
            let (x, y) = f.px(p.from.x, p.from.y);
            let (lx, mut ly) = (x + 8.0, y - th - 4.0);
            for _ in 0..40 {
                if !placed.iter().any(|r| lx < r.2 && lx + tw > r.0 && ly < r.3 && ly + th > r.1) {
                    break;
                }
                ly += th + 2.0;
            }
            placed.push((lx - 2.0, ly - 1.0, lx + tw + 2.0, ly + th + 1.0));
            rect(pm, lx - 2.0, ly - 1.0, lx + tw + 2.0, ly + th + 1.0, [0, 0, 0], 190);
            small.draw(pm, lx, ly, &s, zcolor(p.from.z, zr));
        }
    }
}

pub fn legend(pm: &mut Pixmap, zr: (f64, f64)) {
    let small = Text::new(12.0);
    let (fw, fh) = (pm.width() as f32, pm.height() as f32);
    let (x0, x1, y0) = (fw - 300.0, fw - 24.0, fh - 64.0);
    rect(pm, x0 - 10.0, y0 - 26.0, fw - 4.0, fh - 4.0, [0, 0, 0], 200);
    small.draw(pm, x0, y0 - 22.0, "height (filled = standing, hollow = duck)", [255, 255, 255]);
    for i in 0..(x1 - x0) as usize {
        let c = zcolor(zr.0 + (zr.1 - zr.0) * i as f64 / (x1 - x0) as f64, zr);
        crate::grid::line(pm, (x0 + i as f32, y0), (x0 + i as f32, y0 + 16.0), c, 255, 1.2);
    }
    small.draw(pm, x0, y0 + 20.0, &format!("z {:.0}", zr.0), [255, 255, 255]);
    let e = format!("z {:.0}", zr.1);
    small.draw(pm, x1 - small.size(&e).0, y0 + 20.0, &e, [255, 255, 255]);
}

pub struct Summary {
    pub stand: usize,
    pub duck: usize,
    pub longest: f64,
}

pub fn find_pixelwalks(bsp: &Bsp, o: &PixelwalkOpts) -> Result<Vec<Pixelwalk>> {
    Ok(merge(&find(bsp, &o.finder)?.finds, o.merge_gap))
}

pub fn world_z(bsp: &Bsp) -> (f64, f64) {
    (bsp.models[0].mins.z, bsp.models[0].maxs.z)
}

#[allow(clippy::too_many_arguments)]
pub fn export_pixelwalks(
    r: &mut Renderer,
    bsp: &Bsp,
    dirs: &[PathBuf],
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &PixelwalkOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<(Vec<PathBuf>, Summary, Vec<Pixelwalk>)> {
    let t0 = std::time::Instant::now();
    rep.step(0.0)?;
    let found = find(bsp, &o.finder)?;
    let pws = merge(&found.finds, o.merge_gap);
    let stand = pws.iter().filter(|p| p.stance == Stance::Stand).count();
    rep.log(format!(
        "  {} seams, {} finds, {} pixelwalks ({stand} standing, {} duck) ({:.1}s)",
        found.seams,
        found.finds.len(),
        pws.len(),
        pws.len() - stand,
        t0.elapsed().as_secs_f64()
    ));
    rep.step(0.7)?;
    let zr = world_z(bsp);
    let (secs, why) = o.split.sections(None, bsp, cuts);
    if let Some(w) = why {
        rep.log(format!("  sections: {w}"));
    }
    let bases = crate::wallbang::export::render_bases(r, bsp, dirs, cuts, o.size, o.base, &secs, rep)?;
    let mut files = Partial::default();
    for b in &bases {
        let sel: Vec<(usize, &Pixelwalk)> =
            pws.iter().enumerate().filter(|(_, p)| b.section.contains(p.from.z.min(p.to.z) - p.stance.feet())).collect();
        let mut pm = Pixmap::from_vec(b.px.clone(), tiny_skia::IntSize::from_wh(b.frame.w, b.frame.h).context("bad size")?).context("pixmap")?;
        draw2d::markers(&mut pm, bsp, &b.frame);
        draw(&mut pm, &b.frame, &sel, zr, o.labels);
        let st = sel.iter().filter(|p| p.1.stance == Stance::Stand).count();
        draw2d::title(
            &mut pm,
            &format!(
                "{name}{}  {} pixelwalks ({st} standing, {} duck); line = walkable span, spike = direction to hold +forward",
                b.title,
                sel.len(),
                sel.len() - st
            ),
        );
        legend(&mut pm, zr);
        draw2d::scale_bar(&mut pm, &b.frame);
        let f = free_name(out, &format!("{name}{cut_tag}_pixelwalks{}", b.tag), ".png");
        files.add(f.clone());
        draw2d::save_png(&pm, &f)?;
        rep.log(format!("  {}", f.display()));
    }
    let f = free_name(out, &format!("{name}{cut_tag}_pixelwalks"), ".txt");
    files.add(f.clone());
    std::fs::write(&f, txt(name, &pws, zr))?;
    rep.log(format!("  {}", f.display()));
    rep.step(1.0)?;
    let longest = pws.iter().map(Pixelwalk::span).fold(0.0, f64::max);
    Ok((files.keep(), Summary { stand, duck: pws.len() - stand, longest }, pws))
}
