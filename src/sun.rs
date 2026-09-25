use anyhow::{Result, bail};
use glam::DVec3;

use crate::bsp::Bsp;
use crate::look::Look;

pub const SUNRISE: f64 = 5.0;
pub const SUNSET: f64 = 19.0;
const SUN_GAIN: f64 = 0.72;
const MOON_GAIN: f64 = 0.36;
const MAP_TINT: f64 = 0.5;
const DEFAULT_PITCH: f64 = -60.0;
const DEFAULT_YAW: f64 = 45.0;
const DEFAULT_COLOR: [f64; 3] = [1.0, 0.95, 0.85];
const DAY_SKY: [f64; 3] = [0.36, 0.39, 0.45];
const MOON_COLOR: [f64; 3] = [0.55, 0.66, 1.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunEnv {
    pub yaw: f64,
    pub pitch: f64,
    pub color: [f64; 3],
    pub sky: Option<[f64; 3]>,
    pub found: bool,
}

impl Default for SunEnv {
    fn default() -> Self {
        SunEnv { yaw: DEFAULT_YAW, pitch: DEFAULT_PITCH, color: DEFAULT_COLOR, sky: None, found: false }
    }
}

impl SunEnv {
    pub fn describe(&self) -> String {
        let c = self.color.map(|v| (v * 255.0).round() as u8);
        let src = if self.found { "light_environment" } else { "no light_environment, default" };
        format!("{src}: yaw {:.0}, pitch {:.0}, colour {} {} {}", self.yaw, self.pitch, c[0], c[1], c[2])
    }
}

fn nums(s: Option<&str>) -> Vec<f64> {
    s.map(|v| v.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default()
}

fn color(s: Option<&str>) -> Option<[f64; 3]> {
    let v = nums(s);
    if v.len() < 3 {
        return None;
    }
    let m = v[0].max(v[1]).max(v[2]);
    (m > 0.0).then(|| [v[0] / m, v[1] / m, v[2] / m])
}

pub fn sun_env(bsp: &Bsp) -> SunEnv {
    let Some(e) = bsp.entities.iter().find(|e| e.class() == "light_environment") else {
        return SunEnv::default();
    };
    let angles = nums(e.get("angles"));
    let a = |i: usize| angles.get(i).copied().unwrap_or(0.0);
    let angle = nums(e.get("angle")).first().copied().unwrap_or(0.0);
    let mut yaw = if angle != 0.0 { angle } else { a(1) };
    let mut pitch = nums(e.get("pitch")).first().copied().filter(|p| *p != 0.0).unwrap_or(a(0));
    if angle == -1.0 || angle == -2.0 {
        yaw = DEFAULT_YAW;
        pitch = if angle == -2.0 { -90.0 } else { DEFAULT_PITCH };
    }
    if !(-90.0..=-5.0).contains(&pitch) {
        pitch = DEFAULT_PITCH;
    }
    SunEnv {
        yaw,
        pitch,
        color: color(e.get("_light")).unwrap_or(DEFAULT_COLOR),
        sky: color(e.get("_diffuse_light")),
        found: true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sun {
    pub dir: DVec3,
    pub color: [f64; 3],
    pub ambient: [f64; 3],
    pub sky_tint: [f64; 3],
    pub elevation: f64,
}

fn smooth(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn ramp(stops: &[(f64, [f64; 3])], x: f64) -> [f64; 3] {
    if x <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if x <= b {
            let t = (x - a) / (b - a);
            return [0, 1, 2].map(|i| ca[i] + (cb[i] - ca[i]) * t);
        }
    }
    stops[stops.len() - 1].1
}

fn mul(a: [f64; 3], b: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * b[0] * k, a[1] * b[1] * k, a[2] * b[2] * k]
}

fn arc(theta: f64, elev: f64, az: f64) -> DVec3 {
    let (a, e) = (az.to_radians(), elev.to_radians());
    let fwd = DVec3::new(a.cos(), a.sin(), 0.0);
    let side = DVec3::new(a.sin(), -a.cos(), 0.0);
    fwd * (theta.sin() * e.cos()) - side * theta.cos() + DVec3::Z * (theta.sin() * e.sin())
}

fn from_angles(az: f64, el: f64) -> DVec3 {
    let (a, e) = (az.to_radians(), el.to_radians());
    DVec3::new(e.cos() * a.cos(), e.cos() * a.sin(), e.sin())
}

fn angles_of(d: DVec3) -> (f64, f64) {
    (d.y.atan2(d.x).to_degrees(), d.z.clamp(-1.0, 1.0).asin().to_degrees())
}

pub fn body_dirs(env: &SunEnv, hours: f64) -> (DVec3, DVec3) {
    let h = hours.rem_euclid(24.0);
    let (elev, az) = (-env.pitch, env.yaw + 180.0);
    let day = SUNSET - SUNRISE;
    let night = 24.0 - day;
    let n = (h - SUNSET).rem_euclid(24.0);
    let sun = if (SUNRISE..=SUNSET).contains(&h) {
        arc((h - SUNRISE) / day * std::f64::consts::PI, elev, az)
    } else {
        arc(std::f64::consts::PI * (1.0 + n / night), elev, az)
    };
    let moon = arc(n / night * std::f64::consts::PI, elev, az);
    (sun, moon)
}

pub fn sun_at(env: &SunEnv, look: &Look) -> Sun {
    let (mut sun, mut moon) = body_dirs(env, look.time);
    if look.sun_az.is_some() || look.sun_el.is_some() {
        let (az, el) = angles_of(sun);
        sun = from_angles(look.sun_az.unwrap_or(az), look.sun_el.unwrap_or(el).clamp(-90.0, 90.0));
        if look.sun_el.is_some() {
            moon = -DVec3::Z;
        } else {
            let (_, mel) = angles_of(moon);
            moon = from_angles(look.sun_az.unwrap_or(az), mel);
        }
    }
    let el = angles_of(sun).1;
    let mel = angles_of(moon).1;
    let tint = [0, 1, 2].map(|i| 1.0 + (env.color[i] - 1.0) * MAP_TINT);
    let warm = ramp(
        &[
            (0.0, [1.0, 0.42, 0.14]),
            (6.0, [1.0, 0.62, 0.34]),
            (15.0, [1.0, 0.82, 0.62]),
            (35.0, [1.0, 1.0, 1.0]),
        ],
        el,
    );
    let sun_k = SUN_GAIN * smooth(-2.0, 7.0, el);
    let moon_k = MOON_GAIN * smooth(-2.0, 10.0, mel);
    let use_sun = el > -3.0 || sun_k >= moon_k;
    let (dir, color) = if use_sun { (sun, mul(warm, tint, sun_k)) } else { (moon, MOON_COLOR.map(|c| c * moon_k)) };
    let sky = env.sky.map(|s| s.map(|c| c * DAY_SKY.iter().sum::<f64>() / 3.0)).unwrap_or(DAY_SKY);
    let ambient = ramp(
        &[
            (-14.0, [0.085, 0.105, 0.19]),
            (-5.0, [0.15, 0.14, 0.22]),
            (0.0, [0.29, 0.24, 0.27]),
            (10.0, [0.34, 0.33, 0.36]),
            (30.0, sky),
        ],
        el,
    );
    let sky_tint = ramp(
        &[
            (-14.0, [0.12, 0.14, 0.26]),
            (-4.0, [0.42, 0.34, 0.46]),
            (2.0, [1.0, 0.66, 0.5]),
            (12.0, [1.0, 0.9, 0.8]),
            (30.0, [1.0, 1.0, 1.0]),
        ],
        el,
    );
    Sun { dir: dir.normalize(), color, ambient, sky_tint, elevation: el }
}

pub fn baked_sun(env: &SunEnv) -> Sun {
    sun_at(env, &Look { time: 12.0, sun_az: None, sun_el: None, ..Look::default() })
}

pub fn parse_time(s: &str) -> Result<f64> {
    let s = s.trim();
    let v = match s.split_once(':') {
        Some((h, m)) => {
            let (h, m): (f64, f64) = (h.trim().parse()?, m.trim().parse()?);
            if !(0.0..60.0).contains(&m) {
                bail!("bad minutes in {s}");
            }
            h + m / 60.0
        }
        None => s.parse()?,
    };
    if !(0.0..=24.0).contains(&v) {
        bail!("time must be 00:00..24:00: {s}");
    }
    Ok(v)
}

pub fn hhmm(h: f64) -> String {
    let m = (h.rem_euclid(24.0) * 60.0).round() as i64 % (24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noon_matches_map() {
        let env = SunEnv { yaw: 43.0, pitch: -60.0, ..SunEnv::default() };
        let (s, _) = body_dirs(&env, 12.0);
        let want = from_angles(223.0, 60.0);
        assert!((s - want).length() < 1e-9, "{s} {want}");
        let (s, m) = body_dirs(&env, 0.0);
        assert!(s.z < -0.5 && m.z > 0.5);
        assert!(body_dirs(&env, 6.0).0.z > 0.1 && body_dirs(&env, 18.5).0.z > 0.0);
        assert!(body_dirs(&env, 23.0).0.z < 0.0);
        assert!((parse_time("18:30").unwrap() - 18.5).abs() < 1e-9);
        assert_eq!(hhmm(18.5), "18:30");
    }
}
