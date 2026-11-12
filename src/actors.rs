use crate::bsp::Entity;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Show {
    pub items: bool,
    pub spawns: bool,
}

impl Show {
    pub fn any(self) -> bool {
        self.items || self.spawns
    }
}

pub const GOLD_PLAYERS: [&[&str]; 2] = [&["terror", "leet", "arctic", "guerilla"], &["urban", "gsg9", "sas", "gign"]];
pub const CSS_PLAYERS: [&[&str]; 2] = [&["t_phoenix", "t_leet", "t_arctic", "t_guerilla"], &["ct_urban", "ct_gsg9", "ct_sas", "ct_gign"]];
pub const CSGO_PLAYERS: [&[&str]; 2] =
    [&["tm_phoenix", "tm_separatist", "tm_professional", "tm_anarchist"], &["ctm_sas", "ctm_fbi", "ctm_gign", "ctm_idf"]];
pub const TF2_PLAYERS: &[&str] = &["scout", "soldier", "pyro", "demo", "heavy", "engineer", "medic", "sniper", "spy"];
pub const CS2_PLAYERS: [&[&str]; 2] = [
    &["tm_phoenix/tm_phoenix", "tm_leet/tm_leet_varianta", "tm_balkan/tm_balkan_variantf", "tm_jungle_raider/tm_jungle_raider_varianta"],
    &["ctm_sas/ctm_sas", "ctm_fbi/ctm_fbi", "ctm_gendarmerie/ctm_gendarmerie_varianta", "ctm_swat/ctm_swat_variante"],
];

pub fn spawn_team(cls: &str) -> Option<usize> {
    match cls {
        "info_player_deathmatch" => Some(0),
        "info_player_start" => Some(1),
        _ => None,
    }
}

pub struct Spawn {
    pub team: usize,
    pub nth: usize,
    pub index: usize,
}

pub fn spawns(ents: &[Entity]) -> Vec<Spawn> {
    let mut count = [0usize; 2];
    let mut out = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let Some(t) = spawn_team(e.class()) else { continue };
        if e.origin().is_none() {
            continue;
        }
        out.push(Spawn { team: t, nth: count[t], index: i });
        count[t] += 1;
    }
    out
}

pub fn is_item_class(cls: &str) -> bool {
    cls == "armoury_entity" || cls.starts_with("weapon_") || cls.starts_with("item_") || cls.starts_with("ammo_")
}

pub fn own_model<'a>(e: &'a Entity, ext: &str) -> Option<&'a str> {
    e.get("model").map(str::trim).filter(|m| m.to_lowercase().ends_with(ext))
}

const ARMOURY: &[&str] = &[
    "mp5", "tmp", "p90", "mac10", "ak47", "sg552", "m4a1", "aug", "scout", "g3sg1", "awp", "m3", "xm1014", "m249", "flashbang", "hegrenade",
    "kevlar", "assault", "smokegrenade", "shield", "famas", "sg550", "galil", "ump45", "glock18", "usp", "elite", "fiveseven", "p228", "deagle",
];

const GOLD_ALIAS: &[(&str, &str)] = &[
    ("weapon_mp5navy", "mp5"),
    ("weapon_c4", "backpack"),
    ("weapon_hornetgun", "hgun"),
    ("weapon_handgrenade", "grenade"),
    ("weapon_snark", "sqknest"),
    ("weapon_glock", "9mmhandgun"),
    ("weapon_mp5", "9mmar"),
    ("weapon_python", "357"),
    ("item_healthkit", "medkit"),
    ("item_assaultsuit", "assault"),
    ("ammo_9mmclip", "9mmclip"),
    ("ammo_glockclip", "9mmclip"),
    ("ammo_9mmar", "9mmarclip"),
    ("ammo_mp5clip", "9mmarclip"),
    ("ammo_9mmbox", "chainammo"),
    ("ammo_argrenades", "argrenade"),
    ("ammo_mp5grenades", "argrenade"),
    ("ammo_buckshot", "shotbox"),
    ("ammo_357", "357ammobox"),
    ("ammo_crossbow", "crossbow_clip"),
    ("ammo_rpgclip", "rpgammo"),
    ("ammo_gaussclip", "gaussammo"),
    ("ammo_egonclip", "chainammo"),
];

pub fn gold_item(e: &Entity) -> Option<String> {
    let cls = e.class().to_lowercase();
    if let Some(m) = own_model(e, ".mdl") {
        return Some(m.replace('\\', "/"));
    }
    let name = if cls == "armoury_entity" {
        let i: usize = e.get("item").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        ARMOURY.get(i)?.to_string()
    } else if let Some(&(_, a)) = GOLD_ALIAS.iter().find(|(c, _)| *c == cls) {
        a.to_string()
    } else {
        cls.split_once('_')?.1.to_string()
    };
    Some(format!("models/w_{name}.mdl"))
}

const SRC_ALIAS: &[(&str, &[&str])] = &[
    ("hegrenade", &["hegrenade", "fraggrenade"]),
    ("usp_silencer", &["usp_silencer", "223"]),
    ("m4a1_silencer", &["m4a1_s", "m4a1_silencer"]),
    ("cz75a", &["cz_75", "cz75a"]),
    ("m3", &["m3super90"]),
    ("m249", &["m249", "m249para"]),
    ("mp5navy", &["mp5"]),
    ("knife", &["knife_t", "knife"]),
    ("kevlar", &["armor"]),
    ("assaultsuit", &["assault_suit"]),
];

const SRC_CATS: &[&str] = &["rif", "smg", "snip", "shot", "pist", "mach", "eq"];

