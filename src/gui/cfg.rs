use std::path::PathBuf;
use std::str::FromStr;

use super::{App, Exporter, Tab};
use crate::camera::Camera;
use crate::export::{IsoOpts, OverviewOpts};
use crate::health::HealthOpts;
use crate::look::{Look, Tilt};
use crate::render::parse_color;
use crate::scene::LoadOpts;
use crate::spin::{AnimOpts, PeelOpts, SliceOpts};
use crate::svg::SvgOpts;
use crate::stl::StlOpts;
use crate::timing::TimingOpts;

#[cfg(windows)]
fn cfg_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(not(windows))]
fn cfg_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn cfg_path() -> Option<PathBuf> {
    cfg_dir().map(|d| d.join("bsp2img").join("gui.cfg"))
}

pub trait Cfg {
    fn kv(&self) -> Vec<(String, String)>;
    fn set(&mut self, k: &str, v: &str);
}

fn put<T: FromStr>(t: &mut T, v: &str) {
    if let Ok(x) = v.trim().parse() {
        *t = x;
    }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn kvs(items: &[(&str, String)]) -> Vec<(String, String)> {
    items.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

impl Cfg for IsoOpts {
    fn kv(&self) -> Vec<(String, String)> {
        let yaws: Vec<String> = self.yaws.iter().map(|y| y.to_string()).collect();
        kvs(&[
            ("size", self.size.to_string()),
            ("ss", self.ss.to_string()),
            ("pitch", self.pitch.to_string()),
            ("yaws", yaws.join(" ")),
            ("grid", self.grid.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "size" => put(&mut self.size, v),
            "ss" => put(&mut self.ss, v),
            "pitch" => put(&mut self.pitch, v),
            "yaws" => self.yaws = v.split_whitespace().filter_map(|y| y.parse().ok()).collect(),
            "grid" => put(&mut self.grid, v),
            _ => {}
        }
    }
}

impl Cfg for AnimOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("size", self.size.to_string()),
            ("ss", self.ss.to_string()),
            ("seconds", self.seconds.to_string()),
            ("fps", self.fps.to_string()),
            ("start", self.start.to_string()),
            ("ccw", self.ccw.to_string()),
            ("gif", self.gif.to_string()),
            ("mp4", self.mp4.to_string()),
            ("apng", self.apng.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "size" => put(&mut self.size, v),
            "ss" => put(&mut self.ss, v),
            "seconds" => put(&mut self.seconds, v),
            "fps" => put(&mut self.fps, v),
            "start" => put(&mut self.start, v),
            "ccw" => put(&mut self.ccw, v),
            "gif" => put(&mut self.gif, v),
            "mp4" => put(&mut self.mp4, v),
            "apng" => put(&mut self.apng, v),
            _ => {}
        }
    }
}

impl Cfg for PeelOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("roofs", self.roofs.to_string()),
            ("seconds_per", self.seconds_per.to_string()),
            ("hold", self.hold.to_string()),
            ("reverse", self.reverse.to_string()),
            ("then_spin", self.then_spin.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "roofs" => put(&mut self.roofs, v),
            "seconds_per" => put(&mut self.seconds_per, v),
            "hold" => put(&mut self.hold, v),
            "reverse" => put(&mut self.reverse, v),
            "then_spin" => put(&mut self.then_spin, v),
            _ => {}
        }
    }
}

impl Cfg for SliceOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("slices", self.slices.to_string()),
            ("seconds", self.seconds.to_string()),
            ("hold", self.hold.to_string()),
            ("then_spin", self.then_spin.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "slices" => put(&mut self.slices, v),
            "seconds" => put(&mut self.seconds, v),
            "hold" => put(&mut self.hold, v),
            "then_spin" => put(&mut self.then_spin, v),
            _ => {}
        }
    }
}

impl Cfg for OverviewOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("margin", self.margin.to_string()),
            ("ss", self.ss.to_string()),
            ("png", self.png.to_string()),
            ("grid", self.grid.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "margin" => put(&mut self.margin, v),
            "ss" => put(&mut self.ss, v),
            "png" => put(&mut self.png, v),
            "grid" => put(&mut self.grid, v),
            _ => {}
        }
    }
}

