use crate::bsp::Bsp;

use super::hull::PwMap;
use super::vec::V3;

#[derive(Clone, Copy, Debug)]
pub struct Physent {
    pub model: usize,
    pub origin: V3,
    pub angles: V3,
    pub info: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct SolidBrush {
    pub pe: Physent,
    pub wmins: V3,
    pub wmaxs: V3,
}

pub struct WorldModels {
    pub world: Physent,
    pub brushes: Vec<SolidBrush>,
    pub model_solid: Vec<bool>,
    pub model_origin: Vec<V3>,
}

const NONSOLID: [&str; 17] = [
    "func_illusionary",
    "func_ladder",
    "func_water",
    "func_friction",
    "func_buyzone",
    "func_bomb_target",
    "func_hostage_rescue",
    "func_vip_safetyzone",
    "func_escapezone",
    "func_grenade_catch",
    "func_weaponcheck",
    "func_mortar_field",
    "env_bubbles",
    "func_dustcloud",
    "func_dustmotes",
    "func_snow",
    "func_rain",
];

fn classname_is_solid(cn: &str) -> bool {
    !cn.starts_with("trigger") && !NONSOLID.contains(&cn)
}

fn classname_clears_angles(cn: &str) -> bool {
    matches!(cn, "func_door" | "func_water" | "momentary_door" | "func_button" | "func_conveyor")
}

fn float_prefix(s: &str) -> Option<(f32, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    let mant_end = i;
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let d0 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > d0 {
            i = j;
        }
    }
    let end = if i > mant_end { i } else { mant_end };
    let mut txt = s[start..end].to_string();
    if txt.ends_with('.') {
        txt.push('0');
    }
    if txt.starts_with('.') || txt.starts_with("-.") || txt.starts_with("+.") {
        txt = txt.replacen('.', "0.", 1);
    }
    txt.parse::<f32>().ok().map(|v| (v, &s[end..]))
}

fn parse_vec3(v: &str) -> V3 {
    let mut out = [0.0f32; 3];
    let mut rest = v;
    for o in out.iter_mut() {
        match float_prefix(rest) {
            Some((x, r)) => {
                *o = x;
                rest = r;
            }
            None => break,
        }
    }
    out
}

fn atoi(s: &str) -> i64 {
    let b = s.trim_start().as_bytes();
    let (neg, b) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        Some(b'+') => (false, &b[1..]),
        _ => (false, b),
    };
    let mut v: i64 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            break;
        }
        v = v.saturating_mul(10).saturating_add((c - b'0') as i64);
    }
    if neg { -v } else { v }
}

pub fn build_world(bsp: &Bsp, map: &PwMap) -> WorldModels {
    let n = map.models.len();
    let mut wm = WorldModels {
        world: Physent { model: 0, origin: [0.0; 3], angles: [0.0; 3], info: 0 },
        brushes: Vec::new(),
        model_solid: vec![false; n],
        model_origin: vec![[0.0; 3]; n],
    };
    if n > 0 {
        wm.model_solid[0] = true;
    }
    for e in &bsp.entities {
        let Some(m) = e.get("model") else { continue };
        if !m.starts_with('*') {
            continue;
        }
        let idx = atoi(&m[1..]);
        if idx <= 0 || idx >= n as i64 {
            continue;
        }
        let idx = idx as usize;
        let cn = e.get("classname").map(|c| c.to_ascii_lowercase()).unwrap_or_default();
        let origin = e.get("origin").map(parse_vec3).unwrap_or([0.0; 3]);
        let angles = e.get("angles").map(parse_vec3).unwrap_or([0.0; 3]);
        wm.model_origin[idx] = origin;
        if !classname_is_solid(&cn) {
            continue;
        }
        wm.model_solid[idx] = true;
        let keep = !classname_clears_angles(&cn);
        let bm = &map.models[idx];
        wm.brushes.push(SolidBrush {
            pe: Physent { model: idx, origin, angles: if keep { angles } else { [0.0; 3] }, info: idx as i32 },
            wmins: std::array::from_fn(|k| bm.mins[k] + origin[k]),
            wmaxs: std::array::from_fn(|k| bm.maxs[k] + origin[k]),
        });
    }
    wm.brushes.sort_by_key(|b| b.pe.info);
    wm
}

pub fn select_physents(wm: &WorldModels, qmins: &V3, qmaxs: &V3) -> Vec<Physent> {
    const MARGIN: f32 = 40.0;
    let mut out = vec![wm.world];
    for b in &wm.brushes {
        let overlap = (0..3).all(|k| !(b.wmaxs[k] + MARGIN < qmins[k] || b.wmins[k] - MARGIN > qmaxs[k]));
        if overlap {
            out.push(b.pe);
        }
    }
    out
}
