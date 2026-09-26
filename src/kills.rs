use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result};
use glam::DVec3;
use rayon::prelude::*;
use tiny_skia::{PathBuilder, Pixmap, Stroke, Transform};

use crate::bsp::Bsp;
use crate::demo::{self, Death, Event, Team};
use crate::grid::{CT, GridFrame, T, Text, dark_base, line, outline, paint, rect, spawn_dots};
use crate::health::csv_quote;
use crate::nav::{NONE, Nav, STAND_OFS, STEP, zones};
use crate::paths::{Partial, free_name};
use crate::render::{Cuts, Renderer};
use crate::scene::{Cancelled, Report};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TeamSel {
    Both,
    T,
    Ct,
}

impl TeamSel {
    pub const ALL: [TeamSel; 3] = [TeamSel::Both, TeamSel::T, TeamSel::Ct];

    pub fn key(self) -> &'static str {
        match self {
            TeamSel::Both => "both",
            TeamSel::T => "t",
            TeamSel::Ct => "ct",
        }
    }

    pub fn parse(s: &str) -> Option<TeamSel> {
        TeamSel::ALL.into_iter().find(|t| t.key().eq_ignore_ascii_case(s))
    }

    fn keeps(self, t: Team) -> bool {
        match self {
            TeamSel::Both => true,
            TeamSel::T => t == Team::T,
            TeamSel::Ct => t == Team::Ct,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KillOpts {
    pub weapons: Vec<String>,
    pub team: TeamSel,
    pub headshots: bool,
    pub lines: bool,
    pub rounds: Option<(u32, u32)>,
    pub radius: f64,
    pub presence: bool,
    pub presence_every: f64,
    pub size: u32,
}

impl Default for KillOpts {
    fn default() -> Self {
        KillOpts {
            weapons: Vec::new(),
            team: TeamSel::Both,
            headshots: false,
            lines: false,
            rounds: None,
            radius: 96.0,
            presence: true,
            presence_every: 0.5,
            size: 1600,
        }
    }
}

impl KillOpts {
    fn every(&self) -> f32 {
        if self.presence { self.presence_every.max(0.05) as f32 } else { 0.0 }
    }

    fn filters(&self) -> String {
        let mut f = Vec::new();
        if !self.weapons.is_empty() {
            f.push(format!("weapon {}", self.weapons.join(",")));
        }
        match self.team {
            TeamSel::T => f.push("T victims".into()),
            TeamSel::Ct => f.push("CT victims".into()),
            TeamSel::Both => {}
        }
        if self.headshots {
            f.push("headshots".into());
        }
        if let Some((a, b)) = self.rounds {
            f.push(format!("rounds {a}-{b}"));
        }
        f.join(", ")
    }

    fn keeps(&self, d: &Death) -> bool {
        self.rounds.is_none_or(|(a, b)| d.round >= a && d.round <= b)
            && (self.weapons.is_empty() || self.weapons.iter().any(|w| w.eq_ignore_ascii_case(&d.weapon)))
            && (!self.headshots || d.headshot)
            && self.team.keeps(d.victim_team)
    }
}

pub fn parse_rounds(s: &str) -> Result<(u32, u32), String> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let a: u32 = a.trim().parse().map_err(|_| format!("bad round range: {s}"))?;
    let b: u32 = if b.trim().is_empty() { u32::MAX } else { b.trim().parse().map_err(|_| format!("bad round range: {s}"))? };
    Ok((a.min(b), a.max(b)))
}

#[derive(Clone, Copy)]
pub struct Sample {
    pub pos: [i16; 3],
    pub round: u16,
    pub team: Team,
}

pub struct DemoData {
    pub file: String,
    pub map: String,
    pub deaths: Vec<Death>,
    pub presence: Vec<Sample>,
    pub rounds: u32,
    pub wins: [u32; 3],
    pub seconds: f32,
    pub notes: Vec<String>,
}

fn parse_demo(p: &Path, every: f32, stop: &AtomicBool) -> Result<DemoData> {
    let mut deaths = Vec::new();
    let mut presence = Vec::new();
    let mut wins = [0u32; 3];
    let s = demo::scan(p, every, &|| stop.load(Ordering::Relaxed), &mut |e| match e {
        Event::Death(d) => deaths.push(d),
        Event::Presence(x) => presence.push(Sample {
            pos: x.pos.map(|v| v.round().clamp(-32768.0, 32767.0) as i16),
            round: x.round.min(u16::MAX as u32) as u16,
            team: x.team,
        }),
        Event::RoundEnd { winner } => {
            wins[match winner {
                Some(Team::T) => 0,
                Some(Team::Ct) => 1,
                _ => 2,
            }] += 1
        }
    })?;
    let mut notes: Vec<String> = s.desyncs.iter().map(|d| format!("skipped the rest of a frame: {d}")).collect();
    if s.truncated {
        notes.push("file ends early".into());
    }
    Ok(DemoData {
        file: p.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        map: s.map,
        deaths,
        presence,
        rounds: s.rounds,
        wins,
        seconds: s.seconds,
        notes,
    })
}

pub fn load_demos(paths: &[PathBuf], o: &KillOpts, rep: &mut Report, span: (f32, f32)) -> Result<Vec<DemoData>> {
    let done = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let every = o.every();
    let total = paths.len().max(1);
    let res = std::thread::scope(|s| {
        let h = s.spawn(|| {
            paths
                .par_iter()
                .map(|p| {
                    let r = parse_demo(p, every, &stop);
                    done.fetch_add(1, Ordering::Relaxed);
                    r.with_context(|| p.display().to_string())
                })
                .collect::<Vec<_>>()
        });
        while !h.is_finished() {
            let f = done.load(Ordering::Relaxed) as f32 / total as f32;
            if rep.step(span.0 + (span.1 - span.0) * f).is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        h.join().unwrap_or_default()
    });
    if stop.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    let mut out = Vec::new();
    for r in res {
        match r {
            Ok(d) => {
                for n in &d.notes {
                    rep.log(format!("  {}: {n}", d.file));
                }
                out.push(d);
            }
            Err(e) => rep.log(format!("  skipped {e:#}")),
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Default)]
pub struct KillStats {
    pub demos: usize,
    pub rounds: u32,
    pub deaths: usize,
    pub kills: usize,
    pub headshots: usize,
    pub t_deaths: usize,
    pub ct_deaths: usize,
    pub drawn: usize,
    pub top_weapon: String,
    pub presence: usize,
}

impl KillStats {
    pub fn csv_header() -> &'static str {
        "map,status,demos,rounds,deaths,kills,headshot_pct,t_deaths,ct_deaths,drawn,top_weapon,presence_samples,folder"
    }

    pub fn csv_row(&self, map: &str, folder: &Path) -> String {
        format!(
            "{},ok,{},{},{},{},{:.1},{},{},{},{},{},{}",
            csv_quote(map),
            self.demos,
            self.rounds,
            self.deaths,
            self.kills,
            pct(self.headshots, self.kills),
            self.t_deaths,
            self.ct_deaths,
            self.drawn,
            csv_quote(&self.top_weapon),
            self.presence,
            csv_quote(&folder.display().to_string())
        )
    }
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 }
}

fn is_kill(d: &Death) -> bool {
    d.killer_ent != 0 && d.killer_ent != d.victim_ent
}

struct Floor {
    nav: Nav,
    top: Vec<u32>,
}

impl Floor {
    fn build(bsp: &Bsp, zmax: f64) -> Result<Floor> {
        let nav = Nav::build(bsp, 16.0, 250.0)?;
        let seeds: Vec<usize> = bsp
            .entities
            .iter()
            .filter(|e| matches!(e.class(), "info_player_deathmatch" | "info_player_start"))
            .filter_map(|e| e.origin())
            .filter_map(|p| nav.nearest(p, STAND_OFS))
            .collect();
        let reach = nav.flood(&seeds, 250.0);
        let top = (0..nav.nx * nav.ny)
            .into_par_iter()
            .map(|c| {
                nav.column(c)
                    .rev()
                    .find(|&n| (nav.nodes[n].z as f64) <= zmax && reach[n].is_finite())
                    .map_or(NONE, |n| n as u32)
            })
            .collect();
        Ok(Floor { nav, top })
    }

    fn col_at(&self, x: f64, y: f64) -> Option<usize> {
        let i = ((x - self.nav.lo.x) / self.nav.cell).floor();
        let j = ((y - self.nav.lo.y) / self.nav.cell).floor();
        if i < 0.0 || j < 0.0 || i >= self.nav.nx as f64 || j >= self.nav.ny as f64 {
            return None;
        }
        Some(i as usize * self.nav.ny + j as usize)
    }

    fn visible(&self, p: [f32; 3], zmax: f64) -> bool {
        let q = DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
        match self.nav.nearest(q, STAND_OFS) {
            Some(n) => {
                let t = self.top[self.nav.col[n] as usize];
                t != NONE && (self.nav.nodes[t as usize].z - self.nav.nodes[n].z).abs() <= STEP
            }
            None => {
                let Some(c) = self.col_at(q.x, q.y) else { return false };
                let t = self.top[c];
                t != NONE && q.z - STAND_OFS >= self.nav.nodes[t as usize].z as f64 - STEP as f64 && q.z <= zmax + 72.0
            }
        }
    }

    fn floor_px(&self, g: &GridFrame) -> Vec<bool> {
        let (w, h) = (g.wpx as usize, g.hpx as usize);
        (0..w * h)
            .into_par_iter()
            .map(|p| {
                let wx = g.x0 + ((p % w) as f64 + 0.5) * g.upp;
                let wy = g.y1 - ((p / w) as f64 + 0.5) * g.upp;
                let i = ((wx - self.nav.lo.x) / self.nav.cell - 0.5).round();
                let j = ((wy - self.nav.lo.y) / self.nav.cell - 0.5).round();
                if i < 0.0 || j < 0.0 || i >= self.nav.nx as f64 || j >= self.nav.ny as f64 {
                    return false;
                }
                self.top[i as usize * self.nav.ny + j as usize] != NONE
            })
            .collect()
    }
}

struct Heat {
    w: usize,
    h: usize,
    t: Vec<f32>,
    ct: Vec<f32>,
}

fn blur(v: &mut [f32], w: usize, h: usize, sigma: f64) {
    let r = (sigma * 3.0).ceil().max(1.0) as isize;
    let k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32).collect();
    let rows: Vec<f32> = v
        .par_chunks(w)
        .flat_map_iter(|row| {
            (0..w as isize).map(|x| {
                let mut s = 0.0;
                for (ki, i) in (-r..=r).enumerate() {
                    let xx = x + i;
                    if xx >= 0 && xx < w as isize {
                        s += row[xx as usize] * k[ki];
                    }
                }
                s
            })
        })
        .collect();
    let cols: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|p| {
            let (x, y) = ((p % w) as isize, (p / w) as isize);
            let mut s = 0.0;
            for (ki, i) in (-r..=r).enumerate() {
                let yy = y + i;
                if yy >= 0 && yy < h as isize {
                    s += rows[yy as usize * w + x as usize] * k[ki];
                }
            }
            s
        })
        .collect();
    v.copy_from_slice(&cols);
}

