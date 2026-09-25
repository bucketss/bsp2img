@fragment
fn fs(@builtin(position) fp: vec4<f32>) -> @location(0) vec4<f32> {
    let c = vec2<i32>(fp.xy);
    let maxr = pu.a.x;
    let r = textureLoad(aux, c, 0).r * maxr;
    if (r < 0.5) {
        return textureLoad(src, c, 0);
    }
    let dir = vec2<i32>(i32(pu.a.y), i32(pu.a.z));
    let n = i32(ceil(r));
    let k = -2.0 / (r * r);
    var acc = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var i = -n; i <= n; i++) {
        let q = clampc(c + dir * i);
        let fi = f32(i);
        if (i != 0 && abs(fi) > textureLoad(aux, q, 0).r * maxr + 0.5) {
            continue;
        }
        let w = exp(k * fi * fi);
        acc += textureLoad(src, q, 0) * w;
        wsum += w;
    }
    return acc / wsum;
}
