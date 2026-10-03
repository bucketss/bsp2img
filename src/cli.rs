use std::cell::Cell;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::camera::{Camera, Framing};
use crate::explode::ExplodeArgs;
use crate::light::LightParams;
use crate::export::{IsoOpts, OverviewOpts, export_iso, export_overview};
use crate::gltf::{GltfOpts, Lighting, export_gltf};
use crate::demo;
use crate::health::{HealthOpts, csv_quote, export_health};
use crate::kills::{DemoData, KillOpts, KillStats, TeamSel, export_kills, load_demos, parse_rounds};
use crate::look::{FaceArgs, LookArgs};
use crate::paths::{expand_demos, expand_maps, free_name, resolve_map, run_dir, run_name};
use crate::poster::{Orient, PosterOpts, export_poster};
use crate::render::Gpu;
use crate::scene::{CutOpts, LoadOpts, Report, Scene};
use crate::spin::{Anim, AnimOpts, DayOpts, PeelOpts, SliceOpts, export_anim};
use crate::sun::parse_time;
use crate::svg::{SvgOpts, export_svg};
use crate::stl::{StlOpts, export_stl};
use crate::timing::{TimingOpts, export_timing};

#[derive(Parser)]
#[command(name = "bsp2img", version, about = "Render GoldSrc, Source and CS2 maps: isometric views, animations, overviews, heatmaps, reports and 3D exports. Run without arguments for the GUI.")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
    #[arg(long, global = true, help = "log how long each loading and rendering phase takes")]
    pub timings: bool,
}

#[derive(Subcommand)]
pub enum Cmd {
    #[command(about = "Render isometric diorama views")]
    Iso(IsoArgs),
    #[command(about = "Generate a CS overview (.bmp, .tga, .txt; CS2 and Source: radar .png and .txt)")]
    Overview(OverviewArgs),
    #[command(about = "Render an animation of the map rotating (.mp4 via ffmpeg, optional .gif/.png)")]
    Spin(SpinArgs),
    #[command(about = "Animate roof levels lifting off one by one")]
    Peel(PeelArgs),
    #[command(about = "Animate a height cut rising from the floor so the map builds itself")]
    Slice(SliceArgs),
    #[command(about = "Map how fast each team reaches every spot from its spawns")]
    Timing(TimingArgs),
    #[command(about = "Heatmaps of kills and player positions from HLTV and CS2 demos")]
    Kills(KillsArgs),
    #[command(about = "GoldSrc: map which walls bullets get through and where they go")]
    Wallbang(WallbangArgs),
    #[command(about = "GoldSrc: find pixelwalks (spots where the player catches on a sub-unit seam over the void)")]
    Pixelwalk(PixelwalkArgs),
    #[command(about = "Report missing assets, compile problems, engine limits, spawns and overview readiness")]
    Health(HealthArgs),
    #[command(about = "Write editable SVG line art of the walkable floors, walls, objectives and spawns")]
    Svg(SvgArgs),
    #[command(about = "Write a watertight 3D-printable STL of the playable area")]
    Stl(StlArgs),
    #[command(about = "Write a textured glTF (.glb) of the map for 3D viewers and Blender")]
    Gltf(GltfArgs),
    #[command(about = "Write a textured Wavefront OBJ (.obj, .mtl and PNGs) of the map")]
    Obj(GltfArgs),
    #[command(about = "Render a print-size poster with title, coordinate border, legend and scale bar")]
    Poster(PosterArgs),
    #[command(about = "Open the GUI")]
    Gui(GuiArgs),
}

