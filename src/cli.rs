use std::cell::Cell;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::light::LightParams;
use crate::export::{IsoOpts, OverviewOpts, export_iso, export_overview};
use crate::look::{FaceArgs, LookArgs};
use crate::paths::{resolve_map, run_dir};
use crate::render::Gpu;
use crate::scene::{CutOpts, LoadOpts, Report, Scene};
use crate::spin::{SpinOpts, export_spin};
use crate::timing::{TimingOpts, export_timing};

#[derive(Parser)]
#[command(name = "bsp2img", version, about = "Render GoldSrc BSP maps: isometric views and CS overviews. Run without arguments for the GUI.")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    #[command(about = "Render isometric diorama views")]
    Iso(IsoArgs),
    #[command(about = "Generate a CS overview (.bmp, .tga, .txt)")]
    Overview(OverviewArgs),
    #[command(about = "Render an animation of the map rotating (.mp4 via ffmpeg, optional .gif/.png)")]
    Spin(SpinArgs),
    #[command(about = "Map how fast each team reaches every spot from its spawns")]
    Timing(TimingArgs),
    #[command(about = "Open the GUI")]
    Gui(GuiArgs),
}

#[derive(Args, Clone)]
pub struct Common {
    #[arg(required = true, help = "path to .bsp, or map name with --game")]
    pub maps: Vec<String>,
    #[arg(long, help = "game install root or mod directory")]
    pub game: Option<PathBuf>,
    #[arg(long = "wad-dir", help = "extra directory to search for WADs")]
    pub wad_dir: Vec<PathBuf>,
    #[arg(short, long, default_value = "renders", help = "base folder; each run goes in a new <out>/<map>NN/")]
    pub out: PathBuf,
    #[arg(long, default_value_t = 0, help = "remove the N highest roof levels (0 = keep all)")]
    pub roofs: usize,
    #[arg(long, allow_negative_numbers = true)]
    pub zmin: Option<f64>,
    #[arg(long, allow_negative_numbers = true)]
    pub zmax: Option<f64>,
    #[arg(long, num_args = 4, allow_negative_numbers = true, value_names = ["X0", "Y0", "X1", "Y1"],
          help = "world XY area to keep; disables auto crop")]
    pub crop: Option<Vec<f64>>,
    #[arg(long, allow_negative_numbers = true)]
    pub xmin: Option<f64>,
    #[arg(long, allow_negative_numbers = true)]
    pub xmax: Option<f64>,
    #[arg(long, allow_negative_numbers = true)]
    pub ymin: Option<f64>,
    #[arg(long, allow_negative_numbers = true)]
    pub ymax: Option<f64>,
    #[arg(long, num_args = 0..=1, default_missing_value = "3", value_parser = ["1", "3"],
          help = "mask out areas players cannot walk to, using collision hull 1 (standing) or 3 (crouch, default)")]
    pub hull: Option<String>,
    #[arg(long = "hull-pad", default_value_t = 64.0, help = "units kept around the walkable area")]
    pub hull_pad: f64,
    #[arg(long, help = "write a top-down <map>_grid.png with world coordinates")]
    pub grid: bool,
    #[arg(long = "no-auto-crop", help = "keep sealed rooms that can't be reached from spawns")]
    pub no_auto_crop: bool,
    #[arg(long, default_value_t = 3, help = "supersampling factor")]
    pub ss: u32,
    #[arg(long, default_value_t = 2.5)]
    pub gamma: f64,
    #[arg(long, default_value_t = 2.0)]
    pub texgamma: f64,
    #[arg(long, default_value_t = 2.5)]
    pub lightgamma: f64,
    #[arg(long, default_value_t = 1.0)]
    pub brightness: f64,
    #[arg(long = "light-scale", default_value_t = 1.0)]
    pub light_scale: f64,
    #[arg(long = "all-styles", help = "include switchable light styles")]
    pub all_styles: bool,
}

#[derive(Args)]
pub struct GuiArgs {
    #[arg(help = "map to open")]
    pub map: Option<String>,
    #[arg(long, help = "game install root or mod directory")]
    pub game: Option<PathBuf>,
    #[arg(long, default_value = "iso", value_parser = ["iso", "top", "overview"], help = "starting view")]
    pub view: String,
}

#[derive(Args)]
pub struct IsoArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 2048, help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value_t = 35.264, help = "degrees down; 35.264 true iso, 30 for 2:1")]
    pub pitch: f64,
    #[arg(long, num_args = 1.., default_values_t = [45.0, 135.0, 225.0, 315.0], allow_negative_numbers = true)]
    pub yaw: Vec<f64>,
    #[command(flatten)]
    pub l: LookArgs,
}

#[derive(Args)]
pub struct SpinArgs {
    #[command(flatten)]
    pub c: Common,
    #[command(flatten)]
    pub l: LookArgs,
    #[arg(long, default_value_t = 720, help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value_t = 35.264, help = "degrees down; 35.264 true iso, 30 for 2:1")]
    pub pitch: f64,
    #[arg(long, default_value_t = 12.0, help = "seconds per full turn")]
    pub seconds: f64,
    #[arg(long, default_value_t = 60.0)]
    pub fps: f64,
    #[arg(long, default_value_t = 45.0, allow_negative_numbers = true, help = "yaw of the first frame")]
    pub start: f64,
    #[arg(long, help = "turn the other way")]
    pub ccw: bool,
    #[arg(long, help = "also write an animated PNG")]
    pub apng: bool,
    #[arg(long, help = "also write an animated GIF")]
    pub gif: bool,
    #[arg(long = "no-mp4", help = "skip the MP4")]
    pub no_mp4: bool,
}

