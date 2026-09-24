use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use glam::DVec3;
use rayon::prelude::*;

use crate::camera::camera_basis;
use crate::look::Look;
use crate::paths::{Partial, free_name};
use crate::quant::{median_cut, nearest};
use crate::render::{Cuts, Renderer, View, composite_bg};
use crate::scene::Report;

pub const DEFAULT_BG: [u8; 3] = [0x20, 0x20, 0x20];
const PAD: u32 = 16;
const PALETTE_SAMPLES: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct SpinOpts {
    pub size: u32,
    pub ss: u32,
    pub pitch: f64,
    pub seconds: f64,
    pub fps: f64,
    pub start: f64,
    pub ccw: bool,
    pub gif: bool,
    pub mp4: bool,
    pub apng: bool,
    pub look: Look,
}

impl Default for SpinOpts {
    fn default() -> Self {
        SpinOpts {
            size: 720,
            ss: 3,
            pitch: 35.264,
            seconds: 12.0,
            fps: 60.0,
            start: 45.0,
            ccw: false,
            gif: false,
            mp4: true,
            apng: false,
            look: Look::default(),
        }
    }
}

impl SpinOpts {
    pub fn frames(&self) -> u32 {
        (self.seconds * self.fps).round().max(1.0) as u32
    }

    pub fn yaws(&self) -> Vec<f64> {
        let n = self.frames();
        let dir = if self.ccw { -1.0 } else { 1.0 };
        (0..n).map(|i| self.start + dir * 360.0 * i as f64 / n as f64).collect()
    }
}

pub fn spin_views(r: &Renderer, cuts: &Cuts, yaws: &[f64], pitch: f64, size: u32) -> (Vec<View>, u32, u32) {
    let pts = r.points_in(cuts);
    let (lo, hi) = pts.iter().fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(a, b), p| (a.min(*p), b.max(*p)));
    let pivot = (lo + hi) / 2.0;
    let bases: Vec<_> = yaws.iter().map(|&y| camera_basis(y, pitch)).collect();
    let (mut r0, mut r1, mut u0, mut u1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
    for b in &bases {
        for p in &pts {
            let d = *p - pivot;
            let (x, y) = (d.dot(b.r), d.dot(b.u));
            r0 = r0.min(x);
            r1 = r1.max(x);
            u0 = u0.min(y);
            u1 = u1.max(y);
        }
    }
    let upp = (r1 - r0).max(u1 - u0) / (size as f64 - 2.0 * PAD as f64).max(1.0);
    let even = |v: u32| v + v % 2;
    let wpx = even(((r1 - r0) / upp).ceil() as u32 + 2 * PAD);
    let hpx = even(((u1 - u0) / upp).ceil() as u32 + 2 * PAD);
    let views = bases
        .iter()
        .zip(yaws)
        .map(|(b, &yaw)| View {
            basis: *b,
            cx: pivot.dot(b.r) + (r0 + r1) / 2.0,
            cy: pivot.dot(b.u) + (u0 + u1) / 2.0,
            w: wpx as f64 * upp,
            h: hpx as f64 * upp,
            sky_yaw: Some(yaw),
        })
        .collect();
    (views, wpx, hpx)
}

fn spin_radius(r: &Renderer, cuts: &Cuts, v: &View, wpx: u32) -> f64 {
    let pts = r.points_in(cuts);
    let (lo, hi) = pts.iter().fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(a, b), p| (a.min(*p), b.max(*p)));
    let c = (lo + hi) / 2.0;
    let rad = pts.iter().map(|p| (p.x - c.x).hypot(p.y - c.y)).fold(0.0, f64::max);
    rad * wpx as f64 / v.w
}

struct GifOut {
    enc: gif::Encoder<std::io::BufWriter<std::fs::File>>,
    lut: Vec<u8>,
}

impl GifOut {
    fn new(path: &Path, w: u32, h: u32, samples: &[image::RgbaImage]) -> Result<GifOut> {
        let mut hist: HashMap<[u8; 3], u32> = HashMap::new();
        for img in samples {
            for p in img.pixels() {
                *hist.entry([p[0], p[1], p[2]]).or_default() += 1;
            }
        }
        let pal = median_cut(hist, 256);
        let pal_i: Vec<[i32; 3]> = pal.iter().map(|c| [c[0] as i32, c[1] as i32, c[2] as i32]).collect();
        let lut: Vec<u8> = (0..1usize << 18)
            .into_par_iter()
            .map(|i| {
                let c = [(i >> 12) & 63, (i >> 6) & 63, i & 63].map(|v| (v * 4 + 2) as i32);
                nearest(&pal_i, c) as u8
            })
            .collect();
        let flat: Vec<u8> = pal.iter().flatten().copied().collect();
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut enc = gif::Encoder::new(file, w as u16, h as u16, &flat)?;
        enc.set_repeat(gif::Repeat::Infinite)?;
        Ok(GifOut { enc, lut })
    }

    fn frame(&mut self, img: &image::RgbaImage, delay: u16) -> Result<()> {
        let idx: Vec<u8> = img
            .as_raw()
            .par_chunks_exact(4)
            .map(|p| self.lut[((p[0] as usize >> 2) << 12) | ((p[1] as usize >> 2) << 6) | (p[2] as usize >> 2)])
            .collect();
        let f = gif::Frame {
            width: img.width() as u16,
            height: img.height() as u16,
            delay,
            buffer: Cow::Owned(idx),
            ..Default::default()
        };
        self.enc.write_frame(&f)?;
        Ok(())
    }
}

