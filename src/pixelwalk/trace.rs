use super::hull::{CONTENTS_EMPTY, CONTENTS_SOLID, Hull, PLAYER_MINS, PwMap, hull_index};
use super::vec::{V3, angle_vectors, angle_vectors_transpose, dot};
use super::world::Physent;

#[derive(Clone, Copy, Debug, Default)]
pub struct PmTrace {
    pub allsolid: bool,
    pub startsolid: bool,
    pub inopen: bool,
    pub inwater: bool,
    pub fraction: f32,
    pub endpos: V3,
    pub normal: V3,
    pub dist: f32,
    pub ent: i32,
}

#[allow(clippy::too_many_arguments)]
pub fn recursive_hull_check(
    map: &PwMap,
    hull: &Hull,
    num: i32,
    p1f: f32,
    p2f: f32,
    p1: &V3,
    p2: &V3,
    tr: &mut PmTrace,
    eps: f32,
) -> bool {
    if num < 0 {
        if num == CONTENTS_SOLID {
            tr.startsolid = true;
        } else {
            tr.allsolid = false;
            if num == CONTENTS_EMPTY {
                tr.inopen = true;
            } else {
                tr.inwater = true;
            }
        }
        return true;
    }
    if hull.first >= hull.last {
        tr.allsolid = false;
        tr.inopen = true;
        return true;
    }
    let node = &map.clipnodes[num as usize];
    let plane = &map.planes[node.plane as usize];
    let (t1, t2) = if plane.ty >= 3 {
        (dot(p1, &plane.n) - plane.d, dot(p2, &plane.n) - plane.d)
    } else {
        (p1[plane.ty as usize] - plane.d, p2[plane.ty as usize] - plane.d)
    };
    if t1 >= 0.0 && t2 >= 0.0 {
        return recursive_hull_check(map, hull, node.children[0] as i32, p1f, p2f, p1, p2, tr, eps);
    }
    let mut midf = if t1 >= 0.0 {
        t1 - eps
    } else {
        if t2 < 0.0 {
            return recursive_hull_check(map, hull, node.children[1] as i32, p1f, p2f, p1, p2, tr, eps);
        }
        t1 + eps
    };
    midf /= t1 - t2;
    if midf >= 0.0 {
        if midf > 1.0 {
            midf = 1.0;
        }
    } else {
        midf = 0.0;
    }
    let pdif = p2f - p1f;
    let mut frac = pdif * midf + p1f;
    let mut mid: V3 = std::array::from_fn(|k| (p2[k] - p1[k]) * midf + p1[k]);
    let side = if t1 >= 0.0 { 0 } else { 1 };
    if !recursive_hull_check(map, hull, node.children[side] as i32, p1f, frac, p1, &mid, tr, eps) {
        return false;
    }
    if map.point_contents(hull, node.children[side ^ 1] as i32, &mid) != CONTENTS_SOLID {
        return recursive_hull_check(map, hull, node.children[side ^ 1] as i32, frac, p2f, &mid, p2, tr, eps);
    }
    if tr.allsolid {
        return false;
    }
    if side != 0 {
        tr.normal = [-plane.n[0], -plane.n[1], -plane.n[2]];
        tr.dist = -plane.d;
    } else {
        tr.normal = plane.n;
        tr.dist = plane.d;
    }
    while map.point_contents(hull, hull.first, &mid) == CONTENTS_SOLID {
        midf = (midf as f64 - 0.05) as f32;
        if midf < 0.0 {
            tr.fraction = frac;
            tr.endpos = mid;
            return false;
        }
        frac = pdif * midf + p1f;
        mid = std::array::from_fn(|k| (p2[k] - p1[k]) * midf + p1[k]);
    }
    tr.fraction = frac;
    tr.endpos = mid;
    false
}

pub fn player_trace(map: &PwMap, phys: &[Physent], usehull: usize, eps: f32, start: &V3, end: &V3) -> PmTrace {
    let mut trace = PmTrace { fraction: 1.0, ent: -1, endpos: *end, ..Default::default() };
    let hi = hull_index(usehull);
    for (i, pe) in phys.iter().enumerate() {
        let hull = map.hull(pe.model, hi);
        let offset: V3 = std::array::from_fn(|k| hull.clip_mins[k] - PLAYER_MINS[usehull][k] + pe.origin[k]);
        let mut start_l: V3 = std::array::from_fn(|k| start[k] - offset[k]);
        let mut end_l: V3 = std::array::from_fn(|k| end[k] - offset[k]);
        let rotated = pe.angles[0] != 0.0 || pe.angles[1] != 0.0 || pe.angles[2] != 0.0;
        if rotated {
            let (f, r, u) = angle_vectors(&pe.angles);
            let ts = start_l;
            start_l = [dot(&f, &ts), -dot(&r, &ts), dot(&u, &ts)];
            let te = end_l;
            end_l = [dot(&f, &te), -dot(&r, &te), dot(&u, &te)];
        }
        let mut total = PmTrace { fraction: 1.0, allsolid: true, endpos: *end, ..Default::default() };
        recursive_hull_check(map, &hull, hull.first, 0.0, 1.0, &start_l, &end_l, &mut total, eps);
        if total.allsolid {
            total.startsolid = true;
        }
        if total.startsolid {
            total.fraction = 0.0;
        }
        if total.fraction != 1.0 {
            if rotated {
                let (f, r, u) = angle_vectors_transpose(&pe.angles);
                let tmp = total.normal;
                total.normal = [dot(&f, &tmp), dot(&r, &tmp), dot(&u, &tmp)];
            }
            total.endpos = std::array::from_fn(|k| (end[k] - start[k]) * total.fraction + start[k]);
        }
        if total.fraction < trace.fraction {
            trace = total;
            trace.ent = i as i32;
        }
    }
    trace
}
