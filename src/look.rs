use anyhow::Result;
use clap::Args;

use crate::render::parse_color;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    Blueprint,
    Comic,
}

impl Style {
    pub fn parse(s: &str) -> Option<Style> {
        match s.trim().to_lowercase().as_str() {
            "blueprint" => Some(Style::Blueprint),
            "comic" => Some(Style::Comic),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tilt {
    Off,
    Shift,
    Dof,
}

impl Tilt {
    pub fn key(self) -> &'static str {
        match self {
            Tilt::Off => "off",
            Tilt::Shift => "tilt-shift",
            Tilt::Dof => "dof",
        }
    }

    pub fn parse(s: &str) -> Option<Tilt> {
        [Tilt::Off, Tilt::Shift, Tilt::Dof].into_iter().find(|t| t.key() == s.trim())
    }
}

pub const BLUEPRINT_BG: [u8; 3] = [0x1d, 0x3b, 0x6e];
pub const MINI_SATURATION: f64 = 1.25;
pub const MINI_CONTRAST: f64 = 1.1;
pub const INK_WIDTH: f64 = 1.5;
pub const AO_RADIUS: f64 = 48.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub bg: Option<[u8; 3]>,
    pub sky: bool,
    pub sky_name: String,
    pub sky_fov: f64,
    pub sky_pitch: f64,
    pub cull: bool,
    pub nearest: bool,
    pub anim_textures: bool,
    pub ao: bool,
    pub ao_strength: f64,
    pub ao_radius: f64,
    pub ink: bool,
    pub ink_width: f64,
    pub ink_color: [u8; 3],
    pub saturation: f64,
    pub tint: [u8; 3],
    pub tint_amount: f64,
    pub contrast: f64,
    pub tilt: Tilt,
    pub focus_y: f64,
    pub band: f64,
    pub blur: f64,
    pub focus_dist: f64,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            bg: None,
            sky: false,
            sky_name: String::new(),
            sky_fov: 90.0,
            sky_pitch: 10.0,
            cull: true,
            nearest: false,
            anim_textures: false,
            ao: false,
            ao_strength: 1.0,
            ao_radius: AO_RADIUS,
            ink: false,
            ink_width: INK_WIDTH,
            ink_color: [0, 0, 0],
            saturation: 1.0,
            tint: BLUEPRINT_BG,
            tint_amount: 0.0,
            contrast: 1.0,
            tilt: Tilt::Off,
            focus_y: 0.5,
            band: 0.2,
            blur: 12.0,
            focus_dist: 0.0,
        }
    }
}

impl Look {
    pub fn plain(cull: bool, bg: Option<[u8; 3]>) -> Look {
        Look { cull, bg, ..Look::default() }
    }

    pub fn sky_spec(&self) -> Option<(String, f64, f64)> {
        self.sky.then(|| (self.sky_name.trim().to_string(), self.sky_fov, self.sky_pitch))
    }

    pub fn clear_effects(&mut self) {
        let d = Look::default();
        (self.ao, self.ink, self.ink_width, self.ink_color) = (d.ao, d.ink, d.ink_width, d.ink_color);
        (self.saturation, self.tint, self.tint_amount, self.contrast) = (d.saturation, d.tint, d.tint_amount, d.contrast);
    }

    pub fn miniature(&mut self) {
        if self.tilt == Tilt::Off {
            self.tilt = Tilt::Shift;
        }
        self.saturation *= MINI_SATURATION;
        self.contrast *= MINI_CONTRAST;
    }

    pub fn apply_style(&mut self, s: Style) {
        self.clear_effects();
        match s {
            Style::Blueprint => {
                self.bg = Some(BLUEPRINT_BG);
                self.saturation = 0.0;
                self.tint = BLUEPRINT_BG;
                self.tint_amount = 0.25;
                self.ink = true;
                self.ink_color = [255, 255, 255];
                self.ao = false;
            }
            Style::Comic => {
                self.ao = true;
                self.ink = true;
                self.ink_color = [0, 0, 0];
                self.ink_width = 2.0;
                self.saturation = 1.2;
            }
        }
    }
}

#[derive(Args, Clone)]
pub struct FaceArgs {
    #[arg(long = "no-cull", help = "draw back faces")]
    pub no_cull: bool,
    #[arg(long, help = "pixelated texture filtering")]
    pub nearest: bool,
}

