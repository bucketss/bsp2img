use super::hull0::V3;
use super::{World, add};

#[derive(Clone, Copy, Debug)]
pub struct Weapon {
    pub name: &'static str,
    pub distance: f32,
    pub penetration: i32,
    pub power: i32,
    pub pen_dist: f32,
    pub damage: f32,
    pub rangemod: f32,
}

pub const WEAPONS: [Weapon; 6] = [
    Weapon { name: "ak47", distance: 8192.0, penetration: 2, power: 39, pen_dist: 5000.0, damage: 36.0, rangemod: 0.98 },
    Weapon { name: "awp", distance: 8192.0, penetration: 3, power: 45, pen_dist: 8000.0, damage: 115.0, rangemod: 0.99 },
    Weapon { name: "m4a1", distance: 8192.0, penetration: 2, power: 35, pen_dist: 4000.0, damage: 32.0, rangemod: 0.97 },
    Weapon { name: "deagle", distance: 4096.0, penetration: 2, power: 30, pen_dist: 1000.0, damage: 54.0, rangemod: 0.81 },
    Weapon { name: "scout", distance: 8192.0, penetration: 3, power: 39, pen_dist: 5000.0, damage: 75.0, rangemod: 0.98 },
    Weapon { name: "m4a1_sil", distance: 8192.0, penetration: 2, power: 35, pen_dist: 4000.0, damage: 33.0, rangemod: 0.95 },
];

pub fn weapon(name: &str) -> Option<&'static Weapon> {
    WEAPONS.iter().find(|w| w.name.eq_ignore_ascii_case(name))
}

pub const IT_ALLSOLID: u8 = 1;
pub const IT_STARTSOLID: u8 = 2;
pub const IT_ABORTED: u8 = 4;
pub const IT_HIT: u8 = 8;
pub const MAX_ITER: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Iter {
    pub src: V3,
    pub dist: f32,
    pub fraction: f32,
    pub endpos: V3,
    pub abortpos: V3,
    pub face: i32,
    pub power: i16,
    pub damage: i16,
    pub flags: u8,
    pub tex: u8,
    pub model: u16,
    pub normal: V3,
    pub dmg_in: i16,
}

impl Iter {
    pub fn dead(&self) -> bool {
        self.fraction == 1.0 && self.flags & IT_ALLSOLID != 0
    }

    pub fn death(&self) -> V3 {
        if self.flags & IT_ABORTED != 0 { self.abortpos } else { self.src }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Missed,
    Made,
    Died,
    Stopped,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Fired {
    pub n: usize,
    pub it: [Iter; MAX_ITER],
}

#[cfg(test)]
impl Fired {
    pub fn outcome(&self) -> Outcome {
        if self.it[0].flags & IT_HIT == 0 {
            Outcome::Missed
        } else if self.n < 2 {
            Outcome::Stopped
        } else if self.it[1].dead() {
            Outcome::Died
        } else {
            Outcome::Made
        }
    }
}

#[inline]
fn scale(a: &V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn falloff(a: f32, b: f32) -> f64 {
    f64::from(a).powf(f64::from(b))
}

pub fn fire(world: &World, w: &Weapon, src: &V3, dir: &V3, ents: bool, record: bool) -> Fired {
    let mut out = Fired::default();
    let mut src = *src;
    let mut dist = w.distance;
    let mut pen = w.penetration;
    let mut power = w.power;
    let mut damage = w.damage as i32;
    let mut end = add(&src, &scale(dir, dist));
    let mut dmg_mod: f32 = 0.5;
    while pen != 0 {
        let (tr, model) = world.trace(&src, &end, ents);
        let n = out.n;
        out.n += 1;
        let it = if n < MAX_ITER { Some(n) } else { None };
        if let Some(i) = it {
            let r = &mut out.it[i];
            r.src = src;
            r.dist = dist;
            r.dmg_in = damage as i16;
            r.fraction = tr.fraction;
            r.endpos = tr.endpos;
            r.abortpos = tr.abortpos;
            r.normal = tr.normal;
            r.model = model as u16;
            r.flags = if tr.allsolid { IT_ALLSOLID } else { 0 }
                | if tr.startsolid { IT_STARTSOLID } else { 0 }
                | if tr.aborted { IT_ABORTED } else { 0 }
                | if tr.fraction != 1.0 { IT_HIT } else { 0 };
        }
        let (face, tex) = if record || tr.fraction != 1.0 { world.texture_hit(model, &src, &end) } else { (None, 0) };
        match tex {
            b'M' => {
                power = (f64::from(power) * 0.15) as i32;
                dmg_mod = 0.2;
            }
            b'C' => power = (f64::from(power) * 0.25) as i32,
            b'G' => {
                power = (f64::from(power) * 0.5) as i32;
                dmg_mod = 0.4;
            }
            b'V' => {
                power = (f64::from(power) * 0.5) as i32;
                dmg_mod = 0.45;
            }
            b'T' => {
                power = (f64::from(power) * 0.65) as i32;
                dmg_mod = 0.3;
            }
            b'P' => {
                power = (f64::from(power) * 0.4) as i32;
                dmg_mod = 0.45;
            }
            b'W' => dmg_mod = 0.6,
            _ => {}
        }
        if tr.fraction != 1.0 {
            pen -= 1;
            let cur = tr.fraction * dist;
            damage = (f64::from(damage) * falloff(w.rangemod, cur / 500.0)) as i32;
            if cur > w.pen_dist {
                pen = 0;
            }
            let dist_mod: f32 = if pen == 0 || !world.bsp_solid(model) {
                power = 42;
                dmg_mod = 0.75;
                0.75
            } else {
                0.5
            };
            src = add(&tr.endpos, &scale(dir, power as f32));
            dist = ((f64::from(dist) - f64::from(cur)) * f64::from(dist_mod)) as f32;
            end = add(&src, &scale(dir, dist));
            if let Some(i) = it {
                out.it[i].damage = damage as i16;
            }
            damage = (f64::from(damage) * f64::from(dmg_mod)) as i32;
        } else {
            pen = 0;
        }
        if let Some(i) = it {
            let r = &mut out.it[i];
            r.face = face.map_or(-1, |f| f as i32);
            r.tex = tex;
            r.power = power as i16;
        }
    }
    out
}
