use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use glam::{DVec3, Vec3};

use super::export::Base;
use super::shots::{Counts, Crossing, MAX_WALLS, Shot, ShotOpts, Spot};
use super::{EntOpts, NAV_CELL, NAV_STEP};
use crate::bsp::Bsp;
use crate::nav::Stance;
use crate::s2::surfprop::{FNV0, fnv};

pub const SIM_VERSION: u32 = 3;

const MAGIC: &[u8; 8] = b"B2IWBSIM";

#[derive(Clone, Debug, Default, PartialEq)]
pub enum CacheLoc {
    #[default]
    Program,
    AppData,
    Custom(String),
}

impl CacheLoc {
    pub fn key(&self) -> &'static str {
        match self {
            CacheLoc::Program => "program",
            CacheLoc::AppData => "appdata",
            CacheLoc::Custom(_) => "custom",
        }
    }

    pub fn from_kv(kind: &str, custom: &str) -> CacheLoc {
        match kind.trim() {
            "appdata" => CacheLoc::AppData,
            "custom" => CacheLoc::Custom(custom.to_string()),
            _ => CacheLoc::Program,
        }
    }

    pub fn dir(&self) -> Option<PathBuf> {
        match self {
            CacheLoc::Program => std::env::current_dir().ok().map(|d| d.join("cache")),
            CacheLoc::AppData => crate::paths::app_dir().map(|d| d.join("cache")),
            CacheLoc::Custom(p) if !p.trim().is_empty() => Some(PathBuf::from(p.trim())),
            CacheLoc::Custom(_) => None,
        }
    }

    pub fn saved() -> CacheLoc {
        let text = crate::paths::app_dir().and_then(|d| std::fs::read_to_string(d.join("gui.cfg")).ok()).unwrap_or_default();
        let get = |k: &str| text.lines().find_map(|l| l.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v.to_string())).unwrap_or_default();
        CacheLoc::from_kv(&get("cache"), &get("cache_dir"))
    }
}

pub fn file_hash(p: &Path) -> Option<u64> {
    let d = std::fs::read(p).ok()?;
    let mut h = FNV0;
    fnv(&mut h, &d);
    Some(h)
}

pub struct Key {
    bytes: Vec<u8>,
    name: String,
}

pub fn key(bsp: &Bsp, base: &Base, ents: EntOpts, o: &ShotOpts) -> Option<Key> {
    let mut b = Vec::new();
    b.extend(SIM_VERSION.to_le_bytes());
    b.push(base.world.kind());
    b.extend(file_hash(&bsp.path)?.to_le_bytes());
    b.extend(base.mat_hash.to_le_bytes());
    b.extend(base.data_hash.to_le_bytes());
    let ents = if matches!(base.world, super::World::Gold(_)) { ents } else { EntOpts::default() };
    b.extend([ents.skip_translucent.is_some() as u8, ents.skip_translucent.unwrap_or(0), ents.skip_doors as u8]);
    b.extend(o.weapon.name.as_bytes());
    b.extend([0, o.stand as u8, o.duck as u8]);
    for v in [o.yaw_step, o.pitch.0, o.pitch.1, o.pitch.2, o.spot_cell, o.max_hit, o.min_thickness, o.slab, NAV_CELL, NAV_STEP] {
        b.extend(v.to_le_bytes());
    }
    let mut h = FNV0;
    fnv(&mut h, &b);
    let st = match (o.stand, o.duck) {
        (true, true) => "both",
        (true, false) => "stand",
        _ => "duck",
    };
    Some(Key { bytes: b, name: format!("{}_{}_{st}_{h:016x}.bin", bsp.name(), o.weapon.name) })
}

