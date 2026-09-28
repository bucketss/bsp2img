use anyhow::{Context, Result};

use crate::radar::{Official, stem};
use crate::s2::texture::Image;
use crate::s2::vpk::Fs;

pub const TXT_DIR: &str = "resource/overviews/";
pub const DDS_DIR: &str = "resource/overviews/";
pub const VTF_DIR: &str = "materials/overviews/";

pub fn txt_name(map: &str) -> String {
    format!("{TXT_DIR}{map}.txt")
}

pub fn parse(b: &[u8], map: &str) -> Result<Official> {
    let mut o = Official::parse_bytes(b).with_context(|| format!("parsing {}", txt_name(map)))?;
    if o.name.is_empty() {
        o.name = map.to_string();
    }
    Ok(o)
}

pub fn load(fs: &Fs, map: &str, log: &mut dyn FnMut(String)) -> (Option<Official>, Option<String>) {
    let n = txt_name(map);
    if !fs.has(&n) {
        log(format!("no {n}"));
        return (None, None);
    }
    match fs.read(&n).and_then(|b| parse(&b, map)) {
        Ok(o) => {
            let secs: Vec<&str> = o.sections.iter().map(|s| s.name.as_str()).collect();
            let (f, num) = (o.frame, crate::scene::num);
            let ims: Vec<String> = images(|k| fs.has(k), map, &o).into_iter().map(|(s, i)| format!("{s}={}", i.unwrap_or_else(|| "none".into()))).collect();
            log(format!("overview {n}: pos_x {} pos_y {} scale {}, sections {}, images {}", num(f.pos_x), num(f.pos_y), num(f.scale), secs.join(" "), ims.join(" ")));
            (Some(o), None)
        }
        Err(e) => {
            log(format!("overview: {e:#}"));
            (None, Some(format!("{e:#}")))
        }
    }
}

pub fn decode(name: &str, b: &[u8], max_dim: usize) -> Result<Image> {
    if name.ends_with(".dds") { super::dds::decode(b, max_dim) } else { super::vtf::image(b, max_dim) }
}

pub fn dims(name: &str, head: &[u8]) -> Option<(usize, usize)> {
    if name.ends_with(".dds") {
        super::dds::info(head).ok().map(|i| (i.w, i.h))
    } else {
        super::vtf::info(head).ok().map(|i| (i.w, i.h))
    }
}

pub fn image(fs: &Fs, map: &str, section: &str, material: Option<&str>, max_dim: usize) -> Result<Option<(String, Image)>> {
    let Some(n) = image_name(|k| fs.has(k), map, section, material) else { return Ok(None) };
    let img = decode(&n, &fs.read(&n)?, max_dim).with_context(|| format!("decoding {n}"))?;
    Ok(Some((n, img)))
}

