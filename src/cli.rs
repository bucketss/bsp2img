use std::cell::Cell;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::camera::{Camera, Framing};
use crate::light::LightParams;
use crate::export::{IsoOpts, OverviewOpts, export_iso, export_overview};
use crate::gltf::{GltfOpts, Lighting, export_gltf};
use crate::health::{HealthOpts, csv_quote, export_health};
use crate::look::{FaceArgs, LookArgs};
use crate::paths::{expand_maps, free_name, resolve_map, run_dir};
use crate::render::Gpu;
use crate::scene::{CutOpts, LoadOpts, Report, Scene};
use crate::spin::{Anim, AnimOpts, PeelOpts, SliceOpts, export_anim};
use crate::svg::{SvgOpts, export_svg};
use crate::stl::{StlOpts, export_stl};
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
    #[command(about = "Animate roof levels lifting off one by one")]
    Peel(PeelArgs),
    #[command(about = "Animate a height cut rising from the floor so the map builds itself")]
    Slice(SliceArgs),
    #[command(about = "Map how fast each team reaches every spot from its spawns")]
    Timing(TimingArgs),
    #[command(about = "Report missing assets, compile problems, engine limits, spawns and overview readiness")]
    Health(HealthArgs),
    #[command(about = "Write editable SVG line art of the walkable floors, walls, objectives and spawns")]
    Svg(SvgArgs),
    #[command(about = "Write a watertight 3D-printable STL of the playable area")]
    Stl(StlArgs),
    #[command(about = "Write a textured glTF (.glb) of the map for 3D viewers and Blender")]
    Gltf(GltfArgs),
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
    #[arg(long, default_value = "iso", value_parser = ["iso", "top", "overview", "free"], help = "starting view")]
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
    #[command(flatten)]
    pub cam: CamArgs,
}

#[derive(Args, Clone)]
pub struct CamArgs {
    #[arg(long, value_name = "FILE", help = "use a .cam camera saved from the GUI instead of automatic framing")]
    pub camera: Option<PathBuf>,
    #[arg(long, value_name = "FOV", conflicts_with = "camera", help = "perspective automatic framing, vertical field of view in degrees")]
    pub persp: Option<f64>,
}

impl CamArgs {
    pub fn framing(&self) -> Result<Framing> {
        let camera = self.camera.as_deref().map(Camera::load).transpose()?;
        let persp = self.persp.map(|f| f.clamp(1.0, 170.0));
        Ok(Framing { camera, persp })
    }
}

#[derive(Args)]
pub struct SpinArgs {
    #[command(flatten)]
    pub c: Common,
    #[command(flatten)]
    pub l: LookArgs,
    #[command(flatten)]
    pub cam: CamArgs,
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
pub struct AnimArgs {
    #[arg(long, default_value_t = 720, help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value_t = 35.264, help = "degrees down; 35.264 true iso, 30 for 2:1")]
    pub pitch: f64,
    #[arg(long, default_value_t = 60.0)]
    pub fps: f64,
    #[arg(long, default_value_t = 45.0, allow_negative_numbers = true, help = "yaw of the view")]
    pub start: f64,
    #[arg(long = "then-spin", help = "follow with a full turn in the same file")]
    pub then_spin: bool,
    #[arg(long = "spin-seconds", default_value_t = 12.0, help = "seconds per full turn with --then-spin")]
    pub spin_seconds: f64,
    #[arg(long, help = "turn the other way with --then-spin")]
    pub ccw: bool,
    #[arg(long, help = "also write an animated PNG")]
    pub apng: bool,
    #[arg(long, help = "also write an animated GIF")]
    pub gif: bool,
    #[arg(long = "no-mp4", help = "skip the MP4")]
    pub no_mp4: bool,
    #[command(flatten)]
    pub cam: CamArgs,
}

#[derive(Args)]
pub struct PeelArgs {
    #[command(flatten)]
    pub c: Common,
    #[command(flatten)]
    pub l: LookArgs,
    #[command(flatten)]
    pub a: AnimArgs,
    #[arg(long, default_value_t = 0, help = "roof levels to lift off (0 = all but the lowest)")]
    pub count: usize,
    #[arg(long = "seconds-per", default_value_t = 1.5, help = "seconds to lift each level")]
    pub seconds_per: f64,
    #[arg(long, default_value_t = 0.5, help = "seconds to pause before and after each level")]
    pub hold: f64,
    #[arg(long, help = "play backwards: roofs drop into place")]
    pub reverse: bool,
}

#[derive(Args)]
pub struct SliceArgs {
    #[command(flatten)]
    pub c: Common,
    #[command(flatten)]
    pub l: LookArgs,
    #[command(flatten)]
    pub a: AnimArgs,
    #[arg(long, default_value_t = 0, help = "number of steps (0 = continuous sweep)")]
    pub count: u32,
    #[arg(long, default_value_t = 6.0, help = "sweep duration with --count 0")]
    pub seconds: f64,
    #[arg(long, default_value_t = 0.4, help = "seconds to pause at each step and at the end")]
    pub hold: f64,
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
pub struct HealthArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 8.0, help = "walk grid spacing in units")]
    pub cell: f64,
    #[arg(long, default_value_t = 600, help = "longest side of the map thumbnail in pixels")]
    pub size: u32,
    #[command(flatten)]
    pub f: FaceArgs,
}

