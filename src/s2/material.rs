use anyhow::Result;

use super::kv3::Value;
use super::resource::Res;
use super::vpk::Fs;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Opaque,
    AlphaTest,
    Blend,
    Additive,
    Mod2x,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    Painted,
    Height,
}

#[derive(Clone, Debug)]
pub enum Fill {
    Texture(String),
    Solid([u8; 4]),
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub tex: String,
    pub uv: [f32; 4],
    pub set: u32,
}

#[derive(Clone, Debug)]
pub struct Material {
    pub fill: Fill,
    pub layer2: Option<Layer>,
    pub layer3: Option<Layer>,
    pub blend: Blend,
    pub mode: Mode,
    pub alpha: f32,
    pub tint: [f32; 3],
    pub tint_mask: Option<String>,
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
    pub uv_set: u32,
    pub paint: bool,
    pub decal: bool,
}

impl Material {
    pub fn solid(c: [u8; 4]) -> Material {
        Material {
            fill: Fill::Solid(c),
            layer2: None,
            layer3: None,
            blend: Blend::Height,
            mode: Mode::Opaque,
            alpha: 1.0,
            tint: [1.0; 3],
            tint_mask: None,
            uv_scale: [1.0; 2],
            uv_offset: [0.0; 2],
            uv_set: 0,
            paint: false,
            decal: false,
        }
    }
}

const SKIP_SHADERS: &[&str] = &["csgo_effects.vfx", "sky.vfx", "csgo_sky.vfx", "tools_sprite.vfx", "csgo_projected_decals.vfx"];
const COLOR_PARAMS: &[&str] = &["g_tColor", "g_tColor1", "g_tColorA", "g_tLayer1Color", "g_tGlassTintColor"];
const LAYER2_PARAMS: &[&str] = &["g_tLayer2Color", "g_tColor2", "g_tColorB"];
const LAYER3_PARAMS: &[&str] = &["g_tLayer3Color", "g_tColor3", "g_tColorC"];
const GLASS_ALPHA: f32 = 0.35;
const WATER_ALPHA: f32 = 0.75;

fn param<'a>(d: &'a Value, list: &str, key: &str, name: &str) -> Option<&'a Value> {
    d.get(list).arr().iter().find(|p| p.get("m_name").s() == name).map(|p| p.get(key))
}

fn int(d: &Value, name: &str) -> i64 {
    param(d, "m_intParams", "m_nValue", name).map_or(0, |v| v.i())
}

fn vector(d: &Value, name: &str) -> Option<Vec<f64>> {
    param(d, "m_vectorParams", "m_value", name).map(|v| v.fv()).filter(|v| v.len() >= 3)
}

fn texture(d: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| param(d, "m_textureParams", "m_pValue", n)).map(|v| v.s().to_lowercase()).filter(|s| !s.is_empty())
}

fn first_vec(d: &Value, names: &[&str]) -> Option<Vec<f64>> {
    names.iter().find_map(|n| vector(d, n))
}

fn transform(d: &Value, scales: &[&str], offsets: &[&str]) -> [f32; 4] {
    let s = first_vec(d, scales).map_or([1.0; 2], |v| [v[0], v[1]]);
    let o = first_vec(d, offsets).map_or([0.0; 2], |v| [v[0], v[1]]);
    [s[0] as f32, s[1] as f32, o[0] as f32, o[1] as f32]
}

pub fn skip_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("materials/tools/") || n.contains("/tools/tools") || n.contains("nodraw") || n.contains("toolsblocklight")
}

fn overlay_mode(v: i64) -> Mode {
    match v {
        2 => Mode::AlphaTest,
        3 | 6 => Mode::Mod2x,
        4 => Mode::Additive,
        _ => Mode::Blend,
    }
}

