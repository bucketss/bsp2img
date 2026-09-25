use eframe::egui;

use super::App;
use super::jobs::Job;
use crate::gltf::Lighting;
use crate::spin::Anim;

#[derive(Clone, Copy, PartialEq)]
pub enum Exporter {
    Iso,
    Spin,
    Peel,
    Slice,
    Overview,
    Timing,
    Health,
    Svg,
    Stl,
    Gltf,
}

const GROUPS: &[(&str, &[Exporter])] = &[
    ("Images", &[Exporter::Iso]),
    ("Animation", &[Exporter::Spin, Exporter::Peel, Exporter::Slice]),
    ("Counter-Strike", &[Exporter::Overview]),
    ("Analysis", &[Exporter::Timing, Exporter::Health]),
    ("3D and vector", &[Exporter::Svg, Exporter::Stl, Exporter::Gltf]),
];

impl Exporter {
    pub fn key(self) -> &'static str {
        match self {
            Exporter::Iso => "iso",
            Exporter::Spin => "spin",
            Exporter::Peel => "peel",
            Exporter::Slice => "slice",
            Exporter::Overview => "overview",
            Exporter::Timing => "timing",
            Exporter::Health => "health",
            Exporter::Svg => "svg",
            Exporter::Stl => "stl",
            Exporter::Gltf => "gltf",
        }
    }

    pub fn from_key(k: &str) -> Option<Exporter> {
        GROUPS.iter().flat_map(|(_, e)| e.iter()).copied().find(|e| e.key() == k)
    }

    fn label(self) -> &'static str {
        match self {
            Exporter::Iso => "Isometric",
            Exporter::Spin => "Spin",
            Exporter::Peel => "Roof peel",
            Exporter::Slice => "Slice",
            Exporter::Overview => "Overview",
            Exporter::Timing => "Rush timings",
            Exporter::Health => "Health report",
            Exporter::Svg => "SVG callouts",
            Exporter::Stl => "STL diorama",
            Exporter::Gltf => "glTF",
        }
    }
}

impl App {
    pub(super) fn yaws(&self) -> Vec<f64> {
        let y: Vec<f64> = self.yaws_text.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        if y.is_empty() { vec![self.yaw] } else { y }
    }

    fn job_for(&self, e: Exporter) -> Job {
        match e {
            Exporter::Iso => {
                let mut o = self.iso.clone();
                o.yaws = self.yaws();
                o.look = self.look.clone();
                o.framing.camera = self.cam_export.then_some(self.cam);
                Job::Iso(o)
            }
            Exporter::Spin | Exporter::Peel | Exporter::Slice => {
                let mut o = self.spin.clone();
                o.pitch = self.iso.pitch;
                o.look = self.look.clone();
                o.framing.camera = self.cam_export.then_some(self.cam);
                o.kind = match e {
                    Exporter::Peel => Anim::Peel(self.peel.clone()),
                    Exporter::Slice => Anim::Slice(self.slice.clone()),
                    _ => Anim::Spin,
                };
                Job::Anim(o)
            }
            Exporter::Overview => {
                let mut o = self.ov.clone();
                o.cull = self.look.cull;
                Job::Overview(o)
            }
            Exporter::Timing => Job::Timing(self.timing.clone()),
            Exporter::Health => Job::Health(self.health.clone()),
            Exporter::Svg => Job::Svg(self.svg.clone()),
            Exporter::Stl => Job::Stl(self.stl.clone()),
            Exporter::Gltf => Job::Gltf(self.gltf.clone()),
        }
    }