impl Heat {
    fn new(g: &GridFrame, pts: impl Iterator<Item = ([f32; 3], Team)>, radius: f64, floor: Option<&[bool]>) -> Heat {
        let (w, h) = (g.wpx as usize, g.hpx as usize);
        let cell = (radius / 8.0).max(g.upp);
        let cw = ((g.x1 - g.x0) / cell).ceil() as usize + 2;
        let chh = ((g.y1 - g.y0) / cell).ceil() as usize + 2;
        let mut t = vec![0f32; cw * chh];
        let mut ct = vec![0f32; cw * chh];
        for (p, team) in pts {
            let fx = (p[0] as f64 - g.x0) / cell - 0.5;
            let fy = (g.y1 - p[1] as f64) / cell - 0.5;
            let (x0, y0) = (fx.floor(), fy.floor());
            let (u, v) = ((fx - x0) as f32, (fy - y0) as f32);
            let grid = match team {
                Team::T => &mut t,
                Team::Ct => &mut ct,
                _ => continue,
            };
            for (dx, dy, wgt) in [(0, 0, (1.0 - u) * (1.0 - v)), (1, 0, u * (1.0 - v)), (0, 1, (1.0 - u) * v), (1, 1, u * v)] {
                let (x, y) = (x0 as i64 + dx, y0 as i64 + dy);
                if x >= 0 && y >= 0 && (x as usize) < cw && (y as usize) < chh {
                    grid[y as usize * cw + x as usize] += wgt;
                }
            }
        }
        let sigma = (radius / 2.0 / cell).max(0.5);
        blur(&mut t, cw, chh, sigma);
        blur(&mut ct, cw, chh, sigma);
        let sample = |v: &[f32], p: usize| -> f32 {
            if floor.is_some_and(|f| !f[p]) {
                return 0.0;
            }
            let fx = ((p % w) as f64 + 0.5) * g.upp / cell - 0.5;
            let fy = ((p / w) as f64 + 0.5) * g.upp / cell - 0.5;
            let (x0, y0) = (fx.floor().max(0.0), fy.floor().max(0.0));
            let (x0, y0) = ((x0 as usize).min(cw - 2), (y0 as usize).min(chh - 2));
            let (u, vv) = ((fx - x0 as f64).clamp(0.0, 1.0) as f32, (fy - y0 as f64).clamp(0.0, 1.0) as f32);
            let at = |x: usize, y: usize| v[y * cw + x];
            let a = at(x0, y0) * (1.0 - u) + at(x0 + 1, y0) * u;
            let b = at(x0, y0 + 1) * (1.0 - u) + at(x0 + 1, y0 + 1) * u;
            a * (1.0 - vv) + b * vv
        };
        let t_px: Vec<f32> = (0..w * h).into_par_iter().map(|p| sample(&t, p)).collect();
        let ct_px: Vec<f32> = (0..w * h).into_par_iter().map(|p| sample(&ct, p)).collect();
        Heat { w, h, t: t_px, ct: ct_px }
    }

