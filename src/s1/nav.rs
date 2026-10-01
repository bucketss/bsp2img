use anyhow::{Result, bail, ensure};

use crate::navmesh::{Area, Ladder, NavMesh, Places};
use crate::s2::vpk::Fs;
use crate::s2::buf::Rd;

pub const MAGIC: u32 = 0xFEEDFACE;
pub const MAX_VERSION: u32 = 16;
const CROUCH: u32 = 0x1;
pub const SPAWN_PLAYER_START: u32 = 0x80;
pub const SPAWN_CHECKPOINT: u32 = 0x800;
pub const MIN_AREAS: usize = 16;

pub struct SrcNav {
    pub version: u32,
    pub sub_version: u32,
    pub bsp_size: u32,
    pub analyzed: bool,
    pub places: Vec<String>,
    #[allow(dead_code)]
    pub area_ids: Vec<u32>,
    pub area_place: Vec<u16>,
    pub spawn_attrs: Vec<u32>,
    pub connections: usize,
    pub dangling: usize,
    pub mesh: NavMesh,
}

impl SrcNav {
    pub fn places(&self) -> Places {
        Places { names: self.places.clone(), area: self.area_place.clone() }
    }

    pub fn place(&self, area: usize) -> Option<&str> {
        let p = *self.area_place.get(area)? as usize;
        self.places.get(p.checked_sub(1)?).map(String::as_str)
    }
}

pub fn name(map: &str) -> String {
    format!("maps/{map}.nav")
}

pub fn load(fs: &Fs, map: &str, bsp_len: usize, log: &mut dyn FnMut(String)) -> (Option<NavMesh>, Option<Places>, Vec<u32>) {
    let n = name(map);
    if std::env::var("BSP2IMG_SOURCE_NAV").is_ok_and(|v| v == "off") {
        log(format!("BSP2IMG_SOURCE_NAV=off: {n} ignored, timing, svg, auto crop and --hull walk the brush collision instead"));
        return (None, None, Vec::new());
    }
    if !fs.has(&n) {
        log(format!("no {n}: timing, svg, auto crop and --hull walk the brush collision instead"));
        return (None, None, Vec::new());
    }
    match fs.read(&n).and_then(|b| parse(&b)) {
        Ok(s) if s.mesh.areas.len() >= MIN_AREAS => {
            let named = (0..s.mesh.areas.len()).filter(|&a| s.place(a).is_some()).count();
            log(format!(
                "nav {n}: v{}.{}{}, {} areas, {} connections ({} dangling), {} ladders, {} places on {named} areas",
                s.version,
                s.sub_version,
                if s.analyzed { ", analyzed" } else { "" },
                s.mesh.areas.len(),
                s.connections,
                s.dangling,
                s.mesh.ladders.len(),
                s.places.len()
            ));
            if s.bsp_size != 0 && s.bsp_size as usize != bsp_len {
                log(format!("nav {n} was generated for a {} byte BSP, this one is {bsp_len}", s.bsp_size));
            }
            let p = s.places();
            (Some(s.mesh), Some(p), s.spawn_attrs)
        }
        Ok(_) => {
            log(format!("nav {n} has fewer than {MIN_AREAS} areas: timing, svg, auto crop and --hull walk the brush collision instead"));
            (None, None, Vec::new())
        }
        Err(e) => {
            log(format!("nav {n}: {e:#}; timing, svg, auto crop and --hull walk the brush collision instead"));
            (None, None, Vec::new())
        }
    }
}

fn count(r: &mut Rd, each: usize, what: &str) -> Result<usize> {
    let n = r.u32()? as usize;
    ensure!(n.saturating_mul(each) <= r.b.len() - r.p, "nav {what} count {n} past end of file at {}", r.p);
    Ok(n)
}

fn skip_approach(r: &mut Rd) -> Result<()> {
    let n = r.u8()? as usize;
    r.skip(n * 14)
}

