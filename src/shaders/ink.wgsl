@fragment
fn fs(@builtin(position) fp: vec4<f32>) -> @location(0) vec4<f32> {
    let c = vec2<i32>(fp.xy);
    let col = textureLoad(src, c, 0);
    let r = pu.a.x;
    let reach = i32(ceil(r + 0.5));
    let d0 = raw_depth(c);
    let bg0 = is_bg(d0);
    let z0 = lin(d0);
    let n0 = normal_at(c);
    let upp = upp_at(z0);
    var nz = min(n0.z, -0.05);
    if (persp()) {
        let xc = (f32(c.x) + 0.5 - pu.px.x * 0.5) * pu.cam.z;
        let yc = (f32(c.y) + 0.5 - pu.px.y * 0.5) * pu.cam.z;
        nz = min(n0.z + n0.x * xc - n0.y * yc, -0.05);
    }
    let gx = -n0.x / nz * upp;
    let gy = n0.y / nz * upp;
    var md = 1e9;
    for (var dy = -reach; dy <= reach; dy++) {
        for (var dx = -reach; dx <= reach; dx++) {
            let dist = length(vec2<f32>(f32(dx), f32(dy)));
            if (dist < 0.5 || dist >= r + 1.0 || dist >= md) {
                continue;
            }
            let q = c + vec2<i32>(dx, dy);
            let dq = raw_depth(q);
            let bgq = is_bg(dq);
            var edge = bg0 != bgq;
            if (!edge && !bg0) {
                if (dot(n0, normal_at(q)) < pu.a.z) {
                    edge = true;
                } else {
                    let pred = z0 + gx * f32(dx) + gy * f32(dy);
                    edge = abs(lin(dq) - pred) > pu.a.y;
                }
            }
            if (edge) {
                md = dist;
            }
        }
    }
    let a = clamp(r - md + 1.0, 0.0, 1.0);
    if (a <= 0.0) {
        return col;
    }
    return vec4<f32>(pu.b.rgb * a + col.rgb * (1.0 - a), a + col.a * (1.0 - a));
}
