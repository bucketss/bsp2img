use eframe::egui;

use super::App;
use super::jobs::Job;
use crate::demo;
use crate::gltf::Lighting;
use crate::kills::TeamSel;
use crate::paths::expand_demos;
use crate::poster::{Orient, PAPERS};
use crate::spin::Anim;
use crate::sun::{hhmm, parse_time};

#[derive(Clone, Copy, PartialEq)]
pub enum Exporter {
    Iso,
    Spin,
    Peel,
    Slice,
    Day,
    Overview,
    Timing,
    Kills,
    Health,
    Svg,
    Stl,
    Gltf,
    Poster,
}

const GROUPS: &[(&str, &[Exporter])] = &[
    ("Images", &[Exporter::Iso, Exporter::Poster]),
    ("Animation", &[Exporter::Spin, Exporter::Peel, Exporter::Slice, Exporter::Day]),
    ("Counter-Strike", &[Exporter::Overview]),
    ("Analysis", &[Exporter::Timing, Exporter::Kills, Exporter::Health]),
    ("3D and vector", &[Exporter::Svg, Exporter::Stl, Exporter::Gltf]),
];

impl Exporter {
    pub fn key(self) -> &'static str {
        match self {
            Exporter::Iso => "iso",
            Exporter::Spin => "spin",
            Exporter::Peel => "peel",
            Exporter::Slice => "slice",
            Exporter::Day => "day",
            Exporter::Overview => "overview",
            Exporter::Timing => "timing",
            Exporter::Kills => "kills",
            Exporter::Health => "health",
            Exporter::Svg => "svg",
            Exporter::Stl => "stl",
            Exporter::Gltf => "gltf",
            Exporter::Poster => "poster",
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
            Exporter::Day => "Day cycle",
            Exporter::Overview => "Overview",
            Exporter::Timing => "Rush timings",
            Exporter::Kills => "Kill heatmap",
            Exporter::Health => "Health report",
            Exporter::Svg => "SVG callouts",
            Exporter::Stl => "STL diorama",
            Exporter::Gltf => "glTF / OBJ",
            Exporter::Poster => "Poster",
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
            Exporter::Spin | Exporter::Peel | Exporter::Slice | Exporter::Day => {
                let mut o = self.spin.clone();
                o.pitch = self.iso.pitch;
                o.look = self.look.clone();
                o.framing.camera = self.cam_export.then_some(self.cam);
                o.kind = match e {
                    Exporter::Peel => Anim::Peel(self.peel.clone()),
                    Exporter::Slice => Anim::Slice(self.slice.clone()),
                    Exporter::Day => Anim::Day(self.day.clone()),
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
            Exporter::Kills => Job::Kills(self.kills.clone(), self.kill_list.iter().filter(|x| x.1).map(|x| x.0.clone()).collect()),
            Exporter::Health => Job::Health(self.health.clone()),
            Exporter::Svg => {
                let mut o = self.svg.clone();
                if let Some(e) = self.explode_planes() {
                    o.planes = Some(e.planes);
                }
                Job::Svg(o)
            }
            Exporter::Stl => Job::Stl(self.stl.clone()),
            Exporter::Gltf => Job::Gltf(self.gltf.clone()),
            Exporter::Poster => {
                let mut o = self.poster.clone();
                o.pitch = self.iso.pitch;
                o.look = self.look.clone();
                o.framing.camera = self.cam_export.then_some(self.cam);
                Job::Poster(o)
            }
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
            Exporter::Day => self.form_day(ui),
            Exporter::Overview => self.form_overview(ui),
            Exporter::Timing => self.form_timing(ui),
            Exporter::Kills => self.form_kills(ui),
            Exporter::Health => self.form_health(ui),
            Exporter::Svg => self.form_svg(ui),
            Exporter::Stl => self.form_stl(ui),
            Exporter::Gltf => self.form_gltf(ui),
            Exporter::Poster => self.form_poster(ui),
        }
        ui.add_space(6.0);
        let ready = self.scene.is_some() && !self.busy() && (self.exporter != Exporter::Kills || self.kill_list.iter().any(|x| x.1));
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

    fn form_day(&mut self, ui: &mut egui::Ui) {
        let d = &mut self.day;
        for (v, t) in [(&mut d.from, "from"), (&mut d.to, "to")] {
            ui.add(
                egui::Slider::new(v, 0.0..=24.0)
                    .text(t)
                    .custom_formatter(|x, _| hhmm(x))
                    .custom_parser(|s| parse_time(s).ok()),
            );
        }
        ui.checkbox(&mut d.turn, "Turn while the day passes");
        self.form_anim(ui, true);
        ui.weak("The clip lasts one turn. 00:00 to 24:00 loops. Sun, keep-lights and blend are on the Look tab; relighting is switched on if Baked.");
    }

    fn form_overview(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.ov.margin, 0.0..=0.3).text("margin"));
        ui.add(egui::Slider::new(&mut self.ov.ss, 1..=4).text("supersample"));
        ui.checkbox(&mut self.ov.png, "Also write PNG");
        ui.checkbox(&mut self.ov.grid, "Also write grid preview");
        ui.horizontal(|ui| {
            let mut on = self.ov.from_txt.is_some();
            if ui.checkbox(&mut on, "Reuse framing from .txt").changed() {
                self.ov.from_txt = if on { rfd::FileDialog::new().add_filter("overview", &["txt"]).pick_file() } else { None };
            }
            if let Some(f) = &self.ov.from_txt {
                ui.weak(f.file_name().unwrap_or_default().to_string_lossy());
            }
        });
    }

