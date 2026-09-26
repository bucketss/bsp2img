use anyhow::{Result, bail, ensure};

use super::buf::Rd;
use super::kv3;

pub const CROUCH: u64 = 0x10000;

pub struct Area {
    pub poly: Vec<[f32; 3]>,
    pub flags: u64,
}

pub struct Ladder {
    pub top: [f32; 3],
    pub bottom: [f32; 3],
    pub width: f32,
}

pub struct NavMesh {
    pub areas: Vec<Area>,
    pub ladders: Vec<Ladder>,
}

fn skip_kv3(r: &mut Rd) -> Result<()> {
    r.align(8);
    let (_, n) = kv3::parse_len(&r.b[r.p..])?;
    r.p += n;
    Ok(())
}

pub fn parse(b: &[u8]) -> Result<NavMesh> {
    let mut r = Rd::new(b);
    let magic = r.u32()?;
    ensure!(magic == 0xFEEDFACE, "not a nav file");
    let ver = r.u32()?;
    if !(31..=36).contains(&ver) {
        bail!("nav version {ver} not supported (31 to 36 are)");
    }
    let _sub = r.u32()?;
    let _flags = r.u32()?;
    if ver >= 36 {
        skip_kv3(&mut r)?;
    }
    let nc = r.u32()? as usize;
    ensure!(nc * 12 <= b.len(), "nav corner count");
    let corners = (0..nc).map(|_| r.vec3()).collect::<Result<Vec<_>>>()?;
    let np = r.u32()? as usize;
    ensure!(np * 5 <= b.len(), "nav polygon count");
    let mut polys = Vec::with_capacity(np);
    for _ in 0..np {
        let n = r.u8()? as usize;
        let idx = (0..n).map(|_| r.u32().map(|i| i as usize)).collect::<Result<Vec<_>>>()?;
        if ver >= 35 {
            r.u32()?;
        }
        ensure!(idx.iter().all(|&i| i < nc), "nav corner index");
        polys.push(idx);
    }
    if ver >= 32 {
        r.u32()?;
    }
    if ver >= 35 {
        let nm = r.u32()?;
        for _ in 0..nm {
            r.cstr()?;
            r.skip(48)?;
        }
    }
    if ver >= 36 {
        skip_kv3(&mut r)?;
    }
    let na = r.u32()? as usize;
    ensure!(na * 20 <= b.len(), "nav area count");
    let mut areas = Vec::with_capacity(na);
    for _ in 0..na {
        let _id = r.u32()?;
        let flags = r.u64()?;
        let _hull = r.u8()?;
        let pi = r.u32()? as usize;
        r.f32()?;
        let Some(poly) = polys.get(pi) else { bail!("nav polygon index") };
        for _ in 0..poly.len() {
            let n = r.u32()? as usize;
            r.skip(n * 8)?;
        }
        r.u8()?;
        r.u32()?;
        for _ in 0..2 {
            let n = r.u32()? as usize;
            r.skip(n * 4)?;
        }
        areas.push(Area { poly: poly.iter().map(|&i| corners[i]).collect(), flags });
    }
    let nl = r.u32()? as usize;
    ensure!(nl * 40 <= b.len(), "nav ladder count");
    let mut ladders = Vec::with_capacity(nl);
    for _ in 0..nl {
        let _id = r.u32()?;
        let width = r.f32()?;
        let top = r.vec3()?;
        let bottom = r.vec3()?;
        r.f32()?;
        r.skip(4 * if ver >= 35 { 8 } else { 6 })?;
        ladders.push(Ladder { top, bottom, width });
    }
    Ok(NavMesh { areas, ladders })
}

impl NavMesh {
    pub fn floor_below(&self, x: f32, y: f32, zmax: f32) -> Option<f32> {
        let mut best: Option<f32> = None;
        for a in &self.areas {
            let p = &a.poly;
            for i in 1..p.len().saturating_sub(1) {
                let (a, b, c) = (p[0], p[i], p[i + 1]);
                let d = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
                if d.abs() < 1e-6 {
                    continue;
                }
                let w0 = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / d;
                let w1 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / d;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                if z <= zmax && best.is_none_or(|b| z > b) {
                    best = Some(z);
                }
            }
        }
        best
    }
}
