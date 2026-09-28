use anyhow::{Result, bail, ensure};

use super::buf::Rd;
use super::kv3;
use crate::navmesh::{Area, Ladder, NavMesh};

const CROUCH: u64 = 0x10000;

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
    let mut ids = Vec::with_capacity(na);
    for _ in 0..na {
        ids.push(r.u32()?);
        let flags = r.u64()?;
        let _hull = r.u8()?;
        let pi = r.u32()? as usize;
        r.f32()?;
        let Some(poly) = polys.get(pi) else { bail!("nav polygon index") };
        let mut conns = Vec::new();
        for _ in 0..poly.len() {
            let n = r.u32()? as usize;
            ensure!(n * 8 <= b.len(), "nav connection count");
            for _ in 0..n {
                conns.push(r.u32()?);
                r.u32()?;
            }
        }
        r.u8()?;
        r.u32()?;
        for _ in 0..2 {
            let n = r.u32()? as usize;
            r.skip(n * 4)?;
        }
        areas.push(Area { poly: poly.iter().map(|&i| corners[i]).collect(), crouch: flags & CROUCH != 0, conns });
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
        r.u32()?;
        let top_areas = (0..4).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
        let bottom_areas = (0..if ver >= 35 { 3 } else { 1 }).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
        ladders.push(Ladder { top, bottom, width, top_areas, bottom_areas });
    }
    Ok(NavMesh::from_ids(&ids, areas, ladders))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_way_connection() {
        let mut b = Vec::new();
        let u = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        let f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        for v in [0xFEEDFACE, 31, 0, 0, 6] {
            u(&mut b, v);
        }
        for (x, y, z) in [(0.0, 0.0, 0.0), (32.0, 0.0, 0.0), (32.0, 32.0, 0.0), (0.0, 32.0, 0.0), (64.0, 0.0, -80.0), (64.0, 32.0, -80.0)] {
            f(&mut b, x);
            f(&mut b, y);
            f(&mut b, z);
        }
        u(&mut b, 2);
        for poly in [[0, 1, 2, 3], [1, 4, 5, 2]] {
            b.push(4);
            for i in poly {
                u(&mut b, i);
            }
        }
        u(&mut b, 2);
        for (id, pi, flags, edges) in [(10u32, 0u32, 0u64, [0u32, 1, 0, 0]), (20, 1, CROUCH, [0, 0, 0, 0])] {
            u(&mut b, id);
            b.extend_from_slice(&flags.to_le_bytes());
            b.push(0);
            u(&mut b, pi);
            f(&mut b, 0.0);
            for n in edges {
                u(&mut b, n);
                for _ in 0..n {
                    u(&mut b, 20);
                    u(&mut b, 0);
                }
            }
            b.push(0);
            for _ in 0..3 {
                u(&mut b, 0);
            }
        }
        u(&mut b, 1);
        u(&mut b, 7);
        f(&mut b, 16.0);
        for v in [32.0, 16.0, 0.0, 64.0, 16.0, -80.0, 80.0] {
            f(&mut b, v);
        }
        for v in [0, 10, 99, 0, 0, 20] {
            u(&mut b, v);
        }
        u(&mut b, 0);
        let m = parse(&b).unwrap();
        assert_eq!(m.areas.len(), 2);
        assert_eq!(m.areas[0].conns, vec![1]);
        assert!(m.areas[1].conns.is_empty());
        assert!(!m.areas[0].crouch && m.areas[1].crouch);
        assert_eq!(m.areas[1].poly[1], [64.0, 0.0, -80.0]);
        assert_eq!(m.ladders.len(), 1);
        assert_eq!((m.ladders[0].top_areas.clone(), m.ladders[0].bottom_areas.clone()), (vec![0], vec![1]));
        assert_eq!(m.ladders[0].bottom, [64.0, 16.0, -80.0]);
    }
}
