use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;

use anyhow::Result;
use eframe::egui;
use egui::mutex::RwLock;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use glam::DVec3;

use crate::camera::{camera_basis, extents, top_down};
use crate::spin::{SpinOpts, export_spin};
use crate::timing::{TimingOpts, export_timing};
use crate::export::{IsoOpts, OverviewOpts, export_iso, export_overview, overview_params};
use crate::grid::{entity_marks, nice_step};
use crate::mesh::roof_zmax;
use crate::overview;
use crate::paths::{list_maps, run_dir};
use crate::render::{Cuts, Gpu, NO_CLIP, Renderer, View, wgpu};
use crate::scene::{CutOpts, LoadOpts, Scene};

pub fn run(map: Option<String>, game: Option<PathBuf>, view: &str) -> Result<()> {
    let mode = match view {
        "top" => Mode::Top,
        "overview" => Mode::Overview,
        _ => Mode::Iso,
    };
    let mut opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 950.0]).with_title("bsp2img"),
        ..Default::default()
    };
    let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut opts.wgpu_options.wgpu_setup else {
        anyhow::bail!("unexpected wgpu setup");
    };
    setup.power_preference = wgpu::PowerPreference::HighPerformance;
    setup.device_descriptor = Arc::new(|adapter| {
        let l = adapter.limits();
        wgpu::DeviceDescriptor {
            label: Some("bsp2img"),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: l.max_texture_dimension_2d,
                max_buffer_size: l.max_buffer_size,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        }
    });
    eframe::run_native("bsp2img", opts, Box::new(move |cc| {
        let mut app = App::new(cc);
        app.mode = mode;
        if let Some(g) = game {
            app.game = g.to_string_lossy().into_owned();
            app.refresh_maps();
        }
        if let Some(m) = map {
            let game = (!app.game.is_empty()).then(|| PathBuf::from(&app.game));
            match crate::paths::resolve_map(&m, game.as_deref()) {
                Ok(p) => {
                    app.current = Some(p);
                    app.start_load(&cc.egui_ctx);
                }
                Err(e) => app.log.push(format!("error: {e}")),
            }
        }
        Ok(Box::new(app))
    }))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

enum Msg {
    Log(String),
    Loaded(Box<Result<Scene, String>>),
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Iso,
    Top,
    Overview,
}

#[derive(Clone, Copy, PartialEq)]
enum Job {
    Iso,
    Overview,
    Spin,
    Timing,
}

struct Preview {
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    id: egui::TextureId,
    size: [u32; 2],
}

struct Frame {
    rect: Rect,
    view: View,
    upp: f64,
}

struct App {
    gpu: Gpu,
    egui_rend: Arc<RwLock<egui_wgpu::Renderer>>,
    game: String,
    maps: Vec<(String, PathBuf)>,
    filter: String,
    current: Option<PathBuf>,
    load: LoadOpts,
    hull_on: bool,
    hull_kind: usize,
    nearest: bool,
    cut: CutOpts,
    z_on: [bool; 2],
    z_val: [f64; 2],
    xy_on: [bool; 4],
    xy_val: [f64; 4],
    scene: Option<Scene>,
    renderer: Option<Renderer>,
    rx: Option<Receiver<Msg>>,
    log: Vec<String>,
    mode: Mode,
    yaw: f64,
    zoom: f64,
    pan: DVec3,
    sky: bool,
    sky_name: String,
    sky_fov: f64,
    sky_pitch: f64,
    sky_loaded: Option<String>,
    bg_on: bool,
    bg: [u8; 3],
    iso: IsoOpts,
    yaws_text: String,
    ov: OverviewOpts,
    spin: SpinOpts,
    timing: TimingOpts,
    out: String,
    preview: Option<Preview>,
    last_key: String,
    frame: Option<Frame>,
    drag_start: Option<Pos2>,
    pending: Option<Job>,
    pending_shown: bool,
    status: String,
    last_out: Option<PathBuf>,
}

#[cfg(windows)]
fn cfg_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(not(windows))]
fn cfg_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn cfg_path() -> Option<PathBuf> {
    cfg_dir().map(|d| d.join("bsp2img").join("gui.cfg"))
}