    fn form_timing(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.timing.speed, 100.0..=260.0).text("speed u/s"));
        ui.add(egui::Slider::new(&mut self.timing.cell, 4.0..=32.0).text("walk grid units"));
        ui.add(egui::Slider::new(&mut self.timing.interval, 1.0..=10.0).text("contour s"));
        ui.add(egui::Slider::new(&mut self.timing.size, 512..=4096).text("size px"));
        ui.weak("Uses the roof and height cuts on the Scene tab.");
    }

    fn kill_scan(&mut self) {
        let map = self.scene.as_ref().map(|s| s.bsp.name()).unwrap_or_default();
        let key = (self.kill_dir.clone(), map.clone());
        if key == self.kill_key {
            return;
        }
        self.kill_key = key;
        self.kill_list.clear();
        self.kill_others = 0;
        if self.kill_dir.trim().is_empty() || map.is_empty() {
            return;
        }
        for f in expand_demos(&[self.kill_dir.trim().to_string()]) {
            match demo::header(&f) {
                Ok(h) if h.map.eq_ignore_ascii_case(&map) => self.kill_list.push((f, true)),
                _ => self.kill_others += 1,
            }
        }
    }

    fn form_kills(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Demos");
            ui.add(egui::TextEdit::singleline(&mut self.kill_dir).desired_width(200.0).hint_text("folder of .dem files"));
            if ui.button("...").clicked()
                && let Some(d) = rfd::FileDialog::new().pick_folder()
            {
                self.kill_dir = d.to_string_lossy().into_owned();
            }
        });
        self.kill_scan();
        let on = self.kill_list.iter().filter(|x| x.1).count();
        ui.horizontal(|ui| {
            ui.weak(format!("{on} of {} demos of this map, {} others", self.kill_list.len(), self.kill_others));
            if ui.small_button("all").clicked() {
                self.kill_list.iter_mut().for_each(|x| x.1 = true);
            }
            if ui.small_button("none").clicked() {
                self.kill_list.iter_mut().for_each(|x| x.1 = false);
            }
            if ui.small_button("rescan").clicked() {
                self.kill_key = Default::default();
            }
        });
        if !self.kill_list.is_empty() {
            egui::ScrollArea::vertical().id_salt("kill_demos").max_height(150.0).show(ui, |ui| {
                for (p, on) in &mut self.kill_list {
                    ui.checkbox(on, p.file_name().unwrap_or_default().to_string_lossy());
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("weapons");
            if ui.add(egui::TextEdit::singleline(&mut self.kill_weapons).desired_width(140.0).hint_text("all, or awp,scout")).changed() {
                self.kills.weapons =
                    self.kill_weapons.split(',').map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()).collect();
            }
        });
        ui.horizontal(|ui| {
            ui.label("victims");
            for t in TeamSel::ALL {
                ui.selectable_value(&mut self.kills.team, t, t.key());
            }
        });
        ui.checkbox(&mut self.kills.headshots, "Headshots only");
        ui.checkbox(&mut self.kills.lines, "Killer-to-victim lines");
        ui.horizontal(|ui| {
            let mut on = self.kills.rounds.is_some();
            if ui.checkbox(&mut on, "Rounds").changed() {
                self.kills.rounds = on.then_some((1, 30));
            }
            if let Some((a, b)) = &mut self.kills.rounds {
                ui.add(egui::DragValue::new(a).range(1..=999));
                ui.label("to");
                ui.add(egui::DragValue::new(b).range(1..=999));
                if *b < *a {
                    *b = *a;
                }
            }
        });
        ui.add(egui::Slider::new(&mut self.kills.radius, 16.0..=384.0).text("radius units"));
        ui.checkbox(&mut self.kills.presence, "Presence map");
        if self.kills.presence {
            ui.add(egui::Slider::new(&mut self.kills.presence_every, 0.1..=5.0).text("sample every s"));
        }
        ui.add(egui::Slider::new(&mut self.kills.size, 512..=4096).text("size px"));
        ui.weak("Uses the roof, height and XY cuts on the Scene tab. Kills on floors under the visible one are left out.");
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
        if self.explode.on {
            ui.weak("Exploded floors are on (Scene tab): floors split at the same heights.");
        }
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
            ui.label("format");
            ui.selectable_value(&mut self.gltf.obj, false, "glb");
            ui.selectable_value(&mut self.gltf.obj, true, "obj");
        });
        if self.gltf.obj && self.gltf.lighting == Lighting::Separate {
            self.gltf.lighting = Lighting::Baked;
        }
        ui.horizontal(|ui| {
            ui.label("lighting");
            for l in Lighting::ALL {
                ui.add_enabled_ui(!(self.gltf.obj && l == Lighting::Separate), |ui| ui.selectable_value(&mut self.gltf.lighting, l, l.key()));
            }
        });
        if self.gltf.lighting == Lighting::Baked {
            ui.add(egui::Slider::new(&mut self.gltf.texel, 0.5..=16.0).text("units per texel"));
        }
        ui.checkbox(&mut self.gltf.nearest, "Pixelated textures");
        ui.weak("Uses the roof, height, XY and hull cuts on the Scene tab.");
    }

    fn form_poster(&mut self, ui: &mut egui::Ui) {
        let p = &mut self.poster;
        let mut custom = p.px.is_some();
        ui.checkbox(&mut custom, "Custom pixel size");
        if custom != p.px.is_some() {
            p.px = custom.then_some([4096, 4096]);
        }
        if let Some(px) = &mut p.px {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut px[0]).range(256..=65535).prefix("w "));
                ui.add(egui::DragValue::new(&mut px[1]).range(256..=65535).prefix("h "));
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                for (k, _, _) in PAPERS {
                    ui.selectable_value(&mut p.paper, k.to_string(), k.to_uppercase());
                }
            });
            ui.horizontal(|ui| {
                for o in Orient::ALL {
                    ui.selectable_value(&mut p.orient, o, o.key());
                }
            });
        }
        ui.add(egui::Slider::new(&mut p.dpi, 72.0..=600.0).text("dpi"));
        ui.add(egui::Slider::new(&mut p.ss, 1..=4).text("supersample"));
        ui.checkbox(&mut p.top, "Top-down");
        ui.add_enabled(!p.top, egui::DragValue::new(&mut p.yaw).speed(1.0).prefix("yaw "));
        ui.checkbox(&mut p.layout, "Title, border, legend and scale bar");
        if let Ok((w, h)) = p.page(1.5) {
            ui.weak(format!("about {w}x{h} px; rendered in tiles and streamed to disk"));
        }
        ui.weak("Pitch is on the Camera tab, effects on the Look tab.");
        if self.cam_export {
            ui.weak("Using the free camera (Camera tab).");
        }
    }
}
