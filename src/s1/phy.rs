use anyhow::{Result, bail, ensure};
use glam::Vec3;

const INCH: f32 = 1.0 / 0.0254;
const HEADER: usize = 16;
const SURFACE: usize = 48;
const NODE: usize = 28;
const LEDGE: usize = 16;
const TRI: usize = 16;
const MAX_LEDGES: usize = 1 << 16;

pub struct Convex {
    pub pts: Vec<Vec3>,
    pub tris: Vec<[u32; 3]>,
}

impl Convex {
    pub fn planes(&self) -> Vec<[f32; 4]> {
        let c = self.pts.iter().copied().sum::<Vec3>() / self.pts.len().max(1) as f32;
        let mut out: Vec<[f32; 4]> = Vec::new();
        for t in &self.tris {
            let [a, b, d] = t.map(|i| self.pts[i as usize]);
            let Some(mut n) = (b - a).cross(d - a).try_normalize() else { continue };
            if n.dot(c - a) > 0.0 {
                n = -n;
            }
            let dist = n.dot(a);
            if !out.iter().any(|p| Vec3::new(p[0], p[1], p[2]).dot(n) > 0.9999 && (p[3] - dist).abs() < 0.01) {
                out.push([n.x, n.y, n.z, dist]);
            }
        }
        out
    }
}

fn i32_at(b: &[u8], o: usize) -> Result<i32> {
    let Some(s) = b.get(o..o + 4) else { bail!("phy truncated at {o}") };
    Ok(i32::from_le_bytes(s.try_into().unwrap()))
}

fn f32_at(b: &[u8], o: usize) -> Result<f32> {
    Ok(f32::from_bits(i32_at(b, o)? as u32))
}

fn at(base: usize, ofs: i32) -> Result<usize> {
    base.checked_add_signed(ofs as isize).ok_or_else(|| anyhow::anyhow!("phy offset {ofs} out of range"))
}

fn ledge(b: &[u8], l: usize) -> Result<Convex> {
    let pts_at = at(l, i32_at(b, l)?)?;
    let n = (i32_at(b, l + 12)? & 0xffff) as usize;
    let mut tris = Vec::with_capacity(n);
    let mut pts = Vec::new();
    let mut map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for k in 0..n {
        let t = l + LEDGE + k * TRI;
        let mut tri = [0u32; 3];
        for (j, v) in tri.iter_mut().enumerate() {
            let i = i32_at(b, t + 4 + j * 4)? as u32 & 0xffff;
            *v = match map.get(&i) {
                Some(&m) => m,
                None => {
                    let p = pts_at + i as usize * 16;
                    let (x, y, z) = (f32_at(b, p)?, f32_at(b, p + 4)?, f32_at(b, p + 8)?);
                    pts.push(Vec3::new(x, z, -y) * INCH);
                    map.insert(i, pts.len() as u32 - 1);
                    pts.len() as u32 - 1
                }
            };
        }
        tris.push(tri);
    }
    Ok(Convex { pts, tris })
}

fn surface(b: &[u8], s: usize, out: &mut Vec<Convex>) -> Result<()> {
    let root = at(s, i32_at(b, s + 32)?)?;
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        ensure!(out.len() < MAX_LEDGES && stack.len() < MAX_LEDGES, "phy ledge tree too large");
        let right = i32_at(b, n)?;
        if right == 0 {
            let c = ledge(b, at(n, i32_at(b, n + 4)?)?)?;
            if c.pts.len() >= 4 {
                out.push(c);
            }
        } else {
            ensure!(right > 0, "phy ledge tree goes backwards");
            stack.push(n + NODE);
            stack.push(at(n, right)?);
        }
    }
    Ok(())
}

pub fn parse(b: &[u8]) -> Result<Vec<Convex>> {
    ensure!(b.len() >= HEADER && i32_at(b, 0)? == HEADER as i32, "not a phy file");
    let solids = i32_at(b, 8)?;
    ensure!((0..=128).contains(&solids), "phy solid count {solids}");
    let mut p = HEADER;
    let mut out = Vec::new();
    for _ in 0..solids {
        let size = i32_at(b, p)?;
        ensure!(size > 0 && p + 4 + size as usize <= b.len(), "phy solid size {size}");
        let d = p + 4;
        if b[d..].starts_with(b"VPHY") {
            let kind = i32_at(b, d + 4)? >> 16;
            if kind == 0 {
                surface(b, d + 28, &mut out)?;
            }
        } else {
            ensure!(b.get(d + SURFACE - 4..d + SURFACE) == Some(b"IVPS"), "phy solid is neither VPHY nor IVPS");
            surface(b, d, &mut out)?;
        }
        p = d + size as usize;
    }
    Ok(out)
}