#[derive(Args, Clone)]
pub struct Common {
    #[arg(required = true, help = "path to .bsp or CS2 .vpk, or map name with --game; * and ? globs allowed")]
    pub maps: Vec<String>,
    #[arg(long, help = "game install root or mod directory")]
    pub game: Option<PathBuf>,
    #[arg(long = "wad-dir", help = "extra directory to search for WADs")]
    pub wad_dir: Vec<PathBuf>,
    #[arg(short, long, default_value = "renders", help = "base folder; each run goes in a new <out>/<mod>-<map>-YYYY-MMDD-hhmm/")]
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
    #[arg(long = "hull-pad", default_value_t = 64.0, value_parser = non_negative, help = "units kept around the walkable area")]
    pub hull_pad: f64,
    #[arg(long, help = "iso and overview: also write a top-down <map>_grid.png with world coordinates")]
    pub grid: bool,
    #[arg(long = "no-auto-crop", help = "keep sealed rooms that can't be reached from spawns")]
    pub no_auto_crop: bool,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=8), help = "supersampling factor")]
    pub ss: u32,
    #[arg(long, default_value_t = 2.5, value_parser = positive)]
    pub gamma: f64,
    #[arg(long, default_value_t = 2.0, value_parser = positive)]
    pub texgamma: f64,
    #[arg(long, default_value_t = 2.5, value_parser = positive)]
    pub lightgamma: f64,
    #[arg(long, default_value_t = 1.0, value_parser = non_negative)]
    pub brightness: f64,
    #[arg(long = "light-scale", default_value_t = 1.0, value_parser = non_negative)]
    pub light_scale: f64,
    #[arg(long = "all-styles", help = "include switchable light styles")]
    pub all_styles: bool,
    #[arg(long = "tex-max", default_value_t = 512, help = "CS2 and Source: largest texture size to load")]
    pub tex_max: usize,
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
    #[arg(long, default_value_t = 2048, value_parser = clap::value_parser!(u32).range(64..), help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value_t = 35.264, help = "degrees down; 35.264 true iso, 30 for 2:1")]
    pub pitch: f64,
    #[arg(long, num_args = 1.., default_values_t = [45.0, 135.0, 225.0, 315.0], allow_negative_numbers = true)]
    pub yaw: Vec<f64>,
    #[command(flatten)]
    pub l: LookArgs,
    #[command(flatten)]
    pub cam: CamArgs,
    #[command(flatten)]
    pub ex: ExplodeArgs,
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
    #[command(flatten)]
    pub ex: ExplodeArgs,
    #[arg(long, default_value_t = 720, value_parser = clap::value_parser!(u32).range(64..), help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value_t = 35.264, help = "degrees down; 35.264 true iso, 30 for 2:1")]
    pub pitch: f64,
    #[arg(long, default_value_t = 12.0, value_parser = positive, help = "seconds per full turn")]
    pub seconds: f64,
    #[arg(long, default_value_t = 60.0, value_parser = positive)]
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
    #[arg(long = "day-cycle", help = "relight and sweep the time of day over the clip")]
    pub day_cycle: bool,
    #[arg(long = "day-from", value_name = "HH:MM", default_value = "00:00", value_parser = parse_time)]
    pub day_from: f64,
    #[arg(long = "day-to", value_name = "HH:MM", default_value = "24:00", value_parser = parse_time)]
    pub day_to: f64,
    #[arg(long = "no-turn", requires = "day_cycle", help = "with --day-cycle, keep the camera still")]
    pub no_turn: bool,
}

#[derive(Args)]
pub struct AnimArgs {
    #[arg(long, default_value_t = 720, value_parser = clap::value_parser!(u32).range(64..), help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value_t = 35.264, help = "degrees down; 35.264 true iso, 30 for 2:1")]
    pub pitch: f64,
    #[arg(long, default_value_t = 60.0, value_parser = positive)]
    pub fps: f64,
    #[arg(long, default_value_t = 45.0, allow_negative_numbers = true, help = "yaw of the view")]
    pub start: f64,
    #[arg(long = "then-spin", help = "follow with a full turn in the same file")]
    pub then_spin: bool,
    #[arg(long = "spin-seconds", default_value_t = 12.0, value_parser = positive, help = "seconds per full turn with --then-spin")]
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
    #[command(flatten)]
    pub ex: ExplodeArgs,
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
    #[arg(long = "seconds-per", default_value_t = 1.5, value_parser = positive, help = "seconds to lift each level")]
    pub seconds_per: f64,
    #[arg(long, default_value_t = 0.5, value_parser = non_negative, help = "seconds to pause before and after each level")]
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
    #[arg(long, default_value_t = 6.0, value_parser = positive, help = "sweep duration with --count 0")]
    pub seconds: f64,
    #[arg(long, default_value_t = 0.4, value_parser = non_negative, help = "seconds to pause at each step and at the end")]
    pub hold: f64,
}

#[derive(Args)]
pub struct TimingArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 250.0, value_parser = positive, help = "running speed in units/s (250 knife, 221 AK, 230 M4)")]
    pub speed: f64,
    #[arg(long, default_value_t = 8.0, value_parser = at_least_one, help = "walk grid spacing in units")]
    pub cell: f64,
    #[arg(long, default_value_t = 5.0, value_parser = positive, help = "seconds between contour lines")]
    pub interval: f64,
    #[arg(long, default_value_t = 1600, value_parser = clap::value_parser!(u32).range(64..), help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value = "render", value_parser = ["render", "radar"], help = "backdrop: our render, or the official CS2/Source radar (one image per section)")]
    pub base: String,
    #[command(flatten)]
    pub f: FaceArgs,
}