#[derive(Args)]
pub struct TimingArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 250.0, help = "running speed in units/s (250 knife, 221 AK, 230 M4)")]
    pub speed: f64,
    #[arg(long, default_value_t = 8.0, help = "walk grid spacing in units")]
    pub cell: f64,
    #[arg(long, default_value_t = 5.0, help = "seconds between contour lines")]
    pub interval: f64,
    #[arg(long, default_value_t = 1600, help = "longest image side in pixels")]
    pub size: u32,
    #[command(flatten)]
    pub f: FaceArgs,
}

#[derive(Args)]
pub struct OverviewArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 0.04, help = "border around the map")]
    pub margin: f64,
    #[arg(long = "from-txt", help = "reuse ZOOM/ORIGIN/ROTATED from an existing overview .txt")]
    pub from_txt: Option<PathBuf>,
    #[arg(long, help = "also write a transparent PNG")]
    pub png: bool,
    #[command(flatten)]
    pub f: FaceArgs,
}

impl Common {
    pub fn load_opts(&self) -> LoadOpts {
        LoadOpts {
            game: self.game.clone(),
            wad_dirs: self.wad_dir.clone(),
            light: LightParams {
                gamma: self.gamma,
                texgamma: self.texgamma,
                lightgamma: self.lightgamma,
                brightness: self.brightness,
                scale: self.light_scale,
                all_styles: self.all_styles,
            },
            auto_crop: self.crop.is_none() && !self.no_auto_crop,
            hull: self.hull.as_ref().and_then(|h| h.parse().ok()),
            hull_pad: self.hull_pad,
        }
    }

    pub fn cut_opts(&self) -> CutOpts {
        CutOpts {
            roofs: self.roofs,
            zmin: self.zmin,
            zmax: self.zmax,
            crop: self.crop.as_ref().map(|v| [v[0], v[1], v[2], v[3]]),
            xmin: self.xmin,
            xmax: self.xmax,
            ymin: self.ymin,
            ymax: self.ymax,
        }
    }
}

fn say(s: String) {
    println!("{s}");
}

fn reported<T>(f: impl FnOnce(&mut Report) -> Result<T>) -> Result<T> {
    let tty = std::io::stdout().is_terminal();
    let shown = Cell::new(-1i32);
    let clear = || {
        if shown.replace(-1) >= 0 {
            print!("\r        \r");
        }
    };
    let mut log = |s: String| {
        clear();
        println!("{s}");
    };
    let mut progress = |p: f32| {
        let pct = (p * 100.0).floor() as i32;
        if tty && pct != shown.get() {
            print!("\r  {pct:3}%");
            let _ = std::io::stdout().flush();
            shown.set(pct);
        }
        true
    };
    let res = f(&mut Report { log: &mut log, progress: &mut progress });
    clear();
    res
}

pub fn run_spin(a: &SpinArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = SpinOpts {
        size: a.size,
        ss: a.c.ss,
        pitch: a.pitch,
        seconds: a.seconds,
        fps: a.fps,
        start: a.start,
        ccw: a.ccw,
        gif: a.gif,
        mp4: !a.no_mp4,
        apng: a.apng,
        look: a.l.look()?,
    };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let (mut r, sky_tag) = scene.job_renderer(&gpu, o.look.nearest, o.look.sky_spec(), &mut say);
        reported(|rep| export_spin(&mut r, &name, &sky_tag, &co.tag(lo.hull), &cuts, &o, &out, rep))?;
        println!("  {:.1}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}

pub fn run_timing(a: &TimingArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = TimingOpts { speed: a.speed, cell: a.cell, interval: a.interval, size: a.size };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let (mut r, _) = scene.job_renderer(&gpu, a.f.nearest, None, &mut say);
        reported(|rep| export_timing(&mut r, &scene.bsp, &name, &co.tag(lo.hull), &cuts, &o, &out, rep))?;
        println!("  {:.1}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}

pub fn run_iso(a: &IsoArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = IsoOpts {
        size: a.size,
        ss: a.c.ss,
        pitch: a.pitch,
        yaws: a.yaw.clone(),
        grid: a.c.grid,
        look: a.l.look()?,
    };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let (mut r, sky_tag) = scene.job_renderer(&gpu, o.look.nearest, o.look.sky_spec(), &mut say);
        reported(|rep| export_iso(&mut r, &scene.bsp, &name, &sky_tag, &co.tag(lo.hull), &cuts, &o, &out, rep))?;
        println!("  {:.1}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}

pub fn run_overview(a: &OverviewArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = OverviewOpts {
        margin: a.margin,
        ss: a.c.ss,
        cull: !a.f.no_cull,
        from_txt: a.from_txt.clone(),
        png: a.png,
        grid: a.c.grid,
    };
    for m in &a.c.maps {
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let mut log = |s: String| println!("  {s}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut log)?;
        let cuts = scene.cuts(&co, &mut log);
        let (mut r, _) = scene.job_renderer(&gpu, a.f.nearest, None, &mut log);
        reported(|rep| export_overview(&mut r, &scene.bsp, &name, &cuts, &o, &out, rep))?;
    }
    Ok(())
}
