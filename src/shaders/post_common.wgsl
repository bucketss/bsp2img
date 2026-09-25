struct Post {
    px: vec4<f32>,
    cam: vec4<f32>,
    a: vec4<f32>,
    b: vec4<f32>,
};

@group(0) @binding(0) var<uniform> pu: Post;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dep: texture_depth_2d;
@group(0) @binding(3) var nrm: texture_2d<f32>;
@group(0) @binding(4) var aux: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u)) * 2.0 - 1.0;
    return vec4<f32>(p, 0.0, 1.0);
}

fn clampc(c: vec2<i32>) -> vec2<i32> {
    return clamp(c, vec2<i32>(0), vec2<i32>(pu.px.xy) - vec2<i32>(1));
}

fn raw_depth(c: vec2<i32>) -> f32 {
    return textureLoad(dep, clampc(c), 0);
}

fn is_bg(d: f32) -> bool {
    return d >= 1.0;
}

fn lin(d: f32) -> f32 {
    return (d - 0.5) * pu.cam.y + pu.cam.x;
}

fn normal_at(c: vec2<i32>) -> vec3<f32> {
    let n = textureLoad(nrm, clampc(c), 0).xyz;
    let l = length(n);
    if (l < 1e-4) {
        return vec3<f32>(0.0, 0.0, -1.0);
    }
    return n / l;
}
