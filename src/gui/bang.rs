use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use eframe::egui;
use egui::{Color32, Pos2, Stroke};
use glam::DVec3;

use super::App;
use super::jobs::Bg;
use super::preview::{Frame, Mode};
use crate::bsp::Engine;
use crate::gltf::Layers;
use crate::grid::Backdrop;
use crate::mesh::Mesh;
use crate::nav::Stance;
use crate::pixelwalk::present::{Pixelwalk, PixelwalkOpts};
use crate::radar::Split;
use crate::render::{Cuts, GpuScene, MarkLine, Markers, Renderer};
use crate::scene::Cancelled;
use crate::wallbang::draw2d::{self, GREEN, Layer};
use crate::wallbang::export::{Base, Sim};
use crate::wallbang::shots::{self, Shot, ShotOpts, Spot};
use crate::wallbang::sim::{WEAPONS, weapon};
use crate::wallbang::{EntOpts, WallbangOpts};

const RASTER: f64 = 2048.0;
const SIGMA: f64 = 2.0;
const WALL_R: f64 = 24.0;
const REPLAY_S: f64 = 3.0;
const REPLAY_HOLD: usize = 50;
const FLY_DIST: f64 = 320.0;
const PATH_PX: f64 = 1.5;
const CROSS: f64 = 6.0;
const PICK_PX: f32 = 12.0;
const MAX_SIMS: usize = 3;

static GEN: AtomicU64 = AtomicU64::new(1);

fn next_gen() -> u64 {
    GEN.fetch_add(1, Ordering::Relaxed)
}

