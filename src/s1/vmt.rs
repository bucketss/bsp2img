#![allow(dead_code)]

use anyhow::{Context, Result, bail};

use crate::mesh::{FourWay, Mode};
use crate::s2::vpk::Fs;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Block(Vec<(String, Value)>),
}

enum Tok {
    Open,
    Close,
    Str(String),
}

fn tokens(text: &str) -> Vec<Tok> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            ch if ch.is_whitespace() => i += 1,
            '/' if c.get(i + 1) == Some(&'/') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '{' => {
                out.push(Tok::Open);
                i += 1;
            }
            '}' => {
                out.push(Tok::Close);
                i += 1;
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < c.len() && c[i] != '"' && c[i] != '\n' {
                    s.push(c[i]);
                    i += 1;
                }
                i += 1;
                out.push(Tok::Str(s));
            }
            _ => {
                let mut s = String::new();
                while i < c.len() && !c[i].is_whitespace() && !matches!(c[i], '{' | '}' | '"') {
                    if c[i] == '/' && c.get(i + 1) == Some(&'/') {
                        break;
                    }
                    s.push(c[i]);
                    i += 1;
                }
                if !(s.starts_with('[') && s.ends_with(']')) {
                    out.push(Tok::Str(s));
                }
            }
        }
    }
    out
}

fn block(t: &[Tok], i: &mut usize) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    while *i < t.len() {
        let key = match &t[*i] {
            Tok::Close => {
                *i += 1;
                return out;
            }
            Tok::Open => {
                *i += 1;
                block(t, i);
                continue;
            }
            Tok::Str(s) => s.to_lowercase(),
        };
        *i += 1;
        match t.get(*i) {
            Some(Tok::Str(v)) => {
                *i += 1;
                out.push((key, Value::Str(v.clone())));
            }
            Some(Tok::Open) => {
                *i += 1;
                let b = block(t, i);
                out.push((key, Value::Block(b)));
            }
            _ => {}
        }
    }
    out
}

pub fn parse(text: &str) -> Result<(String, Vec<(String, Value)>)> {
    let t = tokens(text);
    let mut i = 0;
    let top = block(&t, &mut i);
    let Some((shader, v)) = top.into_iter().next() else { bail!("empty material") };
    match v {
        Value::Block(b) => Ok((shader, b)),
        Value::Str(_) => bail!("material {shader} has no parameter block"),
    }
}

pub fn parse_bytes(b: &[u8]) -> Result<(String, Vec<(String, Value)>)> {
    let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
    parse(&crate::bsp::latin1(b))
}

pub fn path(name: &str) -> String {
    let n = super::fs::norm(name);
    let n = n.strip_suffix(".vmt").unwrap_or(&n);
    let n = n.strip_prefix("materials/").unwrap_or(n);
    format!("materials/{n}.vmt")
}

fn strings(b: &[(String, Value)]) -> Vec<(String, String)> {
    b.iter().filter_map(|(k, v)| match v {
        Value::Str(s) => Some((k.clone(), s.trim().to_string())),
        Value::Block(_) => None,
    }).collect()
}

fn set(params: &mut Vec<(String, String)>, k: &str, v: &str) {
    match params.iter_mut().find(|(pk, _)| pk == k) {
        Some(p) => p.1 = v.to_string(),
        None => params.push((k.to_string(), v.to_string())),
    }
}

pub fn read(fs: &Fs, file: &str, depth: usize) -> Result<(String, Vec<(String, String)>)> {
    let (shader, b) = parse_bytes(&fs.read(file)?).with_context(|| file.to_string())?;
    let shader = shader.to_lowercase();
    if shader != "patch" {
        return Ok((shader, strings(&b)));
    }
    if depth > 8 {
        bail!("{file}: patch nesting too deep");
    }
    let inc = b.iter().find_map(|(k, v)| match v {
        Value::Str(s) if k == "include" => Some(s.clone()),
        _ => None,
    });
    let inc = inc.with_context(|| format!("{file}: patch without include"))?;
    let (shader, mut params) = read(fs, &super::fs::norm(&inc), depth + 1).with_context(|| format!("{file}: include"))?;
    for (k, v) in &b {
        if let (true, Value::Block(sub)) = (k == "insert" || k == "replace", v) {
            for (pk, pv) in strings(sub) {
                set(&mut params, &pk, &pv);
            }
        }
    }
    Ok((shader, params))
}

#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    pub shader: String,
    pub params: Vec<(String, String)>,
    pub base: Option<String>,
    pub mode: Mode,
    pub alpha: f32,
    pub tint: [f32; 3],
    pub skip: bool,
    pub water: bool,
}

pub struct FourWayParams {
    pub tex: [Option<String>; 3],
    pub uv: [[f32; 2]; 3],
    pub four: FourWay,
}

pub fn truthy(v: &str) -> bool {
    let v = v.trim();
    v.eq_ignore_ascii_case("true") || v.parse::<f64>().is_ok_and(|x| x != 0.0)
}

