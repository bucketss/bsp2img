use anyhow::{Context, Result};
use glam::DVec3;

use super::cache::sum;
use super::sim_s2::MERGE;
use crate::bsp::Bsp;
use crate::extra::{Extra, PhysSrc};
use crate::nav::Stance;
use crate::s2::bullet::{self, Bullet, Crossing};
use crate::s2::surfprop::{FNV0, Surfaces, Weapon, fnv, weapons};

pub const EYE_STAND: f64 = 64.0;
pub const EYE_DUCK: f64 = 46.0;
const VDATA: &str = "scripts/weapons.vdata_c";
const ALIASES: [(&str, &str); 2] = [("scout", "ssg08"), ("m4a1_sil", "m4a1_silencer")];

pub struct Cs2 {
    pub bullet: Bullet,
    pub weapons: Vec<Weapon>,
    pub surf_hash: u64,
    pub data_hash: u64,
    pub map_hash: u64,
    pub root: String,
}

fn map_hash(x: &Extra, map: &str) -> u64 {
    let mut names = vec![format!("maps/{map}/world.vwrld_c"), format!("maps/{map}/world_physics.vmdl_c"), format!("maps/{map}.nav")];
    names.extend(x.fs.names_with(&format!("maps/{map}/entities/")));
    if let PhysSrc::Cs2(s) = &x.phys_src {
        let mut m: Vec<String> = s.ents.iter().map(|e| format!("{}_c", e.0)).collect();
        m.sort_unstable();
        m.dedup();
        names.extend(m);
    }
    let mut h = FNV0;
    for n in names {
        fnv(&mut h, n.as_bytes());
        let v = x.fs.read(&n).map_or(u64::MAX, |d| sum(&d));
        fnv(&mut h, &v.to_le_bytes());
    }
    h
}

impl Cs2 {
    pub fn load(bsp: &Bsp) -> Result<Cs2> {
        let x = bsp.extra.as_ref().context("CS2 map data missing")?;
        let root = x.root.as_deref().context("CS2 install not found; surface properties and weapons.vdata are needed for wallbang (use --game)")?;
        let sp = Surfaces::load(Some(root))?;
        let mut data_hash = FNV0;
        fnv(&mut data_hash, &x.fs.read(VDATA).with_context(|| format!("reading {VDATA}"))?);
        let weapons = weapons(&x.fs)?;
        let map = bsp.name().to_lowercase();
        let bullet = bullet::load(&x.fs, &map, &sp)?;
        fnv(&mut data_hash, &bullet.digest.to_le_bytes());
        let map_hash = map_hash(x, &map);
        Ok(Cs2 { bullet, weapons, surf_hash: sp.digest, data_hash, map_hash, root: root.display().to_string() })
    }

    pub fn weapon(&self, name: &str) -> Option<&Weapon> {
        let n = ALIASES.iter().find(|a| a.0.eq_ignore_ascii_case(name)).map_or(name, |a| a.1);
        self.weapons.iter().find(|w| w.name.eq_ignore_ascii_case(n))
    }

    pub fn eye(stance: Stance) -> f64 {
        match stance {
            Stance::Stand => EYE_STAND,
            Stance::Duck => EYE_DUCK,
        }
    }

    fn query(&self, p: DVec3, d: DVec3) -> Vec<Crossing> {
        let mut buf = Vec::new();
        self.bullet.query(p, d, &mut buf);
        buf
    }

    pub fn open_dist(&self, p: DVec3, d: DVec3, cap: f64) -> Option<f64> {
        let mut last: Option<f64> = None;
        for c in self.query(p, d * cap) {
            if c.exit {
                last = Some(c.t);
            } else if let Some(l) = last
                && (c.t - l) * cap > MERGE as f64
            {
                break;
            }
        }
        last.map(|t| t * cap)
    }

    pub fn solid_at(&self, p: DVec3, cap: f64) -> bool {
        self.query(p, DVec3::Z * cap).first().is_some_and(|c| c.exit)
    }

    pub fn trace(&self, a: DVec3, b: DVec3) -> (f32, DVec3, bool) {
        let xs = self.query(a, b - a);
        let start = xs.first().is_some_and(|c| c.exit);
        match xs.iter().find(|c| !c.exit) {
            Some(c) => (c.t as f32, a + (b - a) * c.t, start),
            None => (1.0, b, start),
        }
    }
}
