struct Frame {
    mvp: mat4x4<f32>,
    clip_xy: vec4<f32>,
    mask_rect: vec4<f32>,
    zr: vec4<f32>,
};

@group(0) @binding(0) var<uniform> fr: Frame;
@group(0) @binding(1) var lmap: texture_2d<f32>;
@group(0) @binding(2) var lsamp: sampler;
@group(0) @binding(3) var xymask: texture_2d<f32>;
@group(0) @binding(4) var msamp: sampler;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tsamp: sampler;
@group(1) @binding(2) var<uniform> bu: vec4<f32>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) lm: vec2<f32>,
    @location(2) world: vec3<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) lm: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = fr.mvp * vec4<f32>(pos, 1.0);
    o.uv = uv;
    o.lm = lm;
    o.world = pos;
    return o;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex, tsamp, i.uv);
    let l = textureSampleLevel(lmap, lsamp, i.lm, 0.0);
    let p = i.world;
    if (p.z < fr.zr.x || p.z > fr.zr.y) {
        discard;
    }
    if (p.x < fr.clip_xy.x || p.y < fr.clip_xy.y || p.x > fr.clip_xy.z || p.y > fr.clip_xy.w) {
        discard;
    }
    if (fr.zr.z > 0.5) {
        let m = (p.xy - fr.mask_rect.xy) / (fr.mask_rect.zw - fr.mask_rect.xy);
        if (m.x < 0.0 || m.y < 0.0 || m.x > 1.0 || m.y > 1.0) {
            discard;
        }
        if (textureSampleLevel(xymask, msamp, m, 0.0).r < 0.5) {
            discard;
        }
    }
    let mode = i32(bu.x + 0.5);
    if (mode == 1 && t.a < 0.5) {
        discard;
    }
    let c = t.rgb * l.rgb;
    var a = 1.0;
    if (mode >= 2) {
        a = bu.y * t.a;
    }
    if (mode == 3) {
        return vec4<f32>(c * a, 0.0);
    }
    return vec4<f32>(c * a, a);
}
