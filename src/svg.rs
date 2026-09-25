use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use glam::DVec2;

use crate::grid::{CT, T, grid_frame, nice_step};
use crate::nav::{NONE, Nav, STAND_OFS, STEP, zones};
use crate::paths::{Partial, free_name};
use crate::render::{Cuts, Renderer};
use crate::scene::{Report, Scene};

#[derive(Clone, Debug, PartialEq)]
pub struct SvgOpts {
    pub cell: f64,
    pub simplify: f64,
    pub scale: f64,
    pub bands: usize,
    pub planes: Option<Vec<f64>>,
}

impl Default for SvgOpts {
    fn default() -> Self {
        SvgOpts { cell: 8.0, simplify: 6.0, scale: 100.0, bands: 2, planes: None }
    }
}

const TINTS: [&str; 6] = ["#dde8f3", "#f4e8d4", "#e0f0dc", "#efdde8", "#ebe9d3", "#dbeef0"];
const MIN_STACK_AREA: f64 = 16384.0;

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Wall,
    Covered,
    Open,
}

type Pt = (i32, i32);

fn contours(nx: usize, ny: usize, inside: &dyn Fn(i64, i64) -> bool) -> Vec<Vec<Pt>> {
    let mut next: HashMap<Pt, Pt> = HashMap::new();
    for i in -1..nx as i64 {
        for j in -1..ny as i64 {
            let a = inside(i, j) as u8;
            let b = inside(i + 1, j) as u8;
            let c = inside(i + 1, j + 1) as u8;
            let d = inside(i, j + 1) as u8;
            let (i2, j2) = (2 * i as i32, 2 * j as i32);
            let bo = (i2 + 1, j2);
            let r = (i2 + 2, j2 + 1);
            let t = (i2 + 1, j2 + 2);
            let l = (i2, j2 + 1);
            let segs: &[(Pt, Pt)] = match a | b << 1 | c << 2 | d << 3 {
                1 => &[(bo, l)],
                2 => &[(r, bo)],
                3 => &[(r, l)],
                4 => &[(t, r)],
                5 => &[(bo, l), (t, r)],
                6 => &[(t, bo)],
                7 => &[(t, l)],
                8 => &[(l, t)],
                9 => &[(bo, t)],
                10 => &[(r, bo), (l, t)],
                11 => &[(r, t)],
                12 => &[(l, r)],
                13 => &[(bo, r)],
                14 => &[(l, bo)],
                _ => &[],
            };
            for &(p, q) in segs {
                next.insert(p, q);
            }
        }
    }
    let mut loops = Vec::new();
    while let Some(&s) = next.keys().next() {
        let mut lp = vec![s];
        let mut cur = next.remove(&s).unwrap();
        while cur != s {
            lp.push(cur);
            match next.remove(&cur) {
                Some(n) => cur = n,
                None => break,
            }
        }
        loops.push(lp);
    }
    loops
}