pub fn color(v: &str) -> Option<[f32; 3]> {
    let t = v.trim();
    let scale = if t.starts_with('{') { 255.0 } else { 1.0 };
    let n: Vec<f32> = t.trim_matches(['[', ']', '{', '}', ' ']).split_whitespace().filter_map(|x| x.parse().ok()).collect();
    match n.len() {
        1 => Some([n[0] / scale; 3]),
        3.. => Some([n[0] / scale, n[1] / scale, n[2] / scale]),
        _ => None,
    }
}

impl Material {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str())
    }

    pub fn flag(&self, k: &str) -> bool {
        self.get(k).is_some_and(truthy)
    }

    pub fn texture(&self, k: &str) -> Option<String> {
        self.get(k).filter(|v| !v.trim().is_empty()).map(super::vtf::path)
    }

    fn num(&self, k: &str, d: f32) -> f32 {
        self.get(k).and_then(|v| v.trim().parse::<f32>().ok()).map_or(d, |v| v.clamp(0.0, 1.0))
    }

    fn scale(&self, k: &str) -> [f32; 2] {
        let n: Vec<f32> = self.get(k).unwrap_or("").trim_matches(['[', ']', '{', '}', ' ']).split_whitespace().filter_map(|x| x.parse().ok()).collect();
        match n[..] {
            [a] => [a, a],
            [a, b, ..] => [a, b],
            _ => [1.0, 1.0],
        }
    }

    pub fn four_way(&self) -> Option<FourWayParams> {
        if self.shader != "lightmapped_4wayblend" {
            return None;
        }
        let n = |k: &str, d: f32| self.num(k, d);
        let four = FourWay {
            tex4: None,
            lum: [
                n("$texture1_lumstart", 0.0),
                n("$texture1_lumend", 1.0),
                n("$texture2_lumstart", 0.0),
                n("$texture2_lumend", 1.0),
                n("$texture3_lumstart", 0.0),
                n("$texture3_lumend", 1.0),
                n("$texture4_lumstart", 0.0),
                n("$texture4_lumend", 1.0),
            ],
            blend: [
                n("$texture2_blendstart", 0.0),
                n("$texture2_blendend", 1.0),
                n("$texture3_blendstart", 0.0),
                n("$texture3_blendend", 1.0),
                n("$texture4_blendstart", 0.0),
                n("$texture4_blendend", 1.0),
            ],
            lumblend: [n("$lumblendfactor2", 1.0), n("$lumblendfactor3", 1.0), n("$lumblendfactor4", 1.0)],
            mul3: self.get("$texture3_blendmode").is_some_and(|v| v.trim() == "1"),
            mul4: self.get("$texture4_blendmode").is_some_and(|v| v.trim() == "1"),
        };
        Some(FourWayParams {
            tex: [self.texture("$basetexture2"), self.texture("$basetexture3"), self.texture("$basetexture4")],
            uv: [self.scale("$texture2_uvscale"), self.scale("$texture3_uvscale"), self.scale("$texture4_uvscale")],
            four,
        })
    }

    pub fn fog(&self) -> Option<[f32; 3]> {
        self.get("$fogcolor").and_then(color)
    }

    pub fn from_params(name: &str, shader: String, params: Vec<(String, String)>) -> Material {
        let mut m = Material { name: name.to_string(), shader, params, base: None, mode: Mode::Opaque, alpha: 1.0, tint: [1.0; 3], skip: false, water: false };
        m.base = m.texture("$basetexture");
        m.alpha = m.get("$alpha").and_then(|v| v.trim().parse::<f32>().ok()).map_or(1.0, |a| a.clamp(0.0, 1.0));
        m.mode = if m.flag("$additive") {
            Mode::Additive
        } else if m.flag("$translucent") || m.alpha < 1.0 {
            Mode::Blend
        } else if m.flag("$alphatest") {
            Mode::AlphaTest
        } else {
            Mode::Opaque
        };
        for k in ["$color", "$color2"] {
            if let Some(c) = m.get(k).and_then(color) {
                m.tint = std::array::from_fn(|i| m.tint[i] * c[i]);
            }
        }
        m.skip = name.starts_with("tools/") || m.flag("%compilenodraw");
        m.water = m.shader == "water";
        m
    }
}

pub fn load(fs: &Fs, name: &str) -> Result<Material> {
    let file = path(name);
    let (shader, params) = read(fs, &file, 0)?;
    let n = file.strip_prefix("materials/").and_then(|n| n.strip_suffix(".vmt")).unwrap_or(&file);
    Ok(Material::from_params(n, shader, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traps() {
        let t = "// comment\nLightmappedGeneric\n{\n$baseTexture \"de_dust\\sand01.vtf\" [$WIN32]\n\"$translucent\" 1\n\"$color\" \"{255 128 0}\"\n\"LightmappedGeneric_DX9\" { \"$basetexture\" \"x\" }\n\"$alphatest\"\n}";
        let (sh, b) = parse(t).unwrap();
        assert_eq!(sh, "lightmappedgeneric");
        let m = Material::from_params("de_dust/x", sh, strings(&b));
        assert_eq!(m.base.as_deref(), Some("materials/de_dust/sand01.vtf"));
        assert_eq!(m.mode, Mode::Blend);
        assert_eq!(m.tint, [1.0, 128.0 / 255.0, 0.0]);
        assert_eq!(color("[0.5 0.5 1]"), Some([0.5, 0.5, 1.0]));
    }
}
