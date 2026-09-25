@fragment
fn fs(@builtin(position) fp: vec4<f32>) -> @location(0) vec4<f32> {
    let c = vec2<i32>(fp.xy);
    var s = 0.0;
    for (var y = -2; y < 2; y++) {
        for (var x = -2; x < 2; x++) {
            s += textureLoad(aux, clampc(c + vec2<i32>(x, y)), 0).r;
        }
    }
    let col = textureLoad(src, c, 0);
    return vec4<f32>(col.rgb * (s / 16.0), col.a);
}
