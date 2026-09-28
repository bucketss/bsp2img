use std::collections::HashMap;

use glam::{DMat3, DVec3};

use crate::bsp::Entity;

pub struct Ents {
    pub list: Vec<Entity>,
    pub disabled: Vec<(String, DVec3)>,
    pub sky: Option<String>,
}

fn nums(s: Option<&String>) -> Vec<f64> {
    s.map(|v| v.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default()
}

fn hdr(kv: &HashMap<String, String>, k: &str) -> Option<String> {
    let v = kv.get(k)?;
    let n = nums(Some(v));
    (n.len() >= 3 && n[..3].iter().any(|&c| c >= 0.0)).then(|| v.clone())
}

pub fn angles(e: &Entity) -> DVec3 {
    let v: Vec<f64> = e.get("angles").map(|s| s.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    let a = DVec3::new(v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0));
    match e.get("angle").and_then(|s| s.trim().parse::<f64>().ok()) {
        Some(y) if a == DVec3::ZERO => DVec3::new(0.0, y, 0.0),
        _ => a,
    }
}

pub fn rotation(e: &Entity) -> DMat3 {
    crate::s2::entities::angle_matrix(angles(e))
}

fn normalize(mut kv: HashMap<String, String>, disabled: &mut Vec<(String, DVec3)>) -> Option<Entity> {
    let cls = kv.get("classname").cloned().unwrap_or_default();
    let spawn = matches!(cls.as_str(), "info_player_terrorist" | "info_player_counterterrorist");
    if spawn && kv.get("enabled").is_some_and(|v| v.trim() == "0") {
        let o = nums(kv.get("origin"));
        disabled.push((cls, DVec3::new(o.first().copied().unwrap_or(0.0), o.get(1).copied().unwrap_or(0.0), o.get(2).copied().unwrap_or(0.0))));
        return None;
    }
    let renamed = match cls.as_str() {
        "info_player_terrorist" => "info_player_deathmatch",
        "info_player_counterterrorist" => "info_player_start",
        "info_player_start" => "info_player_start_source",
        _ => "",
    };
    if !renamed.is_empty() {
        kv.insert("classname".into(), renamed.into());
    }
    if cls == "light_environment" {
        if let Some(v) = hdr(&kv, "_lighthdr") {
            kv.insert("_light".into(), v);
        }
        let amb = hdr(&kv, "_ambienthdr").or_else(|| kv.get("_ambient").cloned());
        if let (Some(a), false) = (amb, kv.contains_key("_diffuse_light")) {
            kv.insert("_diffuse_light".into(), a);
        }
    }
    Some(Entity(kv))
}

pub fn parse(text: &str) -> Ents {
    let mut disabled = Vec::new();
    let mut list: Vec<Entity> = crate::bsp::parse_entities(text)
        .into_iter()
        .filter_map(|e| normalize(e.0.into_iter().map(|(k, v)| (k.to_lowercase(), v)).collect(), &mut disabled))
        .collect();
    match list.iter().position(|e| e.class() == "worldspawn") {
        Some(0) => {}
        Some(i) => {
            let w = list.remove(i);
            list.insert(0, w);
        }
        None => list.insert(0, Entity(HashMap::from([("classname".to_string(), "worldspawn".to_string())]))),
    }
    let sky = list[0].get("skyname").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Ents { list, disabled, sky }
}