#[derive(Args)]
pub struct WallbangArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value = "ak47", value_parser = ["ak47", "awp", "m4a1", "m4a1_sil", "deagle", "scout"], help = "the one weapon to simulate and draw")]
    pub weapon: String,
    #[arg(long, default_value = "both", value_parser = ["stand", "duck", "both"], help = "shooter eye height")]
    pub stance: String,
    #[arg(long = "yaw-step", default_value_t = 2.0, value_parser = positive, value_name = "D", help = "degrees between shots")]
    pub yaw_step: f64,
    #[arg(long, default_value = "-20..20/4", allow_hyphen_values = true, value_parser = parse_pitch, value_name = "A..B/S", help = "pitch range and step in degrees (positive = down)")]
    pub pitch: (f64, f64, f64),
    #[arg(long = "spot-cell", default_value_t = 64.0, value_parser = at_least_one, value_name = "U", help = "shooter spacing in units")]
    pub spot_cell: f64,
    #[arg(long = "max-hit", default_value_t = 2000.0, value_parser = positive, value_name = "U", help = "ignore walls further away")]
    pub max_hit: f64,
    #[arg(long = "skip-translucent", value_name = "A", help = "ignore brush entities with a non-normal rendermode and renderamt below A (0-255)")]
    pub skip_translucent: Option<u8>,
    #[arg(long = "skip-doors", help = "ignore func_door, func_door_rotating and momentary_door")]
    pub skip_doors: bool,
    #[arg(long = "min-thickness", default_value_t = 0.0, value_parser = non_negative, value_name = "U", help = "drop walls thinner than U (measured along the face normal)")]
    pub min_thickness: f64,
    #[arg(long, default_value_t = 256.0, value_parser = positive, value_name = "U", help = "floors and ceilings thicker than this count as ground")]
    pub slab: f64,
    #[arg(long, value_name = "FILE", help = "materials.txt to use instead of the game's")]
    pub materials: Option<PathBuf>,
    #[arg(long, default_value = "both", value_parser = ["lines", "areas", "both"], help = "2D style")]
    pub style: String,
    #[arg(long, default_value_t = 2.0, value_parser = non_negative, value_name = "PX", help = "area smoothing radius in pixels")]
    pub sigma: f64,
    #[arg(long, default_value = "render", value_parser = ["render", "radar"], help = "backdrop: our render, or the official overview framing")]
    pub base: String,
    #[arg(long, default_value_t = 1600, value_parser = clap::value_parser!(u32).range(64..), help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value = "none", value_name = "auto|none|Z,...", help = "per-level images")]
    pub sections: String,
    #[arg(long, default_value = "off", value_parser = ["off", "faint"], help = "shots from other levels: dropped, or drawn faint")]
    pub others: String,
    #[arg(long, help = "also write every shot to a .csv")]
    pub csv: bool,
    #[arg(long, help = "add the pixelwalk layer")]
    pub pixelwalks: bool,
    #[command(flatten)]
    pub f: FaceArgs,
}

#[derive(Args)]
pub struct PixelwalkArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value = "both", value_parser = ["stand", "duck", "both"], help = "stances to search")]
    pub stance: String,
    #[arg(long = "min-samples", default_value_t = 2, value_parser = clap::value_parser!(i32).range(1..), help = "drop finds with fewer sub-pixel hits")]
    pub min_samples: i32,
    #[arg(long = "merge-gap", default_value_t = 64.0, value_parser = non_negative, value_name = "U", help = "join finds on the same wall line within U units")]
    pub merge_gap: f64,
    #[arg(long, default_value = "none", value_name = "auto|none|Z,...", help = "per-level images")]
    pub sections: String,
    #[arg(long, default_value = "render", value_parser = ["render", "radar"], help = "backdrop: our render, or the official overview framing")]
    pub base: String,
    #[arg(long, default_value_t = 1600, value_parser = clap::value_parser!(u32).range(64..), help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, help = "number each pixelwalk on the image to match the .txt")]
    pub labels: bool,
    #[command(flatten)]
    pub f: FaceArgs,
}

fn parse_pitch(s: &str) -> Result<(f64, f64, f64), String> {
    let bad = || format!("expected A..B/S, e.g. -20..20/4, got {s}");
    let (range, step) = s.split_once('/').ok_or_else(bad)?;
    let (a, b) = range.split_once("..").ok_or_else(bad)?;
    let p = |t: &str| t.trim().parse::<f64>().map_err(|_| bad());
    let (a, b, st) = (p(a)?, p(b)?, p(step)?);
    if st <= 0.0 || b < a || a < -89.0 || b > 89.0 {
        return Err(format!("{s}: need -89 <= A <= B <= 89 and S > 0"));
    }
    Ok((a, b, st))
}

#[derive(Args)]
#[command(mut_arg("maps", |a| a.required(false).help("map .bsp or name with --game; omit to do every map the demos were recorded on")))]
pub struct KillsArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, num_args = 1.., required = true, value_name = "PATH", help = "demo files, folders or globs")]
    pub demos: Vec<String>,
    #[arg(long, value_delimiter = ',', value_name = "NAME", help = "only kills with these weapons, e.g. awp,scout")]
    pub weapon: Vec<String>,
    #[arg(long, default_value = "both", value_parser = ["t", "ct", "both"], help = "victim's team")]
    pub team: String,
    #[arg(long, help = "only headshots")]
    pub headshots: bool,
    #[arg(long, help = "draw killer-to-victim lines")]
    pub lines: bool,
    #[arg(long, value_name = "A-B", value_parser = parse_rounds, help = "only these rounds, counted from the start of each demo")]
    pub rounds: Option<(u32, u32)>,
    #[arg(long, default_value_t = 96.0, value_parser = at_least_one, help = "heatmap kernel radius in units")]
    pub radius: f64,
    #[arg(long = "presence-every", default_value_t = 0.5, value_parser = non_negative, value_name = "S", help = "seconds between presence samples; 0 = no presence map")]
    pub presence_every: f64,
    #[arg(long, default_value_t = 1600, value_parser = clap::value_parser!(u32).range(64..), help = "longest image side in pixels")]
    pub size: u32,
    #[arg(long, default_value = "render", value_parser = ["render", "radar"], help = "backdrop: our render, or the official CS2/Source radar (one image set per section)")]
    pub base: String,
    #[command(flatten)]
    pub f: FaceArgs,
}

