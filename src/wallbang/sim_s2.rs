use glam::Vec3;

use crate::s2::bullet::{Bullet, Crossing, PASSBULLETS};

const CUT: f32 = 153.0;
const SOLID_WANT: f32 = 90.0;
const NUDGE: f32 = 0.09375;
pub const MERGE: f32 = 0.001_953_125;
pub const MAX_PEN: i32 = 4;

#[derive(Clone, Copy, Debug)]
pub struct X {
    pub frac: f32,
    pub exit: bool,
    pub normal: Vec3,
    pub surf: u32,
    pub layers: u32,
    pub hull: bool,
}

impl From<&Crossing> for X {
    fn from(c: &Crossing) -> X {
        X { frac: c.t as f32, exit: c.exit, normal: c.normal.as_vec3(), surf: c.surf, layers: c.layers, hull: c.hull }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub t0: f32,
    pub t1: f32,
    pub a: usize,
    pub b: usize,
    pub solid: bool,
}

pub struct Ray {
    pub start: Vec3,
    pub delta: Vec3,
    pub xs: Vec<X>,
    pub segs: Vec<Seg>,
}

#[derive(Clone, Copy, Debug)]
pub struct SegOut {
    pub damage: f32,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub damage: f32,
    pub walls: i32,
}

pub fn length(v: Vec3) -> f32 {
    (v.z * v.z + v.y * v.y + v.x * v.x).sqrt()
}

fn limit(xs: Vec<X>, l: f32) -> Vec<X> {
    let Some(first) = xs.iter().filter(|c| !c.exit).map(|c| c.frac).reduce(f32::min) else { return xs };
    if !(l > 0.0) {
        return xs;
    }
    let lim = CUT / l + first;
    xs.into_iter().filter(|c| c.hull || c.frac <= lim).collect()
}

fn query(sc: &Bullet, buf: &mut Vec<Crossing>, start: Vec3, delta: Vec3, first: bool) -> Vec<X> {
    sc.query(start.as_dvec3(), delta.as_dvec3(), buf);
    let mut xs: Vec<X> = buf.iter().map(X::from).collect();
    xs.sort_by(|a, b| a.frac.total_cmp(&b.frac));
    let l = length(delta);
    if first {
        xs = limit(xs, l);
    }
    for i in 1..xs.len() {
        if !xs[i].exit && xs[i - 1].exit && (xs[i].frac - xs[i - 1].frac) * l <= MERGE {
            let (fa, fb) = (xs[i - 1].frac, xs[i].frac);
            xs.swap(i - 1, i);
            xs[i - 1].frac = fa;
            xs[i].frac = fb;
        }
    }
    xs
}

pub fn build_segments(xs: &[X], cut: f32) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut t0 = 0.0f32;
    let mut first = 0;
    let mut state = true;
    for (i, c) in xs.iter().enumerate() {
        if c.exit == state {
            continue;
        }
        state = c.exit;
        if c.exit {
            continue;
        }
        let mut prev = t0;
        let mut a = first;
        if i > 0 {
            prev = xs[i - 1].frac;
            segs.push(Seg { t0, t1: prev, a, b: i - 1, solid: true });
            a = i - 1;
        }
        segs.push(Seg { t0: prev, t1: c.frac, a, b: i, solid: false });
        t0 = c.frac;
        first = i;
    }
    if let Some(&last) = segs.last().filter(|s| !s.solid) {
        let (e, te, n) = (last.b, last.t1, xs.len());
        if cut < 1.0 && e < n - 1 && xs[n - 1].exit {
            let tl = xs[n - 1].frac;
            segs.push(Seg { t0: te, t1: tl, a: e, b: n - 1, solid: true });
            segs.push(Seg { t0: tl, t1: 1.0, a: n - 1, b: n - 1, solid: false });
        } else {
            segs.push(Seg { t0: te, t1: 1.0, a: e, b: e, solid: true });
        }
    }
    segs
}

