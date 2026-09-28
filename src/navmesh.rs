use std::collections::HashMap;

pub struct Area {
    pub poly: Vec<[f32; 3]>,
    pub crouch: bool,
    pub conns: Vec<u32>,
}

pub struct Ladder {
    pub top: [f32; 3],
    pub bottom: [f32; 3],
    pub width: f32,
    pub top_areas: Vec<u32>,
    pub bottom_areas: Vec<u32>,
}

pub struct NavMesh {
    pub areas: Vec<Area>,
    pub ladders: Vec<Ladder>,
}

impl NavMesh {
    pub fn from_ids(ids: &[u32], mut areas: Vec<Area>, mut ladders: Vec<Ladder>) -> NavMesh {
        let index: HashMap<u32, u32> = ids.iter().enumerate().map(|(k, &id)| (id, k as u32)).collect();
        let map = |v: &[u32], own: Option<u32>| {
            let mut out: Vec<u32> = v.iter().filter_map(|id| index.get(id).copied()).filter(|&k| Some(k) != own).collect();
            out.sort_unstable();
            out.dedup();
            out
        };
        for (k, a) in areas.iter_mut().enumerate() {
            a.conns = map(&a.conns, Some(k as u32));
        }
        for l in &mut ladders {
            l.top_areas = map(&l.top_areas, None);
            l.bottom_areas = map(&l.bottom_areas, None);
        }
        NavMesh { areas, ladders }
    }

    pub fn floor_below(&self, x: f32, y: f32, zmax: f32) -> Option<f32> {
        let mut best: Option<f32> = None;
        for a in &self.areas {
            let p = &a.poly;
            for i in 1..p.len().saturating_sub(1) {
                let (a, b, c) = (p[0], p[i], p[i + 1]);
                let d = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
                if d.abs() < 1e-6 {
                    continue;
                }
                let w0 = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / d;
                let w1 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / d;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                if z <= zmax && best.is_none_or(|b| z > b) {
                    best = Some(z);
                }
            }
        }
        best
    }
}
