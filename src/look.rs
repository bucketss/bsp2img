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

pub const BLUEPRINT_BG: [u8; 3] = [0x1d, 0x3b, 0x6e];
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
        (self.saturation, self.tint, self.tint_amount) = (d.saturation, d.tint, d.tint_amount);
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
            ..Look::default()
        };
        if let Some(s) = self.style.as_deref().and_then(Style::parse) {
            l.apply_style(s);
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
