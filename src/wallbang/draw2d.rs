use anyhow::{Context, Result};
use tiny_skia::{Pixmap, Transform};

use super::shots::{Kind, Shot};
use crate::bsp::Bsp;
use crate::grid::{CT, GridFrame, T, Text, line, paint, rect};

pub const GREEN: [u8; 3] = [70, 230, 80];
pub const RED: [u8; 3] = [235, 55, 45];
pub const YELLOW: [u8; 3] = [240, 200, 50];
const LINE_GREEN: ([u8; 3], f32) = ([60, 240, 70], 70.0 / 255.0);
const LINE_RED: ([u8; 3], f32) = ([240, 50, 40], 40.0 / 255.0);
const LINE_YELLOW: ([u8; 3], f32) = ([245, 205, 50], 60.0 / 255.0);
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
    pub damage: bool,
}

impl Layer<'_> {
    fn kinds(&self) -> impl Iterator<Item = Kind> + '_ {
        Kind::ALL.into_iter().filter(|&k| self.damage || k != Kind::Weak)
    }
}

pub fn color(k: Kind) -> [u8; 3] {
    match k {
        Kind::Died => RED,
        Kind::Weak => YELLOW,
        Kind::Made => GREEN,
    }
}

fn line_color(k: Kind) -> ([u8; 3], f32) {
    match k {
        Kind::Died => LINE_RED,
        Kind::Weak => LINE_YELLOW,
        Kind::Made => LINE_GREEN,
    }
}

