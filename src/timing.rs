use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glam::DVec3;
use rayon::prelude::*;
use tiny_skia::{PathBuilder, Pixmap, Stroke, Transform};

use crate::bsp::Bsp;
use crate::grid::{Text, dark_base, line, outline, paint, rect, spawn_dots};
use crate::nav::{JUMP_COST, LADDER_SPEED, NONE, Nav, STAND_OFS, STEP, Zone, zones};
use crate::paths::{Partial, free_name};
use crate::render::{Cuts, Renderer};
use crate::scene::Report;

#[derive(Clone, Debug, PartialEq)]
pub struct TimingOpts {
    pub speed: f64,
    pub cell: f64,
    pub interval: f64,
    pub size: u32,
}

impl Default for TimingOpts {
    fn default() -> Self {
        TimingOpts { speed: 250.0, cell: 8.0, interval: 5.0, size: 1600 }
    }
}
fn zone_time(nav: &Nav, z: &Zone, d: &[f32]) -> f32 {
    if z.boxed {
        return nav.nodes_in(z.lo - DVec3::new(0.0, 0.0, 72.0), z.hi).into_iter().map(|n| d[n]).fold(f32::INFINITY, f32::min);
    }
    let top = z.hi.z as f32 + 16.0;
    nav.cols_in(z.lo.x, z.lo.y, z.hi.x, z.hi.y)
        .filter_map(|c| nav.column(c).rev().find(|&n| nav.nodes[n].z <= top && nav.nodes[n].z >= top - 272.0))
        .map(|n| d[n])
        .fold(f32::INFINITY, f32::min)
}

fn fmt_t(t: f32) -> String {
    if t.is_finite() { format!("{t:.1}") } else { "-".into() }
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t)
}

fn viridis(t: f64) -> [f64; 3] {
    const S: [[f64; 3]; 5] =
        [[253.0, 231.0, 37.0], [94.0, 201.0, 98.0], [33.0, 145.0, 140.0], [59.0, 82.0, 139.0], [68.0, 1.0, 84.0]];
    let t = t.clamp(0.0, 1.0) * 4.0;
    let i = (t.floor() as usize).min(3);
    lerp3(S[i], S[i + 1], t - i as f64)
}

fn team_color(team_t: bool, f: f64) -> [f64; 3] {
    if team_t {
        lerp3([255.0, 214.0, 140.0], [170.0, 30.0, 20.0], f)
    } else {
        lerp3([170.0, 230.0, 255.0], [20.0, 50.0, 170.0], f)
    }
}

struct Field {
    w: usize,
    h: usize,
    t: Vec<f32>,
    ct: Vec<f32>,
}

impl Field {
    fn first(&self, i: usize) -> f32 {
        self.t[i].min(self.ct[i])
    }
}

