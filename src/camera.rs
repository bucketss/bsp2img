use std::path::Path;

use anyhow::{Context, Result};
use glam::{DMat4, DVec3, DVec4};

use crate::render::{Persp, View};

pub const ISO_PITCH: f64 = 35.264;
const FIT_ITERS: usize = 6;

#[derive(Clone, Copy, Debug)]
pub struct Basis {
    pub r: DVec3,
    pub u: DVec3,
    pub f: DVec3,
}

pub fn camera_basis(yaw: f64, pitch: f64) -> Basis {
    let (y, p) = (yaw.to_radians(), pitch.to_radians());
    let f = DVec3::new(p.cos() * y.cos(), p.cos() * y.sin(), -p.sin());
    let r = DVec3::new(y.sin(), -y.cos(), 0.0);
    let u = r.cross(f);
    Basis { r, u, f }
}

pub fn top_down(r: DVec3, u: DVec3) -> Basis {
    Basis { r, u, f: DVec3::new(0.0, 0.0, -1.0) }
}

pub fn extents(points: &[DVec3], b: &Basis) -> [(f64, f64); 3] {
    let mut e = [(f64::INFINITY, f64::NEG_INFINITY); 3];
    for p in points {
        for (k, v) in [p.dot(b.r), p.dot(b.u), p.dot(b.f)].into_iter().enumerate() {
            e[k].0 = e[k].0.min(v);
            e[k].1 = e[k].1.max(v);
        }
    }
    e
}

pub fn ortho(b: &Basis, cx: f64, cy: f64, w: f64, h: f64, d0: f64, d1: f64) -> DMat4 {
    let d = (d1 - d0) + 64.0;
    let dm = (d0 + d1) / 2.0;
    DMat4::from_cols(
        DVec4::new(2.0 * b.r.x / w, 2.0 * b.u.x / h, b.f.x / d, 0.0),
        DVec4::new(2.0 * b.r.y / w, 2.0 * b.u.y / h, b.f.y / d, 0.0),
        DVec4::new(2.0 * b.r.z / w, 2.0 * b.u.z / h, b.f.z / d, 0.0),
        DVec4::new(-2.0 * cx / w, -2.0 * cy / h, -dm / d + 0.5, 1.0),
    )
}

pub fn persp(b: &Basis, eye: DVec3, fov_y: f64, aspect: f64, near: f64, far: f64) -> DMat4 {
    let t = (fov_y.to_radians() / 2.0).tan();
    let (sx, sy, sz) = (1.0 / (t * aspect), 1.0 / t, far / (far - near));
    DMat4::from_cols(
        DVec4::new(sx * b.r.x, sy * b.u.x, sz * b.f.x, b.f.x),
        DVec4::new(sx * b.r.y, sy * b.u.y, sz * b.f.y, b.f.y),
        DVec4::new(sx * b.r.z, sy * b.u.z, sz * b.f.z, b.f.z),
        DVec4::new(-sx * b.r.dot(eye), -sy * b.u.dot(eye), -sz * (b.f.dot(eye) + near), -b.f.dot(eye)),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: DVec3,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    pub dist: f64,
    pub fov: f64,
    pub ortho: bool,
    pub aspect: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            target: DVec3::ZERO,
            yaw: 45.0,
            pitch: ISO_PITCH,
            roll: 0.0,
            dist: 4096.0,
            fov: 60.0,
            ortho: false,
            aspect: 16.0 / 9.0,
        }
    }
}

fn even(v: u32) -> u32 {
    (v + v % 2).max(2)
}

impl Camera {
    pub fn basis(&self) -> Basis {
        let b = camera_basis(self.yaw, self.pitch);
        if self.roll == 0.0 {
            return b;
        }
        let (s, c) = self.roll.to_radians().sin_cos();
        Basis { r: b.r * c + b.u * s, u: b.u * c - b.r * s, f: b.f }
    }

    pub fn eye(&self) -> DVec3 {
        self.target - self.basis().f * self.dist
    }

    fn tan(&self) -> f64 {
        (self.fov.clamp(1.0, 170.0).to_radians() / 2.0).tan()
    }

    pub fn height(&self) -> f64 {
        2.0 * self.dist * self.tan()
    }

    pub fn set_height(&mut self, h: f64) {
        self.dist = h / (2.0 * self.tan());
    }

    pub fn view(&self, wpx: u32, hpx: u32) -> View {
        let b = self.basis();
        let h = self.height();
        View {
            basis: b,
            cx: self.target.dot(b.r),
            cy: self.target.dot(b.u),
            w: h * wpx as f64 / hpx.max(1) as f64,
            h,
            sky_yaw: Some(self.yaw),
            persp: (!self.ortho).then(|| Persp { eye: self.eye(), fov_y: self.fov.clamp(1.0, 170.0), focus: self.dist }),
        }
    }

    pub fn size(&self, longest: u32) -> (u32, u32) {
        let a = self.aspect.clamp(0.05, 20.0);
        let (w, h) = if a >= 1.0 { (longest as f64, longest as f64 / a) } else { (longest as f64 * a, longest as f64) };
        (even(w.round() as u32), even(h.round() as u32))
    }