impl Cfg for TimingOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("speed", self.speed.to_string()),
            ("cell", self.cell.to_string()),
            ("interval", self.interval.to_string()),
            ("size", self.size.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "speed" => put(&mut self.speed, v),
            "cell" => put(&mut self.cell, v),
            "interval" => put(&mut self.interval, v),
            "size" => put(&mut self.size, v),
            _ => {}
        }
    }
}

impl Cfg for HealthOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[("cell", self.cell.to_string()), ("size", self.size.to_string())])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "cell" => put(&mut self.cell, v),
            "size" => put(&mut self.size, v),
            _ => {}
        }
    }
}

impl Cfg for SvgOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("cell", self.cell.to_string()),
            ("simplify", self.simplify.to_string()),
            ("scale", self.scale.to_string()),
            ("bands", self.bands.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "cell" => put(&mut self.cell, v),
            "simplify" => put(&mut self.simplify, v),
            "scale" => put(&mut self.scale, v),
            "bands" => put(&mut self.bands, v),
            _ => {}
        }
    }
}

impl Cfg for StlOpts {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("voxel", self.voxel.to_string()),
            ("wall", self.wall.to_string()),
            ("base", self.base.to_string()),
            ("print_width", self.print_width.to_string()),
            ("smooth", self.smooth.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "voxel" => put(&mut self.voxel, v),
            "wall" => put(&mut self.wall, v),
            "base" => put(&mut self.base, v),
            "print_width" => put(&mut self.print_width, v),
            "smooth" => put(&mut self.smooth, v),
            _ => {}
        }
    }
}

impl Cfg for Camera {
    fn kv(&self) -> Vec<(String, String)> {
        Camera::kv(self)
    }

    fn set(&mut self, k: &str, v: &str) {
        Camera::set(self, k, v)
    }
}