pub fn path(dir: &Path, k: &Key) -> PathBuf {
    dir.join("wallbang").join(&k.name)
}

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend(v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend(v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend(v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend(v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend(v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend(v.to_le_bytes());
    }
    fn v3(&mut self, v: Vec3) {
        self.f32(v.x);
        self.f32(v.y);
        self.f32(v.z);
    }
}

struct R<'a>(&'a [u8]);

impl R<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.0.len() < N {
            bail!("truncated");
        }
        let (a, b) = self.0.split_at(N);
        self.0 = b;
        Ok(a.try_into().unwrap())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take()?))
    }
    fn v3(&mut self) -> Result<Vec3> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
}

fn stance_of(v: u8) -> Result<Stance> {
    match v {
        0 => Ok(Stance::Stand),
        1 => Ok(Stance::Duck),
        _ => bail!("bad stance"),
    }
}

fn counts_list(c: &mut Counts) -> [&mut u64; 14] {
    [
        &mut c.fired,
        &mut c.hits,
        &mut c.far,
        &mut c.no_exit,
        &mut c.behind,
        &mut c.slab,
        &mut c.column,
        &mut c.void,
        &mut c.graze,
        &mut c.corner,
        &mut c.thin,
        &mut c.cut,
        &mut c.made,
        &mut c.died,
    ]
}

pub fn encode(spots: &[Spot], shots: &[Shot], counts: &Counts) -> Vec<u8> {
    let mut w = W(Vec::with_capacity(64 + spots.len() * 25 + shots.len() * 100));
    let mut c = *counts;
    for v in counts_list(&mut c) {
        w.u64(*v);
    }
    w.u64(spots.len() as u64);
    for s in spots {
        w.f64(s.feet.x);
        w.f64(s.feet.y);
        w.f64(s.feet.z);
        w.u8(s.stance as u8);
    }
    w.u64(shots.len() as u64);
    for s in shots {
        w.v3(s.dir);
        w.u32(s.shooter);
        w.u16(s.yaw_idx);
        w.u8(s.pitch_idx);
        w.u8(s.stance as u8);
        w.u8(s.walls);
        for c in s.crossings() {
            w.v3(c.entry);
            w.i32(c.entry_face);
            w.f32(c.thickness);
            w.f32(c.path);
            w.f32(c.death_at);
            w.u16(c.model);
            w.u16(c.damage);
            w.u8(c.material);
            w.u8(c.made as u8);
        }
    }
    w.0
}

pub fn decode(d: &[u8]) -> Result<(Vec<Spot>, Vec<Shot>, Counts)> {
    let mut r = R(d);
    let mut counts = Counts::default();
    for v in counts_list(&mut counts) {
        *v = r.u64()?;
    }
    let n = r.u64()? as usize;
    let mut spots = Vec::with_capacity(n.min(d.len() / 25));
    for _ in 0..n {
        let feet = DVec3::new(r.f64()?, r.f64()?, r.f64()?);
        spots.push(Spot { feet, stance: stance_of(r.u8()?)? });
    }
    let n = r.u64()? as usize;
    let mut shots = Vec::with_capacity(n.min(d.len() / 24));
    for _ in 0..n {
        let dir = r.v3()?;
        let shooter = r.u32()?;
        let yaw_idx = r.u16()?;
        let pitch_idx = r.u8()?;
        let stance = stance_of(r.u8()?)?;
        let walls = r.u8()?;
        if walls == 0 || walls as usize > MAX_WALLS || shooter as usize >= spots.len() {
            bail!("bad shot");
        }
        let mut cross = [Crossing::default(); MAX_WALLS];
        for c in &mut cross[..walls as usize] {
            c.entry = r.v3()?;
            c.entry_face = r.i32()?;
            c.thickness = r.f32()?;
            c.path = r.f32()?;
            c.death_at = r.f32()?;
            c.model = r.u16()?;
            c.damage = r.u16()?;
            c.material = r.u8()?;
            c.made = r.u8()? != 0;
        }
        shots.push(Shot { dir, shooter, yaw_idx, pitch_idx, stance, walls, cross });
    }
    if !r.0.is_empty() {
        bail!("trailing data");
    }
    Ok((spots, shots, counts))
}

