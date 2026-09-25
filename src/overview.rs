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
    let (w, h) = img.dimensions();
    let mut out = vec![0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(&[24, 0]);
    let px = |x: u32, y: u32| -> [u8; 3] {
        let p = img.get_pixel(x, y);
        if p[3] < 128 { [KEY[2], KEY[1], KEY[0]] } else { [p[2], p[1], p[0]] }
    };
    for y in (0..h).rev() {
        let row: Vec<[u8; 3]> = (0..w).map(|x| px(x, y)).collect();
        let mut i = 0;
        while i < row.len() {
            let mut run = 1;
            while i + run < row.len() && run < 128 && row[i + run] == row[i] {
                run += 1;
            }
            if run > 1 {
                out.push(0x80 | (run as u8 - 1));
                out.extend_from_slice(&row[i]);
                i += run;
                continue;
            }
            let mut n = 1;
            while i + n < row.len() && n < 128 && (i + n + 1 >= row.len() || row[i + n] != row[i + n + 1]) {
                n += 1;
            }
            out.push(n as u8 - 1);
            for p in &row[i..i + n] {
                out.extend_from_slice(p);
            }
            i += n;
        }
    }
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(b"TRUEVISION-XFILE.\0");
    out
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
