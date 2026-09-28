#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use glam::{DMat3, DVec3};

use super::model::norm_name;
use crate::s2::world::Xf;

pub const GAME_LUMP: usize = 35;
pub const SPRP: [u8; 4] = *b"sprp";
pub const COMPRESSED: u16 = 1;

#[derive(Clone, Copy, Debug)]
pub struct GameLump {
    pub id: [u8; 4],
    pub flags: u16,
    pub version: u16,
    pub ofs: usize,
    pub len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    V4,
    V5,
    V6,
    Tf2,
    L4d,
    L4d2,
    Portal2,
    Csgo10,
    Csgo11,
}

#[derive(Clone, Copy, Debug)]
pub struct StaticProp {
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub model: u16,
    pub first_leaf: u16,
    pub leaf_count: u16,
    pub solid: u8,
    pub flags: u32,
    pub flags_ex: u32,
    pub skin: i32,
    pub fade: [f32; 2],
    pub lighting_origin: [f32; 3],
    pub forced_fade: f32,
    pub scale: f32,
    pub color: [u8; 4],
}

pub struct StaticProps {
    pub version: u16,
    pub record: usize,
    pub layout: Layout,
    pub names: Vec<String>,
    pub leaves: Vec<u16>,
    pub props: Vec<StaticProp>,
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn f32_at(d: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn vec_at(d: &[u8], o: usize) -> [f32; 3] {
    [f32_at(d, o), f32_at(d, o + 4), f32_at(d, o + 8)]
}

fn count_at(d: &[u8], o: usize, what: &str) -> Result<usize> {
    let raw = d.get(o..o + 4).with_context(|| format!("sprp truncated at the {what} count"))?;
    let n = i32::from_le_bytes(raw.try_into().unwrap());
    if n < 0 {
        bail!("sprp {what} count is negative ({n})");
    }
    Ok(n as usize)
}

pub fn game_lumps(lump: &[u8]) -> Result<Vec<GameLump>> {
    let n = count_at(lump, 0, "game lump")?;
    if 4 + n * 16 > lump.len() {
        bail!("game lump directory has {n} entries but the lump is {} bytes", lump.len());
    }
    Ok((0..n)
        .map(|i| {
            let o = 4 + i * 16;
            let id = u32_at(lump, o).to_be_bytes();
            GameLump { id, flags: u16_at(lump, o + 4), version: u16_at(lump, o + 6), ofs: u32_at(lump, o + 8) as usize, len: u32_at(lump, o + 12) as usize }
        })
        .collect())
}

impl GameLump {
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.id).into_owned()
    }

    pub fn data<'a>(&self, file: &'a [u8]) -> Result<&'a [u8]> {
        if self.flags & COMPRESSED != 0 {
            bail!("game lump {} v{} is LZMA-compressed; not supported", self.name(), self.version);
        }
        file.get(self.ofs..self.ofs + self.len).with_context(|| format!("game lump {} ({}+{}) runs past the end of the file", self.name(), self.ofs, self.len))
    }
}

pub fn layout(version: u16, size: usize) -> Option<Layout> {
    Some(match (version, size) {
        (4, 56) => Layout::V4,
        (5, 60) => Layout::V5,
        (6, 64) => Layout::V6,
        (7, 68) => Layout::L4d,
        (7 | 10, 72) => Layout::Tf2,
        (8, 68) => Layout::L4d2,
        (9, 72) => Layout::Portal2,
        (10, 76) => Layout::Csgo10,
        (11, 80) => Layout::Csgo11,
        _ => return None,
    })
}

fn record(r: &[u8], lay: Layout) -> StaticProp {
    let mut p = StaticProp {
        origin: vec_at(r, 0),
        angles: vec_at(r, 12),
        model: u16_at(r, 24),
        first_leaf: u16_at(r, 26),
        leaf_count: u16_at(r, 28),
        solid: r[30],
        flags: r[31] as u32,
        flags_ex: 0,
        skin: u32_at(r, 32) as i32,
        fade: [f32_at(r, 36), f32_at(r, 40)],
        lighting_origin: vec_at(r, 44),
        forced_fade: 1.0,
        scale: 1.0,
        color: [255; 4],
    };
    if lay != Layout::V4 {
        p.forced_fade = f32_at(r, 56);
    }
    match lay {
        Layout::Tf2 => p.flags |= u32_at(r, 64),
        Layout::Csgo10 => {
            p.flags_ex = u32_at(r, 72);
            p.color = r[64..68].try_into().unwrap();
        }
        Layout::Csgo11 => {
            p.flags_ex = u32_at(r, 72);
            p.scale = f32_at(r, 76);
            p.color = r[64..68].try_into().unwrap();
        }
        _ => {}
    }
    p
}