    pub(super) fn tab_export(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Output");
            ui.add(egui::TextEdit::singleline(&mut self.out).desired_width(200.0));
            if ui.button("...").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.out = d.to_string_lossy().into_owned();
                }
            }
        });
        ui.separator();
        for (group, items) in GROUPS {
            ui.weak(*group);
            ui.indent(*group, |ui| {
                for &e in *items {
                    ui.selectable_value(&mut self.exporter, e, e.label());
                }
            });
        }
        ui.separator();
        ui.heading(self.exporter.label());
        match self.exporter {
            Exporter::Iso => self.form_iso(ui),
            Exporter::Spin => self.form_anim(ui, true),
            Exporter::Peel => self.form_peel(ui),
            Exporter::Slice => self.form_slice(ui),
            Exporter::Overview => self.form_overview(ui),
            Exporter::Timing => self.form_timing(ui),
            Exporter::Health => self.form_health(ui),
            Exporter::Svg => self.form_svg(ui),
            Exporter::Stl => self.form_stl(ui),
            Exporter::Gltf => self.form_gltf(ui),
        }
        ui.add_space(6.0);
        let ready = self.scene.is_some() && !self.busy();
        let text = format!("Export {}", self.exporter.label().to_lowercase());
        if ui.add_enabled(ready, egui::Button::new(text)).clicked() {
            let job = self.job_for(self.exporter);
            self.start_job(job, ui.ctx());
        }
        if self.busy() {
            ui.weak("an export is running");
        }
    }

    fn form_iso(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("yaws");
            ui.add(egui::TextEdit::singleline(&mut self.yaws_text).desired_width(140.0));
        });
        ui.add(egui::Slider::new(&mut self.iso.size, 512..=8192).text("size px"));
        ui.add(egui::Slider::new(&mut self.iso.ss, 1..=4).text("supersample"));
        ui.checkbox(&mut self.iso.grid, "Also write grid preview");
        ui.weak("Pitch is on the Camera tab, sky and background on the Look tab.");
        if self.cam_export {
            ui.weak("Using the free camera (Camera tab): one image, yaws ignored.");
        }
    }

    fn form_anim(&mut self, ui: &mut egui::Ui, turn: bool) {
        ui.add(egui::Slider::new(&mut self.spin.size, 256..=2048).text("size px"));
        ui.add(egui::Slider::new(&mut self.spin.ss, 1..=4).text("supersample"));
        if turn {
            ui.add(egui::Slider::new(&mut self.spin.seconds, 2.0..=60.0).text("seconds per turn"));
        }
        ui.add(egui::Slider::new(&mut self.spin.fps, 10.0..=60.0).text("fps"));
        ui.add(egui::DragValue::new(&mut self.spin.start).speed(1.0).prefix("start yaw "));
        if turn {
            ui.checkbox(&mut self.spin.ccw, "Turn the other way");
        }
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.spin.mp4, "MP4");
            ui.checkbox(&mut self.spin.gif, "GIF");
            ui.checkbox(&mut self.spin.apng, "APNG");
        });
        ui.weak("Pitch is on the Camera tab.");
        if self.cam_export {
            ui.weak("Using the free camera (Camera tab): orbits its target, start yaw ignored.");
        }
    }

    fn form_peel(&mut self, ui: &mut egui::Ui) {
        let max = self.scene.as_ref().map_or(8, |s| s.levels.len().saturating_sub(1).max(1));
        ui.add(egui::Slider::new(&mut self.peel.roofs, 0..=max).text("roofs (0 = all)"));
        ui.add(egui::Slider::new(&mut self.peel.seconds_per, 0.2..=6.0).text("seconds per roof"));
        ui.add(egui::Slider::new(&mut self.peel.hold, 0.0..=3.0).text("hold s"));
        ui.checkbox(&mut self.peel.reverse, "Reverse (roofs drop into place)");
        ui.checkbox(&mut self.peel.then_spin, "Then spin");
        self.form_anim(ui, self.peel.then_spin);
        ui.weak("Starts from the roof and height cuts on the Scene tab.");
    }

    fn form_slice(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.slice.slices, 0..=32).text("slices (0 = sweep)"));
        if self.slice.slices == 0 {
            ui.add(egui::Slider::new(&mut self.slice.seconds, 1.0..=30.0).text("sweep seconds"));
        }
        ui.add(egui::Slider::new(&mut self.slice.hold, 0.0..=3.0).text("hold s"));
        ui.checkbox(&mut self.slice.then_spin, "Then spin");
        self.form_anim(ui, self.slice.then_spin);
        ui.weak("Sliced walls show their hollow insides.");
    }

    fn form_overview(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.ov.margin, 0.0..=0.3).text("margin"));
        ui.add(egui::Slider::new(&mut self.ov.ss, 1..=4).text("supersample"));
        ui.checkbox(&mut self.ov.png, "Also write PNG");
        ui.checkbox(&mut self.ov.grid, "Also write grid preview");
    }

    fn form_timing(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.timing.speed, 100.0..=260.0).text("speed u/s"));
        ui.add(egui::Slider::new(&mut self.timing.interval, 1.0..=10.0).text("contour s"));
        ui.add(egui::Slider::new(&mut self.timing.size, 512..=4096).text("size px"));
        ui.weak("Uses the roof and height cuts on the Scene tab.");
    }

    fn form_health(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.health.cell, 4.0..=32.0).text("walk grid units"));
        ui.add(egui::Slider::new(&mut self.health.size, 300..=2048).text("thumbnail px"));
        ui.weak("Missing assets, VIS and lighting, engine limits, spawns, overview files and open areas.");
    }

    fn form_svg(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.svg.cell, 4.0..=32.0).text("walk grid units"));
        ui.add(egui::Slider::new(&mut self.svg.simplify, 0.0..=32.0).text("simplify units"));
        ui.add(egui::DragValue::new(&mut self.svg.scale).range(1.0..=100000.0).speed(10.0).prefix("scale 1:"));
        let mut pick = self.svg.planes.is_some();
        if ui.checkbox(&mut pick, "Pick split heights").changed() {
            self.svg.planes = pick.then(Vec::new);
        }
        match (&mut self.svg.planes, &self.scene) {
            (None, _) => {
                ui.add(egui::Slider::new(&mut self.svg.bands, 1..=6).text("max floor bands"));
            }
            (Some(planes), Some(scene)) => {
                for (lo, _, _) in scene.levels.iter().rev() {
                    let z = lo - 1.0;
                    let mut on = planes.contains(&z);
                    if ui.checkbox(&mut on, format!("split below z {lo:.0}")).changed() {
                        planes.retain(|&p| p != z);
                        if on {
                            planes.push(z);
                        }
                    }
                }
            }
            (Some(_), None) => {}
        }
        ui.weak("Uses the roof, height and XY cuts on the Scene tab.");
    }

    fn form_stl(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.stl.voxel, 2.0..=32.0).text("voxel units"));
        ui.add(egui::Slider::new(&mut self.stl.wall, 0.0..=128.0).text("wall units"));
        ui.add(egui::Slider::new(&mut self.stl.base, 0.0..=128.0).text("base units"));
        ui.add(egui::Slider::new(&mut self.stl.print_width, 50.0..=500.0).text("print width mm"));
        ui.checkbox(&mut self.stl.smooth, "Smooth surface");
        ui.weak("Uses the roof and height cuts on the Scene tab. Smaller voxels need much more memory.");
    }

    fn form_gltf(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("lighting");
            for l in Lighting::ALL {
                ui.selectable_value(&mut self.gltf.lighting, l, l.key());
            }
        });
        if self.gltf.lighting == Lighting::Baked {
            ui.add(egui::Slider::new(&mut self.gltf.texel, 0.5..=16.0).text("units per texel"));
        }
        ui.checkbox(&mut self.gltf.nearest, "Pixelated textures");
        ui.weak("Uses the roof, height, XY and hull cuts on the Scene tab.");
    }
}