impl Cfg for Look {
    fn kv(&self) -> Vec<(String, String)> {
        kvs(&[
            ("bg", self.bg.map(hex).unwrap_or_default()),
            ("sky", self.sky.to_string()),
            ("sky_name", self.sky_name.clone()),
            ("sky_fov", self.sky_fov.to_string()),
            ("sky_pitch", self.sky_pitch.to_string()),
            ("cull", self.cull.to_string()),
            ("nearest", self.nearest.to_string()),
            ("anim_textures", self.anim_textures.to_string()),
            ("ao", self.ao.to_string()),
            ("ao_strength", self.ao_strength.to_string()),
            ("ao_radius", self.ao_radius.to_string()),
            ("ink", self.ink.to_string()),
            ("ink_width", self.ink_width.to_string()),
            ("ink_color", hex(self.ink_color)),
            ("saturation", self.saturation.to_string()),
            ("tint", hex(self.tint)),
            ("tint_amount", self.tint_amount.to_string()),
            ("contrast", self.contrast.to_string()),
            ("tilt", self.tilt.key().to_string()),
            ("focus_y", self.focus_y.to_string()),
            ("band", self.band.to_string()),
            ("blur", self.blur.to_string()),
            ("focus_dist", self.focus_dist.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "bg" => self.bg = parse_color(v).ok(),
            "sky" => put(&mut self.sky, v),
            "sky_name" => self.sky_name = v.to_string(),
            "sky_fov" => put(&mut self.sky_fov, v),
            "sky_pitch" => put(&mut self.sky_pitch, v),
            "cull" => put(&mut self.cull, v),
            "nearest" => put(&mut self.nearest, v),
            "anim_textures" => put(&mut self.anim_textures, v),
            "ao" => put(&mut self.ao, v),
            "ao_strength" => put(&mut self.ao_strength, v),
            "ao_radius" => put(&mut self.ao_radius, v),
            "ink" => put(&mut self.ink, v),
            "ink_width" => put(&mut self.ink_width, v),
            "ink_color" => self.ink_color = parse_color(v).unwrap_or(self.ink_color),
            "saturation" => put(&mut self.saturation, v),
            "tint" => self.tint = parse_color(v).unwrap_or(self.tint),
            "tint_amount" => put(&mut self.tint_amount, v),
            "contrast" => put(&mut self.contrast, v),
            "tilt" => self.tilt = Tilt::parse(v).unwrap_or(self.tilt),
            "focus_y" => put(&mut self.focus_y, v),
            "band" => put(&mut self.band, v),
            "blur" => put(&mut self.blur, v),
            "focus_dist" => put(&mut self.focus_dist, v),
            _ => {}
        }
    }
}

impl Cfg for LoadOpts {
    fn kv(&self) -> Vec<(String, String)> {
        let l = &self.light;
        kvs(&[
            ("auto_crop", self.auto_crop.to_string()),
            ("hull", self.hull.map(|h| h.to_string()).unwrap_or_default()),
            ("hull_pad", self.hull_pad.to_string()),
            ("gamma", l.gamma.to_string()),
            ("texgamma", l.texgamma.to_string()),
            ("lightgamma", l.lightgamma.to_string()),
            ("brightness", l.brightness.to_string()),
            ("light_scale", l.scale.to_string()),
            ("all_styles", l.all_styles.to_string()),
        ])
    }

    fn set(&mut self, k: &str, v: &str) {
        let l = &mut self.light;
        match k {
            "auto_crop" => put(&mut self.auto_crop, v),
            "hull" => self.hull = v.trim().parse().ok(),
            "hull_pad" => put(&mut self.hull_pad, v),
            "gamma" => put(&mut l.gamma, v),
            "texgamma" => put(&mut l.texgamma, v),
            "lightgamma" => put(&mut l.lightgamma, v),
            "brightness" => put(&mut l.brightness, v),
            "light_scale" => put(&mut l.scale, v),
            "all_styles" => put(&mut l.all_styles, v),
            _ => {}
        }
    }
}

impl App {
    fn cfg_text(&self) -> String {
        let mut iso = self.iso.clone();
        iso.yaws = self.yaws();
        let mut lines = vec![
            format!("game={}", self.game),
            format!("out={}", self.out),
            format!("tab={}", self.tab.key()),
            format!("exporter={}", self.exporter.key()),
            format!("bg_last={}", hex(self.bg_last)),
            format!("log_open={}", self.log_open),
            format!("cam_export={}", self.cam_export),
        ];
        let sections: [(&str, &dyn Cfg); 12] = [
            ("load", &self.wanted_load()),
            ("look", &self.look),
            ("iso", &iso),
            ("spin", &self.spin),
            ("peel", &self.peel),
            ("slice", &self.slice),
            ("overview", &self.ov),
            ("timing", &self.timing),
            ("health", &self.health),
            ("svg", &self.svg),
            ("stl", &self.stl),
            ("cam", &self.cam),
        ];
        for (s, c) in sections {
            lines.extend(c.kv().into_iter().map(|(k, v)| format!("{s}.{k}={v}")));
        }
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    pub(super) fn load_cfg(&mut self) {
        let text = cfg_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.split_once('.') {
                Some(("load", k)) => self.load.set(k, v),
                Some(("look", k)) => self.look.set(k, v),
                Some(("iso", k)) => self.iso.set(k, v),
                Some(("spin", k)) => self.spin.set(k, v),
                Some(("peel", k)) => self.peel.set(k, v),
                Some(("slice", k)) => self.slice.set(k, v),
                Some(("overview", k)) => self.ov.set(k, v),
                Some(("timing", k)) => self.timing.set(k, v),
                Some(("health", k)) => self.health.set(k, v),
                Some(("svg", k)) => self.svg.set(k, v),
                Some(("stl", k)) => self.stl.set(k, v),
                Some(("cam", k)) => Cfg::set(&mut self.cam, k, v),
                Some(_) => {}
                None => match k {
                    "game" => self.game = v.to_string(),
                    "out" => self.out = v.to_string(),
                    "tab" => self.tab = Tab::from_key(v).unwrap_or(self.tab),
                    "exporter" => self.exporter = Exporter::from_key(v).unwrap_or(self.exporter),
                    "bg_last" => self.bg_last = parse_color(v).unwrap_or(self.bg_last),
                    "log_open" => put(&mut self.log_open, v),
                    "cam_export" => put(&mut self.cam_export, v),
                    _ => {}
                },
            }
        }
        self.hull_on = self.load.hull.is_some();
        self.hull_kind = self.load.hull.unwrap_or(3);
        self.load.hull = None;
        let yaws: Vec<String> = self.iso.yaws.iter().map(|y| y.to_string()).collect();
        self.yaws_text = yaws.join(" ");
        self.cfg_saved = self.cfg_text();
    }

    pub(super) fn save_cfg(&mut self) {
        let text = self.cfg_text();
        if text == self.cfg_saved {
            return;
        }
        if let Some(p) = cfg_path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            if std::fs::write(p, &text).is_ok() {
                self.cfg_saved = text;
            }
        }
    }
}
