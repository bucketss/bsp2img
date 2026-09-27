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
    tint: vec4<f32>,
    uv2: vec4<f32>,
    uv3: vec4<f32>,
    f: vec4<f32>,
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
    hdr: vec4<f32>,
    sun: vec4<f32>,
};

struct Lamp {
    pos: vec4<f32>,
    color: vec4<f32>,
    dir: vec4<f32>,
    cone: vec4<f32>,
};

const LAMP_REF: f32 = 32.0;

@group(0) @binding(0) var<uniform> fr: Frame;
@group(0) @binding(1) var lmap: texture_2d<f32>;
@group(0) @binding(2) var lsamp: sampler;
@group(0) @binding(3) var xymask: texture_2d<f32>;
@group(0) @binding(4) var msamp: sampler;
@group(0) @binding(5) var shmask: texture_2d<f32>;
@group(0) @binding(6) var<storage, read> lamps: array<Lamp>;
@group(0) @binding(7) var tsamp: sampler;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(2) var<uniform> bu: BatchU;
@group(1) @binding(3) var tex2: texture_2d<f32>;
@group(1) @binding(4) var tex3: texture_2d<f32>;
@group(1) @binding(5) var tmask: texture_2d<f32>;
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
    @location(5) blend: vec2<f32>,
    @location(6) uv2: vec2<f32>,
    @location(7) paint: vec4<f32>,
    @location(8) vlight: vec4<f32>,
    @location(9) mask: vec4<f32>,
};

fn unpack9e5(v: u32) -> vec3<f32> {
    let d = exp2(f32(v >> 27u) - 24.0);
    return vec3<f32>(f32(v & 511u), f32((v >> 9u) & 511u), f32((v >> 18u) & 511u)) * d;
}

fn unpack_nb(v: u32) -> vec4<f32> {
    let e = vec2<f32>(f32(v & 16383u), f32((v >> 14u) & 16383u)) / 16383.0 * 2.0 - 1.0;
    var n = vec3<f32>(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    let t = max(-n.z, 0.0);
    n.x += select(t, -t, n.x >= 0.0);
    n.y += select(t, -t, n.y >= 0.0);
    return vec4<f32>(normalize(n), f32(v >> 28u));
}

struct POut {
    @builtin(position) pos: vec4<f32>,
    @location(0) world: vec3<f32>,
};

@vertex
fn vs_pos(@location(0) pos: vec3<f32>) -> POut {
    var o: POut;
    o.pos = fr.mvp * vec4<f32>(pos + vec3<f32>(0.0, 0.0, bu.e.x * fr.view_r.w), 1.0);
    o.world = pos;
    return o;
}

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) uv2: vec2<f32>, @location(3) lm: vec2<f32>, @location(4) nb: u32,
      @location(5) blend: vec2<f32>, @location(6) paint: vec4<f32>, @location(7) vlight: u32, @location(8) mask: vec4<f32>) -> VOut {
    var o: VOut;
    let nbv = unpack_nb(nb);
    let normal = nbv.xyz;
    let bias = nbv.w;
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
    o.blend = blend;
    o.uv2 = uv2;
    o.paint = paint;
    o.vlight = vec4<f32>(0.0);
    if (vlight != 0u) {
        o.vlight = vec4<f32>(unpack9e5(vlight), 1.0);
    }
    o.mask = mask;
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
    if (p.z < fr.zr.x || p.z > fr.zr.y || p.z <= fr.view_u.w) {
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

fn tone(e: vec3<f32>) -> vec3<f32> {
    let v = max(e * lu.hdr.x, vec3<f32>(0.0));
    let g3 = lu.hdr.y;
    let f = select(vec3<f32>(0.125) + (v - vec3<f32>(g3)) / (1.0 - g3) * 0.875, v / g3 * 0.125, v <= vec3<f32>(g3));
    return pow(f, vec3<f32>(lu.hdr.z));
}

fn lamp_light(p: vec3<f32>, n: vec3<f32>, sm: vec4<f32>) -> vec3<f32> {
    var e = vec3<f32>(0.0);
    let cnt = u32(lu.sun.w);
    for (var k = 0u; k < cnt; k++) {
        let lp = lamps[k];
        let d = lp.pos.xyz - p;
        let dist = length(d);
        if (dist >= lp.pos.w) {
            continue;
        }
        let l = d / max(dist, 1e-3);
        let ndl = dot(n, l);
        if (ndl <= 0.0) {
            continue;
        }
        let r = dist / lp.pos.w;
        let win = clamp(1.0 - r * r * r * r, 0.0, 1.0);
        var att = win * win / (1.0 + dist * dist / (LAMP_REF * LAMP_REF));
        let kind = u32(lp.color.w + 0.5);
        let cd = dot(-l, lp.dir.xyz);
        if (kind == 1u) {
            att *= max(cd, 0.0);
        } else if (kind == 2u) {
            att *= smoothstep(lp.cone.x, lp.cone.y, cd);
        }
        let vis = sm[min(u32(lp.dir.w + 0.5), 3u)];
        e += lp.color.rgb * (ndl * att * vis);
    }
    return e;
}

fn shade(i: VOut, front: bool) -> vec4<f32> {
    let uv1 = tex_uv(i.uv);
    var t = textureSample(tex, tsamp, uv1);
    let t2 = textureSample(tex2, tsamp, i.uv2 * bu.uv2.xy + bu.uv2.zw);
    var q3 = i.uv;
    if (bu.f.y > 0.5) {
        q3 = i.uv2;
    }
    let t3 = textureSample(tex3, tsamp, q3 * bu.uv3.xy + bu.uv3.zw);
    let tm = textureSample(tmask, tsamp, uv1).r;
    if (bu.e.z > 0.5) {
        t = vec4<f32>(mix(t.rgb, t2.rgb, clamp(i.blend.x, 0.0, 1.0)), t.a);
    }
    if (bu.f.x > 0.5) {
        t = vec4<f32>(mix(t.rgb, t3.rgb, clamp(i.blend.y, 0.0, 1.0)), t.a);
    }
    var tint = bu.tint.rgb;
    if (bu.f.z > 0.5) {
        tint = mix(vec3<f32>(1.0), tint, tm);
    }
    t = vec4<f32>(t.rgb * i.paint.rgb, t.a * i.paint.a);
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
    if (mode == 4) {
        let a4 = bu.a.y * t.a;
        return vec4<f32>(mix(vec3<f32>(0.5), t.rgb * tint, a4), 1.0);
    }
    var lr = l.rgb;
    var n = normalize(i.normal);
    if (!front) {
        n = -n;
    }
    if (lu.hdr.w > 0.5 && !unlit && mode != 3) {
        var e = l.rgb;
        var sm = textureSampleLevel(shmask, lsamp, i.lm, 0.0);
        if (i.vlight.w > 0.5) {
            e = i.vlight.rgb;
            sm = i.mask;
        }
        let ndl = dot(n, lu.dir0.xyz);
        if (ndl > 0.0) {
            e += lu.sun.rgb * (ndl * sun_shadow(smap0, lu.mvp0, lu.shadow0, i.lpos, n, ndl));
        }
        e += lamp_light(i.lpos, n, sm);
        lr = tone(e);
    }
    if (lu.dir.w > 0.0 && !unlit && mode != 3) {
        lr = relight(lr, i.lpos, n);
    }
    let c = t.rgb * tint * lr;
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
fn fs_shadow_pos(i: POut) {
    if (cut_away(i.world)) {
        discard;
    }
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
