use glam::DVec3;

use super::present::{Pixelwalk, Stance, zcolor};
use crate::mesh::overlay::Splat;
use crate::render::{MarkLine, Markers};

pub const SEAM: [u8; 3] = [255, 40, 255];
pub const SEAM_LAYER: u8 = 3;
const HALF: f64 = 16.0;
const GHOST_ALPHA: u8 = 70;
const EDGE_PX: f64 = 1.5;
const RIBBON_PX: f64 = 4.0;
const ARROW_PX: f64 = 2.5;
const ARROW: f64 = 48.0;
const HEAD: f64 = 12.0;
const SEAM_STEP: f64 = 8.0;
const SEAM_SIGMA: f64 = 4.0;
const WALL_UP: f64 = 8.0;

fn half_height(s: Stance) -> f64 {
    s.feet()
}

fn yaw_dir(p: &Pixelwalk) -> DVec3 {
    let y = (p.yaw as f64).to_radians();
    DVec3::new(y.cos(), y.sin(), 0.0)
}

fn feet(p: DVec3, s: Stance) -> DVec3 {
    p - DVec3::Z * half_height(s)
}

pub fn ghost(m: &mut Markers, o: DVec3, s: Stance, c: [u8; 3]) {
    let h = DVec3::new(HALF, HALF, half_height(s));
    let corner = |i: usize| o + DVec3::new(if i & 1 != 0 { h.x } else { -h.x }, if i & 2 != 0 { h.y } else { -h.y }, if i & 4 != 0 { h.z } else { -h.z });
    let fill = [c[0], c[1], c[2], GHOST_ALPHA];
    for q in [[0, 1, 3, 2], [4, 6, 7, 5], [0, 4, 5, 1], [2, 3, 7, 6], [0, 2, 6, 4], [1, 5, 7, 3]] {
        for t in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            m.tris.extend(t.map(|i| (corner(i), fill)));
        }
    }
    let edge = [c[0], c[1], c[2], 255];
    for (a, b) in [(0, 1), (1, 3), (3, 2), (2, 0), (4, 5), (5, 7), (7, 6), (6, 4), (0, 4), (1, 5), (2, 6), (3, 7)] {
        m.lines.push(MarkLine { a: corner(a), b: corner(b), color: edge, px: EDGE_PX });
    }
}

pub fn markers(pws: &[Pixelwalk], zr: (f64, f64)) -> Markers {
    let mut m = Markers::default();
    for p in pws {
        let c = zcolor(p.mid().z, zr);
        let rgba = [c[0], c[1], c[2], 255];
        ghost(&mut m, p.from, p.stance, c);
        if p.from.distance(p.to) > 0.5 {
            ghost(&mut m, p.to, p.stance, c);
            m.lines.push(MarkLine { a: feet(p.from, p.stance), b: feet(p.to, p.stance), color: rgba, px: RIBBON_PX });
        }
        let d = yaw_dir(p);
        let a = feet(p.mid(), p.stance);
        let tip = a + d * ARROW;
        m.lines.push(MarkLine { a, b: tip, color: rgba, px: ARROW_PX });
        for s in [1.0, -1.0] {
            let back = DVec3::new(-d.x - s * d.y, -d.y + s * d.x, 0.0).normalize() * HEAD;
            m.lines.push(MarkLine { a: tip, b: tip + back, color: rgba, px: ARROW_PX });
        }
    }
    m
}

pub fn seam_splats(pws: &[Pixelwalk]) -> Vec<Splat> {
    let mut out = Vec::new();
    for p in pws {
        let d = yaw_dir(p);
        let support = HALF * (d.x.abs() + d.y.abs());
        for f in &p.finds {
            let a = DVec3::new(f.pos[0] as f64, f.pos[1] as f64, f.pos[2] as f64);
            let b = if f.length > 0.5 { DVec3::new(f.to[0] as f64, f.to[1] as f64, f.to[2] as f64) } else { a };
            let n = ((a.distance(b) / SEAM_STEP).ceil() as usize).max(1);
            for i in 0..=n {
                let o = feet(a + (b - a) * (i as f64 / n as f64), p.stance);
                let model = |m: i32| u32::try_from(m).ok();
                let splat = |q: DVec3, facing: DVec3, m: i32| Splat {
                    p: q,
                    facing: Some(facing),
                    model: model(m),
                    layer: SEAM_LAYER,
                    sigma: SEAM_SIGMA,
                };
                out.push(splat(o + d * support + DVec3::Z * WALL_UP, d, p.wall_model));
                out.push(splat(o + d * (support - 1.0), -DVec3::Z, p.floor_model));
            }
        }
    }
    out
}
