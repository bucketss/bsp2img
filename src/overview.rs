use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use glam::DVec3;

use crate::camera::top_down;
use crate::quant::quantize;
use crate::render::{Cuts, Renderer, View};

pub const IMG_W: u32 = 1024;
pub const IMG_H: u32 = 768;
pub const KEY: [u8; 3] = [0, 255, 0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overview {
    pub zoom: f64,
    pub origin: [f64; 3],
    pub rotated: i32,
    pub height: f64,
}

pub fn read_txt(path: &Path) -> Result<Overview> {
    let text: String = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?
        .iter()
        .map(|&c| c as char)
        .collect();
    let toks: Vec<&str> = text.split_whitespace().collect();
    let after = |key: &str, n: usize| -> Option<Vec<f64>> {
        let i = toks.iter().position(|t| t.eq_ignore_ascii_case(key))?;
        toks.get(i + 1..i + 1 + n)?.iter().map(|t| t.parse().ok()).collect()
    };
    let zoom = after("ZOOM", 1).context("ZOOM missing")?[0];
    let o = after("ORIGIN", 3).context("ORIGIN missing")?;
    let rotated = after("ROTATED", 1).context("ROTATED missing")?[0] as i32;
    let height = after("HEIGHT", 1).map(|v| v[0]).unwrap_or(o[2]);
    Ok(Overview { zoom, origin: [o[0], o[1], o[2]], rotated, height })
}

pub fn txt(name: &str, ov: &Overview) -> String {
    let lines = [
        format!("// overview description file for {name}.bsp"),
        String::new(),
        "global ".into(),
        "{".into(),
        format!("\tZOOM\t{:.2}", ov.zoom),
        format!("\tORIGIN\t{:.0}\t{:.0}\t{:.0}", ov.origin[0], ov.origin[1], ov.origin[2]),
        format!("\tROTATED\t{}", ov.rotated),
        "}".into(),
        String::new(),
        "layer ".into(),
        "{".into(),
        format!("\tIMAGE\t\"overviews/{name}.bmp\""),
        format!("\tHEIGHT\t{:.0}", ov.height),
        "}".into(),
    ];
    lines.iter().map(|l| format!("{l}\r\n")).collect()
}

pub fn axes(rotated: i32) -> (DVec3, DVec3) {
    if rotated != 0 {
        (DVec3::X, DVec3::Y)
    } else {
        (DVec3::NEG_Y, DVec3::X)
    }
}

pub fn fit(points: &[DVec3], margin: f64) -> Overview {
    let lo = points.iter().copied().fold(DVec3::splat(f64::INFINITY), DVec3::min);
    let hi = points.iter().copied().fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max);
    let size = hi - lo;
    let rotated = if size.x >= size.y { 1 } else { 0 };
    let (r, u) = axes(rotated);
    let w = size.dot(r).abs() * (1.0 + margin);
    let h = size.dot(u).abs() * (1.0 + margin);
    let zoom = ((8192.0 / w).min(6144.0 / h) * 100.0).floor() / 100.0;
    let c = (lo + hi) / 2.0;
    let z = lo.z.round_ties_even();
    Overview { zoom, origin: [c.x.round_ties_even(), c.y.round_ties_even(), z], rotated, height: z }
}

pub fn view(ov: &Overview) -> View {
    let (r, u) = axes(ov.rotated);
    let o = DVec3::from_array(ov.origin);
    View { basis: top_down(r, u), cx: o.dot(r), cy: o.dot(u), w: 8192.0 / ov.zoom, h: 6144.0 / ov.zoom, sky_yaw: None, persp: None }
}

pub fn render(r: &mut Renderer, ov: &Overview, ss: u32, cuts: &Cuts, cull: bool) -> Result<image::RgbaImage> {
    r.render_view(&view(ov), IMG_W, IMG_H, ss, cuts, &crate::look::Look::plain(cull, None), 0.0)
}

pub fn bmp_bytes(img: &image::RgbaImage) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let (pal, idx) = quantize(img, 254);
    let mut palette = [[0u8; 3]; 256];
    for (i, c) in pal.iter().enumerate() {
        palette[i] = *c;
    }
    palette[254] = KEY;
    let row = (w as usize).div_ceil(4) * 4;
    let img_size = row * h as usize;
    let off = 14 + 40 + 1024;
    let mut out = Vec::with_capacity(off + img_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((off + img_size) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(off as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(img_size as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&256u32.to_le_bytes());
    out.extend_from_slice(&256u32.to_le_bytes());
    for c in palette {
        out.extend_from_slice(&[c[2], c[1], c[0], 0]);
    }
    for y in (0..h as usize).rev() {
        for x in 0..w as usize {
            let a = img.get_pixel(x as u32, y as u32)[3];
            out.push(if a < 128 { 254 } else { idx[y * w as usize + x] });
        }
        out.extend(std::iter::repeat_n(0, row - w as usize));
    }
    out
}

pub fn tga_bytes(img: &image::RgbaImage) -> Vec<u8> {
    tga(img, false)
}

pub fn tga_rgba_bytes(img: &image::RgbaImage) -> Vec<u8> {
    tga(img, true)
}

fn tga(img: &image::RgbaImage, alpha: bool) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let mut out = vec![0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(if alpha { &[32, 8] } else { &[24, 0] });
    let n = if alpha { 4 } else { 3 };
    let px = |x: u32, y: u32| -> [u8; 4] {
        let p = img.get_pixel(x, y);
        if alpha {
            [p[2], p[1], p[0], p[3]]
        } else if p[3] < 128 {
            [KEY[2], KEY[1], KEY[0], 0]
        } else {
            [p[2], p[1], p[0], 0]
        }
    };
    for y in (0..h).rev() {
        let row: Vec<[u8; 4]> = (0..w).map(|x| px(x, y)).collect();
        let mut i = 0;
        while i < row.len() {
            let mut run = 1;
            while i + run < row.len() && run < 128 && row[i + run] == row[i] {
                run += 1;
            }
            if run > 1 {
                out.push(0x80 | (run as u8 - 1));
                out.extend_from_slice(&row[i][..n]);
                i += run;
                continue;
            }
            let mut k = 1;
            while i + k < row.len() && k < 128 && (i + k + 1 >= row.len() || row[i + k] != row[i + k + 1]) {
                k += 1;
            }
            out.push(k as u8 - 1);
            for p in &row[i..i + k] {
                out.extend_from_slice(&p[..n]);
            }
            i += k;
        }
    }
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(b"TRUEVISION-XFILE.\0");
    out
}

pub const RADAR: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radar {
    pub pos_x: f64,
    pub pos_y: f64,
    pub scale: f64,
}

impl Radar {
    pub fn size(self) -> f64 {
        self.scale * RADAR as f64
    }

    pub fn center(self) -> (f64, f64) {
        (self.pos_x + self.size() / 2.0, self.pos_y - self.size() / 2.0)
    }

    pub fn world_to_px(self, x: f64, y: f64, size: f64) -> (f64, f64) {
        let k = size / RADAR as f64;
        ((x - self.pos_x) / self.scale * k, (self.pos_y - y) / self.scale * k)
    }

    pub fn norm(self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.pos_x) / self.size(), (self.pos_y - y) / self.size())
    }

    pub fn world(self, u: f64, v: f64) -> (f64, f64) {
        (self.pos_x + u * self.size(), self.pos_y - v * self.size())
    }
}

