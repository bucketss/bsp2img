mod cfg;
mod export_ui;
mod jobs;
mod preview;
mod tabs;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use anyhow::Result;
use eframe::egui;
use egui::mutex::RwLock;
use egui::Pos2;
use glam::DVec3;

use crate::export::{IsoOpts, OverviewOpts};
use crate::health::HealthOpts;
use crate::look::Look;
use crate::mesh::roof_zmax;
use crate::paths::list_maps;
use crate::render::{Cuts, Gpu, Renderer, wgpu};
use crate::scene::{CutOpts, LoadOpts, Scene};
use crate::spin::{AnimOpts, PeelOpts, SliceOpts};
use crate::timing::TimingOpts;

use export_ui::Exporter;
use jobs::Running;
use preview::{Frame, Mode, Preview};

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
    Progress(f32, String),
    JobDone(Result<PathBuf, String>),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Map,
    Scene,
    Look,
    Camera,
    Export,
}

impl Tab {
    const ALL: [(Tab, &'static str); 5] =
        [(Tab::Map, "Map"), (Tab::Scene, "Scene"), (Tab::Look, "Look"), (Tab::Camera, "Camera"), (Tab::Export, "Export")];

    fn key(self) -> &'static str {
        Tab::ALL.iter().find(|(t, _)| *t == self).map(|(_, n)| *n).unwrap_or("Map")
    }

    fn from_key(k: &str) -> Option<Tab> {
        Tab::ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(k)).map(|(t, _)| *t)
    }
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
    cut: CutOpts,
    z_on: [bool; 2],
    z_val: [f64; 2],
    xy_on: [bool; 4],
    xy_val: [f64; 4],
    scene: Option<Arc<Scene>>,
    renderer: Option<Renderer>,
    load_rx: Option<Receiver<Msg>>,
    log: Vec<String>,
    log_open: bool,
    tab: Tab,
    mode: Mode,
    yaw: f64,
    zoom: f64,
    pan: DVec3,
    look: Look,
    bg_last: [u8; 3],
    sky_loaded: Option<String>,
    iso: IsoOpts,
    yaws_text: String,
    ov: OverviewOpts,
    spin: AnimOpts,
    peel: PeelOpts,
    slice: SliceOpts,
    timing: TimingOpts,
    health: HealthOpts,
    health_text: Option<(String, String)>,
    exporter: Exporter,
    out: String,
    preview: Option<Preview>,
    last_key: String,
    frame: Option<Frame>,
    drag_start: Option<Pos2>,
    job: Option<Running>,
    status: String,
    last_out: Option<PathBuf>,
    cfg_saved: String,
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
            cut: CutOpts::default(),
            z_on: [false; 2],
            z_val: [0.0; 2],
            xy_on: [false; 4],
            xy_val: [0.0; 4],
            scene: None,
            renderer: None,
            load_rx: None,
            log: Vec::new(),
            log_open: true,
            tab: Tab::Map,
            mode: Mode::Iso,
            yaw: 45.0,
            zoom: 1.0,
            pan: DVec3::ZERO,
            look: Look::default(),
            bg_last: [0x20, 0x20, 0x20],
            sky_loaded: None,
            iso: IsoOpts::default(),
            yaws_text: String::new(),
            ov: OverviewOpts::default(),
            spin: AnimOpts::default(),
            peel: PeelOpts::default(),
            slice: SliceOpts::default(),
            timing: TimingOpts::default(),
            health: HealthOpts::default(),
            health_text: None,
            exporter: Exporter::Iso,
            out: std::env::current_dir().unwrap_or_default().join("renders").to_string_lossy().into_owned(),
            preview: None,
            last_key: String::new(),
            frame: None,
            drag_start: None,
            job: None,
            status: String::new(),
            last_out: None,
            cfg_saved: String::new(),
        };
        app.load_cfg();
        app.refresh_maps();
        app
    }

    fn refresh_maps(&mut self) {
        self.maps = if self.game.is_empty() { Vec::new() } else { list_maps(Path::new(&self.game)) };
    }

    fn map_name(&self) -> Option<String> {
        self.current.as_ref().map(|p| p.file_stem().unwrap_or_default().to_string_lossy().into_owned())
    }

    fn start_load(&mut self, ctx: &egui::Context) {
        let Some(path) = self.current.clone() else { return };
        let (tx, rx) = channel();
        self.load_rx = Some(rx);
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
        if let Some(rx) = &self.load_rx {
            let mut done = None;
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Log(s) => self.log.push(s),
                    Msg::Loaded(r) => done = Some(*r),
                    _ => {}
                }
            }
            if let Some(r) = done {
                self.load_rx = None;
                match r {
                    Ok(scene) => {
                        self.renderer = Some(scene.renderer(&self.gpu, self.look.nearest));
                        self.scene = Some(Arc::new(scene));
                        self.sky_loaded = None;
                        self.last_key.clear();
                    }
                    Err(e) => self.log.push(format!("error: {e}")),
                }
            }
        }
        self.poll_job();
    }

    fn rebuild_renderer(&mut self) {
        if let Some(s) = &self.scene {
            self.renderer = Some(s.renderer(&self.gpu, self.look.nearest));
            self.sky_loaded = None;
            self.last_key.clear();
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            match self.map_name() {
                Some(n) => {
                    ui.strong(n);
                    if let Some(d) = self.current.as_ref().and_then(|p| p.parent()) {
                        ui.weak(format!("({})", d.display()));
                    }
                }
                None => {
                    ui.weak("no map");
                }
            }
            if ui.button("Open .bsp").clicked() {
                self.open_bsp(&ctx);
            }
            let can_reload = self.current.is_some() && self.load_rx.is_none();
            if ui.add_enabled(can_reload, egui::Button::new("Reload")).clicked() {
                self.start_load(&ctx);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(j) = &self.job {
                    if ui.button("Cancel").clicked() {
                        j.cancel();
                    }
                    let text = if j.cancelling() {
                        "cancelling...".to_string()
                    } else {
                        format!("{} {:.0}%", j.label, j.progress * 100.0)
                    };
                    ui.add(egui::ProgressBar::new(j.progress).desired_width(260.0).text(text));
                } else {
                    if let Some(o) = &self.last_out {
                        if ui.button("Open folder").clicked() {
                            let _ = std::process::Command::new("explorer").arg(o).spawn();
                        }
                    }
                    if !self.status.is_empty() {
                        ui.label(&self.status);
                    }
                }
                if self.load_rx.is_some() {
                    ui.label("loading...");
                    ui.spinner();
                }
            });
        });
    }

    fn open_bsp(&mut self, ctx: &egui::Context) {
        if let Some(f) = rfd::FileDialog::new().add_filter("BSP", &["bsp"]).pick_file() {
            self.current = Some(f);
            self.start_load(ctx);
        }
    }

    fn show_health(&mut self, out: &Path) {
        let Ok(rd) = std::fs::read_dir(out) else { return };
        let mut txt: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().contains("_health") && n.to_string_lossy().ends_with(".txt")))
            .collect();
        txt.sort();
        if let Some(p) = txt.first() {
            if let Ok(t) = std::fs::read_to_string(p) {
                let title = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                self.health_text = Some((title, t));
            }
        }
    }

    fn health_window(&mut self, ctx: &egui::Context) {
        let Some((title, text)) = &self.health_text else { return };
        let mut open = true;
        egui::Window::new(title.as_str()).open(&mut open).default_size([640.0, 560.0]).show(ctx, |ui| {
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text.as_str()).monospace()).extend());
            });
        });
        if !open {
            self.health_text = None;
        }
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        if self.log_open {
            egui::Panel::bottom("log").resizable(true).default_size(110.0).show(ui, |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    for l in &self.log {
                        ui.monospace(l);
                    }
                });
            });
        }
        egui::Panel::bottom("logbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.toggle_value(&mut self.log_open, "Log");
                if !self.log_open {
                    if let Some(l) = self.log.last() {
                        ui.add(egui::Label::new(egui::RichText::new(l).monospace().weak()).truncate());
                    }
                }
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        self.log_panel(ui);
        egui::Panel::left("side").resizable(true).default_size(340.0).show(ui, |ui| self.side(ui));
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
        self.health_window(&ui.ctx().clone());
        if !ui.input(|i| i.pointer.any_down()) {
            self.save_cfg();
        }
    }

    fn on_exit(&mut self) {
        if let Some(j) = &self.job {
            j.cancel();
        }
        self.save_cfg();
    }
}
