use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use super::kv3::{self, Value};
use super::resource::Res;
use super::vpk::{Fs, Vpk};

const VSURF: &str = "surfaceproperties/surfaceproperties.vsurf_c";
const GAME_TXT: &str = "scripts/surfaceproperties_game.txt";

#[derive(Clone, Debug, PartialEq)]
pub struct Surf {
    pub name: String,
    pub dist_mod: f32,
    pub mat: u8,
}

pub fn murmur2(data: &[u8], seed: u32) -> u32 {
    const M: u32 = 0x5BD1_E995;
    let mut h = seed ^ data.len() as u32;
    let mut c = data.chunks_exact(4);
    for k in c.by_ref() {
        let mut k = u32::from_le_bytes(k.try_into().unwrap()).wrapping_mul(M);
        k ^= k >> 24;
        h = h.wrapping_mul(M) ^ k.wrapping_mul(M);
    }
    let t = c.remainder();
    if t.len() == 3 {
        h ^= u32::from(t[2]) << 16;
    }
    if t.len() >= 2 {
        h ^= u32::from(t[1]) << 8;
    }
    if !t.is_empty() {
        h ^= u32::from(t[0]);
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}

pub fn surface_hash(name: &str) -> u32 {
    murmur2(name.to_lowercase().as_bytes(), 0x3141_5926)
}

#[derive(Default)]
pub struct Surfaces {
    base: HashMap<String, String>,
    game: HashMap<String, HashMap<String, Value>>,
    by_hash: HashMap<u32, String>,
    pub digest: u64,
}

pub const FNV0: u64 = 0xcbf2_9ce4_8422_2325;

pub fn fnv(h: &mut u64, b: &[u8]) {
    for &x in b {
        *h ^= x as u64;
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

fn read_mod(root: &Path, m: &str, name: &str) -> Option<Vec<u8>> {
    let v = Vpk::open(&root.join(m).join("pak01_dir.vpk")).ok()?;
    let e = v.entry(name)?;
    v.read_limit(e, usize::MAX).ok()
}

impl Surfaces {
    pub fn load(root: Option<&Path>) -> Result<Surfaces> {
        let mut s = Surfaces { digest: FNV0, ..Default::default() };
        let Some(root) = root else { return Ok(s) };
        for m in ["core", "csgo"] {
            let Some(b) = read_mod(root, m, VSURF) else { continue };
            fnv(&mut s.digest, &b);
            let d = Res::new(b)?.kv("DATA").with_context(|| format!("{m} {VSURF}"))?;
            for p in d.get("SurfacePropertiesList").arr() {
                s.base.insert(p.get("surfacePropertyName").s().to_lowercase(), p.get("base").s().to_lowercase());
            }
        }
        for m in ["core", "csgo"] {
            let Some(b) = read_mod(root, m, GAME_TXT) else { continue };
            fnv(&mut s.digest, &b);
            let d = kv3::parse_text(&String::from_utf8_lossy(&b)).with_context(|| format!("{m} {GAME_TXT}"))?;
            for p in d.get("SurfacePropertiesList").arr() {
                let e = s.game.entry(p.get("surfacePropertyName").s().to_lowercase()).or_default();
                for (k, v) in p.obj() {
                    e.insert(k.clone(), v.clone());
                }
            }
        }
        for n in s.base.keys().chain(s.game.keys()) {
            s.by_hash.insert(surface_hash(n), n.clone());
        }
        Ok(s)
    }

    fn get(&self, name: &str, key: &str) -> Option<&Value> {
        let mut seen = Vec::new();
        let mut n = name.to_lowercase();
        while !n.is_empty() && !seen.contains(&n) {
            if let Some(v) = self.game.get(&n).and_then(|g| g.get(key)) {
                return Some(v);
            }
            let next = self.base.get(&n).cloned().unwrap_or_default();
            seen.push(n);
            n = next;
        }
        None
    }

    pub fn props(&self, name: &str) -> Surf {
        let dist_mod = self.get(name, "bulletPenetrationDistanceModifier").map_or(1.0, |v| v.f() as f32);
        let mat = self.get(name, "gamematerial").and_then(|v| v.s().bytes().next()).unwrap_or(b'C');
        Surf { name: name.to_lowercase(), dist_mod, mat }
    }

    pub fn name_of(&self, hash: u32) -> &str {
        self.by_hash.get(&hash).map_or("default", |s| s.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weapon {
    pub name: String,
    pub damage: f32,
    pub range_mod: f32,
    pub pen: f32,
    pub range: f32,
}

pub fn weapons(fs: &Fs) -> Result<Vec<Weapon>> {
    let d = Res::new(fs.read("scripts/weapons.vdata_c")?)?.kv("DATA").context("weapons.vdata")?;
    let mut out: Vec<Weapon> = d
        .obj()
        .iter()
        .filter(|(_, v)| !v.get("m_flPenetration").is_null())
        .map(|(k, v)| Weapon {
            name: k.strip_prefix("weapon_").unwrap_or(k).to_string(),
            damage: v.get("m_nDamage").f() as f32,
            range_mod: v.get("m_flRangeModifier").f() as f32,
            pen: v.get("m_flPenetration").f() as f32,
            range: v.get("m_flRange").f() as f32,
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash() {
        assert_eq!(murmur2(b"", 0), 0);
        let n = "default";
        assert_eq!(surface_hash(n), surface_hash("DEFAULT"));
    }

    #[test]
    fn text() {
        let v = kv3::parse_text("<!-- kv3 -->\n{ a = [ { n = \"x\" b = 0.5 }, { n = \"y\" // c\n c = false } ] }").unwrap();
        let a = v.get("a").arr();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].get("b").f(), 0.5);
        assert_eq!(a[1].get("n").s(), "y");
        assert!(!a[1].get("c").b());
    }
}