#[derive(Args)]
pub struct HealthArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 8.0, value_parser = at_least_one, help = "walk grid spacing in units")]
    pub cell: f64,
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u32).range(64..), help = "longest side of the map thumbnail in pixels")]
    pub size: u32,
    #[arg(long, help = "GoldSrc: also search for pixelwalks (adds seconds per map)")]
    pub pixelwalks: bool,
    #[command(flatten)]
    pub f: FaceArgs,
}

#[derive(Args)]
pub struct SvgArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 8.0, value_parser = at_least_one, help = "walk grid spacing in units")]
    pub cell: f64,
    #[arg(long, default_value_t = 6.0, value_parser = non_negative, help = "outline simplification tolerance in units")]
    pub simplify: f64,
    #[arg(long, default_value = "1:100", help = "print scale; 1 unit = 1 inch")]
    pub scale: String,
    #[arg(long, default_value_t = 2, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..), help = "most floor bands to split stacked areas into")]
    pub bands: usize,
    #[arg(long, value_delimiter = ',', allow_negative_numbers = true, help = "split floors at these heights instead")]
    pub planes: Option<Vec<f64>>,
    #[command(flatten)]
    pub ex: ExplodeArgs,
}

#[derive(Args)]
pub struct StlArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, value_parser = at_least_one, help = "voxel size in units (default 8, raised automatically for large maps)")]
    pub voxel: Option<f64>,
    #[arg(long, default_value_t = 32.0, value_parser = non_negative, help = "units kept around the walkable area")]
    pub wall: f64,
    #[arg(long, default_value_t = 16.0, value_parser = non_negative, help = "base plate thickness below the lowest floor in units")]
    pub base: f64,
    #[arg(long = "print-width", default_value_t = 200.0, value_parser = positive, help = "longest side of the print in mm")]
    pub print_width: f64,
    #[arg(long, help = "smooth surface (surface nets) instead of blocky voxels")]
    pub smooth: bool,
}

#[derive(Args)]
pub struct GltfArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, value_parser = ["baked", "separate", "none"],
          help = "baked: lightmap baked into one atlas; separate: tiled textures plus lightmap on UV 2; none: textures only [default: baked, CS2: separate for glTF, none for OBJ]")]
    pub lighting: Option<String>,
    #[arg(long, default_value_t = 2.0, value_parser = positive, help = "units per atlas texel with --lighting baked")]
    pub texel: f64,
    #[arg(long, help = "pixelated texture filtering")]
    pub nearest: bool,
}

#[derive(Args)]
pub struct PosterArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value = "a2", value_parser = ["a0", "a1", "a2", "a3", "a4", "letter", "tabloid"])]
    pub paper: String,
    #[arg(long, default_value_t = 300.0, value_parser = positive)]
    pub dpi: f64,
    #[arg(long, conflicts_with = "portrait", help = "force landscape (default: follow the map's shape)")]
    pub landscape: bool,
    #[arg(long, help = "force portrait")]
    pub portrait: bool,
    #[arg(long, num_args = 2, value_names = ["W", "H"], help = "page size in pixels instead of --paper")]
    pub px: Option<Vec<u32>>,
    #[arg(long, help = "top-down instead of isometric")]
    pub top: bool,
    #[arg(long, default_value_t = 45.0, allow_negative_numbers = true)]
    pub yaw: f64,
    #[arg(long, default_value_t = 35.264, help = "degrees down; 35.264 true iso, 30 for 2:1")]
    pub pitch: f64,
    #[arg(long = "no-layout", help = "map only: no title block, border or legend")]
    pub no_layout: bool,
    #[arg(long, hide = true)]
    pub tile: Option<u32>,
    #[command(flatten)]
    pub l: LookArgs,
    #[command(flatten)]
    pub cam: CamArgs,
    #[command(flatten)]
    pub ex: ExplodeArgs,
}

