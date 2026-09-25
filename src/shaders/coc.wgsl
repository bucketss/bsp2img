@fragment
fn fs(@builtin(position) fp: vec4<f32>) -> @location(0) vec4<f32> {
    var x = abs((fp.y + pu.tile.y) / pu.tile.w - pu.a.y);
    if (pu.a.x > 1.5) {
        let d = raw_depth(vec2<i32>(fp.xy));
        if (is_bg(d)) {
            return vec4<f32>(1.0, 0.0, 0.0, 1.0);
        }
        x = abs(lin(d) - pu.b.x) / max(pu.b.x, 1.0);
    }
    let lo = pu.a.z * 0.5;
    return vec4<f32>(smoothstep(lo, lo + pu.a.w, x), 0.0, 0.0, 1.0);
}