pub fn load(p: &Path, k: &Key) -> Result<(Vec<Spot>, Vec<Shot>, Counts)> {
    let d = std::fs::read(p)?;
    let head = MAGIC.len() + 4 + k.bytes.len();
    if d.len() < head || &d[..MAGIC.len()] != MAGIC || d[MAGIC.len()..MAGIC.len() + 4] != (k.bytes.len() as u32).to_le_bytes() || d[MAGIC.len() + 4..head] != k.bytes[..] {
        bail!("key mismatch");
    }
    let raw = lz4_flex::block::decompress_size_prepended(&d[head..]).map_err(|e| anyhow::anyhow!("lz4: {e}"))?;
    decode(&raw)
}

pub fn save(p: &Path, k: &Key, spots: &[Spot], shots: &[Shot], counts: &Counts) -> Result<()> {
    let body = lz4_flex::block::compress_prepend_size(&encode(spots, shots, counts));
    let mut d = Vec::with_capacity(MAGIC.len() + 4 + k.bytes.len() + body.len());
    d.extend(MAGIC);
    d.extend((k.bytes.len() as u32).to_le_bytes());
    d.extend(&k.bytes);
    d.extend(body);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, &d)?;
    std::fs::rename(&tmp, p).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &Shot, b: &Shot) -> bool {
        let c = |x: &Crossing, y: &Crossing| {
            (x.entry, x.entry_face, x.thickness, x.path, x.death_at, x.model, x.damage, x.material, x.made)
                == (y.entry, y.entry_face, y.thickness, y.path, y.death_at, y.model, y.damage, y.material, y.made)
        };
        (a.dir, a.shooter, a.yaw_idx, a.pitch_idx, a.stance, a.walls) == (b.dir, b.shooter, b.yaw_idx, b.pitch_idx, b.stance, b.walls)
            && a.crossings().iter().zip(b.crossings()).all(|(x, y)| c(x, y))
    }

    #[test]
    fn round_trip() {
        let spots = vec![
            Spot { feet: DVec3::new(1.5, -2.25, 64.125), stance: Stance::Stand },
            Spot { feet: DVec3::new(-1e5, 3.0, 0.1), stance: Stance::Duck },
        ];
        let cross = |k: f32, made: bool| Crossing {
            entry: Vec3::new(k, -k, k * 0.5),
            entry_face: if made { 7 } else { -1 },
            thickness: k * 0.1,
            path: k * 0.2,
            death_at: f32::from_bits(0x3f80_0001),
            model: 3,
            damage: 41,
            material: b'M',
            made,
        };
        let shots = vec![
            Shot { dir: Vec3::new(0.6, 0.8, 0.0), shooter: 0, yaw_idx: 179, pitch_idx: 4, stance: Stance::Stand, walls: 1, cross: [cross(10.0, true), Crossing::default()] },
            Shot { dir: Vec3::new(0.0, -1.0, 0.0), shooter: 1, yaw_idx: 0, pitch_idx: 10, stance: Stance::Duck, walls: 2, cross: [cross(1.0, true), cross(2.0, false)] },
        ];
        let counts = Counts { fired: 9, hits: 8, far: 1, made: 2, died: 1, thin: 3, ..Default::default() };
        let (sp, sh, co) = decode(&encode(&spots, &shots, &counts)).unwrap();
        assert_eq!(sp.len(), 2);
        assert!(sp.iter().zip(&spots).all(|(a, b)| a.feet == b.feet && a.stance == b.stance));
        assert!(sh.len() == 2 && sh.iter().zip(&shots).all(|(a, b)| same(a, b)));
        assert_eq!((co.fired, co.hits, co.far, co.made, co.died, co.thin), (9, 8, 1, 2, 1, 3));
        assert!(decode(&encode(&spots, &shots, &counts)[..40]).is_err());
    }
}