#[derive(Args)]
pub struct OverviewArgs {
    #[command(flatten)]
    pub c: Common,
    #[arg(long, default_value_t = 0.04, value_parser = non_negative, help = "border around the map")]
    pub margin: f64,
    #[arg(long = "from-txt", help = "reuse the framing from an existing overview .txt (GoldSrc ZOOM/ORIGIN/ROTATED, CS2/Source pos_x/pos_y/scale and sections)")]
    pub from_txt: Option<PathBuf>,
    #[arg(long, help = "also write a transparent PNG")]
    pub png: bool,
    #[arg(long, help = "CS2 and Source: fit the radar to the geometry instead of the map's official overview")]
    pub fit: bool,
    #[arg(long, allow_hyphen_values = true, value_name = "auto|none|Z,..", help = "CS2 and Source: one radar per vertical section: auto, none, or split heights (default: the overview's sections)")]
    pub sections: Option<String>,
    #[arg(long, help = "CS2: also write 32-bit <map>_radar.tga with alpha")]
    pub tga: bool,
    #[arg(long, help = "CS2 and Source: also write the decoded official radar images as PNG")]
    pub official: bool,
    #[arg(long, value_name = "ADDON", help = "CS2: write the radar TGA, .vtex and .txt into this existing csgo_addons addon and compile it with resourcecompiler")]
    pub compile: Option<String>,
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
            tex_max: self.tex_max.max(16),
            cancel: Default::default(),
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

fn number(s: &str, ok: impl Fn(f64) -> bool, want: &str) -> Result<f64, String> {
    let v: f64 = s.trim().parse().map_err(|e| format!("{e}"))?;
    if v.is_finite() && ok(v) { Ok(v) } else { Err(format!("must be {want}")) }
}

pub fn positive(s: &str) -> Result<f64, String> {
    number(s, |v| v > 0.0, "above 0")
}

pub fn non_negative(s: &str) -> Result<f64, String> {
    number(s, |v| v >= 0.0, "0 or more")
}

pub fn at_least_one(s: &str) -> Result<f64, String> {
    number(s, |v| v >= 1.0, "at least 1")
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
        kind: if a.day_cycle { Anim::Day(DayOpts { from: a.day_from, to: a.day_to, turn: !a.no_turn }) } else { Anim::Spin },
        framing: a.cam.framing()?,
    };
    run_anim(&a.c, &o, &a.ex)
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
    run_anim(&a.c, &a.a.opts(&a.c, &a.l, Anim::Peel(p))?, &a.a.ex)
}

pub fn run_slice(a: &SliceArgs) -> Result<()> {
    let s = SliceOpts { slices: a.count, seconds: a.seconds, hold: a.hold, then_spin: a.a.then_spin };
    run_anim(&a.c, &a.a.opts(&a.c, &a.l, Anim::Slice(s))?, &a.a.ex)
}

fn each_map(c: &Common, mut job: impl FnMut(&std::path::Path, &str) -> Result<()>) -> Result<()> {
    let maps = expand_maps(&c.maps, c.game.as_deref());
    if maps.is_empty() {
        anyhow::bail!("no maps match");
    }
    let mut failed = Vec::new();
    for m in &maps {
        let t0 = Instant::now();
        let res = resolve_map(m, c.game.as_deref()).and_then(|path| {
            let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            println!("== {name}");
            job(&path, &name)
        });
        match res {
            Ok(()) => println!("  {:.1}s", t0.elapsed().as_secs_f64()),
            Err(e) if maps.len() == 1 => return Err(e),
            Err(e) => {
                println!("  error: {m}: {e:#}");
                failed.push(m.as_str());
            }
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("{} of {} maps failed: {}", failed.len(), maps.len(), failed.join(", "));
    }
    Ok(())
}

fn in_run_dir<T>(c: &Common, bsp: &crate::bsp::Bsp, f: impl FnOnce(&std::path::Path) -> Result<T>) -> Result<T> {
    let out = run_dir(&c.out, &run_name(bsp))?;
    let res = f(&out);
    if res.is_err() {
        let _ = std::fs::remove_dir(&out);
    }
    res
}

fn run_anim(c: &Common, o: &AnimOpts, ex: &ExplodeArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = c.load_opts();
    let co = c.cut_opts();
    let ex = ex.opts();
    each_map(c, |path, name| {
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut say)?;
        in_run_dir(c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, sky_tag) = scene.job_renderer(&gpu, None, o.look.nearest, o.look.sky_spec(), &mut say)?;
            let mut tag = co.tag(lo.hull) + &scene.apply_explode(&mut r, &ex, &cuts, &mut say);
            if !matches!(o.kind, Anim::Day(_)) {
                scene.log_light(&o.look, &mut say);
                tag += &o.look.light_tag();
            }
            reported(|rep| export_anim(&mut r, &scene.levels, name, &sky_tag, &tag, &cuts, o, out, rep))
        })
        .map(drop)
    })
}

pub fn run_timing(a: &TimingArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let base = crate::grid::Backdrop::from_key(&a.base).unwrap_or_default();
    let o = TimingOpts { speed: a.speed, cell: a.cell, interval: a.interval, size: a.size, base };
    each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut say)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, _) = scene.job_renderer(&gpu, None, a.f.nearest, None, &mut say)?;
            reported(|rep| export_timing(&mut r, &scene.bsp, name, &co.tag(lo.hull), &cuts, &o, out, rep))
        })
        .map(drop)
    })
}

