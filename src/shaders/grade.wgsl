@fragment
fn fs(@builtin(position) fp: vec4<f32>) -> @location(0) vec4<f32> {
    let col = textureLoad(src, vec2<i32>(fp.xy), 0);
    if (col.a <= 0.0) {
        return col;
    }
    var c = col.rgb / col.a;
    let l = dot(c, vec3<f32>(0.299, 0.587, 0.114));
    c = mix(vec3<f32>(l), c, pu.a.x);
    c = mix(c, pu.b.rgb, pu.a.y);
    c = (c - 0.5) * pu.a.z + 0.5;
    return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)) * col.a, col.a);
}
