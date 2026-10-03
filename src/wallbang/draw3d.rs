use std::collections::BTreeMap;

use glam::DVec3;

use super::draw2d::color;
use super::export::Sim;
use super::shots::{Kind, Shot};
use crate::mesh::overlay::{SIGMA, Splat};
use crate::render::{MarkLine, Markers};

pub const DIED: u8 = 0;
pub const WEAK: u8 = 1;
pub const MADE: u8 = 2;
pub const COLORS: [[u8; 3]; 4] = [super::draw2d::RED, super::draw2d::YELLOW, super::draw2d::GREEN, crate::pixelwalk::draw3d::SEAM];
const BUNDLE_CELL: f64 = 128.0;
const YAW_BINS: f64 = 8.0;
const PITCH_BIN: f64 = 45.0;
const PATH_ALPHA: u8 = 200;
const PATH_PX: f64 = 1.0;
const PATH_PX_PER_DOUBLING: f64 = 0.75;
const PATH_PX_MAX: f64 = 6.0;

pub fn layer(k: Kind) -> u8 {
    match k {
        Kind::Died => DIED,
        Kind::Weak => WEAK,
        Kind::Made => MADE,
    }
}

pub fn splats(sim: &Sim, damage: bool) -> Vec<Splat> {
    let mut out = Vec::with_capacity(sim.shots.len() * 2);
    for s in &sim.shots {
        for c in &s.crossings {
            let layer = layer(c.kind(damage));
            let model = Some(c.model as u32);
            out.push(Splat { p: c.entry, facing: Some(s.dir), model, layer, sigma: SIGMA });
            out.push(Splat { p: c.exit, facing: Some(-s.dir), model, layer, sigma: SIGMA });
        }
    }
    out
}

pub struct Bundle {
    pub a: DVec3,
    pub b: DVec3,
    pub n: u32,
    pub kind: Kind,
}

type Key = (Kind, u16, [i32; 3], i32, i32);

pub fn bundles(shots: &[Shot], damage: bool) -> Vec<Bundle> {
    let mut acc: BTreeMap<Key, (DVec3, DVec3, u32)> = BTreeMap::new();
    for s in shots {
        let c = &s.crossings[0];
        let cell = (c.entry / BUNDLE_CELL).floor().as_ivec3().to_array();
        let yaw = (s.dir.y.atan2(s.dir.x) / std::f64::consts::TAU * YAW_BINS).floor() as i32;
        let pitch = (s.dir.z.clamp(-1.0, 1.0).asin().to_degrees() / PITCH_BIN).floor() as i32;
        let e = acc.entry((s.kind(damage), c.model, cell, yaw, pitch)).or_insert((DVec3::ZERO, DVec3::ZERO, 0));
        e.0 += s.start();
        e.1 += s.end();
        e.2 += 1;
    }
    acc.into_iter().map(|(k, (a, b, n))| Bundle { a: a / n as f64, b: b / n as f64, n, kind: k.0 }).collect()
}

pub fn path_markers(bs: &[Bundle]) -> Markers {
    let mut m = Markers::default();
    for b in bs {
        let c = color(b.kind);
        let px = (PATH_PX + PATH_PX_PER_DOUBLING * (b.n as f64).log2()).min(PATH_PX_MAX);
        m.lines.push(MarkLine { a: b.a, b: b.b, color: [c[0], c[1], c[2], PATH_ALPHA], px });
    }
    m
}
