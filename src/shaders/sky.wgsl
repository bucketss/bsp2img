struct Sky {
    r: vec4<f32>,
    u: vec4<f32>,
    f: vec4<f32>,
    tanfov: vec4<f32>,
};

@group(0) @binding(0) var<uniform> sk: Sky;
@group(0) @binding(1) var sky: texture_2d_array<f32>;
@group(0) @binding(2) var ssamp: sampler;

struct SOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> SOut {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u)) * 2.0 - 1.0;
    var o: SOut;
    o.pos = vec4<f32>(p, 0.5, 1.0);
    o.ndc = p;
    return o;
}

fn sky_color(i: SOut) -> vec4<f32> {
    let d = normalize(sk.f.xyz + i.ndc.x * sk.tanfov.x * sk.r.xyz + i.ndc.y * sk.tanfov.y * sk.u.xyz);
    let a = abs(d);
    var s = 0.0;
    var t = 0.0;
    var layer = 0;
    if (a.x >= a.y && a.x >= a.z) {
        if (d.x > 0.0) { layer = 0; s = -d.y / a.x; t = d.z / a.x; }
        else { layer = 1; s = d.y / a.x; t = d.z / a.x; }
    } else if (a.y >= a.z) {
        if (d.y > 0.0) { layer = 2; s = d.x / a.y; t = d.z / a.y; }
        else { layer = 3; s = -d.x / a.y; t = d.z / a.y; }
    } else {
        if (d.z > 0.0) { layer = 4; s = -d.y / a.z; t = -d.x / a.z; }
        else { layer = 5; s = -d.y / a.z; t = d.x / a.z; }
    }
    let c = textureSampleLevel(sky, ssamp, vec2<f32>((s + 1.0) * 0.5, (1.0 - t) * 0.5), layer, 0.0);
    return vec4<f32>(c.rgb, 1.0);
}

@fragment
fn fs(i: SOut) -> @location(0) vec4<f32> {
    return sky_color(i);
}

struct SkyN {
    @location(0) color: vec4<f32>,
    @location(1) normal: vec4<f32>,
};

@fragment
fn fs_n(i: SOut) -> SkyN {
    var o: SkyN;
    o.color = sky_color(i);
    o.normal = vec4<f32>(0.0);
    return o;
}
