use anyhow::{Context, Result};
use glam::DVec3;

use super::sim_s2::MERGE;
use crate::bsp::Bsp;
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
    pub root: String,
}

impl Cs2 {
    pub fn load(bsp: &Bsp) -> Result<Cs2> {
        let x = bsp.extra.as_ref().context("CS2 map data missing")?;
        let root = x.root.as_deref().context("CS2 install not found; surface properties and weapons.vdata are needed for wallbang (use --game)")?;
        let sp = Surfaces::load(Some(root))?;
        let mut data_hash = FNV0;
        fnv(&mut data_hash, &x.fs.read(VDATA).with_context(|| format!("reading {VDATA}"))?);
        let weapons = weapons(&x.fs)?;
        let bullet = bullet::load(&x.fs, &bsp.name().to_lowercase(), &sp)?;
        fnv(&mut data_hash, &bullet.digest.to_le_bytes());
        Ok(Cs2 { bullet, weapons, surf_hash: sp.digest, data_hash, root: root.display().to_string() })
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