    pub fn frame_points(&mut self, pts: &[DVec3], aspect: f64, fill: f64) {
        if pts.is_empty() {
            return;
        }
        let (lo, hi) = pts.iter().fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(a, b), p| (a.min(*p), b.max(*p)));
        self.target = (lo + hi) / 2.0;
        let b = self.basis();
        let th = self.tan() * fill.clamp(0.1, 1.0);
        let tw = th * aspect;
        if self.ortho {
            let e = extents(pts, &b);
            self.target += b.r * ((e[0].0 + e[0].1) / 2.0 - self.target.dot(b.r));
            self.target += b.u * ((e[1].0 + e[1].1) / 2.0 - self.target.dot(b.u));
            let half = ((e[1].1 - e[1].0) / 2.0).max((e[0].1 - e[0].0) / 2.0 / aspect).max(1.0);
            self.dist = half / th;
            return;
        }
        for i in 0..FIT_ITERS {
            self.dist = self.fit_dist(pts, &b, tw, th);
            if i + 1 == FIT_ITERS {
                break;
            }
            let (mut x0, mut x1, mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
            for p in pts {
                let d = *p - self.target;
                let z = (d.dot(b.f) + self.dist).max(1e-6);
                let (x, y) = (d.dot(b.r) / (z * tw), d.dot(b.u) / (z * th));
                (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            }
            self.target += b.r * ((x0 + x1) / 2.0 * self.dist * tw) + b.u * ((y0 + y1) / 2.0 * self.dist * th);
        }
    }

    fn fit_dist(&self, pts: &[DVec3], b: &Basis, tw: f64, th: f64) -> f64 {
        pts.iter()
            .map(|p| {
                let d = *p - self.target;
                let z = d.dot(b.f);
                (d.dot(b.r).abs() / tw - z).max(d.dot(b.u).abs() / th - z)
            })
            .fold(1.0, f64::max)
    }

    pub fn orbit_dist(&self, pts: &[DVec3], yaws: &[f64], aspect: f64, fill: f64) -> f64 {
        let th = self.tan() * fill.clamp(0.1, 1.0);
        let tw = th * aspect;
        yaws.iter()
            .map(|&y| {
                let c = Camera { yaw: y, ..*self };
                c.fit_dist(pts, &c.basis(), tw, th)
            })
            .fold(1.0, f64::max)
    }

    pub fn orbit(&mut self, dx: f64, dy: f64) {
        self.yaw = (self.yaw - dx * 0.4).rem_euclid(360.0);
        self.pitch = (self.pitch + dy * 0.25).clamp(-90.0, 90.0);
    }

    pub fn pan(&mut self, dx: f64, dy: f64, upp: f64) {
        let b = self.basis();
        self.target += -b.r * dx * upp + b.u * dy * upp;
    }

    pub fn dolly(&mut self, scroll: f64) {
        self.dist = (self.dist * (-scroll * 0.003).exp()).clamp(1.0, 1e6);
    }

    pub fn fly(&mut self, fwd: f64, right: f64, up: f64) {
        let b = self.basis();
        self.target += b.f * fwd + b.r * right + DVec3::Z * up;
    }

    pub fn refocus(&mut self, p: DVec3) {
        let d = (p - self.eye()).dot(self.basis().f);
        if !self.ortho && d > 1.0 {
            self.dist = d;
        }
        self.target = p;
    }

    pub fn kv(&self) -> Vec<(String, String)> {
        let t = self.target;
        vec![
            ("proj".into(), if self.ortho { "ortho" } else { "persp" }.into()),
            ("target".into(), format!("{} {} {}", t.x, t.y, t.z)),
            ("yaw".into(), self.yaw.to_string()),
            ("pitch".into(), self.pitch.to_string()),
            ("roll".into(), self.roll.to_string()),
            ("dist".into(), self.dist.to_string()),
            ("fov".into(), self.fov.to_string()),
            ("aspect".into(), self.aspect.to_string()),
        ]
    }

    pub fn set(&mut self, k: &str, v: &str) {
        let v = v.trim();
        let num = |t: &mut f64| {
            if let Ok(x) = v.parse::<f64>() {
                if x.is_finite() {
                    *t = x;
                }
            }
        };
        match k.trim() {
            "proj" => self.ortho = v.eq_ignore_ascii_case("ortho"),
            "target" => {
                let p: Vec<f64> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if let [x, y, z] = p[..] {
                    self.target = DVec3::new(x, y, z);
                }
            }
            "yaw" => num(&mut self.yaw),
            "pitch" => num(&mut self.pitch),
            "roll" => num(&mut self.roll),
            "dist" => num(&mut self.dist),
            "fov" => num(&mut self.fov),
            "aspect" => num(&mut self.aspect),
            _ => {}
        }
        self.pitch = self.pitch.clamp(-90.0, 90.0);
        self.dist = self.dist.clamp(1.0, 1e6);
        self.fov = self.fov.clamp(1.0, 170.0);
        self.aspect = self.aspect.clamp(0.05, 20.0);
    }

    pub fn text(&self) -> String {
        self.kv().into_iter().map(|(k, v)| format!("{k}={v}\n")).collect()
    }

    pub fn parse(text: &str) -> Camera {
        let mut c = Camera::default();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                c.set(k, v);
            }
        }
        c
    }

    pub fn load(path: &Path) -> Result<Camera> {
        let t = std::fs::read_to_string(path).with_context(|| format!("reading camera {}", path.display()))?;
        Ok(Camera::parse(&t))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.text()).with_context(|| format!("writing camera {}", path.display()))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Framing {
    pub camera: Option<Camera>,
    pub persp: Option<f64>,
}