pub fn trace_ray(sc: &Bullet, start: Vec3, end: Vec3) -> Option<Ray> {
    let mut buf = Vec::new();
    let full = end - start;
    let unit = full / length(full);
    let mut delta = full;
    let mut xs = query(sc, &mut buf, start, delta, true);
    let mut cut = 1.0f32;
    if let Some(x0) = xs.first() {
        let l = length(delta);
        if l > 0.0 {
            cut = CUT / l + x0.frac;
        }
    }
    let mut segs = build_segments(&xs, cut);
    if segs.first().is_some_and(|s| s.solid) {
        return None;
    }
    let mut passes = 0;
    let mut ext = 0;
    loop {
        let l = length(delta);
        let mut s = 0.0f32;
        for g in segs.iter().filter(|g| g.solid) {
            s += g.t1.min(cut) - g.t0.min(cut);
        }
        if SOLID_WANT <= s * l {
            break;
        }
        passes += 1;
        if passes > 4 {
            break;
        }
        match xs.last() {
            None => break,
            Some(lx) if lx.exit => {
                ext += 1;
                if ext > MAX_PEN {
                    break;
                }
                let p = delta * lx.frac + start + lx.normal * NUDGE;
                let rest = start + full - p;
                delta = match sc.first_entry(p.as_dvec3(), rest.as_dvec3()) {
                    Some(t) => {
                        let hp = p + rest * (t as f32);
                        unit * (length(hp - start) + SOLID_WANT)
                    }
                    None => full,
                };
            }
            Some(_) => delta = unit * (l + SOLID_WANT),
        }
        xs = query(sc, &mut buf, start, delta, false);
        cut = 1.0;
        segs = build_segments(&xs, cut);
    }
    if cut < 1.0 {
        let inv = 1.0 / cut;
        for c in &mut xs {
            c.frac = (inv * c.frac).min(1.0);
        }
        let mut stop = None;
        for (i, s) in segs.iter_mut().enumerate() {
            let t0 = s.t0;
            s.t0 = inv * t0;
            s.t1 = (inv * s.t1).min(1.0);
            if stop.is_none() && 1.0 < inv * t0 {
                stop = Some(i);
            }
        }
        if let Some(i) = stop {
            segs.truncate(i);
        }
        if let Some(s) = segs.last() {
            xs.truncate(s.b + 1);
        }
        delta *= cut;
    }
    Some(Ray { start, delta, xs, segs })
}

