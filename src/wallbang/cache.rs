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
            CacheLoc::Program => crate::paths::exe_dir().map(|d| d.join("cache")),
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

pub fn sum(d: &[u8]) -> u64 {
    let mut h = FNV0 ^ d.len() as u64;
    let mut it = d.chunks_exact(8);
    for c in &mut it {
        h = (h ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(0x100_0000_01b3).rotate_left(29);
    }
    fnv(&mut h, it.remainder());
    h
}

pub fn file_hash(p: &Path) -> Option<u64> {
    std::fs::read(p).ok().map(|d| sum(&d))
}

fn version() -> u64 {
    let mut h = FNV0;
    fnv(&mut h, &SIM_VERSION.to_le_bytes());
    fnv(&mut h, env!("BSP2IMG_SIM_SRC").as_bytes());
    h
}

pub struct Key {
    bytes: Vec<u8>,
    slot: u64,
    name: String,
}

pub fn key(bsp: &Bsp, base: &Base, ents: EntOpts, o: &ShotOpts) -> Option<Key> {
    let mut b = Vec::new();
    b.push(base.world.kind());
    let id = std::path::absolute(&bsp.path).unwrap_or_else(|_| bsp.path.clone()).to_string_lossy().to_lowercase();
    b.extend((id.len() as u32).to_le_bytes());
    b.extend(id.as_bytes());
    let ents = if matches!(base.world, super::World::Gold(_)) { ents } else { EntOpts::default() };
    b.extend([ents.skip_translucent.is_some() as u8, ents.skip_translucent.unwrap_or(0), ents.skip_doors as u8]);
    b.extend(o.weapon.name.as_bytes());
    b.extend([0, o.stand as u8, o.duck as u8, crate::nav::links_gate() as u8]);
    if !matches!(base.world, super::World::Gold(_)) {
        b.push(o.walls as u8);
    }
    for v in [o.yaw_step, o.pitch.0, o.pitch.1, o.pitch.2, o.spot_cell, o.max_hit, o.min_thickness, o.slab, NAV_CELL, NAV_STEP] {
        b.extend(v.to_le_bytes());
    }
    let mut slot = FNV0;
    fnv(&mut slot, &b);
    for v in [base.map_hash?, base.mat_hash, base.data_hash] {
        b.extend(v.to_le_bytes());
    }
    let mut h = version();
    fnv(&mut h, &b);
    let st = match (o.stand, o.duck) {
        (true, true) => "both",
        (true, false) => "stand",
        _ => "duck",
    };
    Some(Key { bytes: b, slot, name: format!("{}_{}_{st}_{h:016x}.bin", bsp.name(), o.weapon.name) })
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

pub fn decode(d: &[u8], yaws: usize, pitches: usize) -> Result<(Vec<Spot>, Vec<Shot>, Counts)> {
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
        if walls == 0 || walls as usize > MAX_WALLS || shooter as usize >= spots.len() || yaw_idx as usize >= yaws || pitch_idx as usize >= pitches {
            bail!("bad shot");
        }
        let mut shot = Shot { dir, shooter, yaw_idx, pitch_idx, stance, walls: 0, cross: Default::default(), more: None };
        for _ in 0..walls {
            let mut c = Crossing::default();
            c.entry = r.v3()?;
            c.entry_face = r.i32()?;
            c.thickness = r.f32()?;
            c.path = r.f32()?;
            c.death_at = r.f32()?;
            c.model = r.u16()?;
            c.damage = r.u16()?;
            c.material = r.u8()?;
            c.made = r.u8()? != 0;
            shot.push(c);
        }
        shots.push(shot);
    }
    if !r.0.is_empty() {
        bail!("trailing data");
    }
    Ok((spots, shots, counts))
}

const HEAD: usize = MAGIC.len() + 8 + 8 + 8 + 4;
const MAX_BYTES: u64 = 4 << 30;

pub fn load(p: &Path, k: &Key, yaws: usize, pitches: usize) -> Result<(Vec<Spot>, Vec<Shot>, Counts)> {
    let d = std::fs::read(p)?;
    let head = HEAD + k.bytes.len();
    let u64_at = |i: usize| u64::from_le_bytes(d[i..i + 8].try_into().unwrap());
    if d.len() < head
        || &d[..MAGIC.len()] != MAGIC
        || u64_at(8) != version()
        || u64_at(16) != k.slot
        || d[32..36] != (k.bytes.len() as u32).to_le_bytes()
        || d[HEAD..head] != k.bytes[..]
    {
        bail!("key mismatch");
    }
    if sum(&d[head..]) != u64_at(24) {
        bail!("checksum mismatch");
    }
    let raw = lz4_flex::block::decompress_size_prepended(&d[head..]).map_err(|e| anyhow::anyhow!("lz4: {e}"))?;
    let out = decode(&raw, yaws, pitches)?;
    if let Ok(f) = std::fs::File::options().write(true).open(p) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Ok(out)
}

pub fn save(p: &Path, k: &Key, spots: &[Spot], shots: &[Shot], counts: &Counts) -> Result<()> {
    let body = lz4_flex::block::compress_prepend_size(&encode(spots, shots, counts));
    let mut d = Vec::with_capacity(HEAD + k.bytes.len() + body.len());
    d.extend(MAGIC);
    d.extend(version().to_le_bytes());
    d.extend(k.slot.to_le_bytes());
    d.extend(sum(&body).to_le_bytes());
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

fn head_of(p: &Path) -> Option<(u64, u64)> {
    use std::io::Read;
    let mut b = [0u8; 24];
    std::fs::File::open(p).ok()?.read_exact(&mut b).ok()?;
    (&b[..MAGIC.len()] == MAGIC).then(|| (u64::from_le_bytes(b[8..16].try_into().unwrap()), u64::from_le_bytes(b[16..24].try_into().unwrap())))
}

pub fn prune(dir: &Path, k: &Key) -> (usize, u64) {
    let dir = dir.join("wallbang");
    let Ok(rd) = std::fs::read_dir(&dir) else { return (0, 0) };
    let (ver, mut gone, mut freed) = (version(), 0, 0);
    let mut keep = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Ok(m) = e.metadata() else { continue };
        if !m.is_file() || p.extension().is_none_or(|x| x != "bin") || p.file_name().and_then(|n| n.to_str()) == Some(k.name.as_str()) {
            continue;
        }
        let Some((v, slot)) = head_of(&p) else { continue };
        if v != ver || slot == k.slot {
            if std::fs::remove_file(&p).is_ok() {
                gone += 1;
                freed += m.len();
            }
        } else {
            keep.push((m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), p));
        }
    }
    let cur = std::fs::metadata(dir.join(&k.name)).map_or(0, |m| m.len());
    let mut total = cur + keep.iter().map(|e| e.1).sum::<u64>();
    keep.sort_by_key(|e| e.0);
    for (_, len, p) in keep {
        if total <= MAX_BYTES {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
            gone += 1;
            freed += len;
        }
    }
    (gone, freed)
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
        let shot = |dir: Vec3, shooter: u32, stance: Stance, cs: &[Crossing]| {
            let mut s = Shot { dir, shooter, yaw_idx: 179, pitch_idx: 4, stance, walls: 0, cross: Default::default(), more: None };
            cs.iter().for_each(|c| s.push(*c));
            s
        };
        let shots = vec![
            shot(Vec3::new(0.6, 0.8, 0.0), 0, Stance::Stand, &[cross(10.0, true)]),
            shot(Vec3::new(0.0, -1.0, 0.0), 1, Stance::Duck, &[cross(1.0, true), cross(2.0, false)]),
            shot(Vec3::new(1.0, 0.0, 0.0), 1, Stance::Stand, &[cross(1.0, true), cross(2.0, true), cross(3.0, true), cross(4.0, false)]),
        ];
        let counts = Counts { fired: 9, hits: 8, far: 1, made: 2, died: 1, thin: 3, ..Default::default() };
        let (sp, sh, co) = decode(&encode(&spots, &shots, &counts), 180, 5).unwrap();
        assert_eq!(sp.len(), 2);
        assert!(sp.iter().zip(&spots).all(|(a, b)| a.feet == b.feet && a.stance == b.stance));
        assert!(sh.len() == 3 && sh[2].crossings().len() == 4 && sh.iter().zip(&shots).all(|(a, b)| same(a, b)));
        assert_eq!((co.fired, co.hits, co.far, co.made, co.died, co.thin), (9, 8, 1, 2, 1, 3));
        assert!(decode(&encode(&spots, &shots, &counts)[..40], 180, 5).is_err());
        assert!(decode(&encode(&spots, &shots, &counts), 179, 5).is_err());
        assert!(decode(&encode(&spots, &shots, &counts), 180, 4).is_err());
    }
}