impl Framing {
    pub fn tag(&self) -> String {
        match (&self.camera, self.persp) {
            (Some(_), _) => "_cam".into(),
            (None, Some(f)) => format!("_persp{}", crate::scene::num(f.round())),
            _ => String::new(),
        }
    }
}

pub fn auto_persp(pts: &[DVec3], yaw: f64, pitch: f64, fov: f64, size: u32, pad: u32) -> (View, u32, u32) {
    let mut c = Camera { yaw, pitch, fov, ortho: false, ..Camera::default() };
    let e = extents(pts, &c.basis());
    c.aspect = ((e[0].1 - e[0].0) / (e[1].1 - e[1].0).max(1.0)).clamp(0.2, 5.0);
    let (w, h) = c.size(size);
    c.frame_points(pts, w as f64 / h as f64, 1.0 - 2.0 * pad as f64 / size.max(64) as f64);
    (c.view(w, h), w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(c: &Camera, aspect: f64, near: f64, far: f64) -> DMat4 {
        let b = c.basis();
        if c.ortho {
            let h = c.height();
            let e = c.eye().dot(b.f);
            ortho(&b, c.target.dot(b.r), c.target.dot(b.u), h * aspect, h, e + near, e + far)
        } else {
            persp(&b, c.eye(), c.fov, aspect, near, far)
        }
    }

    fn cube() -> Vec<DVec3> {
        let mut v = Vec::new();
        for x in [-500.0, 700.0] {
            for y in [-300.0, 900.0] {
                for z in [-100.0, 200.0] {
                    v.push(DVec3::new(x, y, z));
                }
            }
        }
        v
    }

    fn project(c: &Camera, p: DVec3, aspect: f64) -> DVec3 {
        let m = matrix(c, aspect, 1.0, 1e5);
        let q = m * p.extend(1.0);
        q.truncate() / q.w
    }

    #[test]
    fn frame_fits_all_points() {
        for ortho in [false, true] {
            for yaw in [0.0, 45.0, 170.0, 300.0] {
                let mut c = Camera { yaw, pitch: 30.0, ortho, ..Camera::default() };
                c.frame_points(&cube(), 1.5, 0.9);
                let mut mx: f64 = 0.0;
                for p in cube() {
                    let n = project(&c, p, 1.5);
                    assert!(n.x.abs() <= 0.9 + 1e-6 && n.y.abs() <= 0.9 + 1e-6, "{ortho} {yaw} {n}");
                    assert!(n.z > 0.0 && n.z < 1.0);
                    mx = mx.max(n.x.abs()).max(n.y.abs());
                }
                assert!(mx > 0.85, "{ortho} {yaw} loose {mx}");
            }
        }
    }

    #[test]
    fn persp_depth_linearises() {
        let c = Camera { target: DVec3::new(10.0, 20.0, 30.0), yaw: 77.0, pitch: 20.0, dist: 900.0, ..Camera::default() };
        let (near, far) = (4.0, 8000.0);
        let m = matrix(&c, 1.0, near, far);
        let b = c.basis();
        let p = c.eye() + b.f * 1234.0 + b.r * 50.0;
        let q = m * p.extend(1.0);
        let d = q.z / q.w;
        let z = near * far / (far - d * (far - near));
        assert!((z - 1234.0).abs() < 1e-6);
    }

    #[test]
    fn input_moves() {
        let mut c = Camera::default();
        let t0 = c.target;
        c.orbit(10.0, -8.0);
        assert!((c.yaw - 41.0).abs() < 1e-9 && (c.pitch - (ISO_PITCH - 2.0)).abs() < 1e-9);
        c.pitch = 89.0;
        c.orbit(0.0, 100.0);
        assert_eq!(c.pitch, 90.0);
        c.pan(10.0, 0.0, 2.0);
        assert!(((c.target - t0).dot(c.basis().r) + 20.0).abs() < 1e-9);
        let d = c.dist;
        c.dolly(100.0);
        assert!(c.dist < d);
        let eye = c.eye();
        let p = eye + c.basis().f * 300.0 + c.basis().r * 40.0;
        c.refocus(p);
        assert!((c.target - p).length() < 1e-6);
        assert!((c.dist - 300.0).abs() < 1e-6);
        let r = Camera::parse(&c.text());
        assert!((r.target - c.target).length() < 1e-9 && r.yaw == c.yaw && r.dist == c.dist && r.ortho == c.ortho);
    }
}