pub fn run_wallbang(a: &WallbangArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = crate::wallbang::WallbangOpts {
        shots: crate::wallbang::shots::ShotOpts {
            weapon: crate::wallbang::sim::weapon(&a.weapon).ok_or_else(|| anyhow::anyhow!("unknown weapon {}", a.weapon))?,
            stand: a.stance != "duck",
            duck: a.stance != "stand",
            yaw_step: a.yaw_step,
            pitch: a.pitch,
            spot_cell: a.spot_cell,
            max_hit: a.max_hit,
            min_thickness: a.min_thickness,
            slab: a.slab,
        },
        ents: crate::wallbang::EntOpts { skip_translucent: a.skip_translucent, skip_doors: a.skip_doors },
        materials: a.materials.clone(),
        areas: a.style != "lines",
        lines: a.style != "areas",
        sigma: a.sigma,
        base: crate::grid::Backdrop::from_key(&a.base).unwrap_or_default(),
        size: a.size,
        split: crate::radar::Split::parse(&a.sections)?,
        others_faint: a.others == "faint",
        csv: a.csv,
        pixelwalks: a.pixelwalks,
    };
    each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut say)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, _) = scene.job_renderer(&gpu, None, a.f.nearest, None, &mut say)?;
            reported(|rep| crate::wallbang::export::export_wallbang(&mut r, &scene.bsp, &scene.dirs, name, &co.tag(lo.hull), &cuts, &o, out, rep))
        })
        .map(drop)
    })
}

pub fn run_pixelwalk(a: &PixelwalkArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = crate::pixelwalk::present::PixelwalkOpts {
        finder: crate::pixelwalk::FinderOpts {
            standing: a.stance != "duck",
            duck: a.stance != "stand",
            min_samples: a.min_samples,
            threads: 0,
        },
        merge_gap: a.merge_gap,
        split: crate::radar::Split::parse(&a.sections)?,
        base: crate::grid::Backdrop::from_key(&a.base).unwrap_or_default(),
        size: a.size,
        labels: a.labels,
    };
    let mut rows: Vec<String> = Vec::new();
    let res = each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut say)?;
        let r = in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, _) = scene.job_renderer(&gpu, None, a.f.nearest, None, &mut say)?;
            reported(|rep| {
                crate::pixelwalk::present::export_pixelwalks(&mut r, &scene.bsp, &scene.dirs, name, &co.tag(lo.hull), &cuts, &o, out, rep)
            })
        });
        match &r {
            Ok((_, s)) => rows.push(format!("{},{},{},{:.1},", csv_quote(name), s.stand, s.duck, s.longest)),
            Err(e) => rows.push(format!("{},,,,{}", csv_quote(name), csv_quote(&format!("error: {e:#}")))),
        }
        r.map(drop)
    });
    if rows.len() > 1 {
        std::fs::create_dir_all(&a.c.out)?;
        let f = free_name(&a.c.out, "pixelwalk_summary", ".csv");
        let mut text = String::from("map,standing,duck,longest_span,error\n");
        for r in &rows {
            text.push_str(r);
            text.push('\n');
        }
        std::fs::write(&f, text)?;
        println!("summary: {}", f.display());
    }
    res
}

fn kill_map(
    gpu: &Gpu,
    c: &Common,
    nearest: bool,
    path: &std::path::Path,
    o: &KillOpts,
    data: &[DemoData],
    rep: &mut crate::scene::Report,
) -> Result<(PathBuf, KillStats)> {
    let lo = c.load_opts();
    let co = c.cut_opts();
    let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let scene = Scene::load(path, &lo, gpu.max_dim, rep.log)?;
    in_run_dir(c, &scene.bsp, |out| {
        let cuts = scene.cuts(&co, rep.log);
        let (mut r, _) = scene.job_renderer(gpu, None, nearest, None, rep.log)?;
        let (_, s) = export_kills(&mut r, &scene.bsp, &name, &co.tag(lo.hull), &cuts, o, data, out, rep, 0.0)?;
        Ok((out.to_path_buf(), s))
    })
}