    fn total_scale(&self) -> f32 {
        let sum: Vec<f32> = self.t.iter().zip(&self.ct).map(|(a, b)| a + b).collect();
        Heat::scale(&sum)
    }

    fn scale(v: &[f32]) -> f32 {
        let max = v.iter().copied().fold(0.0, f32::max);
        if max <= 0.0 {
            return 1.0;
        }
        let mut nz: Vec<f32> = v.iter().copied().filter(|&x| x > max * 0.02).collect();
        nz.sort_by(f32::total_cmp);
        nz[((nz.len() - 1) as f64 * 0.995) as usize].max(max * 0.05)
    }
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t.clamp(0.0, 1.0))
}

fn seq(team: Team, t: f64) -> [f64; 3] {
    const TS: [[f64; 3]; 4] = [[130.0, 0.0, 10.0], [225.0, 45.0, 20.0], [255.0, 150.0, 0.0], [255.0, 240.0, 150.0]];
    const CS: [[f64; 3]; 4] = [[15.0, 35.0, 140.0], [30.0, 105.0, 255.0], [40.0, 200.0, 255.0], [215.0, 250.0, 255.0]];
    let s = if team == Team::T { &TS } else { &CS };
    let f = t.clamp(0.0, 1.0) * 3.0;
    let i = (f.floor() as usize).min(2);
    lerp3(s[i], s[i + 1], f - i as f64)
}