fn chain(segs: &[(Pt, Pt)]) -> Vec<Vec<Pt>> {
    let mut adj: HashMap<Pt, Vec<Pt>> = HashMap::new();
    for &(a, b) in segs {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    let mut starts: Vec<Pt> = adj.iter().filter(|(_, v)| v.len() != 2).map(|(k, _)| *k).collect();
    starts.sort();
    let mut rest: Vec<Pt> = adj.keys().copied().collect();
    rest.sort();
    starts.extend(rest);
    let mut out = Vec::new();
    for s in starts {
        while let Some(n) = adj.get_mut(&s).and_then(|v| v.pop()) {
            let mut line = vec![s];
            let (mut prev, mut cur) = (s, n);
            loop {
                if let Some(v) = adj.get_mut(&cur) {
                    if let Some(k) = v.iter().position(|&q| q == prev) {
                        v.swap_remove(k);
                    }
                }
                line.push(cur);
                let next = adj.get_mut(&cur).filter(|v| v.len() == 1).and_then(|v| v.pop());
                match next {
                    Some(q) if cur != s => {
                        prev = cur;
                        cur = q;
                    }
                    _ => break,
                }
            }
            out.push(line);
        }
    }
    out
}

fn sides(p: Pt, inside: &dyn Fn(i64, i64) -> bool) -> ((i64, i64), (i64, i64)) {
    let (a, b) = (p.0 as i64, p.1 as i64);
    let (u, v) = if a % 2 != 0 {
        (((a - 1) / 2, b / 2), ((a + 1) / 2, b / 2))
    } else {
        ((a / 2, (b - 1) / 2), (a / 2, (b + 1) / 2))
    };
    if inside(u.0, u.1) { (u, v) } else { (v, u) }
}

fn area(p: &[DVec2]) -> f64 {
    let n = p.len();
    (0..n).map(|k| p[k].perp_dot(p[(k + 1) % n])).sum::<f64>() / 2.0
}

fn seg_dist(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let l = ab.length_squared();
    if l <= 1e-12 {
        return p.distance(a);
    }
    p.distance(a + ab * ((p - a).dot(ab) / l).clamp(0.0, 1.0))
}

fn dp(p: &[DVec2], tol: f64, out: &mut Vec<DVec2>) {
    let (a, b) = (p[0], p[p.len() - 1]);
    let far = (1..p.len() - 1).map(|k| (k, seg_dist(p[k], a, b))).max_by(|x, y| x.1.total_cmp(&y.1));
    match far {
        Some((k, d)) if d > tol => {
            dp(&p[..=k], tol, out);
            dp(&p[k..], tol, out);
        }
        _ => {
            if out.last() != Some(&a) {
                out.push(a);
            }
            out.push(b);
        }
    }
}

fn simplify_open(p: &[DVec2], tol: f64) -> Vec<DVec2> {
    if p.len() < 3 {
        return p.to_vec();
    }
    let mut out = Vec::new();
    dp(p, tol, &mut out);
    out
}

fn simplify_closed(p: &[DVec2], tol: f64) -> Vec<DVec2> {
    let far = (0..p.len()).max_by(|&a, &b| p[a].distance_squared(p[0]).total_cmp(&p[b].distance_squared(p[0]))).unwrap_or(0);
    let mut a: Vec<DVec2> = p[..=far].to_vec();
    let mut b: Vec<DVec2> = p[far..].to_vec();
    b.push(p[0]);
    a = simplify_open(&a, tol);
    b = simplify_open(&b, tol);
    a.pop();
    b.pop();
    a.extend(b);
    a
}

fn num(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

fn pts(p: &[DVec2]) -> String {
    let mut s = String::new();
    for (k, q) in p.iter().enumerate() {
        let _ = write!(s, "{}{} {}", if k == 0 { "M" } else { " L" }, num(q.x), num(-q.y));
    }
    s
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn band_of(planes: &[f64], z: f64) -> usize {
    planes.partition_point(|&p| p <= z)
}

fn auto_planes(nav: &Nav, keep: &[bool], levels: &[(f64, f64, f64)], max: usize) -> Vec<f64> {
    let mut cands: Vec<f64> = levels.iter().map(|l| l.0 - 1.0).collect();
    cands.sort_by(f64::total_cmp);
    cands.dedup();
    let cols: Vec<Vec<f64>> = (0..nav.nx * nav.ny)
        .map(|c| nav.column(c).filter(|&n| keep[n]).map(|n| nav.nodes[n].z as f64).collect())
        .collect();
    let links: Vec<(f64, f64)> = (0..nav.len())
        .filter(|&u| keep[u])
        .flat_map(|u| nav.links(u).into_iter().filter(move |&v| v != NONE && (v as usize) > u).map(move |v| (u, v as usize)))
        .filter(|&(_, v)| keep[v])
        .map(|(u, v)| {
            let (a, b) = (nav.nodes[u].z as f64, nav.nodes[v].z as f64);
            (a.min(b), a.max(b))
        })
        .collect();
    let min_cols = (MIN_STACK_AREA / (nav.cell * nav.cell)).ceil() as i64;
    let mut planes: Vec<f64> = Vec::new();
    while planes.len() < max {
        let mut best: Option<(i64, f64)> = None;
        for &p in cands.iter().filter(|p| !planes.contains(p)) {
            let same = |a: f64, b: f64| band_of(&planes, a) == band_of(&planes, b);
            let stacked = cols
                .iter()
                .filter(|zs| zs.windows(2).any(|w| w[0] < p && p <= w[1] && same(w[0], w[1])))
                .count() as i64;
            if stacked < min_cols {
                continue;
            }
            let cut = links.iter().filter(|&&(a, b)| a < p && p <= b && same(a, b)).count() as i64;
            let score = stacked - cut;
            if score > 0 && best.is_none_or(|(s, _)| score > s) {
                best = Some((score, p));
            }
        }
        let Some((_, p)) = best else { break };
        planes.push(p);
        planes.sort_by(f64::total_cmp);
    }
    planes
}

pub fn export_svg(
    r: &mut Renderer,
    scene: &Scene,
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &SvgOpts,
    out: &Path,
    rep: &mut Report,
) -> Result<Vec<PathBuf>> {
    let bsp = &scene.bsp;
    rep.step(0.0)?;
    let nav = Nav::build(bsp, o.cell, 250.0)?;
    rep.step(0.4)?;
    let spawn = |cls: &str| -> Vec<(glam::DVec3, Option<usize>)> {
        bsp.entities.iter().filter(|e| e.class() == cls).filter_map(|e| e.origin()).map(|p| (p, nav.nearest(p, STAND_OFS))).collect()
    };
    let (st, sct) = (spawn("info_player_deathmatch"), spawn("info_player_start"));
    let seeds: Vec<usize> = st.iter().chain(&sct).filter_map(|s| s.1).collect();
    if seeds.is_empty() {
        bail!("none of the {} spawn points is on walkable ground", st.len() + sct.len());
    }
    let dist = nav.flood(&seeds, 250.0);
    let [cx0, cy0, cx1, cy1] = cuts.clip;
    let keep: Vec<bool> = (0..nav.len())
        .map(|n| {
            let z = nav.nodes[n].z as f64;
            let (x, y) = nav.cell_xy(nav.col[n] as usize);
            dist[n].is_finite() && z >= cuts.zmin && z <= cuts.zmax && x >= cx0 && x <= cx1 && y >= cy0 && y <= cy1
        })
        .collect();
    rep.step(0.5)?;

    let mut planes = match &o.planes {
        Some(p) => p.clone(),
        None => auto_planes(&nav, &keep, &scene.levels, o.bands.max(1) - 1),
    };
    planes.sort_by(f64::total_cmp);
    planes.dedup();
    let nb = planes.len() + 1;
    let (nx, ny) = (nav.nx, nav.ny);
    let mut top = vec![vec![NONE; nx * ny]; nb];
    for n in (0..nav.len()).filter(|&n| keep[n]) {
        top[band_of(&planes, nav.nodes[n].z as f64)][nav.col[n] as usize] = n as u32;
    }
    let present: Vec<Vec<bool>> = top.iter().map(|t| t.iter().map(|&n| n != NONE).collect()).collect();
    let mut cover = vec![vec![false; nx * ny]; nb];
    for b in (0..nb.saturating_sub(1)).rev() {
        cover[b] = (0..nx * ny).map(|c| cover[b + 1][c] || present[b + 1][c]).collect();
    }
    let counts: Vec<usize> = present.iter().map(|p| p.iter().filter(|&&v| v).count()).collect();
    rep.log(format!(
        "  svg: {} walkable cells, {} band{}{}",
        keep.iter().filter(|&&k| k).count(),
        nb,
        if nb == 1 { "" } else { "s" },
        if planes.is_empty() {
            String::new()
        } else {
            format!(", split at z {}", planes.iter().map(|p| num(*p)).collect::<Vec<_>>().join(", "))
        }
    ));

    let idx = |i: i64, j: i64| -> Option<usize> {
        (i >= 0 && j >= 0 && i < nx as i64 && j < ny as i64).then(|| i as usize * ny + j as usize)
    };
    let world = |p: Pt| DVec2::new(nav.lo.x + (p.0 as f64 / 2.0 + 0.5) * nav.cell, nav.lo.y + (p.1 as f64 / 2.0 + 0.5) * nav.cell);
    let min_area = nav.cell * nav.cell;
    let tol = o.simplify.max(0.0);

    let mut floors = Vec::new();
    let mut walls = String::new();
    let mut ledges = String::new();
    for b in 0..nb {
        rep.step(0.5 + 0.4 * b as f32 / nb as f32)?;
        let pr = &present[b];
        let cv = &cover[b];
        let inside = |i: i64, j: i64| idx(i, j).is_some_and(|c| pr[c]);
        let open_fill = |i: i64, j: i64| idx(i, j).is_some_and(|c| pr[c] && !cv[c]);
        let mut fill = String::new();
        for lp in contours(nx, ny, &open_fill) {
            let w: Vec<DVec2> = lp.iter().map(|&p| world(p)).collect();
            if area(&w).abs() < min_area {
                continue;
            }
            let s = simplify_closed(&w, tol);
            if s.len() >= 3 {
                let _ = write!(fill, "{} Z ", pts(&s));
            }
        }
        let mut dashed = String::new();
        for lp in contours(nx, ny, &inside) {
            let w: Vec<DVec2> = lp.iter().map(|&p| world(p)).collect();
            if area(&w).abs() < min_area {
                continue;
            }
            let st: Vec<Edge> = lp
                .iter()
                .map(|&p| {
                    let (pi, qo) = sides(p, &inside);
                    let pc = idx(pi.0, pi.1).unwrap();
                    if let Some(qc) = idx(qo.0, qo.1) {
                        let linked = nav.column(pc).filter(|&n| keep[n] && band_of(&planes, nav.nodes[n].z as f64) == b).any(|n| {
                            nav.links(n).iter().any(|&v| v != NONE && keep[v as usize] && nav.col[v as usize] as usize == qc)
                        });
                        if linked {
                            return Edge::Open;
                        }
                    }
                    if cv[pc] { Edge::Covered } else { Edge::Wall }
                })
                .collect();
            let n = w.len();
            let start = (0..n).find(|&k| st[k] != st[(k + n - 1) % n]);
            let runs: Vec<(Edge, Vec<DVec2>)> = match start {
                None => {
                    let mut s = simplify_closed(&w, tol);
                    if let Some(&f) = s.first() {
                        s.push(f);
                    }
                    vec![(st[0], s)]
                }
                Some(s0) => {
                    let mut runs = Vec::new();
                    let mut k = 0;
                    while k < n {
                        let e = st[(s0 + k) % n];
                        let mut line = vec![w[(s0 + k) % n]];
                        while k < n && st[(s0 + k) % n] == e {
                            k += 1;
                            line.push(w[(s0 + k) % n]);
                        }
                        runs.push((e, simplify_open(&line, tol)));
                    }
                    runs
                }
            };
            for (e, line) in runs {
                if line.len() < 2 {
                    continue;
                }
                match e {
                    Edge::Wall => {
                        let _ = write!(walls, "{} ", pts(&line));
                    }
                    Edge::Covered => {
                        let _ = write!(dashed, "{} ", pts(&line));
                    }
                    Edge::Open => {}
                }
            }
        }
        let range = match (b.checked_sub(1).map(|k| planes[k]), planes.get(b)) {
            (None, None) => "all".to_string(),
            (None, Some(hi)) => format!("z below {}", num(*hi)),
            (Some(lo), None) => format!("z from {}", num(lo)),
            (Some(lo), Some(hi)) => format!("z {} to {}", num(lo), num(*hi)),
        };
        floors.push((b, range, counts[b], fill, dashed));
        let tp = &top[b];
        let mut segs = Vec::new();
        for i in 0..nx {
            for j in 0..ny {
                let c = i * ny + j;
                if tp[c] == NONE || cv[c] {
                    continue;
                }
                let z = nav.nodes[tp[c] as usize].z;
                for (di, dj) in [(1usize, 0usize), (0, 1)] {
                    let Some(d) = idx((i + di) as i64, (j + dj) as i64) else { continue };
                    if tp[d] == NONE || cv[d] || (nav.nodes[tp[d] as usize].z - z).abs() <= STEP {
                        continue;
                    }
                    let (a, bb) = (2 * i as i32, 2 * j as i32);
                    segs.push(if di == 1 { ((a + 1, bb - 1), (a + 1, bb + 1)) } else { ((a - 1, bb + 1), (a + 1, bb + 1)) });
                }
            }
        }
        for line in chain(&segs) {
            if line.len() < 3 {
                continue;
            }
            let w: Vec<DVec2> = line.iter().map(|&p| world(p)).collect();
            let _ = write!(ledges, "{} ", pts(&simplify_open(&w, tol)));
        }
    }
    rep.step(0.9)?;

    let g = grid_frame(r, cuts, 1600);
    let (w, h) = (g.x1 - g.x0, g.y1 - g.y0);
    let mm = 25.4 / o.scale.max(1e-6);
    let mut s = String::new();
    writeln!(s, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" width="{}mm" height="{}mm" viewBox="{} {} {} {}">"#,
        num(w * mm),
        num(h * mm),
        num(g.x0),
        num(-g.y1),
        num(w),
        num(h)
    )?;
    writeln!(s, "<title>{} callouts, 1:{}</title>", esc(name), num(o.scale))?;
    let layer = |s: &mut String, id: &str, label: &str, extra: &str| {
        let _ = writeln!(s, r#"<g id="{id}" inkscape:groupmode="layer" inkscape:label="{}"{extra}>"#, esc(label));
    };
    for (b, range, cells, fill, dashed) in floors.iter().rev() {
        let id = format!("floors-{b}");
        layer(&mut s, &id, &format!("{id} ({range})"), &format!(r#" data-cells="{cells}""#));
        if !fill.is_empty() {
            writeln!(s, r#"<path d="{}" fill="{}" fill-rule="evenodd" stroke="none"/>"#, fill.trim_end(), TINTS[b % TINTS.len()])?;
        }
        if !dashed.is_empty() {
            writeln!(
                s,
                r##"<path d="{}" fill="none" stroke="#666" stroke-width="2" stroke-dasharray="12 8" stroke-linejoin="round"/>"##,
                dashed.trim_end()
            )?;
        }
        writeln!(s, "</g>")?;
    }
    layer(&mut s, "walls", "walls", "");
    if !walls.is_empty() {
        writeln!(
            s,
            r##"<path d="{}" fill="none" stroke="#222" stroke-width="3" stroke-linejoin="round" stroke-linecap="round"/>"##,
            walls.trim_end()
        )?;
    }
    if !ledges.is_empty() {
        writeln!(
            s,
            r##"<path d="{}" fill="none" stroke="#444" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"/>"##,
            ledges.trim_end()
        )?;
    }
    writeln!(s, "</g>")?;

    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    layer(&mut s, "objectives", "objectives", "");
    for z in zones(bsp) {
        let c = hex(z.color);
        let (x0, y0, x1, y1) = (z.lo.x, z.lo.y, z.hi.x, z.hi.y);
        if z.boxed {
            writeln!(
                s,
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{c}" fill-opacity="0.15" stroke="{c}" stroke-width="4"/>"#,
                num(x0),
                num(-y1),
                num(x1 - x0),
                num(y1 - y0)
            )?;
        } else {
            writeln!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}" fill="{c}" fill-opacity="0.15" stroke="{c}" stroke-width="4"/>"#,
                num((x0 + x1) / 2.0),
                num(-(y0 + y1) / 2.0),
                num((x1 - x0) / 2.0)
            )?;
        }
        writeln!(
            s,
            r##"<text x="{}" y="{}" font-family="sans-serif" font-size="40" font-weight="bold" text-anchor="middle" fill="{c}" stroke="#fff" stroke-width="6" paint-order="stroke">{}</text>"##,
            num((x0 + x1) / 2.0),
            num(-(y0 + y1) / 2.0 + 14.0),
            esc(&z.label)
        )?;
    }
    writeln!(s, "</g>")?;

    layer(&mut s, "spawns", "spawns", "");
    for (list, c) in [(&st, T), (&sct, CT)] {
        for (p, _) in list.iter() {
            writeln!(s, r##"<circle cx="{}" cy="{}" r="10" fill="{}" stroke="#000" stroke-width="2"/>"##, num(p.x), num(-p.y), hex(c))?;
        }
    }
    writeln!(s, "</g>")?;

    layer(&mut s, "grid", "grid", r#" style="display:none""#);
    let step = nice_step(w.max(h));
    let mut gp = String::new();
    let mut gl = String::new();
    let mut v = (g.x0 / step).ceil() * step;
    while v < g.x1 {
        let _ = write!(gp, "M{} {} V{} ", num(v), num(-g.y1), num(-g.y0));
        let _ = writeln!(gl, r#"<text x="{}" y="{}">x {}</text>"#, num(v + 6.0), num(-g.y1 + 30.0), num(v));
        v += step;
    }
    let mut v = (g.y0 / step).ceil() * step;
    while v < g.y1 {
        let _ = write!(gp, "M{} {} H{} ", num(g.x0), num(-v), num(g.x1));
        let _ = writeln!(gl, r#"<text x="{}" y="{}">y {}</text>"#, num(g.x0 + 6.0), num(-v - 6.0), num(v));
        v += step;
    }
    writeln!(s, r##"<path d="{}" fill="none" stroke="#888" stroke-width="1.5"/>"##, gp.trim_end())?;
    writeln!(s, r##"<g font-family="sans-serif" font-size="24" fill="#555">"##)?;
    s.push_str(&gl);
    writeln!(s, "</g>")?;
    writeln!(s, "</g>")?;

    layer(&mut s, "labels", "labels", "");
    writeln!(s, "</g>")?;
    writeln!(s, "</svg>")?;

    let mut files = Partial::default();
    let f = free_name(out, &format!("{name}{cut_tag}_callouts"), ".svg");
    files.add(f.clone());
    std::fs::write(&f, s)?;
    rep.log(format!("  {}", f.display()));
    rep.step(1.0)?;
    Ok(files.keep())
}