#[derive(Args)]
pub struct SvgArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 8.0, help = "walk grid spacing in units")]
    pub cell: f64,
    #[arg(long, default_value_t = 6.0, help = "outline simplification tolerance in units")]
    pub simplify: f64,
    #[arg(long, default_value = "1:100", help = "print scale; 1 unit = 1 inch")]
    pub scale: String,
    #[arg(long, default_value_t = 2, help = "most floor bands to split stacked areas into")]
    pub bands: usize,
    #[arg(long, value_delimiter = ',', allow_negative_numbers = true, help = "split floors at these heights instead")]
    pub planes: Option<Vec<f64>>,
}

#[derive(Args)]
pub struct StlArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 8.0, help = "voxel size in units")]
    pub voxel: f64,
    #[arg(long, default_value_t = 32.0, help = "units kept around the walkable area")]
    pub wall: f64,
    #[arg(long, default_value_t = 16.0, help = "base plate thickness below the lowest floor in units")]
    pub base: f64,
    #[arg(long = "print-width", default_value_t = 200.0, help = "longest side of the print in mm")]
    pub print_width: f64,
    #[arg(long, help = "smooth surface (surface nets) instead of blocky voxels")]
    pub smooth: bool,
}

#[derive(Args)]
pub struct GltfArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value = "baked", value_parser = ["baked", "separate", "none"],
          help = "baked: lightmap baked into one atlas; separate: tiled textures plus lightmap on UV 2; none: textures only")]
    pub lighting: String,
    #[arg(long, default_value_t = 2.0, help = "units per atlas texel with --lighting baked")]
    pub texel: f64,
    #[arg(long, help = "pixelated texture filtering")]
    pub nearest: bool,
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
    let o = AnimOpts {
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
        kind: Anim::Spin,
        framing: a.cam.framing()?,
    };
    run_anim(&a.c, &o)
}

impl AnimArgs {
    fn opts(&self, c: &Common, l: &LookArgs, kind: Anim) -> Result<AnimOpts> {
        Ok(AnimOpts {
            size: self.size,
            ss: c.ss,
            pitch: self.pitch,
            seconds: self.spin_seconds,
            fps: self.fps,
            start: self.start,
            ccw: self.ccw,
            gif: self.gif,
            mp4: !self.no_mp4,
            apng: self.apng,
            look: l.look()?,
            kind,
            framing: self.cam.framing()?,
        })
    }
}

pub fn run_peel(a: &PeelArgs) -> Result<()> {
    let p = PeelOpts {
        roofs: a.count,
        seconds_per: a.seconds_per,
        hold: a.hold,
        reverse: a.reverse,
        then_spin: a.a.then_spin,
    };
    run_anim(&a.c, &a.a.opts(&a.c, &a.l, Anim::Peel(p))?)
}

pub fn run_slice(a: &SliceArgs) -> Result<()> {
    let s = SliceOpts { slices: a.count, seconds: a.seconds, hold: a.hold, then_spin: a.a.then_spin };
    run_anim(&a.c, &a.a.opts(&a.c, &a.l, Anim::Slice(s))?)
}

