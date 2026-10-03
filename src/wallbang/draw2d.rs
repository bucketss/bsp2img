use anyhow::{Context, Result};
use tiny_skia::{Pixmap, Transform};

use super::shots::Shot;
use crate::bsp::Bsp;
use crate::grid::{CT, GridFrame, T, Text, line, paint, rect};

pub const GREEN: [u8; 3] = [70, 230, 80];
pub const RED: [u8; 3] = [235, 55, 45];
const LINE_GREEN: ([u8; 3], f32) = ([60, 240, 70], 70.0 / 255.0);
const LINE_RED: ([u8; 3], f32) = ([240, 50, 40], 40.0 / 255.0);
const FAINT: f32 = 0.25;
const ALPHA_CUT: f64 = 0.12;
const ALPHA_MAX: f64 = 0.8;
const REF_PCT: f64 = 0.6;

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub m: [f64; 6],
    pub upp: f64,
}

impl Frame {
    pub fn from_grid(g: &GridFrame) -> Frame {
        Frame { w: g.wpx, h: g.hpx, m: [1.0 / g.upp, 0.0, -g.x0 / g.upp, 0.0, -1.0 / g.upp, g.y1 / g.upp], upp: g.upp }
    }

    pub fn px(&self, x: f64, y: f64) -> (f32, f32) {
        let m = &self.m;
        ((m[0] * x + m[1] * y + m[2]) as f32, (m[3] * x + m[4] * y + m[5]) as f32)
    }
}

pub struct Layer<'a> {
    pub main: Vec<&'a Shot>,
    pub faint: Vec<&'a Shot>,
}

fn density(f: &Frame, shots: &[&Shot], made: bool, weight: f32, acc: &mut [f32]) {
    let (w, h) = (f.w as i64, f.h as i64);
    for s in shots.iter().filter(|s| s.made() == made) {
        let (a, b) = (s.start(), s.end());
        let (ax, ay) = f.px(a.x, a.y);
        let (bx, by) = f.px(b.x, b.y);
        let n = ((bx - ax).hypot(by - ay).ceil() as i64).max(1) + 1;
        for i in 0..n {
            let t = i as f32 / (n - 1).max(1) as f32;
            let x = (ax + t * (bx - ax)).round() as i64;
            let y = (ay + t * (by - ay)).round() as i64;
            if x >= 0 && y >= 0 && x < w && y < h {
                acc[(y * w + x) as usize] += weight;
            }
        }
    }
}

fn percentile(v: &[f32], q: f64) -> f64 {
    let mut nz: Vec<f32> = v.iter().copied().filter(|&x| x > 1e-3).collect();
    if nz.is_empty() {
        return 1.0;
    }
    nz.sort_by(f32::total_cmp);
    let pos = (nz.len() - 1) as f64 * q;
    let (i, fr) = (pos.floor() as usize, pos - pos.floor());
    let a = nz[i] as f64;
    let b = nz[(i + 1).min(nz.len() - 1)] as f64;
    a + (b - a) * fr
}

pub fn areas(base: &[u8], f: &Frame, layer: &Layer, sigma: f64) -> Vec<u8> {
    let n = (f.w * f.h) as usize;
    let mut out = base.to_vec();
    for (made, col) in [(false, RED), (true, GREEN)] {
        let mut acc = vec![0f32; n];
        density(f, &layer.main, made, 1.0, &mut acc);
        density(f, &layer.faint, made, FAINT, &mut acc);
        if sigma > 0.0 {
            crate::grid::blur(&mut acc, f.w as usize, f.h as usize, sigma);
        }
        let r = percentile(&acc, REF_PCT);
        for (i, &m) in acc.iter().enumerate() {
            let mut a = (m as f64 / r).clamp(0.0, 1.0).sqrt();
            if a < ALPHA_CUT {
                continue;
            }
            a *= ALPHA_MAX;
            let p = &mut out[i * 4..i * 4 + 3];
            for k in 0..3 {
                p[k] = (p[k] as f64 * (1.0 - a) + col[k] as f64 * a).round() as u8;
            }
        }
    }
    out
}