pub fn run_kills(a: &KillsArgs) -> Result<()> {
    let t0 = Instant::now();
    let o = KillOpts {
        weapons: a.weapon.iter().map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()).collect(),
        team: TeamSel::parse(&a.team).unwrap_or(TeamSel::Both),
        headshots: a.headshots,
        lines: a.lines,
        rounds: a.rounds,
        radius: a.radius.max(1.0),
        presence: a.presence_every > 0.0,
        presence_every: a.presence_every,
        size: a.size,
        base: crate::grid::Backdrop::from_key(&a.base).unwrap_or_default(),
    };
    let files = expand_demos(&a.demos);
    if files.is_empty() {
        anyhow::bail!("no demos found");
    }
    let heads: Vec<(PathBuf, String)> = files
        .iter()
        .filter_map(|f| match demo::map_name(f) {
            Ok(m) => Some((f.clone(), m)),
            Err(e) => {
                println!("skipped {}: {e:#}", f.display());
                None
            }
        })
        .collect();
    if heads.is_empty() {
        anyhow::bail!("no readable demos");
    }
    let gpu = Gpu::headless()?;
    let game = a.c.game.as_deref();

    if !a.c.maps.is_empty() {
        each_map(&a.c, |path, name| {
            let (mine, other): (Vec<_>, Vec<_>) = heads.iter().partition(|(_, map)| map.eq_ignore_ascii_case(name));
            if !other.is_empty() {
                println!("  skipped {} demos of other maps", other.len());
                if other.len() <= 10 {
                    for (f, map) in &other {
                        println!("    {} ({map})", f.file_name().unwrap_or_default().to_string_lossy());
                    }
                }
            }
            if mine.is_empty() {
                println!("  no demos of {name}");
                return Ok(());
            }
            let paths: Vec<PathBuf> = mine.iter().map(|(f, _)| f.clone()).collect();
            reported(|rep| {
                let data = load_demos(&paths, &o, rep, (0.0, 1.0))?;
                println!("  {} demos parsed", data.len());
                let (out, s) = kill_map(&gpu, &a.c, a.f.nearest, path, &o, &data, rep)?;
                println!("  {} deaths, {} drawn: {}", s.deaths, s.drawn, out.display());
                Ok(())
            })
        })?;
        println!("{:.1}s", t0.elapsed().as_secs_f64());
        return Ok(());
    }

    let mut groups: std::collections::BTreeMap<String, Vec<PathBuf>> = std::collections::BTreeMap::new();
    for (f, map) in &heads {
        groups.entry(map.to_lowercase()).or_default().push(f.clone());
    }
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for (map, fs) in &groups {
        match resolve_map(map, game) {
            Ok(p) => found.push((map.clone(), p, fs.clone())),
            Err(_) => missing.push((map.clone(), fs.len())),
        }
    }
    println!("{} demos of {} maps; {} maps have no local .bsp", heads.len(), groups.len(), missing.len());
    for (m, n) in &missing {
        println!("  skipped {m} ({n} demo{})", if *n == 1 { "" } else { "s" });
    }
    let paths: Vec<PathBuf> = found.iter().flat_map(|(_, _, fs)| fs.iter().cloned()).collect();
    let tp = Instant::now();
    let data = reported(|rep| load_demos(&paths, &o, rep, (0.0, 1.0)))?;
    println!("parsed {} demos in {:.1}s", data.len(), tp.elapsed().as_secs_f64());
    let mut by_map: std::collections::HashMap<String, Vec<DemoData>> = std::collections::HashMap::new();
    for d in data {
        by_map.entry(d.map.to_lowercase()).or_default().push(d);
    }
    std::fs::create_dir_all(&a.c.out)?;
    let summary = free_name(&a.c.out, "kills_summary", ".csv");
    let mut rows = Vec::new();
    let mut failed = 0;
    for (map, path, _) in &found {
        let t1 = Instant::now();
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        println!("== {name}");
        let data = by_map.remove(map).unwrap_or_default();
        if data.is_empty() {
            rows.push(format!("{},no demos parsed,0,,,,,,,,,,", csv_quote(&name)));
            continue;
        }
        match reported(|rep| kill_map(&gpu, &a.c, a.f.nearest, path, &o, &data, rep)) {
            Ok((out, s)) => {
                println!("  {} demos, {} deaths, {} drawn ({:.1}s)", s.demos, s.deaths, s.drawn, t1.elapsed().as_secs_f64());
                rows.push(s.csv_row(&name, &out));
            }
            Err(e) => {
                failed += 1;
                println!("  error: {e:#}");
                rows.push(format!("{},{},{},,,,,,,,,,", csv_quote(&name), csv_quote(&format!("error: {e:#}")), data.len()));
            }
        }
    }
    for (m, n) in &missing {
        rows.push(format!("{},no bsp,{n},,,,,,,,,,", csv_quote(m)));
    }
    let mut text = format!("{}\n", KillStats::csv_header());
    for r in &rows {
        text.push_str(r);
        text.push('\n');
    }
    std::fs::write(&summary, text)?;
    println!(
        "{} maps done, {failed} failed, {} skipped without .bsp: {} ({:.1}s)",
        found.len() - failed,
        missing.len(),
        summary.display(),
        t0.elapsed().as_secs_f64()
    );
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
    let ex = a.ex.opts();
    each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut say)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, sky_tag) = scene.job_renderer(&gpu, None, o.look.nearest, o.look.sky_spec(), &mut say)?;
            let tag = co.tag(lo.hull) + &scene.apply_explode(&mut r, &ex, &cuts, &mut say) + &o.look.light_tag();
            scene.log_light(&o.look, &mut say);
            reported(|rep| export_iso(&mut r, &scene.bsp, name, &sky_tag, &tag, &cuts, &o, out, rep))
        })
        .map(drop)
    })
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
        fit: a.fit,
        split: a.sections.as_deref().map(crate::radar::Split::parse).transpose()?.unwrap_or_default(),
        tga: a.tga,
        official: a.official,
        addon: a.compile.clone().unwrap_or_default(),
    };
    each_map(&a.c, |path, name| {
        if !o.addon.trim().is_empty() {
            if !crate::s2::is_vpk(path) {
                anyhow::bail!("--compile is for CS2 maps only: {}", path.display());
            }
            crate::addon::Addon::find(crate::s2::vpk::game_root(path, a.c.game.as_deref()).as_deref(), &o.addon)?;
        }
        let mut log = |s: String| println!("  {s}");
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut log)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut log);
            let (mut r, _) = scene.job_renderer(&gpu, None, a.f.nearest, None, &mut log)?;
            reported(|rep| export_overview(&mut r, &scene.bsp, name, &cuts, &o, out, rep))
        })
        .map(drop)
    })
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
    let base = SvgOpts { cell: a.cell, simplify: a.simplify, scale: parse_scale(&a.scale)?, bands: a.bands, planes: a.planes.clone() };
    let ex = a.ex.opts();
    each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut say)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, _) = scene.job_renderer(&gpu, None, false, None, &mut say)?;
            let mut o = base.clone();
            if let Some(e) = ex.resolve(&scene.levels, &cuts) {
                say(format!("explode: split at z {}", e.planes.iter().map(|p| crate::scene::num(*p)).collect::<Vec<_>>().join(", ")));
                o.planes = Some(e.planes);
            }
            reported(|rep| export_svg(&mut r, &scene, name, &co.tag(lo.hull), &cuts, &o, out, rep))
        })
        .map(drop)
    })
}

