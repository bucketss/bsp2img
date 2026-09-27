use std::collections::HashSet;

use clap::Args;
use glam::DVec3;
use rayon::prelude::*;

use crate::mesh::{Batch, Vertex};

use crate::render::{Cuts, Renderer};
use crate::scene::{Log, Scene, num};

pub const DEFAULT_GAP: f64 = 256.0;

#[derive(Clone, Debug, PartialEq)]
pub struct ExplodeOpts {
    pub on: bool,
    pub count: usize,
    pub at: Option<Vec<f64>>,
    pub gap: f64,
    pub guides: bool,
}

impl Default for ExplodeOpts {
    fn default() -> Self {
        ExplodeOpts { on: false, count: 1, at: None, gap: DEFAULT_GAP, guides: false }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Explode {
    pub planes: Vec<f64>,
    pub gap: f64,
    pub guides: bool,
}

pub fn candidates(levels: &[(f64, f64, f64)], cuts: &Cuts) -> Vec<(f64, f64)> {
    levels
        .windows(2)
        .map(|w| (w[0].0 - 1.0, w[0].0 - w[1].1))
        .filter(|&(z, _)| z > cuts.zmin && z < cuts.zmax)
        .collect()
}

impl ExplodeOpts {
    pub fn planes(&self, levels: &[(f64, f64, f64)], cuts: &Cuts) -> Vec<f64> {
        let mut p: Vec<f64> = match &self.at {
            Some(at) => at.clone(),
            None => {
                let mut c = candidates(levels, cuts);
                c.sort_by(|a, b| b.1.total_cmp(&a.1));
                c.into_iter().take(self.count).map(|(z, _)| z).collect()
            }
        };
        p.retain(|z| z.is_finite());
        p.sort_by(f64::total_cmp);
        p.dedup();
        p
    }

    pub fn resolve(&self, levels: &[(f64, f64, f64)], cuts: &Cuts) -> Option<Explode> {
        if !self.on {
            return None;
        }
        let planes = self.planes(levels, cuts);
        (!planes.is_empty()).then_some(Explode { planes, gap: self.gap, guides: self.guides })
    }
}

impl Explode {
    pub fn tag(&self) -> String {
        format!("_explode{}", self.planes.len())
    }
}

impl Scene {
    pub fn apply_explode(&self, r: &mut Renderer, o: &ExplodeOpts, cuts: &Cuts, log: Log) -> String {
        if !o.on {
            return String::new();
        }
        let Some(e) = o.resolve(&self.levels, cuts) else {
            log("explode: no level boundaries to split at, drawing the map whole".into());
            return String::new();
        };
        if o.at.is_none() && e.planes.len() < o.count {
            log(format!("explode {}: only {} level boundaries", o.count, e.planes.len()));
        }
        log(format!(
            "explode: split at z {}, gap {}",
            e.planes.iter().map(|p| num(*p)).collect::<Vec<_>>().join(", "),
            num(e.gap)
        ));
        let tag = e.tag();
        r.set_explode(&self.mesh, Some(&e));
        tag
    }
}

#[derive(Args, Clone)]
pub struct ExplodeArgs {
    #[arg(long, value_name = "N", help = "split the map into floors at the N largest gaps between levels and lift each floor")]
    pub explode: Option<usize>,
    #[arg(long = "explode-at", value_name = "Z", value_delimiter = ',', allow_negative_numbers = true, conflicts_with = "explode",
          help = "split floors at these heights instead, e.g. 100,300")]
    pub explode_at: Option<Vec<f64>>,
    #[arg(long = "explode-gap", default_value_t = DEFAULT_GAP, help = "units each floor is lifted above the one below")]
    pub explode_gap: f64,
    #[arg(long = "explode-guides", help = "draw guide lines at the corners of each lifted floor")]
    pub explode_guides: bool,
}

impl ExplodeArgs {
    pub fn opts(&self) -> ExplodeOpts {
        let on = self.explode.is_some_and(|n| n > 0) || self.explode_at.as_ref().is_some_and(|a| !a.is_empty());
        ExplodeOpts {
            on,
            count: self.explode.unwrap_or(1),
            at: self.explode_at.clone().filter(|a| !a.is_empty()),
            gap: self.explode_gap,
            guides: self.explode_guides,
        }
    }
}

pub fn band_points(points: &[DVec3], batches: &[Batch], planes: &[f64], cell: Option<f64>) -> (Vec<DVec3>, Vec<u16>) {
    let cross: Vec<(DVec3, u16)> = batches.par_iter().flat_map_iter(|b| b.tris().flat_map(|t| crossings(t, planes))).collect();
    let mut orig = points.to_vec();
    let mut bands: Vec<u16> = points.iter().map(|p| planes.partition_point(|&q| q < p.z) as u16).collect();
    let mut seen = HashSet::new();
    for (p, k) in cross {
        let fresh = match cell {
            Some(c) => seen.insert(((p.x / c).floor() as i32, (p.y / c).floor() as i32, (p.z / c).floor() as i32, k)),
            None => true,
        };
        if fresh {
            orig.push(p);
            bands.push(k);
        }
    }
    (orig, bands)
}

fn crossings(t: [&Vertex; 3], planes: &[f64]) -> Vec<(DVec3, u16)> {
    let p = t.map(|v| DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64));
    let (z0, z1) = (p[0].z.min(p[1].z).min(p[2].z), p[0].z.max(p[1].z).max(p[2].z));
    let first = planes.partition_point(|&q| q <= z0);
    let mut out = Vec::new();
    for (k, &q) in planes.iter().enumerate().skip(first).take_while(|(_, q)| **q < z1) {
        let k = k as u16;
        for i in 0..3 {
            let (a, b) = (p[i], p[(i + 1) % 3]);
            if a.z == q {
                out.extend([(a, k), (a, k + 1)]);
            } else if (a.z - q) * (b.z - q) < 0.0 {
                let m = a + (b - a) * ((q - a.z) / (b.z - a.z));
                out.extend([(m, k), (m, k + 1)]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::crossings;
    use crate::mesh::Vertex;

    fn v(x: f32, z: f32) -> Vertex {
        Vertex { pos: [x, 0.0, z], ..Vertex::BLANK }
    }

    #[test]
    fn crossing_points_in_both_bands() {
        let planes = [-200.0, 100.0, 250.0];
        let (a, b, c) = (v(0.0, 0.0), v(10.0, 0.0), v(0.0, 300.0));
        let pts = crossings([&a, &b, &c], &planes);
        assert_eq!(pts.len(), 8);
        for (p, k) in &pts {
            let z = planes[(*k as usize).min(2)];
            let q = if (p.z - 100.0).abs() < 1e-9 { 100.0 } else { 250.0 };
            assert!((p.z - q).abs() < 1e-9, "{p}");
            assert!(z == q || planes[*k as usize - 1] == q, "band {k} for z {}", p.z);
        }
        let (d, e, f) = (v(0.0, 0.0), v(10.0, 0.0), v(0.0, 50.0));
        assert!(crossings([&d, &e, &f], &planes).is_empty());
    }
}