pub fn parse(b: &[u8]) -> Result<SrcNav> {
    let mut r = Rd::new(b);
    ensure!(b.len() >= 8, "nav file too short ({} bytes)", b.len());
    let magic = r.u32()?;
    ensure!(magic == MAGIC, "not a nav file (magic {magic:#010x})");
    let ver = r.u32()?;
    if !(1..=MAX_VERSION).contains(&ver) {
        bail!("nav version {ver} not supported (1 to {MAX_VERSION} are)");
    }
    let sub_version = if ver >= 10 { r.u32()? } else { 0 };
    let l4d = ver >= 15 && (13..=14).contains(&sub_version);
    if ver >= 15 && sub_version > 2 && !l4d {
        bail!("nav v{ver} sub-version {sub_version} not supported (1, 2, 13 and 14 are)");
    }
    let bsp_size = if ver >= 4 { r.u32()? } else { 0 };
    let analyzed = ver >= 14 && r.u8()? != 0;
    let mut places = Vec::new();
    if ver >= 5 {
        let n = r.u16()? as usize;
        for _ in 0..n {
            let len = r.u16()? as usize;
            let s = r.take(len)?;
            let e = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            places.push(String::from_utf8_lossy(&s[..e]).into_owned());
        }
        if ver > 11 {
            r.u8()?;
        }
    }
    if l4d {
        while r.u8()? != 0 {}
        r.u16()?;
    }
    let na = count(&mut r, 45, "area")?;
    let mut ids = Vec::with_capacity(na);
    let mut areas = Vec::with_capacity(na);
    let mut area_place = Vec::with_capacity(na);
    let mut spawn_attrs = Vec::new();
    let mut connections = 0;
    for _ in 0..na {
        ids.push(r.u32()?);
        let flags = match ver {
            ..=8 => r.u8()? as u32,
            9..=12 => r.u16()? as u32,
            _ => r.u32()?,
        };
        let nw = r.vec3()?;
        let se = r.vec3()?;
        let ne_z = r.f32()?;
        let sw_z = r.f32()?;
        let mut conns = Vec::new();
        for _ in 0..4 {
            let n = count(&mut r, 4, "connection")?;
            connections += n;
            for _ in 0..n {
                conns.push(r.u32()?);
            }
        }
        let nh = r.u8()? as usize;
        r.skip(nh * if ver >= 2 { 17 } else { 12 })?;
        if ver < 15 {
            skip_approach(&mut r)?;
        }
        let ne = count(&mut r, 10, "encounter path")?;
        for _ in 0..ne {
            if ver < 3 {
                r.skip(32)?;
                let n = r.u8()? as usize;
                r.skip(n * 16)?;
            } else {
                r.skip(10)?;
                let n = r.u8()? as usize;
                r.skip(n * 5)?;
            }
        }
        area_place.push(if ver >= 5 { r.u16()? } else { 0 });
        if ver >= 7 {
            for _ in 0..2 {
                let n = count(&mut r, 4, "area ladder")?;
                r.skip(n * 4)?;
            }
        }
        if ver >= 8 {
            r.skip(8)?;
        }
        if ver >= 11 {
            r.skip(16)?;
        }
        if l4d {
            spawn_attrs.push(r.u32()?);
            r.u16()?;
        }
        if ver >= 16 || l4d {
            let n = count(&mut r, 5, "visible area")?;
            r.skip(n * 5 + 4)?;
        }
        if ver >= 15 && !l4d {
            if sub_version >= 2 {
                r.u32()?;
            } else {
                skip_approach(&mut r)?;
            }
        }
        let poly = vec![nw, [se[0], nw[1], ne_z], se, [nw[0], se[1], sw_z]];
        areas.push(Area { poly, crouch: flags & CROUCH != 0, conns });
    }
    let nl = if ver >= 6 { count(&mut r, 60, "ladder")? } else { 0 };
    let mut ladders = Vec::with_capacity(nl);
    for _ in 0..nl {
        let _id = r.u32()?;
        let width = r.f32()?;
        let top = r.vec3()?;
        let bottom = r.vec3()?;
        r.skip(8)?;
        if ver == 6 {
            r.u8()?;
        }
        let top_areas = (0..4).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
        let bottom_areas = vec![r.u32()?];
        ladders.push(Ladder { top, bottom, width, top_areas, bottom_areas });
    }
    if l4d {
        r.f32()?;
    }
    ensure!(r.p == b.len(), "nav v{ver}: {} trailing bytes after {} areas and {nl} ladders", b.len() - r.p, na);
    let known: std::collections::HashSet<u32> = ids.iter().copied().collect();
    let dangling = areas.iter().flat_map(|a| &a.conns).filter(|c| !known.contains(c)).count();
    let mesh = NavMesh::from_ids(&ids, areas, ladders);
    Ok(SrcNav { version: ver, sub_version, bsp_size, analyzed, places, area_ids: ids, area_place, spawn_attrs, connections, dangling, mesh })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(b: &mut Vec<u8>, v: u32) {
        b.extend_from_slice(&v.to_le_bytes());
    }

    fn f(b: &mut Vec<u8>, v: f32) {
        b.extend_from_slice(&v.to_le_bytes());
    }

    fn file(ver: u32) -> Vec<u8> {
        file_sub(ver, 1)
    }

    fn file_sub(ver: u32, sub: u32) -> Vec<u8> {
        let l4d = sub >= 13;
        let mut b = Vec::new();
        for v in [MAGIC, ver, sub, 1234] {
            u(&mut b, v);
        }
        b.push(1);
        b.extend_from_slice(&2u16.to_le_bytes());
        for p in ["CTSpawn", "BombsiteA"] {
            b.extend_from_slice(&(p.len() as u16 + 1).to_le_bytes());
            b.extend_from_slice(p.as_bytes());
            b.push(0);
        }
        b.push(0);
        if l4d {
            b.extend_from_slice(b"hospital\0");
            b.extend_from_slice(&0u16.to_le_bytes());
        }
        u(&mut b, 2);
        for (id, flags, x0, z, conns, place) in [(5u32, 0u32, 0.0f32, 0.0f32, [0u32, 1, 0, 0], 2u16), (9, CROUCH, 32.0, -8.0, [0, 0, 0, 0], 0)] {
            u(&mut b, id);
            u(&mut b, flags);
            for v in [x0, 0.0, z, x0 + 32.0, 32.0, z, z + 1.0, z - 1.0] {
                f(&mut b, v);
            }
            for n in conns {
                u(&mut b, n);
                for _ in 0..n {
                    u(&mut b, 9);
                }
            }
            b.push(1);
            b.extend_from_slice(&[0; 17]);
            u(&mut b, 1);
            b.extend_from_slice(&[0; 10]);
            b.push(2);
            b.extend_from_slice(&[0; 10]);
            b.extend_from_slice(&place.to_le_bytes());
            u(&mut b, 0);
            u(&mut b, 1);
            u(&mut b, 3);
            b.extend_from_slice(&[0; 24]);
            if l4d {
                u(&mut b, if id == 5 { SPAWN_CHECKPOINT } else { 0 });
                b.extend_from_slice(&[0; 2]);
            }
            u(&mut b, 1);
            b.extend_from_slice(&[0; 9]);
            if !l4d {
                b.push(1);
                b.extend_from_slice(&[0; 14]);
            }
        }
        u(&mut b, 1);
        u(&mut b, 3);
        f(&mut b, 20.0);
        for v in [16.0, 16.0, 100.0, 16.0, 16.0, 0.0, 100.0] {
            f(&mut b, v);
        }
        for v in [0, 5, 0, 0, 0, 9] {
            u(&mut b, v);
        }
        if l4d {
            f(&mut b, 1500.0);
        }
        b
    }

    #[test]
    fn v16_areas_places_ladders() {
        let n = parse(&file(16)).unwrap();
        assert_eq!((n.version, n.sub_version, n.bsp_size, n.analyzed), (16, 1, 1234, true));
        assert_eq!(n.mesh.areas.len(), 2);
        assert_eq!(n.mesh.areas[0].conns, vec![1]);
        assert!(!n.mesh.areas[0].crouch && n.mesh.areas[1].crouch);
        assert_eq!(n.mesh.areas[1].poly, vec![[32.0, 0.0, -8.0], [64.0, 0.0, -7.0], [64.0, 32.0, -8.0], [32.0, 32.0, -9.0]]);
        assert_eq!((n.place(0), n.place(1)), (Some("BombsiteA"), None));
        assert_eq!((n.connections, n.dangling), (1, 0));
        assert_eq!(n.mesh.ladders.len(), 1);
        assert_eq!((n.mesh.ladders[0].top_areas.clone(), n.mesh.ladders[0].bottom_areas.clone()), (vec![0], vec![1]));
    }

    #[test]
    fn l4d_layout() {
        for (ver, sub) in [(15, 13), (16, 14)] {
            let n = parse(&file_sub(ver, sub)).unwrap();
            assert_eq!((n.version, n.sub_version), (ver, sub));
            assert_eq!(n.mesh.areas.len(), 2);
            assert_eq!(n.spawn_attrs, vec![SPAWN_CHECKPOINT, 0]);
            assert_eq!(n.mesh.ladders.len(), 1);
            assert_eq!(n.place(0), Some("BombsiteA"));
        }
        assert!(parse(&file_sub(16, 12)).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        let mut b = file(16);
        b.push(0);
        assert!(parse(&b).err().unwrap().to_string().contains("1 trailing bytes"));
        let b = file(16);
        assert!(parse(&b[..b.len() - 3]).is_err());
        let mut b = file(16);
        b[4] = 17;
        assert!(parse(&b).err().unwrap().to_string().contains("version 17"));
        b[0] = 0;
        assert!(parse(&b).err().unwrap().to_string().contains("not a nav file"));
    }

    #[test]
    #[ignore]
    fn real_files() {
        use crate::s2::vpk::Vpk;
        use std::path::Path;
        let css = Path::new(r"E:\steam\steamapps\common\Counter-Strike Source\cstrike");
        let csgo = Path::new(r"E:\steam\steamapps\common\Counter-Strike Global Offensive\csgo\maps");
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let v = Vpk::open(&css.join("cstrike_pak_dir.vpk")).unwrap();
        let mut names: Vec<String> = v.names().filter(|n| n.starts_with("maps/") && n.ends_with(".nav")).map(str::to_string).collect();
        names.sort();
        for n in names {
            let e = v.entry(&n).unwrap();
            files.push((format!("css {n}"), v.read_limit(e, usize::MAX).unwrap()));
        }
        files.push(("css maps/de_season.nav (loose)".into(), std::fs::read(css.join(r"maps\de_season.nav")).unwrap()));
        let mut loose: Vec<_> = std::fs::read_dir(csgo).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "nav")).collect();
        loose.sort();
        for p in loose {
            files.push((format!("csgo {}", p.file_name().unwrap().to_string_lossy()), std::fs::read(&p).unwrap()));
        }
        for root in [r"E:\steam\steamapps\common\left 4 dead", r"E:\steam\steamapps\common\Left 4 Dead 2"] {
            let mut navs: Vec<_> = std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .filter_map(|d| std::fs::read_dir(d.path().join("maps")).ok())
                .flat_map(|rd| rd.flatten().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "nav"))
                .collect();
            navs.sort();
            for p in navs {
                files.push((format!("l4d {}", p.display()), std::fs::read(&p).unwrap()));
            }
        }
        let mut bad = 0;
        for (name, b) in &files {
            match parse(b) {
                Ok(n) => {
                    let m = &n.mesh;
                    let edges: usize = m.areas.iter().map(|a| a.conns.len()).sum();
                    let crouch = m.areas.iter().filter(|a| a.crouch).count();
                    let named = (0..m.areas.len()).filter(|&k| n.place(k).is_some()).count();
                    println!(
                        "{name}: v{}.{} areas {} conns {} dangling {} links {edges} ladders {} places {} named {named} crouch {crouch}",
                        n.version,
                        n.sub_version,
                        m.areas.len(),
                        n.connections,
                        n.dangling,
                        m.ladders.len(),
                        n.places.len()
                    );
                    bad += (n.dangling != 0) as usize;
                }
                Err(e) => {
                    println!("{name}: ERR {e:#}");
                    bad += 1;
                }
            }
        }
        println!("{} files, {bad} bad", files.len());
        assert_eq!(bad, 0);
    }
}
