struct Frame {
    mvp: mat4x4<f32>,
    clip_xy: vec4<f32>,
    mask_rect: vec4<f32>,
    zr: vec4<f32>,
    view_dir: vec4<f32>,
    view_r: vec4<f32>,
    view_u: vec4<f32>,
    eye: vec4<f32>,
};

struct BatchU {
    a: vec4<f32>,
    warp: vec4<f32>,
    e: vec4<f32>,
};

@group(0) @binding(0) var<uniform> fr: Frame;
@group(0) @binding(1) var lmap: texture_2d<f32>;
@group(0) @binding(2) var lsamp: sampler;
@group(0) @binding(3) var xymask: texture_2d<f32>;
@group(0) @binding(4) var msamp: sampler;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tsamp: sampler;
@group(1) @binding(2) var<uniform> bu: BatchU;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) lm: vec2<f32>,
    @location(2) world: vec3<f32>,
    @location(3) normal: vec3<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) lm: vec2<f32>, @location(3) bias: f32, @location(4) normal: vec3<f32>) -> VOut {
    var o: VOut;
    let lp = pos + vec3<f32>(0.0, 0.0, bu.e.x * fr.view_r.w);
    var off = fr.view_dir.xyz * (bias * 0.25);
    if (fr.eye.w > 0.5) {
        let d = lp - fr.eye.xyz;
        let l = max(length(d), 1e-3);
        off = d / l * (bias * max(0.25, l * 2e-4));
    }
    o.pos = fr.mvp * vec4<f32>(lp - off, 1.0);
    o.uv = uv;
    o.lm = lm;
    o.world = pos;
    o.normal = normal;
    return o;
}

fn tex_uv(uv: vec2<f32>) -> vec2<f32> {
    if (bu.a.z > 0.5 && fr.view_dir.w > 0.5) {
        let st = uv * bu.warp.zw - bu.warp.xy;
        return (st + 8.0 * sin(st.yx * 0.125 + vec2<f32>(fr.zr.w))) / 64.0;
    }
    return uv;
}

fn shade(i: VOut) -> vec4<f32> {
    let t = textureSample(tex, tsamp, tex_uv(i.uv));
    var l = textureSampleLevel(lmap, lsamp, i.lm, 0.0);
    let p = i.world;
    let unlit = bu.e.y > 0.5;
    if (unlit) {
        l = vec4<f32>(1.0);
    }
    if (!unlit && (p.z < fr.zr.x || p.z > fr.zr.y)) {
        discard;
    }
    if (!unlit && (p.x < fr.clip_xy.x || p.y < fr.clip_xy.y || p.x > fr.clip_xy.z || p.y > fr.clip_xy.w)) {
        discard;
    }
    if (!unlit && fr.zr.z > 0.5) {
        let m = (p.xy - fr.mask_rect.xy) / (fr.mask_rect.zw - fr.mask_rect.xy);
        if (m.x < 0.0 || m.y < 0.0 || m.x > 1.0 || m.y > 1.0) {
            discard;
        }
        if (textureSampleLevel(xymask, msamp, m, 0.0).r < 0.5) {
            discard;
        }
    }
    let mode = i32(bu.a.x + 0.5);
    if (mode == 1 && t.a < 0.5) {
        discard;
    }
    let c = t.rgb * l.rgb;
    var a = 1.0;
    if (mode >= 2) {
        a = bu.a.y * t.a;
    }
    if (mode == 3) {
        return vec4<f32>(c * a, 0.0);
    }
    return vec4<f32>(c * a, a);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    return shade(i);
}

struct FOut {
    @location(0) color: vec4<f32>,
    @location(1) normal: vec4<f32>,
};

@fragment
fn fs_n(i: VOut, @builtin(front_facing) front: bool) -> FOut {
    var o: FOut;
    o.color = shade(i);
    var n = normalize(i.normal);
    if (!front) {
        n = -n;
    }
    o.normal = vec4<f32>(dot(n, fr.view_r.xyz), dot(n, fr.view_u.xyz), dot(n, fr.view_dir.xyz), 1.0);
    return o;
}
