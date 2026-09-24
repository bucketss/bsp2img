use anyhow::Result;
use clap::Args;

use crate::render::parse_color;

#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub bg: Option<[u8; 3]>,
    pub sky: bool,
    pub sky_name: String,
    pub sky_fov: f64,
    pub sky_pitch: f64,
    pub cull: bool,
    pub nearest: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look { bg: None, sky: false, sky_name: String::new(), sky_fov: 90.0, sky_pitch: 10.0, cull: true, nearest: false }
    }
}

impl Look {
    pub fn sky_spec(&self) -> Option<(String, f64, f64)> {
        self.sky.then(|| (self.sky_name.trim().to_string(), self.sky_fov, self.sky_pitch))
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
}

impl LookArgs {
    pub fn look(&self) -> Result<Look> {
        Ok(Look {
            bg: self.bg.as_deref().map(parse_color).transpose()?,
            sky: self.sky.is_some(),
            sky_name: self.sky.clone().flatten().unwrap_or_default(),
            sky_fov: self.sky_fov,
            sky_pitch: self.sky_pitch,
            cull: !self.f.no_cull,
            nearest: self.f.nearest,
        })
    }
}
