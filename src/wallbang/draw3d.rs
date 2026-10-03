use super::export::Sim;
use crate::mesh::overlay::{SIGMA, Splat};

pub const DIED: u8 = 0;
pub const MADE: u8 = 1;

pub fn splats(sim: &Sim) -> Vec<Splat> {
    let mut out = Vec::with_capacity(sim.shots.len() * 2);
    for s in &sim.shots {
        for c in &s.crossings {
            let layer = if c.made { MADE } else { DIED };
            let model = Some(c.model as u32);
            out.push(Splat { p: c.entry, facing: Some(s.dir), model, layer, sigma: SIGMA });
            out.push(Splat { p: c.exit, facing: Some(-s.dir), model, layer, sigma: SIGMA });
        }
    }
    out
}