fn plot(f: &Frame, s: &Shot, weight: f32, acc: &mut [f32]) {
    let (w, h) = (f.w as i64, f.h as i64);
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

fn densities(f: &Frame, layer: &Layer) -> Vec<(Kind, Vec<f32>)> {
    let mut out: Vec<(Kind, Vec<f32>)> = layer.kinds().map(|k| (k, vec![0f32; (f.w * f.h) as usize])).collect();
    for (set, weight) in [(&layer.main, 1.0), (&layer.faint, FAINT)] {
        for s in set.iter() {
            let k = s.kind(layer.damage);
            if let Some((_, acc)) = out.iter_mut().find(|(x, _)| *x == k) {
                plot(f, s, weight, acc);
            }
        }
    }
    out
}

fn percentile(v: &[f32], q: f64) -> f64 {
    let mut nz: Vec<f32> = v.iter().copied().filter(|&x| x > 1e-3).collect();
    if nz.is_empty() {
        return 1.0;
    }
    let pos = (nz.len() - 1) as f64 * q;
    let (i, fr) = (pos.floor() as usize, pos - pos.floor());
    let (_, a, rest) = nz.select_nth_unstable_by(i, f32::total_cmp);
    let a = *a as f64;
    let b = rest.iter().copied().min_by(f32::total_cmp).map_or(a, f64::from);
    a + (b - a) * fr
}

fn area_alpha(f: &Frame, mut acc: Vec<f32>, sigma: f64) -> Vec<f64> {
    if sigma > 0.0 {
        crate::grid::blur(&mut acc, f.w as usize, f.h as usize, sigma);
    }
    let r = percentile(&acc, REF_PCT);
    acc.iter()
        .map(|&m| {
            let a = (m as f64 / r).clamp(0.0, 1.0).sqrt();
            if a < ALPHA_CUT { 0.0 } else { a * ALPHA_MAX }
        })
        .collect()
}

fn mix(p: &mut [u8], col: [u8; 3], a: f64) {
    for k in 0..3 {
        p[k] = (p[k] as f64 * (1.0 - a) + col[k] as f64 * a).round() as u8;
    }
}

fn blend(out: &mut [u8], col: [u8; 3], alpha: &[f64]) {
    for (i, &a) in alpha.iter().enumerate() {
        if a > 0.0 {
            mix(&mut out[i * 4..i * 4 + 3], col, a);
        }
    }
}

fn mix_over(p: &mut [f64], col: [u8; 3], a: f64) {
    for k in 0..3 {
        p[k] = col[k] as f64 * a + p[k] * (1.0 - a);
    }
    p[3] = a + p[3] * (1.0 - a);
}

fn over(acc: &mut [f64], col: [u8; 3], alpha: &[f64]) {
    for (i, &a) in alpha.iter().enumerate() {
        if a > 0.0 {
            mix_over(&mut acc[i * 4..i * 4 + 4], col, a);
        }
    }
}

fn unpremultiply(acc: &[f64]) -> Vec<u8> {
    acc.chunks_exact(4)
        .flat_map(|p| {
            let a = p[3];
            let c = |v: f64| if a > 0.0 { (v / a).round().clamp(0.0, 255.0) as u8 } else { 0 };
            [c(p[0]), c(p[1]), c(p[2]), (a * 255.0).round() as u8]
        })
        .collect()
}

pub fn areas(base: &[u8], f: &Frame, layer: &Layer, sigma: f64) -> Vec<u8> {
    let mut out = base.to_vec();
    for (k, acc) in densities(f, layer) {
        blend(&mut out, color(k), &area_alpha(f, acc, sigma));
    }
    out
}

pub fn areas_rgba(f: &Frame, layer: &Layer, sigma: f64) -> Vec<u8> {
    let mut acc = vec![0f64; (f.w * f.h) as usize * 4];
    for (k, d) in densities(f, layer) {
        over(&mut acc, color(k), &area_alpha(f, d, sigma));
    }
    unpremultiply(&acc)
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

fn line_masks(f: &Frame, layer: &Layer, mut each: impl FnMut([u8; 3], f64, &[bool])) {
    let (w, h) = (f.w as i64, f.h as i64);
    let mut mask = vec![false; (w * h) as usize];
    for kind in layer.kinds() {
        let (col, alpha) = line_color(kind);
        for (set, k) in [(&layer.faint, FAINT), (&layer.main, 1.0)] {
            mask.fill(false);
            for s in set.iter().filter(|s| s.kind(layer.damage) == kind) {
                let (a, b) = (s.start(), s.end());
                raster(&mut mask, w, h, f.px(a.x, a.y), f.px(b.x, b.y));
            }
            each(col, (alpha * k) as f64, &mask);
        }
    }
}

pub fn lines(px: &mut [u8], f: &Frame, layer: &Layer) {
    line_masks(f, layer, |col, a, mask| {
        for (i, _) in mask.iter().enumerate().filter(|m| *m.1) {
            mix(&mut px[i * 4..i * 4 + 3], col, a);
        }
    });
}

pub fn lines_rgba(f: &Frame, layer: &Layer) -> Vec<u8> {
    let mut acc = vec![0f64; (f.w * f.h) as usize * 4];
    line_masks(f, layer, |col, a, mask| {
        for (i, _) in mask.iter().enumerate().filter(|m| *m.1) {
            mix_over(&mut acc[i * 4..i * 4 + 4], col, a);
        }
    });
    unpremultiply(&acc)
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

#[cfg(test)]
mod tests {
    #[test]
    fn percentile_matches_sort() {
        let mut x = 0x2545_f491u32;
        for n in [0usize, 1, 2, 3, 10, 257, 4000] {
            let v: Vec<f32> = (0..n)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    if x % 4 == 0 { 0.0 } else { (x % 1000) as f32 / 7.0 }
                })
                .collect();
            for q in [0.0, 0.3, 0.6, 1.0] {
                let mut nz: Vec<f32> = v.iter().copied().filter(|&x| x > 1e-3).collect();
                nz.sort_by(f32::total_cmp);
                let want = if nz.is_empty() {
                    1.0
                } else {
                    let pos = (nz.len() - 1) as f64 * q;
                    let (i, fr) = (pos.floor() as usize, pos - pos.floor());
                    let (a, b) = (nz[i] as f64, nz[(i + 1).min(nz.len() - 1)] as f64);
                    a + (b - a) * fr
                };
                assert_eq!(super::percentile(&v, q).to_bits(), want.to_bits());
            }
        }
    }
}