fn sample_field(nav: &Nav, dt: &[f32], dct: &[f32], zmax: f64, g: &crate::grid::GridFrame) -> Field {
    let sel: Vec<u32> = (0..nav.nx * nav.ny)
        .into_par_iter()
        .map(|c| {
            nav.column(c)
                .rev()
                .find(|&n| (nav.nodes[n].z as f64) <= zmax && (dt[n].is_finite() || dct[n].is_finite()))
                .map_or(NONE, |n| n as u32)
        })
        .collect();
    let (w, h) = (g.wpx as usize, g.hpx as usize);
    let at = |i: i64, j: i64| -> Option<usize> {
        if i < 0 || j < 0 || i >= nav.nx as i64 || j >= nav.ny as i64 {
            return None;
        }
        let n = sel[i as usize * nav.ny + j as usize];
        (n != NONE).then_some(n as usize)
    };
    let px: Vec<(f32, f32)> = (0..w * h)
        .into_par_iter()
        .map(|p| {
            let wx = g.x0 + ((p % w) as f64 + 0.5) * g.upp;
            let wy = g.y1 - ((p / w) as f64 + 0.5) * g.upp;
            let fx = (wx - nav.lo.x) / nav.cell - 0.5;
            let fy = (wy - nav.lo.y) / nav.cell - 0.5;
            let (i0, j0) = (fx.floor() as i64, fy.floor() as i64);
            let (tx, ty) = (fx - i0 as f64, fy - j0 as f64);
            let near = at(fx.round() as i64, fy.round() as i64);
            let Some(nn) = near else { return (f32::INFINITY, f32::INFINITY) };
            let quad = [at(i0, j0), at(i0 + 1, j0), at(i0, j0 + 1), at(i0 + 1, j0 + 1)];
            let z0 = nav.nodes[nn].z;
            let smooth = quad.iter().all(|q| q.is_some_and(|n| (nav.nodes[n].z - z0).abs() <= STEP));
            let get = |d: &[f32]| -> f32 {
                if smooth {
                    let v = quad.map(|q| d[q.unwrap()]);
                    if v.iter().all(|x| x.is_finite()) {
                        let a = v[0] as f64 * (1.0 - tx) + v[1] as f64 * tx;
                        let b = v[2] as f64 * (1.0 - tx) + v[3] as f64 * tx;
                        return (a * (1.0 - ty) + b * ty) as f32;
                    }
                }
                d[nn]
            };
            (get(dt), get(dct))
        })
        .collect();
    let (t, ct) = px.into_iter().unzip();
    Field { w, h, t, ct }
}

fn blend(px: &mut [u8], c: [f64; 3], a: f64) {
    for k in 0..3 {
        px[k] = (px[k] as f64 * (1.0 - a) + c[k] * a).round().clamp(0.0, 255.0) as u8;
    }
}

fn band(t: f32, iv: f64) -> i64 {
    if t.is_finite() { (t as f64 / iv).floor() as i64 } else { i64::MIN }
}

enum Kind {
    Both,
    Team(bool),
}

fn paint_heat(base: &[u8], f: &Field, kind: &Kind, tmax: f64, iv: f64) -> Vec<u8> {
    let mut out = base.to_vec();
    let val = |i: usize| match kind {
        Kind::Both => f.first(i),
        Kind::Team(true) => f.t[i],
        Kind::Team(false) => f.ct[i],
    };
    let side = |i: usize| f.t[i] <= f.ct[i];
    for y in 0..f.h {
        for x in 0..f.w {
            let i = y * f.w + x;
            let v = val(i);
            if !v.is_finite() {
                continue;
            }
            let p = &mut out[i * 4..i * 4 + 4];
            let fr = v as f64 / tmax;
            match kind {
                Kind::Both => blend(p, team_color(side(i), fr), 0.62),
                Kind::Team(_) => blend(p, viridis(fr), 0.7),
            }
            let b = band(v, iv);
            let mut edge = false;
            let mut contact = false;
            for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                if nx >= f.w || ny >= f.h {
                    continue;
                }
                let j = ny * f.w + nx;
                if val(j).is_finite() && band(val(j), iv) != b {
                    edge = true;
                }
                if matches!(kind, Kind::Both) && f.t[i].is_finite() && f.ct[i].is_finite() && f.t[j].is_finite() && f.ct[j].is_finite() && side(i) != side(j) {
                    contact = true;
                }
            }
            if contact {
                blend(p, [255.0, 255.0, 255.0], 1.0);
            } else if edge {
                blend(p, [0.0, 0.0, 0.0], 0.8);
            }
        }
    }
    out
}

fn percentile(mut v: Vec<f32>, q: f64) -> f64 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return 1.0;
    }
    v.sort_by(f32::total_cmp);
    (v[((v.len() - 1) as f64 * q) as usize] as f64).max(1.0)
}