pub fn parse(name: &str, d: &Value) -> Option<Material> {
    let shader = d.get("m_shaderName").s().to_lowercase();
    if skip_name(name) || SKIP_SHADERS.contains(&shader.as_str()) {
        return None;
    }
    if shader == "generic.vfx" && name.to_lowercase().contains("tools") {
        return None;
    }
    let overlay = shader == "csgo_static_overlay.vfx";
    let mut mode = if overlay {
        overlay_mode(int(d, "F_BLEND_MODE"))
    } else if shader == "csgo_decalmodulate.vfx" {
        Mode::Mod2x
    } else if int(d, "F_ADDITIVE_BLEND") != 0 {
        Mode::Additive
    } else if int(d, "F_TRANSLUCENT") != 0 {
        Mode::Blend
    } else if int(d, "F_ALPHA_TEST") != 0 {
        Mode::AlphaTest
    } else {
        Mode::Opaque
    };
    let mut alpha = 1.0;
    let mut fill = texture(d, COLOR_PARAMS).map(Fill::Texture);
    let tint_v = vector(d, "g_vColorTint").or_else(|| vector(d, "g_vLayer1Tint")).or_else(|| vector(d, "g_vTextureColorTint1"));
    let mut tint = tint_v.map_or([1.0; 3], |v| [v[0] as f32, v[1] as f32, v[2] as f32]);
    match shader.as_str() {
        "csgo_black_unlit.vfx" => fill = Some(Fill::Solid([0, 0, 0, 255])),
        "csgo_glass.vfx" => {
            mode = Mode::Blend;
            alpha = GLASS_ALPHA;
            if let Some(c) = vector(d, "g_vGlassTintColor") {
                tint = [c[0] as f32, c[1] as f32, c[2] as f32];
            }
        }
        "csgo_water_fancy.vfx" => {
            mode = Mode::Blend;
            alpha = WATER_ALPHA;
            let c = vector(d, "g_vWaterFogColor").unwrap_or(vec![0.2, 0.3, 0.35]);
            let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            fill = Some(Fill::Solid([b(c[0]), b(c[1]), b(c[2]), 255]));
            tint = [1.0; 3];
        }
        _ => {}
    }
    let fill = fill.unwrap_or(Fill::Solid([128, 128, 128, 255]));
    let blend = match shader.as_str() {
        "csgo_environment_blend.vfx" => Some(Blend::Height),
        "csgo_lightmappedgeneric.vfx" if int(d, "F_LAYERS") != 0 => Some(Blend::Painted),
        _ => None,
    };
    let uv1 = transform(
        d,
        &["g_vTexCoordScale", "g_vTexCoordScale1", "g_vLayer1TexCoordScale"],
        &["g_vTexCoordOffset", "g_vTexCoordOffset1", "g_vLayer1TexCoordOffset"],
    );
    let set = |n: &str| (int(d, n) - 1).clamp(0, 1) as u32;
    let layer2 = blend.and_then(|_| texture(d, LAYER2_PARAMS)).map(|tex| Layer {
        tex,
        uv: transform(d, &["g_vTexCoordScale2", "g_vLayer2TexCoordScale"], &["g_vTexCoordOffset2", "g_vLayer2TexCoordOffset"]),
        set: set("g_nUVSet2"),
    });
    let layer3_on = shader != "csgo_environment_blend.vfx" || int(d, "F_ENABLE_LAYER_3") != 0;
    let layer3 = (layer2.is_some() && layer3_on).then(|| texture(d, LAYER3_PARAMS)).flatten().map(|tex| Layer {
        tex,
        uv: transform(d, &["g_vTexCoordScale3", "g_vLayer3TexCoordScale"], &["g_vTexCoordOffset3", "g_vLayer3TexCoordOffset"]),
        set: set("g_nUVSet3"),
    });
    let tint_mask = (int(d, "F_TINT_MASK") != 0).then(|| texture(d, &["g_tTintMask"])).flatten();
    let paint = int(d, "F_VERTEX_COLOR") != 0 || int(d, "F_PAINT_VERTEX_COLORS") != 0;
    let decal = overlay || int(d, "F_OVERLAY") != 0 || int(d, "F_DECAL_TEXTURE") != 0 || int(d, "F_DEPTH_BIAS") != 0;
    Some(Material {
        fill,
        layer2,
        layer3,
        blend: blend.unwrap_or(Blend::Height),
        mode,
        alpha,
        tint,
        tint_mask,
        uv_scale: [uv1[0], uv1[1]],
        uv_offset: [uv1[2], uv1[3]],
        uv_set: set("g_nUVSet1"),
        paint,
        decal,
    })
}

pub fn load(fs: &Fs, name: &str) -> Result<Option<Material>> {
    if skip_name(name) {
        return Ok(None);
    }
    let d = Res::new(fs.read(&format!("{name}_c"))?)?.kv("DATA")?;
    Ok(parse(name, &d))
}
