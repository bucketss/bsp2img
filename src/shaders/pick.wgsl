@group(0) @binding(0) var depth: texture_depth_2d;
@group(0) @binding(1) var<storage, read_write> out: array<f32>;
@group(0) @binding(2) var<uniform> at: vec4<u32>;

@compute @workgroup_size(1)
fn main() {
    out[0] = textureLoad(depth, at.xy, 0);
}