pub fn export_timing(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &TimingOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<Vec<PathBuf>> {
    let t0 = std::time::Instant::now();
    rep.step(0.0)?;
    let nav = Nav::build(bsp, o.cell, o.speed)?;
    rep.step(0.35)?;
    let count = |cls: &str| bsp.entities.iter().filter(|e| e.class() == cls).count();
    let spawn = |cls: &str| -> Vec<usize> {
        bsp.entities
            .iter()
            .filter(|e| e.class() == cls)
            .filter_map(|e| e.origin())
            .filter_map(|p| nav.nearest(p, STAND_OFS))
            .collect()
    };
    let (st, sct) = (spawn("info_player_deathmatch"), spawn("info_player_start"));
    if st.is_empty() && sct.is_empty() {
        let n = bsp.entities.iter().filter(|e| e.class().starts_with("info_player_")).count();
        bail!("none of the {n} spawn points is on walkable ground");
    }
    let dt = nav.flood(&st, o.speed);
    let dct = nav.flood(&sct, o.speed);
    rep.step(0.5)?;
    let reach = (0..nav.len()).filter(|&n| dt[n].is_finite() || dct[n].is_finite()).count();
    rep.log(format!(
        "  nav: {} floor cells, {} reachable, {} ladders, {} blocking entities, spawns placed T {}/{} CT {}/{} ({:.1}s)",
        nav.len(),
        reach,
        nav.ladders,
        nav.blockers,
        st.len(),
        count("info_player_deathmatch"),
        sct.len(),
        count("info_player_start"),
        t0.elapsed().as_secs_f64()
    ));

    let zs = zones(bsp);
    let rows: Vec<(String, f32, f32)> =
        zs.iter().map(|z| (z.label.clone(), zone_time(&nav, z, &dt), zone_time(&nav, z, &dct))).collect();
    let mut txt = String::new();
    writeln!(txt, "{name} rush timings")?;
    writeln!(
        txt,
        "{} units/s on a {}-unit grid; steps up to 18, crouch-jumps up to 63 (+{JUMP_COST} s), crouch-only areas at 1/3 speed, ladders {LADDER_SPEED} units/s",
        o.speed, o.cell
    )?;
    writeln!(txt, "seconds after freeze time for the first player of each team to arrive")?;
    writeln!(txt)?;
    writeln!(txt, "{:<14}{:>8}{:>8}", "", "T", "CT")?;
    for (l, a, b) in &rows {
        writeln!(txt, "{:<14}{:>8}{:>8}", l, fmt_t(*a), fmt_t(*b))?;
    }
    for line in txt.lines().skip(4) {
        rep.log(format!("  {line}"));
    }

    let (g, base) = dark_base(r, cuts, o.size)?;
    let field = sample_field(&nav, &dt, &dct, cuts.zmax, &g);
    let firsts: Vec<f32> = (0..field.t.len()).map(|i| field.first(i)).collect();
    let tmax_both = percentile(firsts, 0.98);
    let tmax_t = percentile(field.t.clone(), 0.98);
    let tmax_ct = percentile(field.ct.clone(), 0.98);
    rep.step(0.7)?;

    let sx = |x: f64| ((x - g.x0) / g.upp) as f32;
    let sy = |y: f64| ((g.y1 - y) / g.upp) as f32;
    let text = Text::new(14.0);
    let small = Text::new(12.0);

    let mut files = Partial::default();
    for (k, (suffix, kind, tmax)) in [
        ("", Kind::Both, tmax_both),
        ("_t", Kind::Team(true), tmax_t),
        ("_ct", Kind::Team(false), tmax_ct),
    ]
    .into_iter()
    .enumerate()
    {
        rep.step(0.7 + 0.1 * k as f32)?;
        let px = paint_heat(&base, &field, &kind, tmax, o.interval);
        let mut pm = Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(g.wpx, g.hpx).context("bad size")?)
            .context("pixmap")?;
        let (fw, fh) = (g.wpx as f32, g.hpx as f32);

        spawn_dots(&mut pm, bsp, &g);
        for (z, (label, a, b)) in zs.iter().zip(&rows) {
            let (x0, y0, x1, y1) = (sx(z.lo.x), sy(z.hi.y), sx(z.hi.x), sy(z.lo.y));
            if z.boxed {
                outline(&mut pm, x0, y0, x1, y1, z.color, 2.0);
            } else if let Some(c) = PathBuilder::from_circle((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0) {
                pm.stroke_path(&c, &paint(z.color, 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
            }
            let s = match kind {
                Kind::Both => format!("{label}  T {}  CT {}", fmt_t(*a), fmt_t(*b)),
                Kind::Team(true) => format!("{label}  {} s", fmt_t(*a)),
                Kind::Team(false) => format!("{label}  {} s", fmt_t(*b)),
            };
            let (tw, th) = small.size(&s);
            let (lx, ly) = (x0.clamp(4.0, (fw - tw - 8.0).max(4.0)), (y0 - th - 6.0).max(4.0));
            rect(&mut pm, lx - 3.0, ly - 1.0, lx + tw + 3.0, ly + th + 1.0, [0, 0, 0], 200);
            small.draw(&mut pm, lx, ly, &s, z.color);
        }

        let title = match kind {
            Kind::Both => format!("{name}  rush timings  {} u/s", o.speed),
            Kind::Team(true) => format!("{name}  T arrival times  {} u/s", o.speed),
            Kind::Team(false) => format!("{name}  CT arrival times  {} u/s", o.speed),
        };
        let (tw, th) = text.size(&title);
        rect(&mut pm, 4.0, 4.0, tw + 14.0, th + 12.0, [0, 0, 0], 200);
        text.draw(&mut pm, 9.0, 8.0, &title, [255, 255, 255]);

        let lw = 190.0;
        let (lx, ly) = (fw - lw - 8.0, fh - 92.0);
        rect(&mut pm, lx - 8.0, ly - 8.0, fw - 4.0, fh - 4.0, [0, 0, 0], 200);
        match kind {
            Kind::Both => {
                let items: [([u8; 3], String); 4] = [
                    ([225, 110, 60], "T arrives first".into()),
                    ([70, 130, 220], "CT arrives first".into()),
                    ([255, 255, 255], "both arrive together".into()),
                    ([30, 30, 30], format!("lines every {} s", o.interval)),
                ];
                for (k, (c, s)) in items.iter().enumerate() {
                    let yy = ly + k as f32 * 20.0;
                    rect(&mut pm, lx, yy + 3.0, lx + 12.0, yy + 15.0, *c, 255);
                    if k == 3 {
                        outline(&mut pm, lx, yy + 3.0, lx + 12.0, yy + 15.0, [200, 200, 200], 1.0);
                    }
                    small.draw(&mut pm, lx + 20.0, yy + 1.0, s, [255, 255, 255]);
                }
            }
            Kind::Team(_) => {
                let bw = lw - 10.0;
                for k in 0..bw as usize {
                    let c = viridis(k as f64 / bw as f64).map(|v| v as u8);
                    line(&mut pm, (lx + k as f32, ly + 24.0), (lx + k as f32, ly + 44.0), c, 255, 1.2);
                }
                small.draw(&mut pm, lx, ly, &format!("seconds, lines every {} s", o.interval), [255, 255, 255]);
                small.draw(&mut pm, lx, ly + 50.0, "0", [255, 255, 255]);
                let e = format!("{tmax:.0}+");
                let (ew, _) = small.size(&e);
                small.draw(&mut pm, lx + bw - ew, ly + 50.0, &e, [255, 255, 255]);
            }
        }

        let rgb: Vec<u8> = pm.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let f = free_name(out, &format!("{name}{cut_tag}_timing{suffix}"), ".png");
        files.add(f.clone());
        image::RgbImage::from_raw(g.wpx, g.hpx, rgb).context("image")?.save(&f)?;
        rep.log(format!("  {}", f.display()));
    }
    rep.step(1.0)?;
    let f = free_name(out, &format!("{name}{cut_tag}_timing"), ".txt");
    files.add(f.clone());
    std::fs::write(&f, txt)?;
    rep.log(format!("  {}", f.display()));
    Ok(files.keep())
}