#[derive(Args, Clone)]
pub struct LookArgs {
    #[arg(long, help = "background colour, e.g. #202020 (default transparent; spin videos use #202020)")]
    pub bg: Option<String>,
    #[arg(long, num_args = 0..=1, value_name = "NAME",
          help = "draw the skybox behind the map (map's own sky, or NAME from gfx/env)")]
    pub sky: Option<Option<String>>,
    #[arg(long = "sky-fov", default_value_t = 90.0)]
    pub sky_fov: f64,
    #[arg(long = "sky-pitch", default_value_t = 10.0)]
    pub sky_pitch: f64,
    #[command(flatten)]
    pub f: FaceArgs,
    #[arg(long = "animate-textures", help = "play +0..+9 texture sequences and warp ! water (animations)")]
    pub animate_textures: bool,
    #[arg(long, help = "ambient occlusion")]
    pub ao: bool,
    #[arg(long = "ao-strength", default_value_t = 1.0)]
    pub ao_strength: f64,
    #[arg(long = "ao-radius", default_value_t = AO_RADIUS, help = "occlusion radius in world units")]
    pub ao_radius: f64,
    #[arg(long, help = "ink outlines")]
    pub ink: bool,
    #[arg(long = "ink-width", help = "outline width in output pixels [default: 1.5]")]
    pub ink_width: Option<f64>,
    #[arg(long = "ink-color", help = "outline colour [default: #000000]")]
    pub ink_color: Option<String>,
    #[arg(long, value_parser = ["blueprint", "comic"], help = "preset: blueprint or comic")]
    pub style: Option<String>,
    #[arg(long, help = "colour saturation, 1 = unchanged")]
    pub saturation: Option<f64>,
    #[arg(long, help = "tint colour, e.g. #1d3b6e")]
    pub tint: Option<String>,
    #[arg(long = "tint-amount", help = "0..1")]
    pub tint_amount: Option<f64>,
    #[arg(long, help = "contrast, 1 = unchanged")]
    pub contrast: Option<f64>,
    #[arg(long = "tilt-shift", help = "blur above and below a horizontal focus band")]
    pub tilt_shift: bool,
    #[arg(long, help = "depth of field blur around the focus distance (perspective only)")]
    pub dof: bool,
    #[arg(long, help = "tilt-shift with saturation +25% and contrast +10%")]
    pub miniature: bool,
    #[arg(long = "focus-y", default_value_t = 0.5, help = "centre of the focus band, 0 = top, 1 = bottom")]
    pub focus_y: f64,
    #[arg(long, default_value_t = 0.2, help = "sharp band height as a fraction of the image (tilt-shift) or of the focus distance (dof)")]
    pub band: f64,
    #[arg(long, default_value_t = 12.0, help = "largest blur radius in output pixels")]
    pub blur: f64,
    #[arg(long = "focus-dist", help = "dof focus distance in units [default: camera target]")]
    pub focus_dist: Option<f64>,
}

impl LookArgs {
    pub fn look(&self) -> Result<Look> {
        let mut l = Look {
            bg: None,
            sky: self.sky.is_some(),
            sky_name: self.sky.clone().flatten().unwrap_or_default(),
            sky_fov: self.sky_fov,
            sky_pitch: self.sky_pitch,
            cull: !self.f.no_cull,
            nearest: self.f.nearest,
            anim_textures: self.animate_textures,
            ao_strength: self.ao_strength,
            ao_radius: self.ao_radius,
            focus_y: self.focus_y.clamp(0.0, 1.0),
            band: self.band.max(0.0),
            blur: self.blur.max(0.0),
            focus_dist: self.focus_dist.unwrap_or(0.0).max(0.0),
            ..Look::default()
        };
        if let Some(s) = self.style.as_deref().and_then(Style::parse) {
            l.apply_style(s);
        }
        if self.dof {
            l.tilt = Tilt::Dof;
        } else if self.tilt_shift {
            l.tilt = Tilt::Shift;
        }
        if self.miniature {
            l.miniature();
        }
        if let Some(c) = self.contrast {
            l.contrast = c;
        }
        if let Some(bg) = &self.bg {
            l.bg = Some(parse_color(bg)?);
        }
        l.ao |= self.ao;
        l.ink |= self.ink;
        if let Some(w) = self.ink_width {
            l.ink_width = w;
        }
        if let Some(c) = &self.ink_color {
            l.ink_color = parse_color(c)?;
        }
        if let Some(s) = self.saturation {
            l.saturation = s;
        }
        if let Some(c) = &self.tint {
            l.tint = parse_color(c)?;
            if self.tint_amount.is_none() && l.tint_amount == 0.0 {
                l.tint_amount = 0.25;
            }
        }
        if let Some(a) = self.tint_amount {
            l.tint_amount = a.clamp(0.0, 1.0);
        }
        Ok(l)
    }
}
