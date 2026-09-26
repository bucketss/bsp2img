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
}

#[derive(Clone, Debug)]
pub enum Fill {
    Texture(String),
    Solid([u8; 4]),
}

#[derive(Clone, Debug)]
pub struct Material {
    pub shader: String,
    pub fill: Fill,
    pub layer2: Option<String>,
    pub mode: Mode,
    pub alpha: f32,
    pub tint: [f32; 3],
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
    pub decal: bool,
}

const SKIP_SHADERS: &[&str] = &["csgo_effects.vfx", "csgo_static_overlay.vfx", "sky.vfx", "csgo_sky.vfx", "tools_sprite.vfx"];
const COLOR_PARAMS: &[&str] = &["g_tColor", "g_tColor1", "g_tColorA", "g_tLayer1Color", "g_tGlassTintColor"];
const LAYER2_PARAMS: &[&str] = &["g_tLayer2Color", "g_tColor2", "g_tColorB"];
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

pub fn skip_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("materials/tools/") || n.contains("/tools/tools") || n.contains("nodraw") || n.contains("toolsblocklight")
}

pub fn parse(name: &str, d: &Value) -> Option<Material> {
    let shader = d.get("m_shaderName").s().to_lowercase();
    if skip_name(name) || SKIP_SHADERS.contains(&shader.as_str()) {
        return None;
    }
    if shader == "generic.vfx" && name.to_lowercase().contains("tools") {
        return None;
    }
    let mut mode = if int(d, "F_ADDITIVE_BLEND") != 0 {
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
    let layers = shader == "csgo_environment_blend.vfx"
        || shader == "csgo_simple_2way_blend.vfx"
        || (shader == "csgo_lightmappedgeneric.vfx" && int(d, "F_LAYERS") != 0);
    let layer2 = if layers { texture(d, LAYER2_PARAMS) } else { None };
    let scale = vector(d, "g_vTexCoordScale").or_else(|| vector(d, "g_vTexCoordScale1")).or_else(|| vector(d, "g_vLayer1TexCoordScale"));
    let offset = vector(d, "g_vTexCoordOffset").or_else(|| vector(d, "g_vTexCoordOffset1")).or_else(|| vector(d, "g_vLayer1TexCoordOffset"));
    let decal = int(d, "F_OVERLAY") != 0 || int(d, "F_DECAL_TEXTURE") != 0 || int(d, "F_DEPTH_BIAS") != 0;
    Some(Material {
        shader,
        fill,
        layer2,
        mode,
        alpha,
        tint,
        uv_scale: scale.map_or([1.0; 2], |v| [v[0] as f32, v[1] as f32]),
        uv_offset: offset.map_or([0.0; 2], |v| [v[0] as f32, v[1] as f32]),
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