fn raster(mask: &mut [bool], w: i64, h: i64, a: (f32, f32), b: (f32, f32)) {
    let (mut x0, mut y0) = (a.0.round() as i64, a.1.round() as i64);
    let (x1, y1) = (b.0.round() as i64, b.1.round() as i64);
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    loop {
        if x0 >= 0 && y0 >= 0 && x0 < w && y0 < h {
            mask[(y0 * w + x0) as usize] = true;
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

pub fn lines(px: &mut [u8], f: &Frame, layer: &Layer) {
    let (w, h) = (f.w as i64, f.h as i64);
    for (made, (col, alpha)) in [(false, LINE_RED), (true, LINE_GREEN)] {
        for (set, k) in [(&layer.faint, FAINT), (&layer.main, 1.0)] {
            let mut mask = vec![false; (w * h) as usize];
            for s in set.iter().filter(|s| s.made() == made) {
                let (a, b) = (s.start(), s.end());
                raster(&mut mask, w, h, f.px(a.x, a.y), f.px(b.x, b.y));
            }
            let a = (alpha * k) as f64;
            for (i, _) in mask.iter().enumerate().filter(|m| *m.1) {
                let p = &mut px[i * 4..i * 4 + 3];
                for c in 0..3 {
                    p[c] = (p[c] as f64 * (1.0 - a) + col[c] as f64 * a).round() as u8;
                }
            }
        }
    }
}

pub fn markers(pm: &mut Pixmap, bsp: &Bsp, f: &Frame) {
    for e in &bsp.entities {
        let col = match e.class() {
            "info_player_deathmatch" => T,
            "info_player_start" => CT,
            _ => continue,
        };
        let Some(p) = e.origin() else { continue };
        let (x, y) = f.px(p.x, p.y);
        if let Some(c) = tiny_skia::PathBuilder::from_circle(x, y, 3.5) {
            pm.fill_path(&c, &paint(col, 255), tiny_skia::FillRule::Winding, Transform::identity(), None);
            pm.stroke_path(&c, &paint([0, 0, 0], 255), &tiny_skia::Stroke::default(), Transform::identity(), None);
        }
    }
}

pub fn title(pm: &mut Pixmap, s: &str) {
    let text = Text::new(14.0);
    let (tw, th) = text.size(s);
    rect(pm, 4.0, 4.0, tw + 14.0, th + 12.0, [0, 0, 0], 200);
    text.draw(pm, 9.0, 8.0, s, [255, 255, 255]);
}

pub fn legend(pm: &mut Pixmap, rows: &[([u8; 3], String)]) {
    let small = Text::new(12.0);
    let (fw, fh) = (pm.width() as f32, pm.height() as f32);
    let lw = rows.iter().map(|(_, s)| small.size(s).0).fold(0.0, f32::max) + 28.0;
    let lh = rows.len() as f32 * 20.0;
    let (lx, ly) = (fw - lw - 8.0, fh - lh - 8.0);
    rect(pm, lx - 8.0, ly - 6.0, fw - 4.0, fh - 4.0, [0, 0, 0], 200);
    for (k, (c, s)) in rows.iter().enumerate() {
        let yy = ly + k as f32 * 20.0;
        rect(pm, lx, yy + 3.0, lx + 12.0, yy + 15.0, *c, 255);
        small.draw(pm, lx + 20.0, yy + 1.0, s, [255, 255, 255]);
    }
}

pub fn scale_bar(pm: &mut Pixmap, f: &Frame) {
    let small = Text::new(12.0);
    let want = 160.0 * f.upp;
    let mag = 10f64.powf(want.log10().floor());
    let step = [5.0, 2.0, 1.0].iter().map(|m| m * mag).find(|&s| s <= want).unwrap_or(mag);
    let len = (step / f.upp) as f32;
    let (x, y) = (16.0, pm.height() as f32 - 22.0);
    rect(pm, x - 8.0, y - 22.0, x + len + 40.0, y + 12.0, [0, 0, 0], 200);
    line(pm, (x, y), (x + len, y), [255, 255, 255], 255, 2.0);
    line(pm, (x, y - 5.0), (x, y + 5.0), [255, 255, 255], 255, 2.0);
    line(pm, (x + len, y - 5.0), (x + len, y + 5.0), [255, 255, 255], 255, 2.0);
    small.draw(pm, x, y - 20.0, &format!("{} units", crate::scene::num(step)), [255, 255, 255]);
}

pub fn save_png(pm: &Pixmap, path: &std::path::Path) -> Result<()> {
    let rgb: Vec<u8> = pm.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    image::RgbImage::from_raw(pm.width(), pm.height(), rgb).context("image")?.save(path)?;
    Ok(())
}