fn mix(share: f64, d: f64) -> [f64; 3] {
    let (ct, both, t) = ([45.0, 115.0, 255.0], [190.0, 80.0, 230.0], [255.0, 65.0, 35.0]);
    let c = if share < 0.5 { lerp3(ct, both, share * 2.0) } else { lerp3(both, t, share * 2.0 - 1.0) };
    lerp3(c, [255.0, 255.0, 255.0], (d - 0.6) / 0.4 * 0.55)
}

fn alpha(t: f64) -> f64 {
    let x = ((t - 0.02) / 0.38).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x) * 0.9
}

#[derive(Clone, Copy)]
enum Key {
    Team(Team, &'static str),
    Mix(&'static str),
}

fn paint_heat(base: &[u8], heat: &Heat, key: Key, scale: f32) -> Vec<u8> {
    let mut out = base.to_vec();
    out.par_chunks_mut(4).enumerate().for_each(|(i, p)| {
        let (t, ct) = (heat.t[i] as f64, heat.ct[i] as f64);
        let (a, c) = match key {
            Key::Team(team, _) => {
                let d = if team == Team::T { t } else { ct } / scale as f64;
                (alpha(d), seq(team, d))
            }
            Key::Mix(_) => {
                let d = (t + ct) / scale as f64;
                (alpha(d), mix(if t + ct > 0.0 { t / (t + ct) } else { 0.5 }, d))
            }
        };
        if a <= 0.0 {
            return;
        }
        for k in 0..3 {
            p[k] = (p[k] as f64 * (1.0 - a) + c[k] * a).round().clamp(0.0, 255.0) as u8;
        }
    });
    out
}

fn peaks(heat: &Heat, g: &GridFrame, radius: f64, n: usize) -> Vec<(f64, f64)> {
    let (w, h) = (heat.w, heat.h);
    let v = |x: usize, y: usize| heat.t[y * w + x] + heat.ct[y * w + x];
    let mut cand: Vec<(f32, usize, usize)> = Vec::new();
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let c = v(x, y);
            if c <= 0.0 {
                continue;
            }
            let mut top = true;
            'n: for dy in 0..3 {
                for dx in 0..3 {
                    if (dx, dy) != (1, 1) && v(x + dx - 1, y + dy - 1) > c {
                        top = false;
                        break 'n;
                    }
                }
            }
            if top {
                cand.push((c, x, y));
            }
        }
    }
    cand.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (_, x, y) in cand {
        let p = (g.x0 + (x as f64 + 0.5) * g.upp, g.y1 - (y as f64 + 0.5) * g.upp);
        if out.iter().all(|q| (q.0 - p.0).hypot(q.1 - p.1) >= radius * 2.0) {
            out.push(p);
            if out.len() == n {
                break;
            }
        }
    }
    out
}

