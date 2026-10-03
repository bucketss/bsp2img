pub type V3 = [f32; 3];

#[inline]
pub fn dot(a: &V3, b: &V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub fn cross(a: &V3, b: &V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[inline]
pub fn length(v: &V3) -> f32 {
    dot(v, v).sqrt()
}

#[inline]
pub fn normalize(v: &mut V3) -> f32 {
    let length = dot(v, v).sqrt();
    if length != 0.0 {
        let il = 1.0 / length;
        v[0] *= il;
        v[1] *= il;
        v[2] *= il;
    }
    length
}

#[inline]
pub fn cmin(a: f32, b: f32) -> f32 {
    if b < a { b } else { a }
}

#[inline]
pub fn cmax(a: f32, b: f32) -> f32 {
    if a < b { b } else { a }
}

#[inline]
pub fn lround(x: f32) -> i64 {
    x.round() as i64
}

pub fn arr_lt(a: &V3, b: &V3) -> bool {
    for k in 0..3 {
        if a[k] < b[k] {
            return true;
        }
        if b[k] < a[k] {
            return false;
        }
    }
    false
}

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
mod x87 {
    use std::arch::asm;

    pub fn sin(a: f32) -> f32 {
        let mut o = 0f32;
        unsafe {
            asm!("fld dword ptr [{a}]", "fsin", "fstp dword ptr [{o}]",
                a = in(reg) &a, o = in(reg) &mut o,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack));
        }
        o
    }

    pub fn cos(a: f32) -> f32 {
        let mut o = 0f32;
        unsafe {
            asm!("fld dword ptr [{a}]", "fcos", "fstp dword ptr [{o}]",
                a = in(reg) &a, o = in(reg) &mut o,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack));
        }
        o
    }

    pub fn atan2f(y: f32, x: f32) -> f32 {
        let mut o = 0f32;
        unsafe {
            asm!("fld dword ptr [{y}]", "fld dword ptr [{x}]", "fpatan", "fstp dword ptr [{o}]",
                y = in(reg) &y, x = in(reg) &x, o = in(reg) &mut o,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack));
        }
        o
    }

    pub fn atan2(y: f64, x: f64) -> f64 {
        let mut o = 0f64;
        unsafe {
            asm!("fld qword ptr [{y}]", "fld qword ptr [{x}]", "fpatan", "fstp qword ptr [{o}]",
                y = in(reg) &y, x = in(reg) &x, o = in(reg) &mut o,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack));
        }
        o
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
mod x87 {
    pub fn sin(a: f32) -> f32 {
        a.sin()
    }
    pub fn cos(a: f32) -> f32 {
        a.cos()
    }
    pub fn atan2f(y: f32, x: f32) -> f32 {
        y.atan2(x)
    }
    pub fn atan2(y: f64, x: f64) -> f64 {
        y.atan2(x)
    }
}

pub use x87::{atan2, atan2f, cos, sin};

const D2R: f64 = std::f64::consts::PI * 2.0 / 360.0;

pub fn angle_vectors(angles: &V3) -> (V3, V3, V3) {
    let a = (angles[1] as f64 * D2R) as f32;
    let (sy, cy) = (sin(a), cos(a));
    let a = (angles[0] as f64 * D2R) as f32;
    let (sp, cp) = (sin(a), cos(a));
    let a = (angles[2] as f64 * D2R) as f32;
    let (sr, cr) = (sin(a), cos(a));
    let forward = [cp * cy, cp * sy, -sp];
    let right = [-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp];
    let up = [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp];
    (forward, right, up)
}

pub fn angle_vectors_transpose(angles: &V3) -> (V3, V3, V3) {
    const DEG2RAD: f32 = std::f32::consts::PI * 2.0f32 / 360.0f32;
    let a = angles[1] * DEG2RAD;
    let (sy, cy) = (sin(a), cos(a));
    let a = angles[0] * DEG2RAD;
    let (sp, cp) = (sin(a), cos(a));
    let a = angles[2] * DEG2RAD;
    let (sr, cr) = (sin(a), cos(a));
    let forward = [cp * cy, sr * sp * cy - cr * sy, cr * sp * cy + sr * sy];
    let right = [cp * sy, sr * sp * sy + cr * cy, cr * sp * sy - sr * cy];
    let up = [-sp, sr * cp, cr * cp];
    (forward, right, up)
}
