use std::path::Path;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use anyhow::{Context, Result};
use glam::DVec3;
use tiny_skia::{Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::bsp::Bsp;
use crate::camera::top_down;
use crate::look::Look;
use crate::render::{Cuts, NO_CLIP, Renderer, View};

pub struct Marker {
    pub pos: DVec3,
    pub bbox: Option<[f64; 4]>,
    pub color: [u8; 3],
    pub label: &'static str,
}

pub const CT: [u8; 3] = [80, 140, 255];
pub const T: [u8; 3] = [255, 70, 60];
pub const HOSTAGE: [u8; 3] = [255, 220, 0];
pub const REMOVED: [u8; 3] = [200, 0, 0];

fn marker_style(cls: &str) -> Option<([u8; 3], &'static str)> {
    Some(match cls {
        "info_player_start" => (CT, "CT"),
        "info_player_deathmatch" => (T, "T"),
        "hostage_entity" => (HOSTAGE, "H"),
        "info_vip_start" => ([60, 220, 90], "VIP"),
        "func_bomb_target" | "info_bomb_target" => ([255, 140, 0], "BOMB"),
        "func_hostage_rescue" => ([0, 220, 220], "RESCUE"),
        "func_vip_safetyzone" => ([0, 220, 220], "ESCAPE"),
        _ => return None,
    })
}

pub fn entity_marks(bsp: &Bsp) -> Vec<Marker> {
    let mut out = Vec::new();
    for e in &bsp.entities {
        let Some((color, label)) = marker_style(e.class()) else { continue };
        if let Some(mi) = e.model() {
            let Some(m) = bsp.models.get(mi) else { continue };
            out.push(Marker {
                pos: (m.mins + m.maxs) / 2.0,
                bbox: Some([m.mins.x, m.mins.y, m.maxs.x, m.maxs.y]),
                color,
                label,
            });
        } else if let Some(p) = e.origin() {
            out.push(Marker { pos: p, bbox: None, color, label });
        }
    }
    out
}

pub fn nice_step(span: f64) -> f64 {
    let raw = span / 14.0;
    for s in [32.0, 64.0, 128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0] {
        if s >= raw {
            return s;
        }
    }
    8192.0
}

pub struct Text {
    font: FontRef<'static>,
    scale: PxScale,
}

impl Text {
    pub fn new(px: f32) -> Text {
        Text { font: FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR).unwrap(), scale: PxScale::from(px) }
    }
    pub fn size(&self, s: &str) -> (f32, f32) {
        let f = self.font.as_scaled(self.scale);
        (s.chars().map(|c| f.h_advance(f.glyph_id(c))).sum(), f.ascent() - f.descent())
    }
    pub fn draw(&self, pm: &mut Pixmap, x: f32, y: f32, s: &str, color: [u8; 3]) {
        let f = self.font.as_scaled(self.scale);
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        let data = pm.data_mut();
        let mut caret = x;
        for c in s.chars() {
            let g = f.glyph_id(c).with_scale_and_position(self.scale, point(caret, y + f.ascent()));
            caret += f.h_advance(f.glyph_id(c));
            let Some(o) = self.font.outline_glyph(g) else { continue };
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px < 0 || py < 0 || px >= w || py >= h {
                    return;
                }
                let i = ((py * w + px) * 4) as usize;
                let a = cov.clamp(0.0, 1.0);
                for k in 0..3 {
                    data[i + k] = (data[i + k] as f32 * (1.0 - a) + color[k] as f32 * a).round() as u8;
                }
            });
        }
    }
}

pub fn paint(c: [u8; 3], a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], a);
    p.anti_alias = true;
    p
}

pub fn line(pm: &mut Pixmap, a: (f32, f32), b: (f32, f32), c: [u8; 3], alpha: u8, width: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(a.0, a.1);
    pb.line_to(b.0, b.1);
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, &paint(c, alpha), &Stroke { width, ..Default::default() }, Transform::identity(), None);
    }
}

pub fn rect(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 3], alpha: u8) {
    if let Some(r) = Rect::from_ltrb(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)) {
        pm.fill_rect(r, &paint(c, alpha), Transform::identity(), None);
    }
}

pub fn outline(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 3], width: f32) {
    if let Some(r) = Rect::from_ltrb(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)) {
        let path = PathBuilder::from_rect(r);
        pm.stroke_path(&path, &paint(c, 255), &Stroke { width, ..Default::default() }, Transform::identity(), None);
    }
}

pub struct GridFrame {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub upp: f64,
    pub wpx: u32,
    pub hpx: u32,
}

pub fn grid_frame(r: &Renderer, cuts: &Cuts, size: u32) -> GridFrame {
    let pts = r.points_in(&Cuts { clip: NO_CLIP, use_mask: false, ..*cuts });
    let lo = pts.iter().copied().fold(DVec3::splat(f64::INFINITY), DVec3::min);
    let hi = pts.iter().copied().fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max);
    let m = (hi.x - lo.x).max(hi.y - lo.y) * 0.04;
    let (x0, y0, x1, y1) = (lo.x - m, lo.y - m, hi.x + m, hi.y + m);
    let upp = (x1 - x0).max(y1 - y0) / size as f64;
    let wpx = ((x1 - x0) / upp).ceil() as u32;
    let hpx = ((y1 - y0) / upp).ceil() as u32;
    GridFrame { x0, y0, x1: x0 + wpx as f64 * upp, y1: y0 + hpx as f64 * upp, upp, wpx, hpx }
}