impl StaticProps {
    pub fn parse(d: &[u8], version: u16) -> Result<StaticProps> {
        let nd = count_at(d, 0, "dictionary")?;
        let mut o = 4;
        let end = o + nd * 128;
        if end > d.len() {
            bail!("sprp dictionary of {nd} names runs past the lump ({} bytes)", d.len());
        }
        let names = (0..nd)
            .map(|i| {
                let s = &d[o + i * 128..o + (i + 1) * 128];
                let n = s.iter().position(|&c| c == 0).unwrap_or(128);
                norm_name(&String::from_utf8_lossy(&s[..n]))
            })
            .collect();
        o = end;
        let nl = count_at(d, o, "leaf")?;
        o += 4;
        let end = o + nl * 2;
        if end > d.len() {
            bail!("sprp leaf list of {nl} runs past the lump ({} bytes)", d.len());
        }
        let leaves = (0..nl).map(|i| u16_at(d, o + i * 2)).collect();
        o = end;
        let n = count_at(d, o, "prop")?;
        o += 4;
        let rest = d.len() - o.min(d.len());
        if n == 0 {
            return Ok(StaticProps { version, record: 0, layout: layout(version, 0).unwrap_or(Layout::V4), names, leaves, props: Vec::new() });
        }
        let size = rest / n;
        if size * n != rest {
            bail!("sprp v{version}: {rest} bytes for {n} props leaves a remainder of {}", rest % n);
        }
        let lay = layout(version, size).with_context(|| format!("unknown static prop layout: version {version}, {size} bytes per prop"))?;
        let props = d[o..].chunks_exact(size).map(|r| record(r, lay)).collect();
        Ok(StaticProps { version, record: size, layout: lay, names, leaves, props })
    }

    pub fn from_bsp(file: &[u8], game_lump: &[u8]) -> Result<Option<StaticProps>> {
        let Some(g) = game_lumps(game_lump)?.into_iter().find(|g| g.id == SPRP) else { return Ok(None) };
        StaticProps::parse(g.data(file)?, g.version).map(Some)
    }

    pub fn name(&self, p: &StaticProp) -> Option<&str> {
        self.names.get(p.model as usize).map(String::as_str)
    }
}

pub fn rotation(angles: [f32; 3]) -> DMat3 {
    let [p, y, r] = angles.map(|a| (a as f64).to_radians());
    DMat3::from_rotation_z(y) * DMat3::from_rotation_y(p) * DMat3::from_rotation_x(r)
}

impl StaticProp {
    pub fn xf(&self) -> Xf {
        let s = if self.scale.is_finite() && self.scale > 0.0 { self.scale as f64 } else { 1.0 };
        Xf { r: rotation(self.angles) * s, t: DVec3::from(self.origin.map(|v| v as f64)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lump(f: &[u8], i: usize) -> &[u8] {
        let o = 8 + i * 16;
        let ofs = u32_at(f, o) as usize;
        let len = u32_at(f, o + 4) as usize;
        &f[ofs..ofs + len]
    }

    #[test]
    fn transform() {
        let r = rotation([0.0, 90.0, 0.0]);
        assert!((r * DVec3::X - DVec3::Y).length() < 1e-9);
        let r = rotation([90.0, 0.0, 0.0]);
        assert!((r * DVec3::X + DVec3::Z).length() < 1e-9);
        let r = rotation([0.0, 0.0, 90.0]);
        assert!((r * DVec3::Y - DVec3::Z).length() < 1e-9);
        let p = StaticProp { origin: [1.0, 2.0, 3.0], angles: [0.0, 90.0, 0.0], model: 0, first_leaf: 0, leaf_count: 0, solid: 0, flags: 0, flags_ex: 0, skin: 0, fade: [0.0; 2], lighting_origin: [0.0; 3], forced_fade: 1.0, scale: 2.0, color: [255; 4] };
        assert!((p.xf().apply(DVec3::X) - DVec3::new(1.0, 4.0, 3.0)).length() < 1e-9);
    }

    #[test]
    #[ignore]
    fn all_maps() {
        let Ok(dirs) = std::env::var("S1_MAPS") else { return };
        for dir in dirs.split(';') {
            let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("bsp"))).collect();
            files.sort();
            let mut tally = std::collections::BTreeMap::new();
            for p in &files {
                let f = std::fs::File::open(p).unwrap();
                let m = unsafe { memmap2::Mmap::map(&f) }.unwrap();
                let name = p.file_name().unwrap().to_string_lossy();
                if &m[..4] != b"VBSP" {
                    println!("{name}: not VBSP");
                    continue;
                }
                let t = std::time::Instant::now();
                let r = StaticProps::from_bsp(&m, lump(&m, GAME_LUMP));
                let dt = t.elapsed().as_secs_f64() * 1e3;
                match r {
                    Ok(Some(s)) => {
                        let scaled: Vec<f32> = s.props.iter().map(|p| p.scale).filter(|v| (v - 1.0).abs() > 1e-4).collect();
                        let lo = s.props.iter().map(|p| p.scale).fold(f32::INFINITY, f32::min);
                        let hi = s.props.iter().map(|p| p.scale).fold(f32::NEG_INFINITY, f32::max);
                        let bad = s.props.iter().filter(|p| s.name(p).is_none()).count();
                        let fx = s.props.iter().filter(|p| p.flags_ex != 0).count();
                        *tally.entry(format!("v{}/{}", s.version, s.record)).or_insert(0) += 1;
                        println!("{name}: bsp v{} sprp v{}/{} {:?} dict {} leaves {} props {} bad index {bad} flagsEx!=0 {fx} scaled {} range {lo:.2}..{hi:.2} {dt:.1}ms", u32_at(&m, 4), s.version, s.record, s.layout, s.names.len(), s.leaves.len(), s.props.len(), scaled.len());
                    }
                    Ok(None) => println!("{name}: no sprp"),
                    Err(e) => println!("{name}: ERROR {e:#}"),
                }
            }
            println!("{dir}: {} maps {tally:?}", files.len());
        }
    }
}