pub fn fit_radar(points: &[DVec3], margin: f64) -> Radar {
    let lo = points.iter().copied().fold(DVec3::splat(f64::INFINITY), DVec3::min);
    let hi = points.iter().copied().fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max);
    let size = (hi.x - lo.x).max(hi.y - lo.y) * (1.0 + margin);
    let scale = (size / RADAR as f64 * 100.0).ceil() / 100.0;
    let half = scale * RADAR as f64 / 2.0;
    let c = (lo + hi) / 2.0;
    Radar { pos_x: (c.x - half).round(), pos_y: (c.y + half).round(), scale }
}

pub fn radar_view(r: &Radar) -> View {
    let (s, (cx, cy)) = (r.size(), r.center());
    View { basis: top_down(DVec3::X, DVec3::Y), cx, cy, w: s, h: s, sky_yaw: None, persp: None }
}

pub fn render_radar(r: &mut Renderer, frame: &Radar, ss: u32, cuts: &Cuts, cull: bool) -> Result<image::RgbaImage> {
    r.render_view(&radar_view(frame), RADAR, RADAR, ss, cuts, &crate::look::Look::plain(cull, None), 0.0)
}

pub fn radar_exts(png: bool, tga: bool) -> Vec<&'static str> {
    [(png, ".png"), (tga, ".tga")].into_iter().filter(|x| x.0).map(|x| x.1).collect()
}

pub fn save_radar(dir: &Path, stem: &str, img: &image::RgbaImage, png: bool, tga: bool) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for ext in radar_exts(png, tga) {
        let f = dir.join(format!("{stem}_radar{ext}"));
        match ext {
            ".png" => img.save(&f)?,
            _ => std::fs::write(&f, tga_rgba_bytes(img))?,
        }
        out.push(f);
    }
    Ok(out)
}

pub fn coverage(frame: &Radar, points: &[DVec3]) -> f64 {
    let s = frame.size();
    let inside = points.iter().filter(|p| p.x >= frame.pos_x && p.x <= frame.pos_x + s && p.y <= frame.pos_y && p.y >= frame.pos_y - s).count();
    inside as f64 / points.len().max(1) as f64
}

pub fn exts(png: bool) -> &'static [&'static str] {
    if png { &[".bmp", ".tga", ".txt", ".png"] } else { &[".bmp", ".tga", ".txt"] }
}

pub fn save(dir: &Path, name: &str, ov: &Overview, img: &image::RgbaImage, png: bool) -> Result<()> {
    std::fs::write(dir.join(format!("{name}.bmp")), bmp_bytes(img))?;
    std::fs::write(dir.join(format!("{name}.tga")), tga_bytes(img))?;
    std::fs::File::create(dir.join(format!("{name}.txt")))?.write_all(txt(name, ov).as_bytes())?;
    if png {
        img.save(dir.join(format!("{name}.png")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_keeps_index_255_free() {
        let img = image::RgbaImage::from_fn(70, 40, |x, y| {
            if x < 5 { image::Rgba([0, 0, 0, 0]) } else { image::Rgba([(x * 3) as u8, (y * 6) as u8, ((x * y) % 251) as u8, 255]) }
        });
        let b = bmp_bytes(&img);
        assert_eq!(b, bmp_bytes(&img));
        assert_eq!(&b[..2], b"BM");
        let off = u32::from_le_bytes(b[10..14].try_into().unwrap()) as usize;
        let pal = |i: usize| [b[54 + i * 4 + 2], b[54 + i * 4 + 1], b[54 + i * 4]];
        assert_eq!(pal(254), KEY);
        assert_eq!(pal(255), [0, 0, 0]);
        let row = 72;
        for y in 0..40 {
            let r = &b[off + y * row..off + y * row + 70];
            assert!(r.iter().all(|&i| i != 255));
            assert!(r[..5].iter().all(|&i| i == 254));
            assert!(r[5..].iter().all(|&i| i < 254));
        }
    }
}