fn halt(stop: &AtomicBool) -> anyhow::Result<()> {
    if stop.load(Ordering::Relaxed) { Err(Cancelled.into()) } else { Ok(()) }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Style {
    Off,
    Areas,
    Lines,
}

impl Style {
    const ALL: [(Style, &'static str); 3] = [(Style::Off, "Off"), (Style::Areas, "Areas"), (Style::Lines, "Lines")];

    fn key(self) -> &'static str {
        Style::ALL.iter().find(|(s, _)| *s == self).map_or("Off", |(_, n)| *n)
    }

    fn step(self, by: usize) -> Style {
        let i = Style::ALL.iter().position(|(s, _)| *s == self).unwrap_or(0);
        Style::ALL[(i + by) % Style::ALL.len()].0
    }
}

pub const STANCES: [&str; 3] = ["both", "stand", "duck"];

pub struct PwData {
    list: Vec<Pixelwalk>,
    mover: crate::pixelwalk::Mover,
    zr: (f64, f64),
    generation: u64,
}

#[derive(Clone, PartialEq)]
struct SimKey {
    weapon: String,
    stance: String,
}

struct SimEntry {
    key: SimKey,
    generation: u64,
    sim: Arc<Sim>,
}

#[derive(Clone, Copy, PartialEq)]
struct PaintKey {
    sim: u64,
    pw: u64,
    damage: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct RasterKey {
    sim: u64,
    style: Style,
    zmin: f64,
    zmax: f64,
    damage: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct MarkKey {
    pw: u64,
    look: u64,
    frame: Option<usize>,
    replay: Option<usize>,
    damage: bool,
    paths: Option<(u64, bool)>,
}

struct RasterOut {
    px: Vec<u8>,
    fr: draw2d::Frame,
    origin: DVec3,
}

struct Inspect {
    sim: u64,
    spots: Vec<Spot>,
    shots: Vec<Shot>,
    wall: Option<DVec3>,
}

impl Inspect {
    fn shown(&self) -> impl Iterator<Item = &Shot> {
        self.shots
            .iter()
            .filter(|s| self.wall.is_none_or(|w| s.crossings().iter().any(|c| seg_dist(w, c.entry(), s.exit(c)) <= WALL_R)))
    }
}

struct Replay {
    pw: usize,
    path: Vec<DVec3>,
    t0: f64,
}

pub struct Bang {
    pub style: Style,
    pub weapon: String,
    pub stance: String,
    pub opacity: f32,
    pub paint: bool,
    pub damage: bool,
    pub paths: bool,
    inspect: bool,
    pixelwalks: bool,
    pw_window: bool,
    scene: u64,
    base: Option<Arc<GpuScene>>,
    wb_base: Option<Arc<Base>>,
    sims: Vec<SimEntry>,
    sim_job: Option<Bg<Sim, SimKey>>,
    failed: Option<SimKey>,
    pw: Option<Arc<PwData>>,
    pw_job: Option<Bg<PwData, ()>>,
    pw_failed: bool,
    raster: Option<(RasterKey, egui::TextureHandle, draw2d::Frame, DVec3)>,
    raster_job: Option<Bg<RasterOut, RasterKey>>,
    look: Option<Inspect>,
    look_gen: u64,
    paint_job: Option<Bg<(Arc<GpuScene>, Arc<Mesh>), PaintKey>>,
    painted: Option<PaintKey>,
    paint_cache: Option<(PaintKey, Arc<GpuScene>, Arc<Mesh>)>,
    path_cache: Option<((u64, bool), Arc<Markers>)>,
    marks: Option<MarkKey>,
    replay: Option<Replay>,
    selected: Option<usize>,
}

impl Default for Bang {
    fn default() -> Bang {
        Bang {
            style: Style::Lines,
            weapon: "ak47".into(),
            stance: "both".into(),
            opacity: 0.8,
            paint: true,
            damage: false,
            paths: false,
            inspect: false,
            pixelwalks: false,
            pw_window: true,
            scene: 0,
            base: None,
            wb_base: None,
            sims: Vec::new(),
            sim_job: None,
            failed: None,
            pw: None,
            pw_job: None,
            pw_failed: false,
            raster: None,
            raster_job: None,
            look: None,
            look_gen: 0,
            paint_job: None,
            painted: None,
            paint_cache: None,
            path_cache: None,
            marks: None,
            replay: None,
            selected: None,
        }
    }
}

fn seg_dist(p: DVec3, a: DVec3, b: DVec3) -> f64 {
    let d = b - a;
    let t = if d.length_squared() > 1e-9 { ((p - a).dot(d) / d.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + d * t)
}

fn project(f: &Frame, p: DVec3) -> Option<Pos2> {
    let b = &f.view.basis;
    let c = f.rect.center();
    match f.view.persp {
        Some(ps) => {
            let v = p - ps.eye;
            let z = v.dot(b.f);
            if z <= 1.0 {
                return None;
            }
            let ty = (ps.fov_y.to_radians() / 2.0).tan();
            let aspect = (f.rect.width() / f.rect.height().max(1.0)) as f64;
            let nx = v.dot(b.r) / (z * ty * aspect);
            let ny = v.dot(b.u) / (z * ty);
            Some(Pos2::new(c.x + nx as f32 * f.rect.width() / 2.0, c.y - ny as f32 * f.rect.height() / 2.0))
        }
        None => Some(Pos2::new(c.x + ((p.dot(b.r) - f.view.cx) / f.upp) as f32, c.y - ((p.dot(b.u) - f.view.cy) / f.upp) as f32)),
    }
}

fn rgba(c: [u8; 3], a: u8) -> [u8; 4] {
    [c[0], c[1], c[2], a]
}

fn color32(c: [u8; 3], a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

impl App {
    fn bang_ok(&self) -> bool {
        self.scene.as_ref().is_some_and(|s| matches!(s.bsp.engine(), Engine::GoldSrc | Engine::Cs2))
    }

    fn pw_ok(&self) -> bool {
        self.scene.as_ref().is_some_and(|s| s.bsp.engine() == Engine::GoldSrc)
    }

    fn pw_on(&self) -> bool {
        self.bang.pixelwalks && self.pw_ok()
    }

    fn is_current(&self, k: &SimKey) -> bool {
        k.weapon == self.bang.weapon && k.stance == self.bang.stance
    }

    fn sim_key(&self) -> SimKey {
        SimKey { weapon: self.bang.weapon.clone(), stance: self.bang.stance.clone() }
    }

    fn shot_opts(&self) -> ShotOpts {
        let w = weapon(&self.bang.weapon).unwrap_or(&WEAPONS[0]);
        ShotOpts::new(w, self.bang.stance != "duck", self.bang.stance != "stand")
    }

    fn current_entry(&self) -> Option<&SimEntry> {
        self.bang.sims.iter().find(|e| self.is_current(&e.key))
    }

    fn current_sim(&self) -> Option<Arc<Sim>> {
        self.current_entry().map(|e| e.sim.clone())
    }

    fn current_gen(&self) -> Option<u64> {
        self.current_entry().map(|e| e.generation)
    }

    fn install_renderer(&mut self, gs: Option<&Arc<GpuScene>>) {
        let Some(s) = &self.scene else { return };
        let r: Result<Renderer, _> = s.renderer(&self.gpu, gs, self.look.nearest);
        match r {
            Ok(r) => {
                self.renderer = Some(r);
                self.sky_loaded = None;
                self.last_key.clear();
                self.bang.marks = None;
            }
            Err(e) => self.log.push(format!("error: {e:#}")),
        }
    }

    pub(super) fn bang_poll(&mut self) {
        if self.bang.scene != self.scene_gen {
            let keep = (self.bang.style, self.bang.weapon.clone(), self.bang.stance.clone(), self.bang.opacity, self.bang.paint);
            let (inspect, pixelwalks, pw_window) = (self.bang.inspect, self.bang.pixelwalks, self.bang.pw_window);
            let (damage, paths) = (self.bang.damage, self.bang.paths);
            self.bang = Bang::default();
            (self.bang.style, self.bang.weapon, self.bang.stance, self.bang.opacity, self.bang.paint) = keep;
            (self.bang.inspect, self.bang.pixelwalks, self.bang.pw_window) = (inspect, pixelwalks, pw_window);
            (self.bang.damage, self.bang.paths) = (damage, paths);
            self.bang.scene = self.scene_gen;
            self.bang.base = self.renderer.as_ref().map(|r| r.scene().clone());
        }
        if let Some(r) = self.bang.sim_job.as_ref().and_then(|j| j.poll(&mut self.log)) {
            let j = self.bang.sim_job.take().unwrap();
            match r {
                Ok(s) => {
                    self.log.push(format!("wallbang ready ({:.1}s)", j.started.elapsed().as_secs_f64()));
                    self.bang.wb_base = Some(s.base.clone());
                    self.bang.sims.retain(|e| e.key != j.key);
                    self.bang.sims.push(SimEntry { key: j.key.clone(), generation: next_gen(), sim: Arc::new(s) });
                    let extra = self.bang.sims.len().saturating_sub(MAX_SIMS);
                    self.bang.sims.drain(..extra);
                }
                Err(e) => {
                    self.log.push(format!("wallbang: {e}"));
                    self.bang.failed = Some(j.key.clone());
                }
            }
        }
        if let Some(r) = self.bang.pw_job.as_ref().and_then(|j| j.poll(&mut self.log)) {
            let j = self.bang.pw_job.take().unwrap();
            match r {
                Ok(p) => {
                    self.log.push(format!("pixelwalks: {} found ({:.1}s)", p.list.len(), j.started.elapsed().as_secs_f64()));
                    self.bang.pw = Some(Arc::new(p));
                }
                Err(e) => {
                    self.log.push(format!("pixelwalks: {e}"));
                    self.bang.pw_failed = true;
                }
            }
        }
        if let Some(r) = self.bang.paint_job.as_ref().and_then(|j| j.poll(&mut self.log)) {
            let j = self.bang.paint_job.take().unwrap();
            match r {
                Ok((gs, mesh)) => {
                    self.bang.paint_cache = Some((j.key, gs, mesh));
                }
                Err(e) => self.log.push(format!("paint: {e}")),
            }
        }
    }

    fn paint_key(&self) -> (Option<PaintKey>, Option<Arc<Sim>>, Option<Arc<PwData>>) {
        if !self.bang.paint || !matches!(self.mode, Mode::Iso | Mode::Free) {
            return (None, None, None);
        }
        let sim = self.current_entry().filter(|_| self.bang.style != Style::Off);
        let pw = self.bang.pw.clone().filter(|_| self.pw_on());
        if sim.is_none() && pw.is_none() {
            return (None, None, None);
        }
        let k = PaintKey {
            sim: sim.map_or(0, |e| e.generation),
            pw: pw.as_ref().map_or(0, |p| p.generation),
            damage: self.bang.damage,
        };
        (Some(k), sim.map(|e| e.sim.clone()), pw)
    }

    pub(super) fn bang_sync(&mut self, ctx: &egui::Context) {
        if !self.bang_ok() || self.load_rx.is_some() {
            return;
        }
        self.bang_inspect_stale();
        let wants = self.bang.style != Style::Off || self.bang.inspect;
        if wants
            && self.current_entry().is_none()
            && self.bang.sim_job.as_ref().is_none_or(|j| !self.is_current(&j.key))
            && self.bang.failed.as_ref().is_none_or(|k| !self.is_current(k))
            && let Some(scene) = self.scene.clone()
        {
            let o = WallbangOpts {
                shots: self.shot_opts(),
                ents: EntOpts::default(),
                materials: None,
                areas: true,
                lines: false,
                sigma: SIGMA,
                base: Backdrop::Render,
                size: 1600,
                split: Split::None,
                others_faint: false,
                csv: false,
                pixelwalks: false,
                damage: false,
                cache: self.cache_loc().dir(),
            };
            let base = self.bang.wb_base.clone();
            self.log.push(format!("== wallbang {} {} ({})", scene.bsp.name(), self.bang.weapon, self.bang.stance));
            self.bang.sim_job = Some(Bg::spawn(self.sim_key(), ctx, move |stop, rep| {
                crate::wallbang::export::simulate(&scene.bsp, &scene.dirs, &o, base, Some(stop), rep)
            }));
        }
        if self.pw_on()
            && self.bang.pw.is_none()
            && self.bang.pw_job.is_none()
            && !self.bang.pw_failed
            && let Some(scene) = self.scene.clone()
        {
            self.log.push(format!("== pixelwalks {}", scene.bsp.name()));
            self.bang.pw_job = Some(Bg::spawn((), ctx, move |stop, _| {
                crate::pixelwalk::check(&scene.bsp)?;
                let mover = crate::pixelwalk::Mover::new(&scene.bsp);
                let list = crate::pixelwalk::present::find_with(&mover, &PixelwalkOpts::default(), Some(stop))?;
                Ok(PwData { list, mover, zr: crate::pixelwalk::present::world_z(&scene.bsp), generation: next_gen() })
            }));
        }
        let (want, sim, pw) = self.paint_key();
        if want != self.bang.painted {
            match want {
                None => {
                    self.bang.paint_job = None;
                    self.bang.painted = None;
                    let base = self.bang.base.clone();
                    self.install_renderer(base.as_ref());
                }
                Some(want) => {
                    if let Some(gs) = self.bang.paint_cache.as_ref().filter(|c| c.0 == want).map(|c| c.1.clone()) {
                        self.install_renderer(Some(&gs));
                        self.bang.painted = Some(want);
                    } else if self.bang.paint_job.as_ref().is_none_or(|j| j.key != want)
                        && let Some(scene) = self.scene.clone()
                    {
                        let gpu = self.gpu.clone();
                        let damage = self.bang.damage;
                        self.bang.paint_job = Some(Bg::spawn(want, ctx, move |stop, rep| {
                            let mut splats = sim.map(|s| crate::wallbang::draw3d::splats(&s, damage)).unwrap_or_default();
                            if let Some(p) = &pw {
                                splats.extend(crate::pixelwalk::draw3d::seam_splats(&p.list));
                            }
                            halt(stop)?;
                            let mut mesh = scene.mesh.clone();
                            let p = crate::mesh::overlay::paint(&mut mesh, &splats, &crate::wallbang::draw3d::COLORS, gpu.max_dim);
                            halt(stop)?;
                            rep.log(format!("paint: {} surface points, {} on no drawn face; {} faces painted ({}u texels)", p.splats, p.missed, p.faces, p.texel));
                            Ok((GpuScene::new(&gpu, &mesh)?, Arc::new(mesh)))
                        }));
                    }
                }
            }
        }
        let now = ctx.input(|i| i.time);
        let frame = self.bang.replay.as_ref().map(|r| {
            ctx.request_repaint();
            let k = ((now - r.t0) * 100.0) as usize % (r.path.len() + REPLAY_HOLD);
            k.min(r.path.len() - 1)
        });
        if let Some(e) = self.current_entry().filter(|_| self.bang.paths && self.bang.style != Style::Off) {
            let k = (e.generation, self.bang.damage);
            if self.bang.path_cache.as_ref().is_none_or(|c| c.0 != k) {
                let sim = e.sim.clone();
                let bs = crate::wallbang::draw3d::bundles(&sim.shots, self.bang.damage);
                self.log.push(format!("paths: {} shots in {} bundles", sim.shots.len(), bs.len()));
                self.bang.path_cache = Some((k, Arc::new(crate::wallbang::draw3d::path_markers(&bs))));
            }
        }
        let mk = MarkKey {
            pw: self.bang.pw.as_ref().filter(|_| self.pw_on()).map_or(0, |p| p.generation),
            look: if self.bang.inspect { self.bang.look_gen } else { 0 },
            frame,
            replay: self.bang.replay.as_ref().map(|r| r.pw),
            damage: self.bang.damage,
            paths: self.shown_paths_key(),
        };
        if self.bang.marks != Some(mk) {
            let m = self.bang_markers(frame);
            if let Some(r) = &mut self.renderer {
                r.set_markers(Some(m));
            }
            self.bang.marks = Some(mk);
            self.last_key.clear();
        }
    }

    fn shown_paths_key(&self) -> Option<(u64, bool)> {
        if !self.bang.paths || self.bang.style == Style::Off || !matches!(self.mode, Mode::Iso | Mode::Free) {
            return None;
        }
        let k = (self.current_gen()?, self.bang.damage);
        self.bang.path_cache.as_ref().filter(|c| c.0 == k).map(|c| c.0)
    }

    fn shown_paths(&self) -> Option<Arc<Markers>> {
        let k = self.shown_paths_key()?;
        self.bang.path_cache.as_ref().filter(|c| c.0 == k).map(|c| c.1.clone())
    }

    pub(super) fn bang_layers(&self) -> Option<Arc<Layers>> {
        if !self.bang_ok() {
            return None;
        }
        let mesh = self.bang.paint_cache.as_ref().filter(|c| self.bang.painted == Some(c.0)).map(|c| c.2.clone());
        let mut nodes = Vec::new();
        if let Some(p) = self.shown_paths() {
            nodes.push(("wallbang_paths".to_string(), (*p).clone()));
        }
        if self.pw_on()
            && let Some(pw) = &self.bang.pw
        {
            nodes.push(("pixelwalks".to_string(), crate::pixelwalk::draw3d::markers(&pw.list, pw.zr)));
        }
        (mesh.is_some() || !nodes.is_empty()).then(|| Arc::new(Layers { mesh, nodes }))
    }

    fn bang_markers(&self, frame: Option<usize>) -> Markers {
        let mut m = Markers::default();
        if let Some(p) = self.shown_paths() {
            m.lines.extend(p.lines.iter().cloned());
        }
        if self.pw_on()
            && let Some(pw) = &self.bang.pw
        {
            let p = crate::pixelwalk::draw3d::markers(&pw.list, pw.zr);
            m.tris.extend(p.tris);
            m.lines.extend(p.lines);
            if let (Some(r), Some(k)) = (&self.bang.replay, frame)
                && let Some(p) = pw.list.get(r.pw)
                && let Some(&at) = r.path.get(k)
            {
                crate::pixelwalk::draw3d::ghost(&mut m, at, p.stance, [255, 255, 255]);
            }
        }
        if self.bang.inspect
            && let Some(l) = &self.bang.look
        {
            for s in l.shown() {
                let c = draw2d::color(s.kind(self.bang.damage));
                m.lines.push(MarkLine { a: s.start(), b: s.end(), color: rgba(c, 220), px: PATH_PX });
                if s.made() {
                    let x = s.exit(s.last());
                    for d in [DVec3::X, DVec3::Y, DVec3::Z] {
                        m.lines.push(MarkLine { a: x - d * CROSS, b: x + d * CROSS, color: rgba(GREEN, 255), px: PATH_PX });
                    }
                }
            }
            for sp in &l.spots {
                crate::pixelwalk::draw3d::ghost(&mut m, sp.origin(), sp.stance, [255, 255, 255]);
            }
        }
        m
    }

    pub(super) fn bang_bar(&mut self, ui: &mut egui::Ui) {
        if !self.bang_ok() {
            return;
        }
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            let b = ui.button(format!("Wallbang: {}", self.bang.style.key())).on_hover_text(
                "click: next, right-click: previous. Areas / Lines are drawn over Top and Overview; Isometric and Free show it as surface paint",
            );
            if b.clicked() {
                self.bang.style = self.bang.style.step(1);
            } else if b.secondary_clicked() {
                self.bang.style = self.bang.style.step(2);
            }
            egui::ComboBox::from_id_salt("bang_weapon").width(80.0).selected_text(self.bang.weapon.clone()).show_ui(ui, |ui| {
                for w in &WEAPONS {
                    ui.selectable_value(&mut self.bang.weapon, w.name.to_string(), w.name);
                }
            });
            egui::ComboBox::from_id_salt("bang_stance").width(60.0).selected_text(self.bang.stance.clone()).show_ui(ui, |ui| {
                for s in STANCES {
                    ui.selectable_value(&mut self.bang.stance, s.to_string(), s);
                }
            });
            ui.add_enabled(self.bang.style != Style::Off, egui::Slider::new(&mut self.bang.opacity, 0.0..=1.0).text("opacity"));
            ui.checkbox(&mut self.bang.paint, "Paint 3D").on_hover_text("tint walls in Isometric and Free with the wallbang result and the pixelwalk seams");
            ui.checkbox(&mut self.bang.damage, "Damage").on_hover_text(format!(
                "made shots: green when a headshot kills ({}+ unarmoured damage past the wall), yellow when it does not",
                shots::LETHAL_HEAD
            ));
            ui.toggle_value(&mut self.bang.paths, "Paths")
                .on_hover_text("Isometric and Free: shot paths bundled by wall spot and direction, seen through walls; wider = more shots");
            let ins = ui.toggle_value(&mut self.bang.inspect, "Inspect").on_hover_text(
                "click a spot: fire the fan from there only. shift+click a wall: keep only shots through it",
            );
            if ins.changed() {
                self.bang.look_gen += 1;
            }
            if self.bang.inspect
                && self.bang.look.as_ref().is_some_and(|l| l.wall.is_some())
                && ui.small_button("All walls").clicked()
                && let Some(l) = &mut self.bang.look
            {
                l.wall = None;
                self.bang.look_gen += 1;
            }
            if self.pw_ok() {
                ui.separator();
                if ui.checkbox(&mut self.bang.pixelwalks, "Pixelwalks").changed() {
                    self.bang.replay = None;
                }
                if self.bang.pixelwalks && self.bang.pw.is_some() {
                    ui.toggle_value(&mut self.bang.pw_window, "List");
                }
            }
            let mut busy = Vec::new();
            if let Some(j) = &self.bang.sim_job {
                busy.push(format!("simulating {:.0}s", j.started.elapsed().as_secs_f64()));
            }
            if let Some(j) = &self.bang.pw_job {
                busy.push(format!("finding pixelwalks {:.0}s", j.started.elapsed().as_secs_f64()));
            }
            if self.bang.paint_job.is_some() {
                busy.push("painting".into());
            }
            if !busy.is_empty() {
                ui.spinner();
                ui.label(busy.join(", "));
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            } else if self.bang.inspect
                && let Some(l) = &self.bang.look
            {
                let (n, made) = l.shown().fold((0, 0), |(n, m), s| (n + 1, m + s.made() as usize));
                ui.label(format!("{n} shots through a wall, {made} made it"));
            } else if self.bang.style != Style::Off
                && let Some(s) = self.current_sim()
            {
                ui.label(format!("{} made, {} died", s.counts.made, s.counts.died));
            }
        });
    }

    fn raster_poll(&mut self, ctx: &egui::Context, want: Option<RasterKey>) {
        if let Some(r) = self.bang.raster_job.as_ref().and_then(|j| j.poll(&mut self.log)) {
            let j = self.bang.raster_job.take().unwrap();
            match r {
                Ok(o) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([o.fr.w as usize, o.fr.h as usize], &o.px);
                    let tex = ctx.load_texture("wallbang", img, egui::TextureOptions::LINEAR);
                    self.bang.raster = Some((j.key, tex, o.fr, o.origin));
                }
                Err(e) => self.log.push(format!("wallbang raster: {e}")),
            }
        }
        let Some(want) = want else { return };
        if self.bang.raster_job.is_some() || self.bang.raster.as_ref().is_some_and(|r| r.0 == want) {
            return;
        }
        let (Some(sim), Some(s)) = (self.current_sim(), &self.scene) else { return };
        let (lo, hi) = (s.bsp.models[0].mins, s.bsp.models[0].maxs);
        let upp = ((hi.x - lo.x).max(hi.y - lo.y) / RASTER).max(1e-3);
        let (w, h) = (((hi.x - lo.x) / upp).ceil() as u32 + 1, ((hi.y - lo.y) / upp).ceil() as u32 + 1);
        let fr = draw2d::Frame { w, h, m: [1.0 / upp, 0.0, -lo.x / upp, 0.0, -1.0 / upp, hi.y / upp], upp };
        let origin = DVec3::new(lo.x, hi.y, 0.0);
        self.bang.raster_job = Some(Bg::spawn(want, ctx, move |_, _| {
            let main: Vec<&Shot> = sim
                .shots
                .iter()
                .filter(|s| {
                    let z = sim.spots[s.shooter as usize].feet.z;
                    z >= want.zmin && z <= want.zmax
                })
                .collect();
            let layer = Layer { main, faint: Vec::new(), damage: want.damage };
            let px = if want.style == Style::Areas { draw2d::areas_rgba(&fr, &layer, SIGMA) } else { draw2d::lines_rgba(&fr, &layer) };
            Ok(RasterOut { px, fr, origin })
        }));
    }

    pub(super) fn bang_draw(&mut self, painter: &egui::Painter, cuts: &Cuts, two_d: bool) {
        let ctx = painter.ctx().clone();
        let want = self
            .current_gen()
            .filter(|_| two_d && self.bang.style != Style::Off)
            .map(|g| RasterKey { sim: g, style: self.bang.style, zmin: cuts.zmin, zmax: cuts.zmax, damage: self.bang.damage });
        self.raster_poll(&ctx, want);
        let Some(f) = &self.frame else { return };
        let rect = f.rect;
        if let Some(want) = want
            && let Some((key, tex, fr, o)) = &self.bang.raster
            && key.sim == want.sim
            && key.style == want.style
        {
            let (w, h) = (fr.w as f64 * fr.upp, fr.h as f64 * fr.upp);
            let corners = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|(dx, dy)| project(f, *o + DVec3::new(dx, -dy, 0.0)));
            if let [Some(a), Some(b), Some(c), Some(d)] = corners {
                let tint = Color32::from_white_alpha((self.bang.opacity.clamp(0.0, 1.0) * 255.0) as u8);
                let mut mesh = egui::Mesh::with_texture(tex.id());
                for (p, uv) in [(a, (0.0, 0.0)), (b, (1.0, 0.0)), (c, (1.0, 1.0)), (d, (0.0, 1.0))] {
                    mesh.vertices.push(egui::epaint::Vertex { pos: p, uv: Pos2::new(uv.0, uv.1), color: tint });
                }
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.with_clip_rect(rect).add(egui::Shape::mesh(mesh));
            }
        }
        if two_d
            && self.bang.inspect
            && let Some(l) = &self.bang.look
        {
            let p = painter.with_clip_rect(rect);
            for s in l.shown() {
                let c = draw2d::color(s.kind(self.bang.damage));
                if let (Some(a), Some(b)) = (project(f, s.start()), project(f, s.end())) {
                    p.line_segment([a, b], Stroke::new(1.0, color32(c, 150)));
                }
                if s.made()
                    && let Some(x) = project(f, s.exit(s.last()))
                {
                    p.circle_filled(x, 2.0, color32(GREEN, 255));
                }
            }
            for sp in &l.spots {
                if let Some(q) = project(f, sp.feet) {
                    p.circle(q, 5.0, Color32::WHITE, Stroke::new(1.5, Color32::BLACK));
                }
            }
            if let Some(q) = l.wall.and_then(|w| project(f, w)) {
                p.circle_stroke(q, 8.0, Stroke::new(2.0, Color32::from_rgb(255, 40, 255)));
            }
        }
    }

    pub(super) fn bang_click(&mut self, resp: &egui::Response, shift: bool, ctrl: bool, ppp: f32) {
        if !self.bang_ok() || !resp.clicked() || ctrl {
            return;
        }
        let Some(pos) = resp.interact_pointer_pos() else { return };
        if !self.bang.inspect {
            self.pick_pixelwalk(pos, &resp.ctx);
            return;
        }
        let Some((pt, _)) = self.pick_point(pos, ppp) else {
            self.status = "no geometry under the cursor".into();
            return;
        };
        if shift {
            if let Some(l) = &mut self.bang.look {
                l.wall = Some(pt);
                self.bang.look_gen += 1;
            }
            return;
        }
        let Some((sim, generation)) = self.current_entry().map(|e| (e.sim.clone(), e.generation)) else {
            self.status = "wallbang: simulation not ready yet".into();
            return;
        };
        let opts = self.shot_opts();
        let nav = &sim.base.nav;
        let (feet, stand_ok) = match nav.nearest(pt, 0.0) {
            Some(n) => {
                let (x, y) = nav.cell_xy(nav.col[n] as usize);
                (DVec3::new(x, y, nav.nodes[n].z as f64), nav.nodes[n].stand)
            }
            None => (pt, true),
        };
        let mut spots = Vec::new();
        if opts.stand && stand_ok {
            spots.push(Spot { feet, stance: Stance::Stand });
        }
        if opts.duck {
            spots.push(Spot { feet, stance: Stance::Duck });
        }
        let t = std::time::Instant::now();
        let (list, c) = shots::simulate(&sim.ctx(&opts), &spots, &|_| {});
        self.status = format!(
            "inspect {:.0} {:.0} {:.0}: {} shots fired, {} through a wall ({:.0} ms)",
            feet.x,
            feet.y,
            feet.z,
            c.fired,
            list.len(),
            t.elapsed().as_secs_f64() * 1000.0
        );
        self.bang.look = Some(Inspect { sim: generation, spots, shots: list, wall: None });
        self.bang.look_gen += 1;
    }

    fn pick_pixelwalk(&mut self, pos: Pos2, ctx: &egui::Context) {
        let (Some(f), Some(pw)) = (&self.frame, self.bang.pw.clone().filter(|_| self.pw_on())) else { return };
        let best = pw
            .list
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let d = [p.from, p.mid(), p.to].iter().filter_map(|q| project(f, *q)).map(|q| q.distance(pos)).fold(f32::INFINITY, f32::min);
                (d <= PICK_PX).then_some((d, i))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, i)) = best {
            self.select_pixelwalk(i, false, ctx);
        }
    }

    fn select_pixelwalk(&mut self, i: usize, fly: bool, ctx: &egui::Context) {
        let Some(pw) = self.bang.pw.clone() else { return };
        let Some(p) = pw.list.get(i) else { return };
        self.bang.selected = Some(i);
        let cmd = crate::pixelwalk::present::setpos(p.from, p.yaw);
        ctx.copy_text(cmd.clone());
        self.status = format!("#{} copied: {cmd}", i + 1);
        if fly {
            self.mode = Mode::Free;
            self.cam.target = p.mid() - DVec3::Z * p.stance.feet();
            self.cam.dist = FLY_DIST;
            self.frame_cam = false;
        }
    }

    pub(super) fn bang_window(&mut self, ctx: &egui::Context) {
        if !self.bang_ok() || !self.pw_on() || !self.bang.pw_window {
            return;
        }
        let Some(pw) = self.bang.pw.clone() else { return };
        let mut open = true;
        let (mut fly, mut replay, mut stop) = (None, None, false);
        egui::Window::new(format!("Pixelwalks ({})", pw.list.len())).open(&mut open).default_size([460.0, 380.0]).show(ctx, |ui| {
            ui.weak("click: fly there and copy its amx_setpos. Replay: hold +forward for 3 s from the spot");
            if self.bang.replay.is_some() && ui.button("Stop replay").clicked() {
                stop = true;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if pw.list.is_empty() {
                    ui.weak("none found");
                }
                for (i, p) in pw.list.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("Replay").clicked() {
                            replay = Some(i);
                        }
                        let text = format!(
                            "#{} {}  yaw {:.1}  z {:.0}  span {:.0}u  {} find{}",
                            i + 1,
                            p.stance.label(),
                            p.yaw,
                            p.from.z,
                            p.span(),
                            p.finds.len(),
                            if p.finds.len() == 1 { "" } else { "s" }
                        );
                        if ui.selectable_label(self.bang.selected == Some(i), text).clicked() {
                            fly = Some(i);
                        }
                    });
                }
            });
        });
        if !open {
            self.bang.pw_window = false;
        }
        if stop {
            self.bang.replay = None;
        }
        if let Some(i) = fly.or(replay) {
            self.select_pixelwalk(i, true, ctx);
        }
        if let Some(i) = replay {
            let path = pw.mover.drive(&pw.list[i], REPLAY_S);
            self.bang.replay = (!path.is_empty()).then(|| Replay { pw: i, path, t0: ctx.input(|x| x.time) });
        }
    }

    fn bang_inspect_stale(&mut self) {
        let g = self.current_gen();
        if self.bang.look.as_ref().is_some_and(|l| Some(l.sim) != g) {
            self.bang.look = None;
            self.bang.look_gen += 1;
        }
    }
}
