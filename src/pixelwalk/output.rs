#[cfg(test)]
use super::finder::Find;
use super::vec::atan2f;

pub fn yaw_of(a: &[f32; 3]) -> f32 {
    let mut y = atan2f(a[1], a[0]) * 180.0f32 / std::f32::consts::PI;
    if y < 0.0 {
        y += 360.0;
    }
    y
}

#[cfg(test)]
pub fn raw_line(f: &Find) -> String {
    let st = if f.usehull == 0 { "stand" } else { "duck " };
    let p = |v: f32| format!("{:.7}", v as f64);
    if f.length > 0.5 {
        format!(
            "  [{st}] setpos {} {} {}  ->  {} {} {}  (len {:.1}u, yaw {}, samples {}, catch {}, floor*{} wall*{})",
            p(f.pos[0]),
            p(f.pos[1]),
            p(f.pos[2]),
            p(f.to[0]),
            p(f.to[1]),
            p(f.to[2]),
            f.length as f64,
            p(yaw_of(&f.approach)),
            f.cluster_size,
            f.catch_frames,
            f.floor_model,
            f.wall_model
        )
    } else {
        format!(
            "  [{st}] setpos {} {} {}  (yaw {}, samples {}, catch {}, floor*{} wall*{})",
            p(f.pos[0]),
            p(f.pos[1]),
            p(f.pos[2]),
            p(yaw_of(&f.approach)),
            f.cluster_size,
            f.catch_frames,
            f.floor_model,
            f.wall_model
        )
    }
}
