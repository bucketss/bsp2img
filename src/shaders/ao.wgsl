@fragment
fn fs(@builtin(position) fp: vec4<f32>) -> @location(0) vec4<f32> {
    let c = vec2<i32>(fp.xy);
    let d = raw_depth(c);
    if (is_bg(d)) {
        return vec4<f32>(1.0);
    }
    let z = lin(d);
    let n = normal_at(c);
    let upp = upp_at(z);
    let radius = pu.a.x;
    let count = u32(pu.a.z);
    var bias = pu.a.w;
    if (persp()) {
        bias = max(1.0, 2.0 * upp);
    }
    let cell = u32(c.x & 3) + 4u * u32(c.y & 3);
    let ang = f32((cell * 7u) % 16u) * 0.39269908;
    var rv = vec3<f32>(cos(ang), sin(ang), 0.0);
    if (abs(dot(rv, n)) > 0.95) {
        rv = vec3<f32>(0.0, cos(ang), sin(ang));
    }
    let t = normalize(rv - n * dot(rv, n));
    let b = cross(n, t);
    var occ = 0.0;
    for (var i = 0u; i < count; i++) {
        let j = (cell * count + i) % 16u;
        let fj = f32(j);
        let kz = (fj + 0.5) / 16.0;
        let kr = sqrt(1.0 - kz * kz);
        let phi = fj * 2.3999632;
        let q = (fj + 1.0) / 16.0;
        let scale = mix(0.1, 1.0, q * q);
        let k = vec3<f32>(kr * cos(phi), kr * sin(phi), kz) * scale;
        let s = (t * k.x + b * k.y + n * k.z) * radius;
        let off = vec2<f32>(s.x, -s.y) / upp;
        let sd = raw_depth(c + vec2<i32>(round(off)));
        if (is_bg(sd)) {
            continue;
        }
        let sz = lin(sd);
        if (sz < z + s.z - bias) {
            occ += smoothstep(0.0, 1.0, radius / max(abs(z - sz), 1e-3));
        }
    }
    let ao = clamp(1.0 - pu.a.y * occ / f32(count), 0.0, 1.0);
    return vec4<f32>(ao, 0.0, 0.0, 1.0);
}