impl App {
    fn new(cc: &eframe::CreationContext) -> App {
        let rs = cc.wgpu_render_state.as_ref().expect("wgpu backend required");
        let gpu = Gpu::from_device(rs.device.clone(), rs.queue.clone());
        let mut app = App {
            gpu,
            egui_rend: rs.renderer.clone(),
            game: String::new(),
            maps: Vec::new(),
            filter: String::new(),
            current: None,
            load: LoadOpts::default(),
            hull_on: false,
            hull_kind: 3,
            nearest: false,
            cut: CutOpts::default(),
            z_on: [false; 2],
            z_val: [0.0; 2],
            xy_on: [false; 4],
            xy_val: [0.0; 4],
            scene: None,
            renderer: None,
            rx: None,
            log: Vec::new(),
            mode: Mode::Iso,
            yaw: 45.0,
            zoom: 1.0,
            pan: DVec3::ZERO,
            sky: false,
            sky_name: String::new(),
            sky_fov: 90.0,
            sky_pitch: 10.0,
            sky_loaded: None,
            bg_on: false,
            bg: [0x20, 0x20, 0x20],
            iso: IsoOpts::default(),
            yaws_text: "45 135 225 315".into(),
            ov: OverviewOpts::default(),
            spin: SpinOpts::default(),
            timing: TimingOpts::default(),
            out: std::env::current_dir().unwrap_or_default().join("renders").to_string_lossy().into_owned(),
            preview: None,
            last_key: String::new(),
            frame: None,
            drag_start: None,
            pending: None,
            pending_shown: false,
            status: String::new(),
            last_out: None,
        };
        if let Some(text) = cfg_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("game=") {
                    app.game = v.to_string();
                } else if let Some(v) = line.strip_prefix("out=") {
                    app.out = v.to_string();
                }
            }
        }
        app.refresh_maps();
        app
    }

    fn save_cfg(&self) {
        if let Some(p) = cfg_path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(p, format!("game={}\nout={}\n", self.game, self.out));
        }
    }

    fn refresh_maps(&mut self) {
        self.maps = if self.game.is_empty() { Vec::new() } else { list_maps(Path::new(&self.game)) };
    }

    fn start_load(&mut self, ctx: &egui::Context) {
        let Some(path) = self.current.clone() else { return };
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let opts = self.wanted_load();
        let max_dim = self.gpu.max_dim;
        let ctx = ctx.clone();
        self.log.push(format!("== {}", path.file_stem().unwrap_or_default().to_string_lossy()));
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let res = Scene::load(&path, &opts, max_dim, &mut |s| {
                let _ = tx.send(Msg::Log(s));
            });
            let _ = tx.send(Msg::Log(format!("loaded in {:.1}s", t.elapsed().as_secs_f64())));
            let _ = tx.send(Msg::Loaded(Box::new(res.map_err(|e| format!("{e:#}")))));
            ctx.request_repaint();
        });
    }

    fn wanted_load(&self) -> LoadOpts {
        let mut o = self.load.clone();
        o.game = (!self.game.is_empty()).then(|| PathBuf::from(&self.game));
        o.hull = self.hull_on.then_some(self.hull_kind);
        o
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut done = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Loaded(r) => done = Some(*r),
            }
        }
        if let Some(r) = done {
            self.rx = None;
            match r {
                Ok(scene) => {
                    self.renderer = Some(scene.renderer(&self.gpu, self.nearest));
                    self.scene = Some(scene);
                    self.sky_loaded = None;
                    self.last_key.clear();
                }
                Err(e) => self.log.push(format!("error: {e}")),
            }
        }
    }

    fn cut_opts(&self) -> CutOpts {
        let o = |on: bool, v: f64| on.then_some(v);
        CutOpts {
            roofs: self.cut.roofs,
            zmin: o(self.z_on[0], self.z_val[0]),
            zmax: o(self.z_on[1], self.z_val[1]),
            crop: None,
            xmin: o(self.xy_on[0], self.xy_val[0]),
            xmax: o(self.xy_on[1], self.xy_val[1]),
            ymin: o(self.xy_on[2], self.xy_val[2]),
            ymax: o(self.xy_on[3], self.xy_val[3]),
        }
    }

    fn cuts(&self) -> Cuts {
        let c = self.cut_opts();
        let levels = self.scene.as_ref().map(|s| s.levels.as_slice()).unwrap_or(&[]);
        let zmax = c.zmax.unwrap_or(1e9).min(roof_zmax(levels, c.roofs, &mut |_| {}));
        Cuts { zmin: c.zmin.unwrap_or(-1e9), zmax, clip: c.clip_box(), use_mask: true }
    }

    fn sync_sky(&mut self) {
        let want = if self.sky {
            self.scene.as_ref().map(|s| s.sky_name(Some(self.sky_name.trim())))
        } else {
            None
        };
        if want == self.sky_loaded {
            return;
        }
        let (Some(scene), Some(r)) = (&self.scene, &mut self.renderer) else { return };
        match &want {
            Some(n) => match scene.load_sky(n) {
                Some(f) => {
                    r.set_sky(Some(f), self.sky_fov, self.sky_pitch);
                    self.log.push(format!("sky: {n}"));
                }
                None => {
                    r.set_sky(None, 0.0, 0.0);
                    self.log.push(format!("sky: {n} not found in gfx/env"));
                }
            },
            None => r.set_sky(None, 0.0, 0.0),
        }
        self.sky_loaded = want;
    }

    fn sky_tag(&self) -> String {
        let n = self.sky_name.trim();
        if self.sky && !n.is_empty() && self.renderer.as_ref().is_some_and(|r| r.has_sky()) {
            format!("_{n}")
        } else {
            String::new()
        }
    }

    fn run_job(&mut self, job: Job) {
        let cuts = self.cuts();
        let co = self.cut_opts();
        let hull = self.scene.as_ref().and_then(|s| s.opts.hull);
        let sky_tag = self.sky_tag();
        let (Some(scene), Some(r)) = (&self.scene, &mut self.renderer) else { return };
        let name = scene.bsp.name();
        let mut lines = Vec::new();
        let mut log = |s: String| lines.push(s);
        let res = run_dir(Path::new(&self.out), &name).and_then(|out| {
            match job {
                Job::Iso => {
                    let mut o = self.iso.clone();
                    o.yaws = self.yaws_text.split_whitespace().filter_map(|v| v.parse().ok()).collect();
                    if o.yaws.is_empty() {
                        o.yaws = vec![self.yaw];
                    }
                    o.bg = self.bg_on.then_some(self.bg);
                    export_iso(r, &scene.bsp, &name, &sky_tag, &co.tag(hull), &cuts, &o, &out, &mut log)?;
                }
                Job::Overview => {
                    export_overview(r, &scene.bsp, &name, &cuts, &self.ov, &out, &mut log)?;
                }
                Job::Spin => {
                    let mut o = self.spin.clone();
                    o.pitch = self.iso.pitch;
                    o.ss = self.iso.ss;
                    o.cull = self.iso.cull;
                    o.bg = self.bg_on.then_some(self.bg);
                    export_spin(r, &name, &sky_tag, &co.tag(hull), &cuts, &o, &out, &mut log)?;
                }
                Job::Timing => {
                    export_timing(r, &scene.bsp, &name, &co.tag(hull), &cuts, &self.timing, &out, &mut log)?;
                }
            }
            Ok(out)
        });
        self.log.push(format!("== export {name}"));
        self.log.extend(lines);
        match res {
            Ok(out) => {
                self.status = format!("wrote {}", out.display());
                self.last_out = Some(out);
            }
            Err(e) => {
                self.status = format!("export failed: {e:#}");
                self.log.push(self.status.clone());
            }
        }
        self.last_key.clear();
        self.save_cfg();
    }

    fn side(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Map");
            ui.horizontal(|ui| {
                ui.label("Game");
                let r = ui.add(egui::TextEdit::singleline(&mut self.game).desired_width(170.0));
                if r.lost_focus() {
                    self.refresh_maps();
                    self.save_cfg();
                }
                if ui.button("...").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.game = d.to_string_lossy().into_owned();
                        self.refresh_maps();
                        self.save_cfg();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Filter");
                ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(150.0));
                if ui.button("Open .bsp").clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("BSP", &["bsp"]).pick_file() {
                        self.current = Some(f);
                        self.start_load(&ctx);
                    }
                }
            });
            let filter = self.filter.to_lowercase();
            let mut pick = None;
            egui::ScrollArea::vertical().id_salt("maps").max_height(220.0).show(ui, |ui| {
                ui.set_min_width(290.0);
                for (n, p) in self.maps.iter().filter(|(n, _)| n.to_lowercase().contains(&filter)) {
                    let sel = self.current.as_ref() == Some(p);
                    if ui.selectable_label(sel, n).clicked() {
                        pick = Some(p.clone());
                    }
                }
                if self.maps.is_empty() {
                    ui.weak("set the game folder to list maps");
                }
            });
            if let Some(p) = pick {
                self.current = Some(p);
                self.start_load(&ctx);
            }
            if self.rx.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("loading...");
                });
            }

            ui.separator();
            ui.heading("Crop");
            ui.checkbox(&mut self.load.auto_crop, "Auto crop (remove sealed rooms)");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.hull_on, "Hull crop");
                ui.radio_value(&mut self.hull_kind, 3, "crouch");
                ui.radio_value(&mut self.hull_kind, 1, "stand");
            });
            ui.add(egui::Slider::new(&mut self.load.hull_pad, 0.0..=256.0).text("hull pad"));
            ui.collapsing("Lighting", |ui| {
                let l = &mut self.load.light;
                ui.add(egui::Slider::new(&mut l.brightness, 0.0..=3.0).text("brightness"));
                ui.add(egui::Slider::new(&mut l.scale, 0.25..=4.0).text("light scale"));
                ui.add(egui::Slider::new(&mut l.gamma, 1.0..=3.0).text("gamma"));
                ui.add(egui::Slider::new(&mut l.texgamma, 1.0..=3.0).text("texgamma"));
                ui.add(egui::Slider::new(&mut l.lightgamma, 1.0..=3.0).text("lightgamma"));
                ui.checkbox(&mut l.all_styles, "All light styles");
            });
            let changed = self.scene.as_ref().is_some_and(|s| s.opts != self.wanted_load());
            ui.horizontal(|ui| {
                let apply = ui.add_enabled(changed && self.rx.is_none(), egui::Button::new("Reload with these"));
                if apply.clicked() {
                    self.start_load(&ctx);
                }
                if changed {
                    ui.weak("needs reload");
                }
            });

            ui.separator();
            ui.heading("Cuts");
            let nlev = self.scene.as_ref().map(|s| s.levels.len()).unwrap_or(0);
            ui.add(egui::Slider::new(&mut self.cut.roofs, 0..=nlev.saturating_sub(1).max(1)).text("roofs removed"));
            if let Some(s) = &self.scene {
                let lv: Vec<String> = s.levels.iter().enumerate().map(|(i, l)| format!("{}:{:.0}", i + 1, l.0)).collect();
                ui.weak(format!("levels {}", lv.join("  ")));
            }
            for (i, label) in ["z min", "z max"].iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.z_on[i], *label);
                    ui.add_enabled(self.z_on[i], egui::DragValue::new(&mut self.z_val[i]).speed(4.0));
                });
            }
            for (i, label) in ["x min", "x max", "y min", "y max"].iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.xy_on[i], *label);
                    ui.add_enabled(self.xy_on[i], egui::DragValue::new(&mut self.xy_val[i]).speed(8.0));
                });
            }
            ui.weak("Top view: shift+drag draws the XY box");
            if ui.button("Clear cuts").clicked() {
                self.cut.roofs = 0;
                self.z_on = [false; 2];
                self.xy_on = [false; 4];
            }

            ui.separator();
            ui.heading("Look");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.sky, "Sky");
                ui.add(egui::TextEdit::singleline(&mut self.sky_name).hint_text("map default").desired_width(110.0));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.bg_on, "Background");
                ui.color_edit_button_srgb(&mut self.bg);
            });
            ui.checkbox(&mut self.iso.cull, "Cutaway (back-face cull)");
            if ui.checkbox(&mut self.nearest, "Pixelated textures").changed() {
                if let Some(s) = &self.scene {
                    let mut r = s.renderer(&self.gpu, self.nearest);
                    r.mask = self.renderer.as_mut().and_then(|r| r.mask.take());
                    self.renderer = Some(r);
                    self.sky_loaded = None;
                }
            }

            ui.separator();
            ui.heading("Export");
            ui.horizontal(|ui| {
                ui.label("Output");
                ui.add(egui::TextEdit::singleline(&mut self.out).desired_width(170.0));
                if ui.button("...").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.out = d.to_string_lossy().into_owned();
                        self.save_cfg();
                    }
                }
            });
            ui.label("Isometric");
            ui.horizontal(|ui| {
                ui.label("yaws");
                ui.add(egui::TextEdit::singleline(&mut self.yaws_text).desired_width(120.0));
            });
            ui.add(egui::Slider::new(&mut self.iso.pitch, 5.0..=89.0).text("pitch"));
            ui.horizontal(|ui| {
                if ui.button("true iso").clicked() {
                    self.iso.pitch = 35.264;
                }
                if ui.button("2:1").clicked() {
                    self.iso.pitch = 30.0;
                }
            });
            ui.add(egui::Slider::new(&mut self.iso.size, 512..=8192).text("size px"));
            ui.add(egui::Slider::new(&mut self.iso.ss, 1..=4).text("supersample"));
            ui.checkbox(&mut self.iso.grid, "Also write grid preview");
            let ready = self.scene.is_some() && self.pending.is_none();
            if ui.add_enabled(ready, egui::Button::new("Export isometric")).clicked() {
                self.pending = Some(Job::Iso);
            }
            ui.label("Animation (uses pitch and supersample above)");
            ui.add(egui::Slider::new(&mut self.spin.size, 256..=2048).text("size px"));
            ui.add(egui::Slider::new(&mut self.spin.seconds, 2.0..=60.0).text("seconds per turn"));
            ui.add(egui::Slider::new(&mut self.spin.fps, 10.0..=60.0).text("fps"));
            ui.checkbox(&mut self.spin.ccw, "Turn the other way");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.spin.mp4, "MP4");
                ui.checkbox(&mut self.spin.gif, "GIF");
                ui.checkbox(&mut self.spin.apng, "APNG");
            });
            if ui.add_enabled(ready, egui::Button::new("Export animation")).clicked() {
                self.pending = Some(Job::Spin);
            }
            ui.label("Rush timings (uses roof and height cuts)");
            ui.add(egui::Slider::new(&mut self.timing.speed, 100.0..=260.0).text("speed u/s"));
            ui.add(egui::Slider::new(&mut self.timing.interval, 1.0..=10.0).text("contour s"));
            if ui.add_enabled(ready, egui::Button::new("Export rush timings")).clicked() {
                self.pending = Some(Job::Timing);
            }
            ui.label("Overview");
            ui.add(egui::Slider::new(&mut self.ov.margin, 0.0..=0.3).text("margin"));
            ui.checkbox(&mut self.ov.png, "Also write PNG");
            ui.checkbox(&mut self.ov.grid, "Also write grid preview");
            if ui.add_enabled(ready, egui::Button::new("Export overview")).clicked() {
                self.pending = Some(Job::Overview);
            }
            if self.pending.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("exporting...");
                });
            } else if !self.status.is_empty() {
                ui.label(&self.status);
                if let Some(o) = &self.last_out {
                    if ui.button("Open folder").clicked() {
                        let _ = std::process::Command::new("explorer").arg(o).spawn();
                    }
                }
            }
        });
    }

    fn ensure_preview(&mut self, w: u32, h: u32) {
        if self.preview.as_ref().is_some_and(|p| p.size == [w, h]) {
            return;
        }
        let Some(r) = &self.renderer else { return };
        let (tex, view, depth) = r.make_targets(w, h, true);
        let mut er = self.egui_rend.write();
        let id = match &self.preview {
            Some(p) => {
                er.update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, p.id);
                p.id
            }
            None => er.register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear),
        };
        drop(er);
        self.preview = Some(Preview { _tex: tex, view, depth, id, size: [w, h] });
        self.last_key.clear();
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Iso, "Isometric");
            ui.selectable_value(&mut self.mode, Mode::Top, "Top");
            ui.selectable_value(&mut self.mode, Mode::Overview, "Overview");
            ui.separator();
            if self.mode == Mode::Iso {
                for y in [45.0, 135.0, 225.0, 315.0] {
                    if ui.button(format!("{y:.0}")).clicked() {
                        self.yaw = y;
                    }
                }
                ui.label(format!("yaw {:.0}", self.yaw.rem_euclid(360.0)));
            }
            if ui.button("Reset view").clicked() {
                self.zoom = 1.0;
                self.pan = DVec3::ZERO;
            }
            ui.weak(match self.mode {
                Mode::Iso => "drag rotate, right-drag pan, wheel zoom",
                Mode::Top => "drag pan, shift+drag XY box, wheel zoom",
                Mode::Overview => "exact overview framing (1024x768)",
            });
        });
        let avail = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(avail, Sense::click_and_drag());
        let painter = ui.painter_at(avail);
        painter.rect_filled(avail, 0.0, Color32::from_gray(24));
        if self.renderer.is_none() {
            painter.text(avail.center(), Align2::CENTER_CENTER, "pick a map", FontId::proportional(18.0), Color32::GRAY);
            return;
        }
        self.sync_sky();
        let ppp = ui.ctx().pixels_per_point();
        let cuts = self.cuts();

        let mut rect = avail;
        if self.mode == Mode::Overview {
            let s = (avail.width() / 4.0).min(avail.height() / 3.0);
            rect = Rect::from_center_size(avail.center(), Vec2::new(s * 4.0, s * 3.0));
        }
        let (wpx, hpx) = (((rect.width() * ppp) as u32).max(16), ((rect.height() * ppp) as u32).max(16));
        if wpx > self.gpu.max_dim || hpx > self.gpu.max_dim {
            return;
        }

        self.handle_input(ui, &resp, rect);

        let key = format!(
            "{:?}{:?}{}{}{}{:?}{}{}{:?}{}{}",
            cuts,
            (self.mode as u8, self.yaw, self.zoom, self.pan.to_array()),
            self.iso.pitch,
            self.iso.cull,
            self.bg_on,
            self.bg,
            wpx,
            hpx,
            self.sky_loaded,
            self.ov.margin,
            self.renderer.as_ref().map(|r| r.points.len()).unwrap_or(0)
        );
        self.ensure_preview(wpx, hpx);
        let Some(r) = &self.renderer else { return };
        let aspect = rect.height() as f64 / rect.width() as f64;
        let (view, upp, rcuts, clear) = match self.mode {
            Mode::Iso => {
                let b = camera_basis(self.yaw, self.iso.pitch);
                let e = extents(&r.points_in(&cuts), &b);
                let fit = ((e[0].1 - e[0].0) / rect.width() as f64).max((e[1].1 - e[1].0) / rect.height() as f64) * 1.08;
                let upp = fit / self.zoom;
                let v = View {
                    basis: b,
                    cx: (e[0].0 + e[0].1) / 2.0 + self.pan.dot(b.r),
                    cy: (e[1].0 + e[1].1) / 2.0 + self.pan.dot(b.u),
                    w: rect.width() as f64 * upp,
                    h: rect.height() as f64 * upp,
                    sky_yaw: Some(self.yaw),
                };
                let bg = if self.bg_on { self.bg.map(|c| c as f64 / 255.0) } else { [0.094, 0.094, 0.1] };
                (v, upp, cuts, [bg[0], bg[1], bg[2], 1.0])
            }
            Mode::Top => {
                let rc = Cuts { clip: NO_CLIP, use_mask: false, ..cuts };
                let pts = r.points_in(&rc);
                let lo = pts.iter().copied().fold(DVec3::splat(f64::INFINITY), DVec3::min);
                let hi = pts.iter().copied().fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max);
                let fit = ((hi.x - lo.x) / rect.width() as f64).max((hi.y - lo.y) / rect.height() as f64) * 1.08;
                let upp = fit / self.zoom;
                let v = View {
                    basis: top_down(DVec3::X, DVec3::Y),
                    cx: (lo.x + hi.x) / 2.0 + self.pan.x,
                    cy: (lo.y + hi.y) / 2.0 + self.pan.y,
                    w: rect.width() as f64 * upp,
                    h: rect.height() as f64 * upp,
                    sky_yaw: None,
                };
                (v, upp, rc, [0.11, 0.11, 0.11, 1.0])
            }
            Mode::Overview => {
                let Ok(ov) = overview_params(r, &cuts, &self.ov) else { return };
                let v = overview::view(&ov);
                let upp = v.w / rect.width() as f64;
                (v, upp, cuts, [0.0, 1.0, 0.0, 1.0])
            }
        };
        if key != self.last_key {
            let p = self.preview.as_ref().unwrap();
            let mut enc = self.gpu.device.create_command_encoder(&Default::default());
            r.encode(&mut enc, &p.view, &p.depth, &view, aspect, &rcuts, self.iso.cull, clear);
            self.gpu.queue.submit([enc.finish()]);
            self.last_key = key;
        }
        let id = self.preview.as_ref().unwrap().id;
        painter.image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        self.frame = Some(Frame { rect, view, upp });
        match self.mode {
            Mode::Top => self.overlay_top(&painter, &resp, &cuts),
            Mode::Overview => {
                if let Some(r) = &self.renderer {
                    if let Ok(ov) = overview_params(r, &cuts, &self.ov) {
                        painter.text(
                            rect.left_top() + Vec2::new(6.0, 6.0),
                            Align2::LEFT_TOP,
                            format!(
                                "ZOOM {:.2}  ORIGIN {:.0} {:.0} {:.0}  ROTATED {}",
                                ov.zoom, ov.origin[0], ov.origin[1], ov.origin[2], ov.rotated
                            ),
                            FontId::monospace(13.0),
                            Color32::BLACK,
                        );
                    }
                }
            }
            Mode::Iso => {}
        }
    }

    fn handle_input(&mut self, ui: &egui::Ui, resp: &egui::Response, rect: Rect) {
        let upp = self.frame.as_ref().map(|f| f.upp).unwrap_or(1.0);
        let d = resp.drag_delta();
        let shift = ui.input(|i| i.modifiers.shift);
        match self.mode {
            Mode::Iso => {
                if resp.dragged_by(egui::PointerButton::Primary) {
                    self.yaw = (self.yaw - d.x as f64 * 0.4).rem_euclid(360.0);
                    self.iso.pitch = (self.iso.pitch + d.y as f64 * 0.25).clamp(5.0, 89.0);
                } else if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
                    let b = camera_basis(self.yaw, self.iso.pitch);
                    self.pan += -b.r * d.x as f64 * upp + b.u * d.y as f64 * upp;
                }
            }
            Mode::Top => {
                if resp.drag_started_by(egui::PointerButton::Primary) && shift {
                    self.drag_start = resp.interact_pointer_pos();
                }
                if self.drag_start.is_some() {
                    if resp.drag_stopped() {
                        if let (Some(a), Some(b), Some(f)) = (self.drag_start, resp.interact_pointer_pos(), &self.frame) {
                            let (wa, wb) = (to_world(f, a), to_world(f, b));
                            self.xy_val = [wa.0.min(wb.0).round(), wa.0.max(wb.0).round(), wa.1.min(wb.1).round(), wa.1.max(wb.1).round()];
                            self.xy_on = [true; 4];
                        }
                        self.drag_start = None;
                    }
                } else if resp.dragged() {
                    self.pan += DVec3::new(-d.x as f64 * upp, d.y as f64 * upp, 0.0);
                }
            }
            Mode::Overview => {}
        }
        if self.mode != Mode::Overview && resp.hovered() {
            let s = ui.input(|i| i.smooth_scroll_delta.y);
            if s != 0.0 {
                self.zoom = (self.zoom * (s as f64 * 0.003).exp()).clamp(0.2, 64.0);
            }
        }
        let _ = rect;
    }

    fn overlay_top(&self, painter: &egui::Painter, resp: &egui::Response, cuts: &Cuts) {
        let Some(f) = &self.frame else { return };
        let rect = f.rect;
        let (x0, y1) = to_world(f, rect.left_top());
        let (x1, y0) = to_world(f, rect.right_bottom());
        let step = nice_step((x1 - x0).max(y1 - y0));
        let font = FontId::monospace(11.0);
        let mut gx = (x0 / step).ceil() * step;
        while gx < x1 {
            let p = to_screen(f, gx, 0.0);
            let major = gx == 0.0 || gx % (step * 4.0) == 0.0;
            let c = if major { Color32::from_rgba_unmultiplied(255, 230, 0, 160) } else { Color32::from_white_alpha(50) };
            painter.line_segment([Pos2::new(p.x, rect.top()), Pos2::new(p.x, rect.bottom())], Stroke::new(1.0, c));
            painter.text(Pos2::new(p.x + 3.0, rect.top() + 3.0), Align2::LEFT_TOP, format!("x {gx:.0}"), font.clone(), Color32::WHITE);
            gx += step;
        }
        let mut gy = (y0 / step).ceil() * step;
        while gy < y1 {
            let p = to_screen(f, 0.0, gy);
            let major = gy == 0.0 || gy % (step * 4.0) == 0.0;
            let c = if major { Color32::from_rgba_unmultiplied(255, 230, 0, 160) } else { Color32::from_white_alpha(50) };
            painter.line_segment([Pos2::new(rect.left(), p.y), Pos2::new(rect.right(), p.y)], Stroke::new(1.0, c));
            painter.text(Pos2::new(rect.left() + 3.0, p.y + 3.0), Align2::LEFT_TOP, format!("y {gy:.0}"), font.clone(), Color32::WHITE);
            gy += step;
        }
        if let Some(s) = &self.scene {
            for m in entity_marks(&s.bsp) {
                let c = Color32::from_rgb(m.color[0], m.color[1], m.color[2]);
                if let Some(b) = m.bbox {
                    let r = Rect::from_two_pos(to_screen(f, b[0], b[1]), to_screen(f, b[2], b[3]));
                    painter.rect_stroke(r, 0.0, Stroke::new(2.0, c), StrokeKind::Inside);
                    painter.text(r.center(), Align2::CENTER_CENTER, m.label, font.clone(), c);
                } else {
                    painter.circle(to_screen(f, m.pos.x, m.pos.y), 4.0, c, Stroke::new(1.0, Color32::BLACK));
                }
            }
        }
        let shade = Color32::from_rgba_unmultiplied(200, 0, 0, 90);
        if cuts.clip != NO_CLIP {
            let [cx0, cy0, cx1, cy1] = cuts.clip;
            let a = to_screen(f, cx0.max(x0 - 1.0), cy1.min(y1 + 1.0));
            let b = to_screen(f, cx1.min(x1 + 1.0), cy0.max(y0 - 1.0));
            let inner = Rect::from_two_pos(a, b).intersect(rect);
            for r in [
                Rect::from_min_max(rect.min, Pos2::new(rect.max.x, inner.min.y)),
                Rect::from_min_max(Pos2::new(rect.min.x, inner.max.y), rect.max),
                Rect::from_min_max(Pos2::new(rect.min.x, inner.min.y), Pos2::new(inner.min.x, inner.max.y)),
                Rect::from_min_max(Pos2::new(inner.max.x, inner.min.y), Pos2::new(rect.max.x, inner.max.y)),
            ] {
                if r.is_positive() {
                    painter.rect_filled(r, 0.0, shade);
                }
            }
            painter.rect_stroke(inner, 0.0, Stroke::new(2.0, Color32::from_rgb(255, 60, 60)), StrokeKind::Inside);
        }
        if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
            painter.rect_stroke(Rect::from_two_pos(a, b), 0.0, Stroke::new(2.0, Color32::from_rgb(255, 120, 60)), StrokeKind::Inside);
        }
        if let Some(p) = resp.hover_pos() {
            let (wx, wy) = to_world(f, p);
            let t = format!("{wx:.0}, {wy:.0}");
            painter.text(rect.right_bottom() - Vec2::new(8.0, 8.0), Align2::RIGHT_BOTTOM, t, FontId::monospace(14.0), Color32::WHITE);
        }
    }
}

fn to_world(f: &Frame, p: Pos2) -> (f64, f64) {
    let c = f.rect.center();
    (f.view.cx + (p.x - c.x) as f64 * f.upp, f.view.cy - (p.y - c.y) as f64 * f.upp)
}

fn to_screen(f: &Frame, x: f64, y: f64) -> Pos2 {
    let c = f.rect.center();
    Pos2::new(c.x + ((x - f.view.cx) / f.upp) as f32, c.y - ((y - f.view.cy) / f.upp) as f32)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        if let Some(job) = self.pending {
            if self.pending_shown {
                self.pending = None;
                self.pending_shown = false;
                self.run_job(job);
            } else {
                self.pending_shown = true;
            }
        }
        egui::Panel::left("side").resizable(true).default_size(330.0).show(ui, |ui| self.side(ui));
        egui::Panel::bottom("log").resizable(true).default_size(110.0).show(ui, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for l in &self.log {
                    ui.monospace(l);
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
        if self.pending.is_some() {
            ui.ctx().request_repaint();
        }
    }
}