pub fn grid_preview(r: &mut Renderer, bsp: &Bsp, path: &Path, cuts: &Cuts) -> Result<()> {
    let g = grid_frame(r, cuts, 1600);
    let (x0, y0, x1, y1, upp) = (g.x0, g.y0, g.x1, g.y1, g.upp);
    let (wpx, hpx) = (g.wpx, g.hpx);
    let view = View {
        basis: top_down(DVec3::X, DVec3::Y),
        cx: (x0 + x1) / 2.0,
        cy: (y0 + y1) / 2.0,
        w: x1 - x0,
        h: y1 - y0,
        sky_yaw: None,
        persp: None,
    };
    let rc = Cuts { clip: NO_CLIP, use_mask: false, ..*cuts };
    let img = r.render_view(&view, wpx, hpx, 2, &rc, &Look::plain(true, Some([0x1c, 0x1c, 0x1c])), 0.0)?;
    let mut pm = Pixmap::from_vec(img.into_raw(), tiny_skia::IntSize::from_wh(wpx, hpx).context("bad size")?)
        .context("pixmap")?;

    let [cx0, cy0, cx1, cy1] = cuts.clip;
    {
        let data = pm.data_mut();
        for py in 0..hpx {
            let wy = y1 - (py as f64 + 0.5) * upp;
            for px in 0..wpx {
                let wx = x0 + (px as f64 + 0.5) * upp;
                let mut removed = wx < cx0 || wx > cx1 || wy < cy0 || wy > cy1;
                if let Some(m) = &r.mask {
                    removed |= !m.test(wx, wy);
                }
                if removed {
                    let i = ((py * wpx + px) * 4) as usize;
                    let a = 110.0 / 255.0;
                    for k in 0..3 {
                        data[i + k] = (data[i + k] as f64 * (1.0 - a) + REMOVED[k] as f64 * a).round() as u8;
                    }
                }
            }
        }
    }

    let text = Text::new(13.0);
    let step = nice_step((x1 - x0).max(y1 - y0));
    let sx = |x: f64| ((x - x0) / upp) as f32;
    let sy = |y: f64| ((y1 - y) / upp) as f32;
    let (fw, fh) = (wpx as f32, hpx as f32);
    let major = |v: f64| v == 0.0 || v % (step * 4.0) == 0.0;
    let xs: Vec<f64> = (0..).map(|i| (x0 / step).ceil() * step + i as f64 * step).take_while(|&v| v < x1).collect();
    let ys: Vec<f64> = (0..).map(|i| (y0 / step).ceil() * step + i as f64 * step).take_while(|&v| v < y1).collect();
    for &gx in &xs {
        let (c, a) = if major(gx) { ([255, 230, 0], 200) } else { ([255, 255, 255], 70) };
        line(&mut pm, (sx(gx), 0.0), (sx(gx), fh), c, a, 1.0);
    }
    for &gy in &ys {
        let (c, a) = if major(gy) { ([255, 230, 0], 200) } else { ([255, 255, 255], 70) };
        line(&mut pm, (0.0, sy(gy)), (fw, sy(gy)), c, a, 1.0);
    }
    for &gx in &xs {
        let t = format!("x {gx:.0}");
        let (tw, th) = text.size(&t);
        rect(&mut pm, sx(gx) + 2.0, 2.0, sx(gx) + 6.0 + tw, 6.0 + th, [0, 0, 0], 180);
        text.draw(&mut pm, sx(gx) + 4.0, 3.0, &t, [255, 255, 255]);
    }
    for &gy in &ys {
        let t = format!("y {gy:.0}");
        let (tw, th) = text.size(&t);
        rect(&mut pm, 2.0, sy(gy) + 2.0, 6.0 + tw, sy(gy) + 6.0 + th, [0, 0, 0], 180);
        text.draw(&mut pm, 4.0, sy(gy) + 3.0, &t, [255, 255, 255]);
    }

    for m in entity_marks(bsp) {
        let (px, py) = (sx(m.pos.x), sy(m.pos.y));
        if let Some(b) = m.bbox {
            outline(&mut pm, sx(b[0]), sy(b[3]), sx(b[2]), sy(b[1]), m.color, 2.0);
            text.draw(&mut pm, px - 14.0, py - 7.0, m.label, m.color);
        } else if let Some(c) = PathBuilder::from_circle(px, py, 4.0) {
            pm.fill_path(&c, &paint(m.color, 255), tiny_skia::FillRule::Winding, Transform::identity(), None);
            pm.stroke_path(&c, &paint([0, 0, 0], 255), &Stroke::default(), Transform::identity(), None);
        }
    }

    if cuts.clip != NO_CLIP {
        outline(&mut pm, sx(cx0.max(x0)), sy(cy1.min(y1)), sx(cx1.min(x1)), sy(cy0.max(y0)), [255, 60, 60], 2.0);
    }

    let legend: [(&str, [u8; 3]); 4] = [("CT", CT), ("T", T), ("hostage", HOSTAGE), ("removed", REMOVED)];
    let (lx, ly) = (fw - 110.0, fh - 18.0 * legend.len() as f32 - 8.0);
    rect(&mut pm, lx - 6.0, ly - 4.0, fw - 4.0, fh - 4.0, [0, 0, 0], 180);
    for (i, (t, c)) in legend.iter().enumerate() {
        let yy = ly + i as f32 * 18.0;
        rect(&mut pm, lx, yy + 3.0, lx + 10.0, yy + 13.0, *c, 255);
        text.draw(&mut pm, lx + 16.0, yy, t, [255, 255, 255]);
    }

    let rgb: Vec<u8> = pm.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    image::RgbImage::from_raw(wpx, hpx, rgb).context("image")?.save(path)?;
    Ok(())
}