fn run_anim(c: &Common, o: &AnimOpts) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = c.load_opts();
    let co = c.cut_opts();
    for m in &c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let (mut r, sky_tag) = scene.job_renderer(&gpu, o.look.nearest, o.look.sky_spec(), &mut say);
        let res = reported(|rep| export_anim(&mut r, &scene.levels, &name, &sky_tag, &co.tag(lo.hull), &cuts, o, &out, rep));
        if res.is_err() {
            let _ = std::fs::remove_dir(&out);
        }
        res?;
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
        framing: a.cam.framing()?,
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

pub fn parse_scale(s: &str) -> Result<f64> {
    let d = s.rsplit(':').next().unwrap_or(s).trim().parse::<f64>()?;
    if d <= 0.0 {
        anyhow::bail!("bad scale: {s}");
    }
    Ok(d)
}

pub fn run_svg(a: &SvgArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = SvgOpts { cell: a.cell, simplify: a.simplify, scale: parse_scale(&a.scale)?, bands: a.bands, planes: a.planes.clone() };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let (mut r, _) = scene.job_renderer(&gpu, false, None, &mut say);
        let res = reported(|rep| export_svg(&mut r, &scene, &name, &co.tag(lo.hull), &cuts, &o, &out, rep));
        if res.is_err() {
            let _ = std::fs::remove_dir(&out);
        }
        res?;
        println!("  {:.1}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}

pub fn run_stl(a: &StlArgs) -> Result<()> {
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = StlOpts { voxel: a.voxel, wall: a.wall, base: a.base, print_width: a.print_width, smooth: a.smooth };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, 8192, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let res = reported(|rep| export_stl(&scene.bsp, &name, &co.tag(lo.hull), &cuts, &o, &out, rep));
        if res.is_err() {
            let _ = std::fs::remove_dir(&out);
        }
        res?;
        println!("  {:.1}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}

pub fn run_gltf(a: &GltfArgs) -> Result<()> {
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = GltfOpts { lighting: Lighting::parse(&a.lighting).unwrap_or(Lighting::Baked), texel: a.texel, nearest: a.nearest };
    for m in &a.c.maps {
        let t0 = Instant::now();
        let path = resolve_map(m, a.c.game.as_deref())?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let out = run_dir(&a.c.out, &name)?;
        println!("== {name}");
        let scene = Scene::load(&path, &lo, 8192, &mut say)?;
        let cuts = scene.cuts(&co, &mut say);
        let res = reported(|rep| export_gltf(&scene, &name, &co.tag(lo.hull), &cuts, &o, &out, rep));
        if res.is_err() {
            let _ = std::fs::remove_dir(&out);
        }
        res?;
        println!("  {:.1}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}

pub fn run_health(a: &HealthArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = HealthOpts { cell: a.cell, size: a.size };
    let maps = expand_maps(&a.c.maps, a.c.game.as_deref());
    if maps.is_empty() {
        anyhow::bail!("no maps match");
    }
    let summary = (maps.len() > 1).then(|| {
        std::fs::create_dir_all(&a.c.out).ok();
        free_name(&a.c.out, "health_summary", ".csv")
    });
    let mut header = String::new();
    let mut rows = Vec::new();
    let mut failed = 0;
    for m in &maps {
        let t0 = Instant::now();
        println!("== {m}");
        let res = (|| -> Result<crate::health::Health> {
            let path = resolve_map(m, a.c.game.as_deref())?;
            let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let out = run_dir(&a.c.out, &name)?;
            let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, _) = scene.job_renderer(&gpu, a.f.nearest, None, &mut say);
            let res = reported(|rep| export_health(&mut r, &scene, &name, &co.tag(lo.hull), &cuts, &o, &out, rep));
            if res.is_err() {
                let _ = std::fs::remove_dir(&out);
            }
            Ok(res?.1)
        })();
        match res {
            Ok(h) => {
                if header.is_empty() {
                    header = h.csv_header();
                }
                rows.push(h.csv_row());
            }
            Err(e) if summary.is_some() => {
                failed += 1;
                println!("  error: {e:#}");
                rows.push(format!("{m},{}", csv_quote(&format!("{e:#}"))));
            }
            Err(e) => return Err(e),
        }
        println!("  {:.1}s", t0.elapsed().as_secs_f64());
        if let Some(f) = &summary {
            let mut text = format!("{header}\n");
            for r in &rows {
                text.push_str(r);
                text.push('\n');
            }
            std::fs::write(f, text)?;
        }
    }
    if let Some(f) = &summary {
        println!("{} maps, {failed} failed: {}", maps.len(), f.display());
    }
    Ok(())
}
