use clap::Args;

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
        (!planes.is_empty()).then(|| Explode { planes, gap: self.gap, guides: self.guides })
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

#[cfg(test)]
mod tests {
    use crate::clip::clip_tris_z;
    use crate::mesh::Vertex;

    fn area(t: &[Vertex]) -> f64 {
        let p: Vec<glam::DVec3> = t.iter().map(|v| glam::DVec3::new(v.pos[0] as f64, v.pos[1] as f64, v.pos[2] as f64)).collect();
        (p[1] - p[0]).cross(p[2] - p[0]).length() / 2.0
    }

    #[test]
    fn bands_keep_all_area() {
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) * 1000.0 - 500.0
        };
        let mut tris = Vec::new();
        for _ in 0..3000 {
            for _ in 0..3 {
                let z = if rnd() > 300.0 { 100.0 } else { rnd() };
                tris.push(Vertex { pos: [rnd() as f32, rnd() as f32, z as f32], uv: [0.0; 2], lm: [0.0; 2], bias: 0.0, normal: [0.0, 0.0, 1.0] });
            }
        }
        let planes = [-200.0, 100.0, 250.0];
        let bands = clip_tris_z(&tris, &planes);
        let before: f64 = tris.chunks_exact(3).map(area).sum();
        let after: f64 = bands.iter().flat_map(|(_, v)| v.chunks_exact(3)).map(area).sum();
        assert!((before - after).abs() / before < 1e-5, "{before} {after}");
        for (k, v) in &bands {
            let lo = if *k == 0 { f64::NEG_INFINITY } else { planes[k - 1] };
            let hi = planes.get(*k).copied().unwrap_or(f64::INFINITY);
            for p in v {
                let z = p.pos[2] as f64;
                assert!(z >= lo - 1e-3 && z <= hi + 1e-3, "band {k} z {z}");
            }
        }
    }
}