pub fn run_stl(a: &StlArgs) -> Result<()> {
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = StlOpts { voxel: a.voxel.unwrap_or(8.0), auto: a.voxel.is_none(), wall: a.wall, base: a.base, print_width: a.print_width, smooth: a.smooth };
    each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, 8192, &mut say)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            reported(|rep| export_stl(&scene.bsp, name, &co.tag(lo.hull), &cuts, &o, out, rep))
        })
        .map(drop)
    })
}

pub fn run_gltf(a: &GltfArgs, obj: bool) -> Result<()> {
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let chosen = a.lighting.as_deref().and_then(Lighting::parse);
    each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, 8192, &mut say)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let lighting = chosen.unwrap_or(match (scene.bsp.modern(), obj) {
                (false, _) => Lighting::Baked,
                (true, false) => Lighting::Separate,
                (true, true) => Lighting::None,
            });
            let o = GltfOpts { lighting, texel: a.texel, nearest: a.nearest, obj };
            reported(|rep| export_gltf(&scene, name, &co.tag(lo.hull), &cuts, &o, out, rep))
        })
        .map(drop)
    })
}

pub fn run_poster(a: &PosterArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = PosterOpts {
        paper: a.paper.clone(),
        dpi: a.dpi.clamp(10.0, 2400.0),
        orient: if a.landscape { Orient::Landscape } else if a.portrait { Orient::Portrait } else { Orient::Auto },
        px: a.px.as_ref().map(|v| [v[0], v[1]]),
        top: a.top,
        yaw: a.yaw,
        pitch: a.pitch,
        ss: a.c.ss,
        layout: !a.no_layout,
        look: a.l.look()?,
        framing: a.cam.framing()?,
        tile: a.tile,
    };
    let ex = a.ex.opts();
    each_map(&a.c, |path, name| {
        let scene = Scene::load(path, &lo, gpu.max_dim, &mut say)?;
        in_run_dir(&a.c, &scene.bsp, |out| {
            let cuts = scene.cuts(&co, &mut say);
            let (mut r, sky_tag) = scene.job_renderer(&gpu, None, o.look.nearest, o.look.sky_spec(), &mut say)?;
            let tag = co.tag(lo.hull) + &scene.apply_explode(&mut r, &ex, &cuts, &mut say) + &o.look.light_tag();
            scene.log_light(&o.look, &mut say);
            reported(|rep| export_poster(&mut r, &scene.bsp, name, &sky_tag, &tag, &cuts, &o, out, rep))
        })
        .map(drop)
    })
}

pub fn run_health(a: &HealthArgs) -> Result<()> {
    let gpu = Gpu::headless()?;
    let lo = a.c.load_opts();
    let co = a.c.cut_opts();
    let o = HealthOpts { cell: a.cell, size: a.size, pixelwalks: a.pixelwalks };
    let maps = expand_maps(&a.c.maps, a.c.game.as_deref());
    if maps.is_empty() {
        anyhow::bail!("no maps match");
    }
    let summary = (maps.len() > 1).then(|| {
        std::fs::create_dir_all(&a.c.out).ok();
        free_name(&a.c.out, "health_summary", ".csv")
    });
    let header = crate::health::Health::csv_header();
    let mut rows = Vec::new();
    let mut failed = 0;
    for m in &maps {
        let t0 = Instant::now();
        println!("== {m}");
        let res = (|| -> Result<crate::health::Health> {
            let path = resolve_map(m, a.c.game.as_deref())?;
            let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let scene = Scene::load(&path, &lo, gpu.max_dim, &mut say)?;
            in_run_dir(&a.c, &scene.bsp, |out| {
                let cuts = scene.cuts(&co, &mut say);
                let (mut r, _) = scene.job_renderer(&gpu, None, a.f.nearest, None, &mut say)?;
                Ok(reported(|rep| export_health(&mut r, &scene, &name, &co.tag(lo.hull), &cuts, &o, out, rep))?.1)
            })
        })();
        match res {
            Ok(h) => rows.push(h.csv_row()),
            Err(e) if summary.is_some() => {
                failed += 1;
                println!("  error: {e:#}");
                rows.push(format!("{},,{}", csv_quote(m), csv_quote(&format!("{e:#}"))));
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