pub fn handle(sc: &Bullet, ray: &Ray, damage: f32, range_mod: f32, pen: f32) -> (Vec<SegOut>, Vec<Hit>) {
    let (start, delta, xs) = (ray.start, ray.delta, &ray.xs);
    let mut d = damage;
    let l = length(delta);
    let mut n = MAX_PEN;
    let mut out = Vec::new();
    let mut hits = Vec::new();
    let pos = |fr: f32| delta * fr + start;
    for s in &ray.segs {
        let d0 = (l * s.t0).max(0.0);
        let d1 = (l * s.t1).max(d0);
        if !s.solid {
            d *= range_mod.powf((d1 - d0) / 500.0);
            out.push(SegOut { damage: d });
            if s.t1 >= 1.0 {
                hits.push(Hit { damage: d, walls: MAX_PEN - n });
            }
            continue;
        }
        hits.push(Hit { damage: d, walls: MAX_PEN - n });
        let (ent, ex) = (&xs[s.a], &xs[s.b]);
        let mut m_ = sc.surf(ent.surf).dist_mod;
        if d1 > 3000.0 || m_ < 0.1 {
            n = 0;
        }
        if n < 1 {
            out.push(SegOut { damage: 0.0 });
            break;
        }
        for x in &xs[s.a..=s.b] {
            let v = sc.surf(x.surf).dist_mod;
            if v <= m_ {
                m_ = v;
            }
        }
        let (pe, px) = (pos(ent.frac), pos(ex.frac));
        let (dx, dy, dz) = (px.x - pe.x, px.y - pe.y, px.z - pe.z);
        let t = (dy * dy + dz * dz + dx * dx).sqrt();
        let mut frac = 0.16f32;
        let modv = m_;
        let (me, mx) = (sc.surf(ent.surf).mat, sc.surf(ex.surf).mat);
        let pb = ent.layers & PASSBULLETS != 0 && ex.layers & PASSBULLETS != 0;
        if 0.1 <= modv && me == mx {
            if me == b'U' || me == b'W' {
                m_ = 3.0;
            } else if me == b'L' {
                m_ = 2.0;
            }
            if t >= 6.0 {
                if pb {
                    m_ = 3.0;
                }
            } else {
                if me == b'G' || me == b'Y' {
                    m_ = 3.0;
                    frac = 0.05;
                }
                if pb {
                    m_ = 32.0;
                    frac = 1e-5;
                }
            }
        }
        let m = (1.0 / m_).max(0.0);
        let k3 = ((3.0 / pen) * 1.25f32).max(0.0);
        let fixed = k3 * (m * 3.0) + frac * d;
        let lost = (t * t * m) / 24.0 + fixed;
        d -= lost.max(0.0);
        if d >= 1.0 {
            n -= 1;
            out.push(SegOut { damage: d });
        } else {
            out.push(SegOut { damage: 0.0 });
            break;
        }
    }
    (out, hits)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use super::*;
    use crate::s2::surfprop::{Surfaces, weapons};
    use crate::s2::{bullet, vpk};

    const MAPS: [&str; 5] = ["de_dust2", "de_mirage", "de_inferno", "de_nuke", "de_ancient"];
    const ACTORS: [&str; 2] = ["chicken", "player"];

    fn game() -> PathBuf {
        PathBuf::from(std::env::var("CS2_GAME").unwrap_or_else(|_| r"F:\servers\cs2-test\game".into()))
    }

    fn v3(v: &serde_json::Value) -> Vec3 {
        let f = |i: usize| v[i].as_f64().unwrap() as f32;
        Vec3::new(f(0), f(1), f(2))
    }

    #[test]
    #[ignore]
    fn weapon_table() {
        let root = game();
        let fs = vpk::map_fs(&root.join("csgo").join("maps").join("de_dust2.vpk"), Some(&root)).unwrap();
        let w = weapons(&fs).unwrap();
        let ak = w.iter().find(|w| w.name == "ak47").unwrap();
        assert_eq!((ak.damage, ak.range_mod, ak.pen, ak.range), (36.0, 0.98, 2.0, 8192.0));
    }

    #[test]
    #[ignore]
    fn golden() {
        let Ok(dir) = std::env::var("CS2_GOLDEN") else { return };
        let dir = Path::new(&dir);
        let only = std::env::var("CS2_MAPS").ok();
        let root = game();
        let sp = Surfaces::load(Some(&root)).unwrap();
        let mut tot: HashMap<&str, usize> = HashMap::new();
        for map in MAPS {
            if only.as_ref().is_some_and(|o| !o.split(',').any(|m| m == map)) {
                continue;
            }
            let t = Instant::now();
            let fs = vpk::map_fs(&root.join("csgo").join("maps").join(format!("{map}.vpk")), Some(&root)).unwrap();
            let sc = bullet::load(&fs, map, &sp).unwrap();
            let tl = t.elapsed().as_secs_f64();
            let f = std::fs::File::open(dir.join(format!("{map}.jsonl.gz"))).unwrap();
            let rd = BufReader::new(flate2::read::GzDecoder::new(f));
            let mut c: HashMap<&str, usize> = HashMap::new();
            let mut rays: HashMap<String, Option<Ray>> = HashMap::new();
            let mut shown = 0;
            let t = Instant::now();
            for line in rd.lines() {
                let r: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
                *c.entry("shots").or_default() += 1;
                let xs = r["crossings"].as_array().unwrap();
                if xs.iter().any(|x| x["Hitbox"].as_bool().unwrap_or(false) || ACTORS.contains(&x["Entity"].as_str().unwrap_or(""))) {
                    *c.entry("skip_actor").or_default() += 1;
                    continue;
                }
                let key = format!("{}{}", r["start"], r["end"]);
                let ray = rays.entry(key).or_insert_with(|| trace_ray(&sc, v3(&r["start"]), v3(&r["end"])));
                let Some(ray) = ray else {
                    *c.entry("skip_start_solid").or_default() += 1;
                    continue;
                };
                let num = |k: &str| r[k].as_f64().unwrap() as f32;
                let (_, hits) = handle(&sc, ray, num("damage"), num("rangeMod"), num("pen"));
                let gh = r["hits"].as_array().unwrap();
                let lg = {
                    let d = &r["rayDelta"];
                    let f = |i: usize| d[i].as_f64().unwrap();
                    (f(0) * f(0) + f(1) * f(1) + f(2) * f(2)).sqrt()
                };
                let lm = length(ray.delta) as f64;
                *c.entry("scored").or_default() += 1;
                *c.entry("len").or_default() += usize::from((lg - lm).abs() <= 0.05);
                let outcome = gh.len() == hits.len();
                let dd: Vec<f64> = gh.iter().zip(&hits).map(|(a, b)| (a["Damage"].as_f64().unwrap() - b.damage as f64).abs()).collect();
                let exact = outcome && gh.iter().zip(&hits).all(|(a, b)| a["Damage"].as_f64().unwrap() as f32 == b.damage);
                let hp1 = outcome && dd.iter().all(|&x| x <= 1.0);
                *c.entry("outcome").or_default() += usize::from(outcome);
                *c.entry("dmg_1hp").or_default() += usize::from(hp1);
                *c.entry("dmg_exact").or_default() += usize::from(exact);
                if !hp1 && shown < 3 {
                    shown += 1;
                    let g: Vec<(f64, i64)> = gh.iter().map(|h| (h["Damage"].as_f64().unwrap(), h["Walls"].as_i64().unwrap())).collect();
                    let m: Vec<(f32, i32)> = hits.iter().map(|h| (h.damage, h.walls)).collect();
                    println!("  MISS {map} {} L game {lg:.2} mine {lm:.2}\n     game {g:?}\n     mine {m:?}", r["id"]);
                }
            }
            let mut keys: Vec<_> = c.iter().collect();
            keys.sort();
            println!("{map}: load {tl:.1} s ({} tris, {} hulls, {} ents), sim {:.1} s {keys:?}", sc.stats.tris, sc.stats.hulls, sc.stats.ents, t.elapsed().as_secs_f64());
            let s = c["scored"];
            let pct = |k: &str| 100.0 * c.get(k).copied().unwrap_or(0) as f64 / s.max(1) as f64;
            println!("  outcome {:.2}%  dmg_1hp {:.2}%  dmg_exact {:.2}%  len {:.2}%", pct("outcome"), pct("dmg_1hp"), pct("dmg_exact"), pct("len"));
            assert!(pct("outcome") >= 99.0, "{map} outcome {:.2}%", pct("outcome"));
            for (k, v) in c {
                *tot.entry(k).or_default() += v;
            }
        }
        let s = tot.get("scored").copied().unwrap_or(0);
        let pct = |k: &str| 100.0 * tot.get(k).copied().unwrap_or(0) as f64 / s.max(1) as f64;
        println!("total {s} scored: outcome {:.2}%  dmg_1hp {:.2}%  dmg_exact {:.2}%  len {:.2}%", pct("outcome"), pct("dmg_1hp"), pct("dmg_exact"), pct("len"));
    }
}
