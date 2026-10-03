use crate::bsp::Bsp;

use super::vec::{V3, dot};

pub const CONTENTS_EMPTY: i32 = -1;
pub const CONTENTS_SOLID: i32 = -2;

pub const PLAYER_MINS: [V3; 4] = [[-16.0, -16.0, -36.0], [-16.0, -16.0, -18.0], [0.0, 0.0, 0.0], [-32.0, -32.0, -32.0]];

const CLIP_MINS: [V3; 4] = [[0.0, 0.0, 0.0], [-16.0, -16.0, -36.0], [-32.0, -32.0, -32.0], [-16.0, -16.0, -18.0]];

pub fn hull_index(usehull: usize) -> usize {
    match usehull {
        1 => 3,
        2 => 0,
        3 => 2,
        _ => 1,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MPlane {
    pub n: V3,
    pub d: f32,
    pub ty: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct CNode {
    pub plane: i32,
    pub children: [i16; 2],
}

#[derive(Clone, Copy, Debug)]
pub struct BModel {
    pub mins: V3,
    pub maxs: V3,
    pub headnode: [i32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct Hull {
    pub first: i32,
    pub last: i32,
    pub clip_mins: V3,
}

pub struct PwMap {
    pub planes: Vec<MPlane>,
    pub clipnodes: Vec<CNode>,
    pub models: Vec<BModel>,
}

impl PwMap {
    pub fn new(bsp: &Bsp) -> PwMap {
        let planes = bsp
            .planes
            .iter()
            .map(|p| MPlane { n: [p.normal.x as f32, p.normal.y as f32, p.normal.z as f32], d: p.dist as f32, ty: p.kind })
            .collect();
        let clipnodes: Vec<CNode> = bsp.clipnodes.iter().map(|n| CNode { plane: n.plane, children: n.children }).collect();
        let nclip = clipnodes.len() as i32;
        let models = bsp
            .models
            .iter()
            .enumerate()
            .map(|(mi, m)| {
                let mut headnode = m.headnode;
                if mi > 0 {
                    for h in headnode.iter_mut().skip(1) {
                        if *h >= nclip {
                            *h = CONTENTS_EMPTY;
                        }
                    }
                }
                BModel {
                    mins: [m.mins.x as f32, m.mins.y as f32, m.mins.z as f32],
                    maxs: [m.maxs.x as f32, m.maxs.y as f32, m.maxs.z as f32],
                    headnode,
                }
            })
            .collect();
        PwMap { planes, clipnodes, models }
    }

    pub fn hull(&self, model: usize, h: usize) -> Hull {
        Hull { first: self.models[model].headnode[h], last: self.clipnodes.len() as i32 - 1, clip_mins: CLIP_MINS[h] }
    }

    pub fn point_contents(&self, hull: &Hull, mut num: i32, p: &V3) -> i32 {
        if hull.first >= hull.last {
            return CONTENTS_EMPTY;
        }
        while num >= 0 {
            if num > hull.last {
                return CONTENTS_SOLID;
            }
            let node = &self.clipnodes[num as usize];
            let pl = &self.planes[node.plane as usize];
            let d = if pl.ty >= 3 { dot(p, &pl.n) - pl.d } else { p[pl.ty as usize] - pl.d };
            num = node.children[if d >= 0.0 { 0 } else { 1 }] as i32;
        }
        num
    }
}