pub fn vmt(texture: &str) -> String {
    format!("\"UnlitGeneric\"
{{
	\"$translucent\" \"1\"
	\"$basetexture\" \"{texture}\"
	\"$vertexalpha\" \"1\"
	\"$no_fullbright\" \"1\"
	\"$ignorez\" \"1\"
}}
")
}

pub fn export(stem: &str, w: usize, h: usize, rgba: &[u8], version: u32) -> Vec<(String, Vec<u8>)> {
    if version >= 21 {
        return vec![(format!("{stem}_radar.dds"), super::dds::encode_bgra(w, h, rgba))];
    }
    let tex = format!("overviews/{stem}_radar");
    vec![
        (format!("{stem}_radar.vtf"), super::vtf::encode_bgra8888(w, h, rgba, super::vtf::FLAGS_RADAR)),
        (format!("{stem}_radar.vmt"), vmt(&tex).into_bytes()),
        (format!("{stem}.vmt"), vmt(&tex).into_bytes()),
    ]
}

fn material_path(m: &str) -> Option<String> {
    let m = m.trim().trim_start_matches(['/', '\\']).replace('\\', "/").to_lowercase();
    let m = m.strip_suffix(".vmt").or_else(|| m.strip_suffix(".vtf")).unwrap_or(&m).to_string();
    if m.is_empty() {
        return None;
    }
    Some(if m.starts_with("materials/") { m } else { format!("materials/{m}") })
}

pub fn image_candidates(map: &str, section: &str, material: Option<&str>) -> Vec<String> {
    let s = stem(map, section).to_lowercase();
    let mat = material.filter(|_| stem(map, section) == map).and_then(material_path);
    let mut out = vec![format!("{DDS_DIR}{s}_radar.dds")];
    out.extend(mat.as_ref().map(|m| format!("{m}_radar.vtf")));
    out.push(format!("{VTF_DIR}{s}_radar.vtf"));
    out.extend(mat.as_ref().map(|m| format!("{m}.vtf")));
    out.push(format!("{DDS_DIR}{s}_radar_spectate.dds"));
    let mut seen = std::collections::HashSet::new();
    out.retain(|n| seen.insert(n.clone()));
    out
}

pub fn image_name(has: impl Fn(&str) -> bool, map: &str, section: &str, material: Option<&str>) -> Option<String> {
    image_candidates(map, section, material).into_iter().find(|n| has(n))
}

pub fn images(has: impl Fn(&str) -> bool, map: &str, o: &Official) -> Vec<(String, Option<String>)> {
    o.levels().map(|s| (s.name.clone(), image_name(&has, map, &s.name, o.material.as_deref()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::Radar;

    #[test]
    fn candidates_and_sections() {
        let o = parse(b"\"de_nuke\" { \"material\" \"overviews/de_nuke\" \"pos_x\" \"-3453\" \"pos_y\" \"2887\" \"scale\" \"7\" \"verticalsections\" { \"default\" { \"AltitudeMax\" \"10000\" \"AltitudeMin\" \"-495\" } \"lower\" { \"AltitudeMax\" \"-495\" \"AltitudeMin\" \"-10000\" } } }", "de_nuke").unwrap();
        assert_eq!(o.frame, Radar { pos_x: -3453.0, pos_y: 2887.0, scale: 7.0 });
        let c = image_candidates("de_nuke", "default", o.material.as_deref());
        assert_eq!(c[..3], ["resource/overviews/de_nuke_radar.dds", "materials/overviews/de_nuke_radar.vtf", "materials/overviews/de_nuke.vtf"]);
        assert_eq!(image_candidates("de_nuke", "lower", Some("overviews/de_nuke"))[1], "materials/overviews/de_nuke_lower_radar.vtf");
        assert_eq!(image_candidates("de_nuke", "lower", Some("overviews/de_nuke")).len(), 3);
        assert_eq!(image_candidates("de_x", "", Some("\\Overviews\\Y.vmt"))[1..4], ["materials/overviews/y_radar.vtf", "materials/overviews/de_x_radar.vtf", "materials/overviews/y.vtf"]);
        let has = |n: &str| n == "resource/overviews/de_nuke_lower_radar.dds" || n == "materials/overviews/de_nuke.vtf";
        let im = images(has, "de_nuke", &o);
        assert_eq!(im[0], ("default".to_string(), Some("materials/overviews/de_nuke.vtf".to_string())));
        assert_eq!(im[1].1.as_deref(), Some("resource/overviews/de_nuke_lower_radar.dds"));
        let o = parse(b"\"pos_x\" \"1\" \"pos_y\" \"2\" \"scale\" \"3\"", "cs_x").unwrap();
        assert_eq!(o.name, "cs_x");
    }

    #[test]
    fn export_round_trip() {
        let (w, h) = (8, 4);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 256) as u8).collect();
        for v in [19, 21] {
            for (n, b) in export("de_x", w, h, &rgba, v) {
                if n.ends_with(".vmt") {
                    let t = String::from_utf8(b).unwrap();
                    let m = crate::s1::vmt::parse(&t).unwrap();
                    assert!(m.0.eq_ignore_ascii_case("UnlitGeneric") && m.1.len() == 5 && t.contains("\"overviews/de_x_radar\""));
                    continue;
                }
                let im = decode(&n, &b, 4096).unwrap();
                assert_eq!((im.w, im.h), (w, h));
                assert_eq!(im.rgba, rgba, "{n}");
                assert_eq!(dims(&n, &b), Some((w, h)));
            }
        }
    }

    fn pak(bsp: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        use crate::s2::buf::{u16_, u32_};
        let f = std::fs::read(bsp).unwrap();
        let (o, n) = (u32_(&f, 8 + 40 * 16).unwrap() as usize, u32_(&f, 12 + 40 * 16).unwrap() as usize);
        let z = &f[o..o + n];
        let e = (0..z.len() - 21).rev().find(|&i| z[i..i + 4] == [0x50, 0x4b, 5, 6]).unwrap();
        let (count, mut p) = (u16_(z, e + 10).unwrap() as usize, u32_(z, e + 16).unwrap() as usize);
        let mut out = std::collections::BTreeMap::new();
        for _ in 0..count {
            let size = u32_(z, p + 20).unwrap() as usize;
            let (nl, xl, cl) = (u16_(z, p + 28).unwrap() as usize, u16_(z, p + 30).unwrap() as usize, u16_(z, p + 32).unwrap() as usize);
            let name = String::from_utf8_lossy(&z[p + 46..p + 46 + nl]).to_lowercase();
            let lo = u32_(z, p + 42).unwrap() as usize;
            if name.contains("overviews/") {
                let d = lo + 30 + u16_(z, lo + 26).unwrap() as usize + u16_(z, lo + 28).unwrap() as usize;
                out.insert(name, z[d..d + size].to_vec());
            }
            p += 46 + nl + xl + cl;
        }
        out
    }

    #[test]
    #[ignore]
    fn real_overviews() {
        use crate::s2::vpk::Vpk;
        use std::collections::BTreeSet;
        use std::path::Path;
        let css = Path::new(r"E:\steam\steamapps\common\Counter-Strike Source\cstrike");
        let csgo = Path::new(r"E:\steam\steamapps\common\Counter-Strike Global Offensive\csgo");
        let v = Vpk::open(&css.join("cstrike_pak_dir.vpk")).unwrap();
        let vnames: BTreeSet<String> = v.names().map(str::to_string).collect();
        let loose = |root: &Path, n: &str| root.join(n).is_file();
        let maps = |dir: &Path| -> BTreeSet<String> {
            std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bsp")).map(|p| p.file_stem().unwrap().to_string_lossy().to_lowercase()).collect()
        };
        let mut bad = 0;
        for (game, root) in [("css", css), ("csgo", csgo)] {
            let mut txts: BTreeSet<String> = BTreeSet::new();
            if game == "css" {
                txts.extend(vnames.iter().filter_map(|n| n.strip_prefix(TXT_DIR)?.strip_suffix(".txt")).map(str::to_string));
            }
            if let Ok(rd) = std::fs::read_dir(root.join(TXT_DIR)) {
                txts.extend(rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "txt")).map(|p| p.file_stem().unwrap().to_string_lossy().to_lowercase()));
            }
            let bsps = maps(&root.join("maps"));
            println!("== {game}: {} txts, {} bsps", txts.len(), bsps.len());
            for m in txts.union(&bsps) {
                let pk = if bsps.contains(m) { pak(&root.join("maps").join(format!("{m}.bsp"))) } else { Default::default() };
                let has = |n: &str| pk.contains_key(n) || (game == "css" && vnames.contains(n)) || loose(root, n);
                let n = txt_name(m);
                let (src, bytes) = if let Some(b) = pk.get(&n) {
                    ("pak", Some(b.clone()))
                } else if game == "css" && vnames.contains(&n) {
                    ("vpk", v.entry(&n).and_then(|e| v.read_limit(e, usize::MAX).ok()))
                } else {
                    ("loose", std::fs::read(root.join(&n)).ok())
                };
                let tag = if bsps.contains(m) { "map" } else { "   " };
                let Some(b) = bytes else {
                    println!("{tag} {m}: NO TXT");
                    continue;
                };
                if !String::from_utf8_lossy(&b).to_lowercase().contains("pos_x") {
                    println!("{tag} {m}: not an overview ({src})");
                    continue;
                }
                match parse(&b, m) {
                    Ok(o) => {
                        let f = o.frame;
                        let secs: Vec<String> = o.levels().map(|s| format!("{} [{} .. {}]", s.name, s.alt_min, s.alt_max)).collect();
                        let ims: Vec<String> = images(has, m, &o).into_iter().map(|(s, i)| format!("{s}={}", i.unwrap_or_else(|| "NONE".into()))).collect();
                        let icons: Vec<String> = o.icons.iter().map(|(n, p)| format!("{n}={},{}", p[0], p[1])).collect();
                        println!(
                            "{tag} {m} ({src}): name {} pos {} {} scale {} rotate {} zoom {} insets {:?} secs {} vols {} icons [{}] mat {:?} img {}",
                            o.name,
                            f.pos_x,
                            f.pos_y,
                            f.scale,
                            o.rotate,
                            o.zoom,
                            o.insets,
                            secs.join(", "),
                            o.volumes().count(),
                            icons.join(" "),
                            o.material.as_deref().unwrap_or("-"),
                            ims.join(" ")
                        );
                    }
                    Err(e) => {
                        bad += 1;
                        println!("{tag} {m} ({src}): ERR {e:#}");
                    }
                }
            }
        }
        assert_eq!(bad, 0);
    }
}
