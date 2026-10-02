use crate::bsp::Bsp;

pub const CONTENTS_EMPTY: i32 = -1;
pub const CONTENTS_SOLID: i32 = -2;
pub const CONTENTS_TRANSLUCENT: i32 = -15;
const DIST_EPSILON: f64 = 0.03125;

pub type V3 = [f32; 3];

#[derive(Clone, Copy, Debug)]
pub struct HPlane {
    pub n: V3,
    pub d: f32,
    pub kind: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct HNode {
    pub plane: u32,
    pub children: [i16; 2],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Trace {
    pub allsolid: bool,
    pub startsolid: bool,
    pub inopen: bool,
    pub inwater: bool,
    pub fraction: f32,
    pub endpos: V3,
    pub normal: V3,
    pub dist: f32,
    pub aborted: bool,
    pub abortpos: V3,
}

pub struct Hull0 {
    pub planes: Vec<HPlane>,
    pub nodes: Vec<HNode>,
}

#[inline]
pub fn dot(a: &V3, b: &V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub fn vma(a: &V3, s: f32, m: &V3) -> V3 {
    [s * m[0] + a[0], s * m[1] + a[1], s * m[2] + a[2]]
}

#[inline]
fn f(x: f32) -> f64 {
    f64::from(x)
}

impl Hull0 {
    pub fn new(bsp: &Bsp) -> Hull0 {
        let planes = bsp
            .planes
            .iter()
            .map(|p| HPlane {
                n: [p.normal.x as f32, p.normal.y as f32, p.normal.z as f32],
                d: p.dist as f32,
                kind: p.kind,
            })
            .collect();
        let nodes = bsp
            .nodes
            .iter()
            .map(|n| HNode {
                plane: n.plane as u32,
                children: n.children.map(|c| {
                    if c >= 0 {
                        c
                    } else {
                        bsp.leaves.get((-1 - c as i32) as usize).map_or(CONTENTS_SOLID, |l| l.contents) as i16
                    }
                }),
            })
            .collect();
        Hull0 { planes, nodes }
    }

    #[inline]
    fn side_dist(pl: &HPlane, p: &V3) -> f32 {
        if pl.kind < 3 {
            (f(p[pl.kind as usize]) - f(pl.d)) as f32
        } else {
            (f(dot(&pl.n, p)) - f(pl.d)) as f32
        }
    }

    pub fn point_contents(&self, mut num: i32, p: &V3) -> i32 {
        while num >= 0 {
            let node = &self.nodes[num as usize];
            let d = Self::side_dist(&self.planes[node.plane as usize], p);
            num = node.children[if d < 0.0 { 1 } else { 0 }] as i32;
        }
        num
    }

    pub fn trace(&self, head: i32, start: &V3, end: &V3) -> Trace {
        let mut tr = Trace { fraction: 1.0, allsolid: true, endpos: *end, ..Default::default() };
        self.rhc(head, head, 0.0, 1.0, *start, end, &mut tr);
        if tr.fraction != 1.0 {
            for i in 0..3 {
                tr.endpos[i] = ((f(end[i]) - f(start[i])) * f(tr.fraction) + f(start[i])) as f32;
            }
        }
        tr
    }

    fn rhc(&self, head: i32, mut num: i32, mut p1f: f32, p2f: f32, mut p1: V3, p2: &V3, tr: &mut Trace) -> bool {
        while num >= 0 {
            let pdif = p2f - p1f;
            let node = &self.nodes[num as usize];
            let plane = &self.planes[node.plane as usize];
            let t1 = Self::side_dist(plane, &p1);
            let t2 = Self::side_dist(plane, p2);
            if t1 >= 0.0 && t2 >= 0.0 {
                num = node.children[0] as i32;
                continue;
            }
            if t1 < 0.0 && t2 < 0.0 {
                num = node.children[1] as i32;
                continue;
            }
            let mut frac = if t1 < 0.0 {
                ((f(t1) + DIST_EPSILON) / (f(t1) - f(t2))) as f32
            } else {
                ((f(t1) - DIST_EPSILON) / (f(t1) - f(t2))) as f32
            };
            if frac < 0.0 {
                frac = 0.0;
            } else if frac > 1.0 {
                frac = 1.0;
            }
            if frac.is_nan() {
                return false;
            }
            let mut midf = (f(p1f) + f(pdif) * f(frac)) as f32;
            let point = [p2[0] - p1[0], p2[1] - p1[1], p2[2] - p1[2]];
            let mut mid = vma(&p1, frac, &point);
            let side = usize::from(t1 < 0.0);
            if !self.rhc(head, node.children[side] as i32, p1f, midf, p1, &mid, tr) {
                return false;
            }
            if self.point_contents(node.children[side ^ 1] as i32, &mid) != CONTENTS_SOLID {
                num = node.children[side ^ 1] as i32;
                p1f = midf;
                p1 = mid;
                continue;
            }
            if tr.allsolid {
                tr.aborted = true;
                tr.abortpos = mid;
                return false;
            }
            if side == 0 {
                tr.normal = plane.n;
                tr.dist = plane.d;
            } else {
                tr.normal = [-plane.n[0], -plane.n[1], -plane.n[2]];
                tr.dist = -plane.d;
            }
            while self.point_contents(head, &mid) == CONTENTS_SOLID {
                frac = (f(frac) - f(0.1f32)) as f32;
                if frac < 0.0 {
                    tr.fraction = midf;
                    tr.endpos = mid;
                    return false;
                }
                midf = (f(p1f) + f(pdif) * f(frac)) as f32;
                let point = [p2[0] - p1[0], p2[1] - p1[1], p2[2] - p1[2]];
                mid = vma(&p1, frac, &point);
            }
            tr.fraction = midf;
            tr.endpos = mid;
            return false;
        }
        if num != CONTENTS_SOLID {
            tr.allsolid = false;
            if num == CONTENTS_EMPTY {
                tr.inopen = true;
            } else if num != CONTENTS_TRANSLUCENT {
                tr.inwater = true;
            }
        } else {
            tr.startsolid = true;
        }
        true
    }
}