type ApngOut = png::Writer<std::io::BufWriter<std::fs::File>>;

fn apng_writer(path: &Path, w: u32, h: u32, frames: u32, fps: f64) -> Result<ApngOut> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Balanced);
    enc.set_animated(frames, 0)?;
    enc.set_frame_delay(100, (fps * 100.0).round().clamp(1.0, 65535.0) as u16)?;
    Ok(enc.write_header()?)
}

struct Ffmpeg(Option<Child>);

impl Ffmpeg {
    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.0.as_mut().unwrap().stdin.as_mut().unwrap().write_all(data)
    }

    fn finish(mut self) -> Result<()> {
        finish_ffmpeg(self.0.take().unwrap())
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn spawn_ffmpeg(path: &Path, w: u32, h: u32, fps: f64) -> std::io::Result<Child> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostats", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{w}x{h}"))
        .arg("-r")
        .arg(format!("{fps}"))
        .args(["-i", "-", "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p"])
        .args(["-movflags", "+faststart"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn()
}

fn finish_ffmpeg(mut child: Child) -> Result<()> {
    drop(child.stdin.take());
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let st = child.wait()?;
    if !st.success() {
        bail!("ffmpeg failed: {}", err.trim());
    }
    Ok(())
}

pub fn export_spin(
    r: &mut Renderer,
    name: &str,
    sky_tag: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &SpinOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<Vec<PathBuf>> {
    if !o.gif && !o.mp4 && !o.apng {
        bail!("nothing to write: GIF, MP4 and APNG are all off");
    }
    let mut o = o.clone();
    if o.gif {
        let cs = (100.0 / o.fps).round().max(2.0);
        let fps = 100.0 / cs;
        if (fps - o.fps).abs() > 1e-6 {
            rep.log(format!("  GIF frame delays are whole centiseconds: using {fps:.2} fps"));
            o.fps = fps;
        }
    }
    let o = &o;
    let yaws = o.yaws();
    let (views, w, h) = spin_views(r, cuts, &yaws, o.pitch, o.size);
    let (cull, bg) = (o.look.cull, o.look.bg);
    let flat = |mut img: image::RgbaImage| {
        if bg.is_none() {
            composite_bg(&mut img, DEFAULT_BG);
        }
        img
    };
    let stem = format!("{name}{sky_tag}{cut_tag}_spin");
    let mut files = Partial::default();

    let mut ff = None;
    if o.mp4 {
        let f = free_name(out, &stem, ".mp4");
        match spawn_ffmpeg(&f, w, h, o.fps) {
            Ok(c) => {
                ff = Some(Ffmpeg(Some(c)));
                files.add(f);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !o.gif && !o.apng {
                    bail!("ffmpeg not found on PATH; install it or use --gif / --apng");
                }
                rep.log("  ffmpeg not found on PATH, skipping MP4".into())
            }
            Err(e) => return Err(e).context("starting ffmpeg"),
        }
    }

    let n = views.len();
    let total = (n + if o.gif { PALETTE_SAMPLES.min(n) } else { 0 }) as f32;
    let mut done = 0;
    let mut cache: HashMap<usize, image::RgbaImage> = HashMap::new();
    let mut gif = None;
    if o.gif {
        let picks: Vec<usize> = (0..PALETTE_SAMPLES.min(n)).map(|k| k * n / PALETTE_SAMPLES.min(n)).collect();
        for &i in &picks {
            rep.step(done as f32 / total)?;
            cache.insert(i, r.render_view(&views[i], w, h, o.ss, cuts, cull, bg)?);
            done += 1;
        }
        let samples: Vec<image::RgbaImage> = picks.iter().map(|i| flat(cache[i].clone())).collect();
        let f = free_name(out, &stem, ".gif");
        files.add(f.clone());
        gif = Some(GifOut::new(&f, w, h, &samples)?);
    }
    let mut apng = None;
    if o.apng {
        let f = free_name(out, &stem, ".png");
        files.add(f.clone());
        apng = Some(apng_writer(&f, w, h, n as u32, o.fps)?);
    }
    if gif.is_none() && ff.is_none() && apng.is_none() {
        return Ok(files.keep());
    }

    let delay = (100.0 / o.fps).round() as u16;
    for (i, view) in views.iter().enumerate() {
        rep.step(done as f32 / total)?;
        let img = match cache.remove(&i) {
            Some(img) => img,
            None => r.render_view(view, w, h, o.ss, cuts, cull, bg)?,
        };
        done += 1;
        if let Some(a) = &mut apng {
            a.write_image_data(img.as_raw())?;
        }
        if gif.is_none() && ff.is_none() {
            continue;
        }
        let img = flat(img);
        if let Some(g) = &mut gif {
            g.frame(&img, delay)?;
        }
        if let Some(c) = &mut ff {
            if let Err(e) = c.write(img.as_raw()) {
                ff.take().unwrap().finish()?;
                return Err(e).context("writing to ffmpeg");
            }
        }
    }
    drop(gif);
    if let Some(a) = apng {
        a.finish()?;
    }
    if let Some(c) = ff {
        c.finish()?;
    }
    let files = files.keep();
    let _ = rep.step(1.0);
    let step = 360.0 / n as f64;
    let edge = spin_radius(r, cuts, &views[0], w) * step.to_radians();
    rep.log(format!("  {n} frames at {:.2} fps, {step:.2} deg/frame, edge moves ~{edge:.1} px/frame", o.fps));
    for f in &files {
        rep.log(format!("  {} {}x{}", f.display(), w, h));
    }
    Ok(files)
}
