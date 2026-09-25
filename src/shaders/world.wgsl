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

struct Light {
    mvp: mat4x4<f32>,
    dir: vec4<f32>,
    color: vec4<f32>,
    ambient: vec4<f32>,
    shadow: vec4<f32>,
    mvp0: mat4x4<f32>,
    dir0: vec4<f32>,
    color0: vec4<f32>,
    ambient0: vec4<f32>,
    shadow0: vec4<f32>,
};

@group(0) @binding(0) var<uniform> fr: Frame;
@group(0) @binding(1) var lmap: texture_2d<f32>;
@group(0) @binding(2) var lsamp: sampler;
@group(0) @binding(3) var xymask: texture_2d<f32>;
@group(0) @binding(4) var msamp: sampler;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tsamp: sampler;
@group(1) @binding(2) var<uniform> bu: BatchU;
@group(2) @binding(0) var<uniform> lu: Light;
@group(2) @binding(1) var smap: texture_depth_2d;
@group(2) @binding(2) var csamp: sampler_comparison;
@group(2) @binding(3) var smap0: texture_depth_2d;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) lm: vec2<f32>,
    @location(2) world: vec3<f32>,
    @location(3) normal: vec3<f32>,
    @location(4) lpos: vec3<f32>,
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
    o.lpos = lp;
    return o;
}

fn tex_uv(uv: vec2<f32>) -> vec2<f32> {
    if (bu.a.z > 0.5 && fr.view_dir.w > 0.5) {
        let st = uv * bu.warp.zw - bu.warp.xy;
        return (st + 8.0 * sin(st.yx * 0.125 + vec2<f32>(fr.zr.w))) / 64.0;
    }
    return uv;
}

fn cut_away(p: vec3<f32>) -> bool {
    if (p.z < fr.zr.x || p.z > fr.zr.y) {
        return true;
    }
    if (p.x < fr.clip_xy.x || p.y < fr.clip_xy.y || p.x > fr.clip_xy.z || p.y > fr.clip_xy.w) {
        return true;
    }
    if (fr.zr.z > 0.5) {
        let m = (p.xy - fr.mask_rect.xy) / (fr.mask_rect.zw - fr.mask_rect.xy);
        if (m.x < 0.0 || m.y < 0.0 || m.x > 1.0 || m.y > 1.0) {
            return true;
        }
        if (textureSampleLevel(xymask, msamp, m, 0.0).r < 0.5) {
            return true;
        }
    }
    return false;
}

fn sun_shadow(sm: texture_depth_2d, m: mat4x4<f32>, sp: vec4<f32>, p: vec3<f32>, n: vec3<f32>, ndl: f32) -> f32 {
    let slope = sqrt(max(1.0 - ndl * ndl, 0.0));
    let q = m * vec4<f32>(p + n * (sp.y * (0.5 + slope)), 1.0);
    let uv = vec2<f32>(q.x * 0.5 + 0.5, 0.5 - q.y * 0.5);
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        return 1.0;
    }
    let r = q.z - sp.z * (1.0 + 2.0 * slope / max(ndl, 0.2));
    var s = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            s += textureSampleCompareLevel(sm, csamp, uv + vec2<f32>(f32(x), f32(y)) * sp.x, r);
        }
    }
    return s / 9.0;
}

fn relight(l: vec3<f32>, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let ndl = dot(n, lu.dir.xyz);
    var vis = 0.0;
    if (ndl > 0.0) {
        vis = sun_shadow(smap, lu.mvp, lu.shadow, p, n, ndl);
    }
    var lit = lu.ambient.rgb * (0.5 + 0.5 * n.z) + lu.color.rgb * (max(ndl, 0.0) * vis);
    if (lu.color.w > 0.5) {
        let shade = 1.0 - vis * smoothstep(0.0, 0.2, ndl);
        let w = vec3<f32>(0.299, 0.587, 0.114);
        let ndl0 = dot(n, lu.dir0.xyz);
        var vis0 = 0.0;
        if (ndl0 > 0.0) {
            vis0 = sun_shadow(smap0, lu.mvp0, lu.shadow0, p, n, ndl0);
        }
        let est = dot(lu.ambient0.rgb * (0.5 + 0.5 * n.z) + lu.color0.rgb * (max(ndl0, 0.0) * vis0), w);
        let lum = dot(l, w);
        let keep = smoothstep(est + lu.ambient.w, est + lu.ambient.w + 0.12, lum) * shade;
        lit = mix(lit, max(lit, l), keep);
    }
    return mix(l, lit, lu.dir.w);
}

fn shade(i: VOut, front: bool) -> vec4<f32> {
    let t = textureSample(tex, tsamp, tex_uv(i.uv));
    var l = textureSampleLevel(lmap, lsamp, i.lm, 0.0);
    let p = i.world;
    let unlit = bu.e.y > 0.5;
    if (unlit) {
        l = vec4<f32>(1.0);
    }
    if (!unlit && cut_away(p)) {
        discard;
    }
    let mode = i32(bu.a.x + 0.5);
    if (mode == 1 && t.a < 0.5) {
        discard;
    }
    var lr = l.rgb;
    if (lu.dir.w > 0.0 && !unlit && mode != 3) {
        var n = normalize(i.normal);
        if (!front) {
            n = -n;
        }
        lr = relight(lr, i.lpos, n);
    }
    let c = t.rgb * lr;
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
fn fs(i: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return shade(i, front);
}

struct FOut {
    @location(0) color: vec4<f32>,
    @location(1) normal: vec4<f32>,
};

@fragment
fn fs_n(i: VOut, @builtin(front_facing) front: bool) -> FOut {
    var o: FOut;
    o.color = shade(i, front);
    var n = normalize(i.normal);
    if (!front) {
        n = -n;
    }
    o.normal = vec4<f32>(dot(n, fr.view_r.xyz), dot(n, fr.view_u.xyz), dot(n, fr.view_dir.xyz), 1.0);
    return o;
}

@fragment
fn fs_shadow(i: VOut) {
    let t = textureSample(tex, tsamp, i.uv);
    if (cut_away(i.world)) {
        discard;
    }
    if (i32(bu.a.x + 0.5) == 1 && t.a < 0.5) {
        discard;
    }
}
