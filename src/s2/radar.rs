use anyhow::{Context, Result};

use super::texture::{self, Image};
use super::vpk::Fs;
use crate::overview::{RADAR, Radar};
use crate::radar::{DEFAULT, Official, stem};

pub const DIR: &str = "panorama/images/overheadmaps/";
const KINDS: [&str; 4] = ["_psd", "_tga", "_png", ""];
const EXTS: [&str; 2] = [".vtex_c", ".ctex_c"];

pub fn txt_name(map: &str) -> String {
    format!("resource/overviews/{map}.txt")
}

pub fn load(fs: &Fs, map: &str) -> Result<Option<Official>> {
    let n = txt_name(map);
    if !fs.has(&n) {
        return Ok(None);
    }
    let mut o = Official::parse_bytes(&fs.read(&n)?).with_context(|| format!("parsing {n}"))?;
    if o.name.is_empty() {
        o.name = map.to_string();
    }
    o.frame = native(fs, map, DEFAULT, o.frame);
    for s in &mut o.sections {
        if let Some(f) = s.frame {
            s.frame = Some(native(fs, map, &s.name, f));
        }
    }
    Ok(Some(o))
}

fn native(fs: &Fs, map: &str, section: &str, f: Radar) -> Radar {
    let w = image_name(fs, map, section)
        .and_then(|n| fs.read_limit(&n, texture::HEAD).ok())
        .and_then(|h| texture::info(&h).ok())
        .map_or(0, |i| i.w);
    if w == 0 || w == RADAR as usize {
        return f;
    }
    Radar { scale: f.scale * w as f64 / RADAR as f64, ..f }
}

pub fn image_name(fs: &Fs, map: &str, section: &str) -> Option<String> {
    let base = format!("{DIR}{}_radar", stem(map, section)).to_lowercase();
    for ext in EXTS {
        for k in KINDS {
            let n = format!("{base}{k}{ext}");
            if fs.has(&n) {
                return Some(n);
            }
        }
    }
    let mut any: Vec<String> = fs
        .names_with(&base)
        .into_iter()
        .filter(|n| EXTS.iter().any(|e| n.ends_with(e)) && n[base.len()..].starts_with('_'))
        .collect();
    any.sort_by_key(|n| (!KINDS[..3].iter().any(|k| n[base.len()..].starts_with(&format!("{k}_"))), n.clone()));
    any.into_iter().next()
}

pub fn image(fs: &Fs, map: &str, section: &str, max_dim: usize) -> Result<Option<(String, Image)>> {
    let Some(n) = image_name(fs, map, section) else { return Ok(None) };
    let img = texture::load(fs, &n, max_dim).with_context(|| format!("decoding {n}"))?;
    Ok(Some((n, img)))
}

