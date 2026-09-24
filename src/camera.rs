use glam::{DMat4, DVec3, DVec4};

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
