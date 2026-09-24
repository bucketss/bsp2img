use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::light::LightParams;
use crate::export::{IsoOpts, OverviewOpts, export_iso, export_overview};
use crate::paths::{resolve_map, run_dir};
use crate::render::{Gpu, Renderer, parse_color};
use crate::scene::{CutOpts, LoadOpts, Scene};
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
    #[arg(long = "no-cull", help = "draw back faces")]
    pub no_cull: bool,
    #[arg(long, help = "pixelated texture filtering")]
    pub nearest: bool,
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
    #[arg(long, help = "background colour, e.g. #202020 (default transparent)")]
    pub bg: Option<String>,
    #[command(flatten)]
    pub s: SkyArgs,
}

#[derive(Args, Clone)]
pub struct SkyArgs {
    #[arg(long, num_args = 0..=1, value_name = "NAME",
          help = "draw the skybox behind the map (map's own sky, or NAME from gfx/env)")]
    pub sky: Option<Option<String>>,
    #[arg(long = "sky-fov", default_value_t = 90.0)]
    pub sky_fov: f64,
    #[arg(long = "sky-pitch", default_value_t = 10.0)]
    pub sky_pitch: f64,
}

#[derive(Args)]
pub struct SpinArgs {
    #[command(flatten)]
    pub c: Common,
    #[command(flatten)]
    pub s: SkyArgs,
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
    #[arg(long, help = "background colour (default #202020; APNG stays transparent unless set)")]
    pub bg: Option<String>,
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

fn apply_sky(scene: &Scene, r: &mut Renderer, a: &SkyArgs) -> String {
    let Some(sky) = &a.sky else { return String::new() };
    let sname = scene.sky_name(sky.as_deref());
    match scene.load_sky(&sname) {
        Some(f) => {
            r.set_sky(Some(f), a.sky_fov, a.sky_pitch);
            println!("sky: {sname}");
            sky.as_deref().filter(|n| !n.is_empty()).map(|n| format!("_{n}")).unwrap_or_default()
        }
        None => {
            println!("sky: {sname} not found in gfx/env, using background");
            String::new()
        }
    }
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
        bg: a.bg.as_deref().map(parse_color).transpose()?,
        cull: !a.c.no_cull,
        gif: a.gif,
        mp4: !a.no_mp4,
        apng: a.apng,
    };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let mut r = scene.renderer(&gpu, a.c.nearest);
        let sky_tag = apply_sky(&scene, &mut r, &a.s);
        export_spin(&mut r, &name, &sky_tag, &co.tag(lo.hull), &cuts, &o, &out, &mut say)?;
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
        let mut r = scene.renderer(&gpu, a.c.nearest);
        export_timing(&mut r, &scene.bsp, &name, &co.tag(lo.hull), &cuts, &o, &out, &mut say)?;
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
        bg: a.bg.as_deref().map(parse_color).transpose()?,
        cull: !a.c.no_cull,
        grid: a.c.grid,
    };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let mut r = scene.renderer(&gpu, a.c.nearest);
        let sky_tag = apply_sky(&scene, &mut r, &a.s);
        export_iso(&mut r, &scene.bsp, &name, &sky_tag, &co.tag(lo.hull), &cuts, &o, &out, &mut say)?;
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
        cull: !a.c.no_cull,
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
        let mut r = scene.renderer(&gpu, a.c.nearest);
        export_overview(&mut r, &scene.bsp, &name, &cuts, &o, &out, &mut say)?;
    }
    Ok(())
}