const TF2_ITEMS: &[(&str, &str)] = &[
    ("item_healthkit_small", "models/items/medkit_small.mdl"),
    ("item_healthkit_medium", "models/items/medkit_medium.mdl"),
    ("item_healthkit_full", "models/items/medkit_large.mdl"),
    ("item_ammopack_small", "models/items/ammopack_small.mdl"),
    ("item_ammopack_medium", "models/items/ammopack_medium.mdl"),
    ("item_ammopack_full", "models/items/ammopack_large.mdl"),
];

fn item_name(cls: &str) -> Option<&str> {
    cls.strip_prefix("weapon_").or_else(|| cls.strip_prefix("item_"))
}

pub fn source_item(e: &Entity, has: impl Fn(&str) -> bool) -> Option<String> {
    let cls = e.class().to_lowercase();
    if let Some(m) = own_model(e, ".mdl") {
        return Some(m.replace('\\', "/"));
    }
    if let Some(&(_, m)) = TF2_ITEMS.iter().find(|(c, _)| *c == cls) {
        return has(m).then(|| m.to_string());
    }
    let n = item_name(&cls)?;
    let names: Vec<&str> = SRC_ALIAS.iter().find(|(k, _)| *k == n).map_or(vec![n], |(_, v)| v.to_vec());
    for n in names {
        for suffix in ["_dropped.mdl", ".mdl"] {
            let mut c: Vec<String> = SRC_CATS.iter().map(|cat| format!("models/weapons/w_{cat}_{n}{suffix}")).collect();
            c.push(format!("models/weapons/w_{n}{suffix}"));
            if let Some(p) = c.into_iter().find(|p| has(p)) {
                return Some(p);
            }
        }
    }
    None
}

const CS2_ALIAS: &[(&str, &[&str])] = &[("m4a1", &["m4a4", "m4a1"]), ("knife", &["knife_default_ct"])];

pub fn cs2_item(e: &Entity, names: &[String]) -> Option<String> {
    let cls = e.class().to_lowercase();
    if let Some(m) = own_model(e, ".vmdl") {
        return Some(m.to_lowercase().replace('\\', "/"));
    }
    let n = item_name(&cls)?;
    let alts: Vec<&str> = CS2_ALIAS.iter().find(|(k, _)| *k == n).map_or(vec![n], |(_, v)| v.to_vec());
    for n in alts {
        let exact = [format!("/weapon_{n}.vmdl_c"), format!("/{n}.vmdl_c")];
        let cat = format!("_{n}.vmdl_c");
        let hit = names.iter().find(|p| exact.iter().any(|x| p.ends_with(x.as_str()))).or_else(|| {
            names.iter().find(|p| {
                let file = p.rsplit('/').next().unwrap_or(p);
                file.strip_prefix("weapon_").and_then(|f| f.strip_suffix(cat.as_str())).is_some_and(|c| !c.is_empty() && !c.contains('_'))
            })
        });
        if let Some(p) = hit {
            return Some(p.trim_end_matches("_c").to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn ent(kv: &[(&str, &str)]) -> Entity {
        Entity(kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>())
    }

    #[test]
    fn gold() {
        assert_eq!(gold_item(&ent(&[("classname", "armoury_entity"), ("item", "10")])).as_deref(), Some("models/w_awp.mdl"));
        assert_eq!(gold_item(&ent(&[("classname", "armoury_entity")])).as_deref(), Some("models/w_mp5.mdl"));
        assert_eq!(gold_item(&ent(&[("classname", "weapon_c4")])).as_deref(), Some("models/w_backpack.mdl"));
        assert_eq!(gold_item(&ent(&[("classname", "weapon_deagle")])).as_deref(), Some("models/w_deagle.mdl"));
    }

    #[test]
    fn source() {
        let have = ["models/weapons/w_rif_ak47.mdl", "models/weapons/w_rif_ak47_dropped.mdl", "models/weapons/w_eq_fraggrenade.mdl"];
        let has = |p: &str| have.contains(&p);
        assert_eq!(source_item(&ent(&[("classname", "weapon_ak47")]), has).as_deref(), Some("models/weapons/w_rif_ak47_dropped.mdl"));
        assert_eq!(source_item(&ent(&[("classname", "weapon_hegrenade")]), has).as_deref(), Some("models/weapons/w_eq_fraggrenade.mdl"));
        assert_eq!(source_item(&ent(&[("classname", "weapon_awp")]), has), None);
    }

    #[test]
    fn cs2() {
        let names: Vec<String> = ["weapons/models/m4a4/weapon_rif_m4a4.vmdl_c", "weapons/models/grenade/hegrenade/weapon_hegrenade.vmdl_c", "weapons/models/defuser/defuser.vmdl_c", "weapons/models/ak47/weapon_rif_ak47_ag2.vmdl_c", "weapons/models/ak47/weapon_rif_ak47.vmdl_c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let get = |c: &str| cs2_item(&ent(&[("classname", c)]), &names);
        assert_eq!(get("weapon_m4a1").as_deref(), Some("weapons/models/m4a4/weapon_rif_m4a4.vmdl"));
        assert_eq!(get("weapon_hegrenade").as_deref(), Some("weapons/models/grenade/hegrenade/weapon_hegrenade.vmdl"));
        assert_eq!(get("item_defuser").as_deref(), Some("weapons/models/defuser/defuser.vmdl"));
        assert_eq!(get("weapon_ak47").as_deref(), Some("weapons/models/ak47/weapon_rif_ak47.vmdl"));
    }
}