fn f3(p: Option<[f32; 3]>) -> String {
    match p {
        Some(p) => format!("{:.0},{:.0},{:.0}", p[0], p[1], p[2]),
        None => ",,".into(),
    }
}

fn mmss(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

#[allow(clippy::too_many_arguments)]
pub fn export_kills(
    r: &mut Renderer,
    bsp: &Bsp,
    name: &str,
    cut_tag: &str,
    cuts: &Cuts,
    o: &KillOpts,
    data: &[DemoData],
    out: &Path,
    rep: &mut Report,
    p0: f32,
) -> Result<(Vec<PathBuf>, KillStats)> {
    let step = |rep: &mut Report, f: f32| rep.step(p0 + (1.0 - p0) * f);
    step(rep, 0.0)?;
    let t0 = std::time::Instant::now();
    let floor = match Floor::build(bsp, cuts.zmax) {
        Ok(f) => Some(f),
        Err(e) => {
            rep.log(format!("  {e:#}: events are not matched to floors"));
            None
        }
    };
    step(rep, 0.3)?;
    let mask = r.mask.clone();
    let [cx0, cy0, cx1, cy1] = cuts.clip;
    let visible = |p: [f32; 3]| -> bool {
        let (x, y, z) = (p[0] as f64, p[1] as f64, p[2] as f64);
        if x < cx0 || x > cx1 || y < cy0 || y > cy1 || z < cuts.zmin {
            return false;
        }
        if mask.as_ref().is_some_and(|m| !m.test(x, y)) {
            return false;
        }
        match &floor {
            Some(f) => f.visible(p, cuts.zmax),
            None => z - STAND_OFS <= cuts.zmax,
        }
    };

    let sel: Vec<(usize, &Death)> =
        data.iter().enumerate().flat_map(|(i, d)| d.deaths.iter().map(move |x| (i, x))).filter(|(_, d)| o.keeps(d)).collect();
    let drawn: Vec<bool> =
        sel.par_iter().map(|(_, d)| is_kill(d) && d.victim_pos.is_some_and(visible) && d.victim_team.playing()).collect();
    let pres: Vec<([f32; 3], Team)> = if o.presence {
        data.par_iter()
            .flat_map_iter(|d| d.presence.iter())
            .filter(|s| o.rounds.is_none_or(|(a, b)| (s.round as u32) >= a && (s.round as u32) <= b) && o.team.keeps(s.team))
            .map(|s| (s.pos.map(|v| v as f32), s.team))
            .filter(|(p, _)| visible(*p))
            .collect()
    } else {
        Vec::new()
    };
    rep.log(format!(
        "  {} of {} deaths drawn, {} presence samples ({:.1}s)",
        drawn.iter().filter(|&&b| b).count(),
        sel.len(),
        pres.len(),
        t0.elapsed().as_secs_f64()
    ));
    step(rep, 0.45)?;

    let (g, base) = dark_base(r, cuts, o.size)?;
    let fpx = floor.as_ref().map(|f| f.floor_px(&g));
    let kills = || sel.iter().zip(&drawn).filter(|(_, b)| **b).map(|((_, d), _)| *d);
    let heat = Heat::new(&g, kills().map(|d| (d.victim_pos.unwrap_or_default(), d.victim_team)), o.radius, fpx.as_deref());
    step(rep, 0.6)?;
    let (st, sct) = (Heat::scale(&heat.t), Heat::scale(&heat.ct));
    let both = heat.total_scale();
    let tops = peaks(&heat, &g, o.radius, 8);

    let sx = |x: f64| ((x - g.x0) / g.upp) as f32;
    let sy = |y: f64| ((g.y1 - y) / g.upp) as f32;
    let (fw, fh) = (g.wpx as f32, g.hpx as f32);
    let text = Text::new(14.0);
    let small = Text::new(12.0);
    let zs = zones(bsp);
    let ndemos = data.len();
    let nkills = kills().count();

    let decorate = |pm: &mut Pixmap, title: &str, key: Key, teams: &[Team], lines: bool, marks: bool| {
        for z in &zs {
            if !z.boxed && z.label.starts_with("hostage") {
                continue;
            }
            let (x0, y0, x1, y1) = (sx(z.lo.x), sy(z.hi.y), sx(z.hi.x), sy(z.lo.y));
            if z.boxed {
                outline(pm, x0, y0, x1, y1, z.color, 2.0);
            } else if let Some(c) = PathBuilder::from_circle((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0) {
                pm.stroke_path(&c, &paint(z.color, 255), &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
            }
            let (tw, th) = small.size(&z.label);
            let (lx, ly) = (x0.clamp(4.0, (fw - tw - 8.0).max(4.0)), (y0 - th - 6.0).max(4.0));
            rect(pm, lx - 3.0, ly - 1.0, lx + tw + 3.0, ly + th + 1.0, [0, 0, 0], 200);
            small.draw(pm, lx, ly, &z.label, z.color);
        }
        spawn_dots(pm, bsp, &g);
        if lines {
            for d in kills() {
                let (Some(a), Some(b)) = (d.killer_pos, d.victim_pos) else { continue };
                if !teams.contains(&d.victim_team) {
                    continue;
                }
                let c = if d.killer_team == Team::T { T } else { CT };
                line(pm, (sx(a[0] as f64), sy(a[1] as f64)), (sx(b[0] as f64), sy(b[1] as f64)), c, 70, 1.0);
            }
        }
        if marks {
            for (k, p) in tops.iter().enumerate() {
                let (x, y) = (sx(p.0), sy(p.1));
                let s = format!("{}", k + 1);
                let (tw, th) = small.size(&s);
                if let Some(c) = PathBuilder::from_circle(x, y, 9.0) {
                    pm.fill_path(&c, &paint([0, 0, 0], 200), tiny_skia::FillRule::Winding, Transform::identity(), None);
                    pm.stroke_path(&c, &paint([255, 255, 255], 255), &Stroke { width: 1.5, ..Default::default() }, Transform::identity(), None);
                }
                small.draw(pm, x - tw / 2.0, y - th / 2.0, &s, [255, 255, 255]);
            }
        }
        let (tw, th) = text.size(title);
        rect(pm, 4.0, 4.0, tw + 14.0, th + 12.0, [0, 0, 0], 200);
        text.draw(pm, 9.0, 8.0, title, [255, 255, 255]);
        let lw = 200.0;
        let rows = if matches!(key, Key::Mix(_)) { 2.0 } else { 1.0 } + if lines { 0.6 } else { 0.0 };
        let (lx, ly) = (fw - lw - 8.0, fh - 36.0 * rows - 12.0);
        rect(pm, lx - 8.0, ly - 8.0, fw - 4.0, fh - 4.0, [0, 0, 0], 200);
        let bw = lw - 10.0;
        let bar = |pm: &mut Pixmap, yy: f32, f: &dyn Fn(f64) -> ([f64; 3], f64)| {
            for x in 0..bw as usize {
                let (c, a) = f(x as f64 / bw as f64);
                line(pm, (lx + x as f32, yy), (lx + x as f32, yy + 12.0), c.map(|v| v as u8), (a * 255.0) as u8, 1.2);
            }
        };
        let mut yy = ly;
        match key {
            Key::Team(team, label) => {
                small.draw(pm, lx, yy, label, [255, 255, 255]);
                bar(pm, yy + 17.0, &|t| (seq(team, t), alpha(t).max(0.3)));
                yy += 36.0;
            }
            Key::Mix(label) => {
                small.draw(pm, lx, yy, label, [255, 255, 255]);
                bar(pm, yy + 17.0, &|t| (mix(t, 0.5), 0.9));
                let (tw, _) = small.size("T");
                small.draw(pm, lx, yy + 32.0, "CT", [255, 255, 255]);
                small.draw(pm, lx + bw / 2.0 - 14.0, yy + 32.0, "both", [255, 255, 255]);
                small.draw(pm, lx + bw - tw, yy + 32.0, "T", [255, 255, 255]);
                yy += 54.0;
                small.draw(pm, lx, yy, "fainter = fewer", [200, 200, 200]);
                yy += 20.0;
            }
        }
        if lines {
            line(pm, (lx, yy + 9.0), (lx + 12.0, yy + 9.0), T, 255, 2.0);
            line(pm, (lx + 14.0, yy + 9.0), (lx + 26.0, yy + 9.0), CT, 255, 2.0);
            small.draw(pm, lx + 34.0, yy + 1.0, "killer to victim", [255, 255, 255]);
        }
    };

    let save = |px: Vec<u8>, suffix: &str, files: &mut Partial, rep: &mut Report, dec: &dyn Fn(&mut Pixmap)| -> Result<()> {
        let mut pm = Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(g.wpx, g.hpx).context("bad size")?).context("pixmap")?;
        dec(&mut pm);
        let rgb: Vec<u8> = pm.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let f = free_name(out, &format!("{name}{cut_tag}_{suffix}"), ".png");
        files.add(f.clone());
        let enc = image::codecs::png::PngEncoder::new_with_quality(
            std::io::BufWriter::new(std::fs::File::create(&f)?),
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        );
        image::ImageEncoder::write_image(enc, &rgb, g.wpx, g.hpx, image::ExtendedColorType::Rgb8)?;
        rep.log(format!("  {}", f.display()));
        Ok(())
    };

    let mut files = Partial::default();
    let head = |what: &str| format!("{name}  {what}  {ndemos} demo{}", if ndemos == 1 { "" } else { "s" });
    let filt = o.filters();
    let tail = if filt.is_empty() { String::new() } else { format!("  ({filt})") };
    let n_t = kills().filter(|d| d.victim_team == Team::T).count();
    let n_ct = nkills - n_t;
    let variants: [(&str, bool, bool, String); 3] = [
        ("kills", o.team.keeps(Team::T), o.team.keeps(Team::Ct), format!("{}  {nkills} deaths{tail}", head("kills"))),
        ("kills_t", true, false, format!("{}  {n_t} T deaths{tail}", head("kills"))),
        ("kills_ct", false, true, format!("{}  {n_ct} CT deaths{tail}", head("kills"))),
    ];
    for (k, (suffix, show_t, show_ct, title)) in variants.iter().enumerate() {
        step(rep, 0.6 + 0.08 * k as f32)?;
        let key = match (show_t, show_ct, k) {
            (_, _, 0) => Key::Mix("deaths by victim team"),
            (true, _, _) => Key::Team(Team::T, "T deaths"),
            _ => Key::Team(Team::Ct, "CT deaths"),
        };
        let scale = match k {
            0 => both,
            1 => st,
            _ => sct,
        };
        let teams: Vec<Team> = [(Team::T, *show_t), (Team::Ct, *show_ct)].into_iter().filter(|x| x.1).map(|x| x.0).collect();
        let px = paint_heat(&base, &heat, key, scale);
        save(px, suffix, &mut files, rep, &|pm| decorate(pm, title, key, &teams, o.lines, k == 0))?;
    }

    if o.presence && !pres.is_empty() {
        step(rep, 0.85)?;
        let ph = Heat::new(&g, pres.iter().copied(), o.radius, fpx.as_deref());
        let px = paint_heat(&base, &ph, Key::Mix("player time by team"), ph.total_scale());
        let title = format!("{}  {} samples every {} s{tail}", head("presence"), pres.len(), o.presence_every);
        save(px, "presence", &mut files, rep, &|pm| decorate(pm, &title, Key::Mix("player time by team"), &[], false, false))?;
    }
    step(rep, 0.93)?;

    let mut csv = String::from(
        "demo,round,time,killer,victim,killer_team,victim_team,weapon,headshot,killer_x,killer_y,killer_z,victim_x,victim_y,victim_z,drawn\n",
    );
    for ((i, d), dr) in sel.iter().zip(&drawn) {
        writeln!(
            csv,
            "{},{},{:.2},{},{},{},{},{},{},{},{},{}",
            csv_quote(&data[*i].file),
            d.round,
            d.time,
            csv_quote(d.killer.as_deref().unwrap_or("world")),
            csv_quote(&d.victim),
            d.killer_team.key(),
            d.victim_team.key(),
            csv_quote(&d.weapon),
            d.headshot as u8,
            f3(d.killer_pos),
            f3(d.victim_pos),
            *dr as u8
        )?;
    }
    let f = free_name(out, &format!("{name}{cut_tag}_kills"), ".csv");
    files.add(f.clone());
    std::fs::write(&f, csv)?;
    rep.log(format!("  {}", f.display()));

    let all_kills: Vec<&Death> = sel.iter().map(|(_, d)| *d).filter(|d| is_kill(d)).collect();
    let tk = all_kills.iter().filter(|d| d.killer_team == d.victim_team && d.killer_team.playing()).count();
    let hs = all_kills.iter().filter(|d| d.headshot).count();
    let rounds: u32 = data.iter().map(|d| d.rounds).sum();
    let wins = data.iter().fold([0u32; 3], |a, d| [a[0] + d.wins[0], a[1] + d.wins[1], a[2] + d.wins[2]]);
    let secs: f64 = data.iter().map(|d| d.seconds as f64).sum();
    let mut per_weapon: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for d in &all_kills {
        let e = per_weapon.entry(d.weapon.as_str()).or_default();
        e.0 += 1;
        e.1 += d.headshot as usize;
    }
    let mut weapons: Vec<(&str, (usize, usize))> = per_weapon.into_iter().collect();
    weapons.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    let n_drawn = drawn.iter().filter(|&&b| b).count();

    let mut txt = String::new();
    writeln!(txt, "{name} kills from HLTV demos")?;
    writeln!(txt, "{ndemos} demos, {rounds} rounds (T won {}, CT won {}, other {}), {} of play", wins[0], wins[1], wins[2], mmss(secs))?;
    if !filt.is_empty() {
        writeln!(txt, "filters: {filt}")?;
    }
    writeln!(txt)?;
    writeln!(
        txt,
        "{} deaths: {} kills ({} team kills), {} suicides and world deaths",
        sel.len(),
        all_kills.len(),
        tk,
        sel.len() - all_kills.len()
    )?;
    writeln!(txt, "headshots: {hs} of {} kills, {:.1}%", all_kills.len(), pct(hs, all_kills.len()))?;
    writeln!(
        txt,
        "drawn: {n_drawn} kills on this floor; {} kills were on other floors, outside the cuts or had no position",
        all_kills.len() - n_drawn
    )?;
    if (all_kills.len() - n_drawn) * 10 > all_kills.len() {
        writeln!(txt, "lower floors under the visible one are hidden; cut roofs or height (--roofs, --zmax) to map them")?;
    }
    writeln!(txt)?;
    writeln!(txt, "{:<10}{:>8}{:>8}{:>12}", "team", "deaths", "kills", "headshot %")?;
    for team in [Team::T, Team::Ct] {
        let deaths = sel.iter().filter(|(_, d)| d.victim_team == team).count();
        let k: Vec<&&Death> = all_kills.iter().filter(|d| d.killer_team == team).collect();
        let h = k.iter().filter(|d| d.headshot).count();
        writeln!(txt, "{:<10}{:>8}{:>8}{:>12.1}", team.key(), deaths, k.len(), pct(h, k.len()))?;
    }
    writeln!(txt)?;
    writeln!(txt, "{:<16}{:>8}{:>12}", "weapon", "kills", "headshot %")?;
    for (w, (n, h)) in &weapons {
        writeln!(txt, "{:<16}{:>8}{:>12.1}", w, n, pct(*h, *n))?;
    }
    writeln!(txt)?;
    writeln!(txt, "busiest areas, numbered on {name}{cut_tag}_kills.png (world X Y; kills within {} units)", o.radius)?;
    for (k, p) in tops.iter().enumerate() {
        let near: Vec<&Death> = kills()
            .filter(|d| d.victim_pos.is_some_and(|v| (v[0] as f64 - p.0).hypot(v[1] as f64 - p.1) <= o.radius))
            .collect();
        let t = near.iter().filter(|d| d.victim_team == Team::T).count();
        writeln!(txt, "{:>2}  {:>7.0} {:>7.0}  {:>5}  (T {t}, CT {})", k + 1, p.0, p.1, near.len(), near.len() - t)?;
    }
    if o.presence {
        writeln!(txt)?;
        writeln!(txt, "presence: {} samples of living players every {} s, freeze time left out", pres.len(), o.presence_every)?;
    }
    writeln!(txt)?;
    writeln!(txt, "{:<48}{:>8}{:>8}{:>10}", "demo", "rounds", "deaths", "length")?;
    for d in data {
        writeln!(txt, "{:<48}{:>8}{:>8}{:>10}", d.file, d.rounds, d.deaths.len(), mmss(d.seconds as f64))?;
        for n in &d.notes {
            writeln!(txt, "    {n}")?;
        }
    }
    let f = free_name(out, &format!("{name}{cut_tag}_kills"), ".txt");
    files.add(f.clone());
    std::fs::write(&f, txt)?;
    rep.log(format!("  {}", f.display()));
    step(rep, 1.0)?;

    let stats = KillStats {
        demos: ndemos,
        rounds,
        deaths: sel.len(),
        kills: all_kills.len(),
        headshots: hs,
        t_deaths: sel.iter().filter(|(_, d)| d.victim_team == Team::T).count(),
        ct_deaths: sel.iter().filter(|(_, d)| d.victim_team == Team::Ct).count(),
        drawn: n_drawn,
        top_weapon: weapons.first().map(|w| w.0.to_string()).unwrap_or_default(),
        presence: pres.len(),
    };
    Ok((files.keep(), stats))
}
