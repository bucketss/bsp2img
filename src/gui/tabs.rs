use eframe::egui;

use super::{App, Tab};

impl App {
    pub(super) fn side(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (t, n) in Tab::ALL {
                ui.selectable_value(&mut self.tab, t, n);
            }
        });
        ui.separator();
        if self.tab == Tab::Map {
            tab_map(self, ui);
            return;
        }
        egui::ScrollArea::vertical().id_salt(self.tab.key()).auto_shrink([false, false]).show(ui, |ui| match self.tab {
            Tab::Map => {}
            Tab::Scene => tab_scene(self, ui),
            Tab::Look => tab_look(self, ui),
            Tab::Camera => tab_camera(self, ui),
            Tab::Export => self.tab_export(ui),
        });
    }
}

pub fn tab_map(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    ui.horizontal(|ui| {
        ui.label("Game");
        let r = ui.add(egui::TextEdit::singleline(&mut app.game).desired_width(200.0));
        if r.lost_focus() {
            app.refresh_maps();
        }
        if ui.button("...").clicked() {
            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                app.game = d.to_string_lossy().into_owned();
                app.refresh_maps();
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Filter");
        ui.add(egui::TextEdit::singleline(&mut app.filter).desired_width(170.0));
        if ui.button("Open .bsp").clicked() {
            app.open_bsp(&ctx);
        }
    });
    let filter = app.filter.to_lowercase();
    let mut pick = None;
    egui::ScrollArea::vertical().id_salt("maps").auto_shrink([false, false]).show(ui, |ui| {
        for (n, p) in app.maps.iter().filter(|(n, _)| n.to_lowercase().contains(&filter)) {
            let sel = app.current.as_ref() == Some(p);
            if ui.selectable_label(sel, n).clicked() {
                pick = Some(p.clone());
            }
        }
        if app.maps.is_empty() {
            ui.weak("set the game folder to list maps");
        }
    });
    if let Some(p) = pick {
        app.current = Some(p);
        app.start_load(&ctx);
    }
}

pub fn tab_scene(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    ui.heading("Crop");
    ui.checkbox(&mut app.load.auto_crop, "Auto crop (remove sealed rooms)");
    ui.horizontal(|ui| {
        ui.checkbox(&mut app.hull_on, "Hull crop");
        ui.radio_value(&mut app.hull_kind, 3, "crouch");
        ui.radio_value(&mut app.hull_kind, 1, "stand");
    });
    ui.add(egui::Slider::new(&mut app.load.hull_pad, 0.0..=256.0).text("hull pad"));
    egui::CollapsingHeader::new("Lighting").show(ui, |ui| {
        let l = &mut app.load.light;
        ui.add(egui::Slider::new(&mut l.brightness, 0.0..=3.0).text("brightness"));
        ui.add(egui::Slider::new(&mut l.scale, 0.25..=4.0).text("light scale"));
        ui.add(egui::Slider::new(&mut l.gamma, 1.0..=3.0).text("gamma"));
        ui.add(egui::Slider::new(&mut l.texgamma, 1.0..=3.0).text("texgamma"));
        ui.add(egui::Slider::new(&mut l.lightgamma, 1.0..=3.0).text("lightgamma"));
        ui.checkbox(&mut l.all_styles, "All light styles");
    });
    let changed = app.scene.as_ref().is_some_and(|s| s.opts != app.wanted_load());
    ui.horizontal(|ui| {
        let apply = ui.add_enabled(changed && app.load_rx.is_none(), egui::Button::new("Reload with these"));
        if apply.clicked() {
            app.start_load(&ctx);
        }
        if changed {
            ui.weak("needs reload");
        }
    });

    ui.separator();
    ui.heading("Cuts");
    let nlev = app.scene.as_ref().map(|s| s.levels.len()).unwrap_or(0);
    ui.add(egui::Slider::new(&mut app.cut.roofs, 0..=nlev.saturating_sub(1).max(1)).text("roofs removed"));
    if let Some(s) = &app.scene {
        let lv: Vec<String> = s.levels.iter().enumerate().map(|(i, l)| format!("{}:{:.0}", i + 1, l.0)).collect();
        ui.weak(format!("levels {}", lv.join("  ")));
    }
    for (i, label) in ["z min", "z max"].iter().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.z_on[i], *label);
            ui.add_enabled(app.z_on[i], egui::DragValue::new(&mut app.z_val[i]).speed(4.0));
        });
    }
    for (i, label) in ["x min", "x max", "y min", "y max"].iter().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.xy_on[i], *label);
            ui.add_enabled(app.xy_on[i], egui::DragValue::new(&mut app.xy_val[i]).speed(8.0));
        });
    }
    ui.weak("Top view: shift+drag draws the XY box");
    if ui.button("Clear cuts").clicked() {
        app.cut.roofs = 0;
        app.z_on = [false; 2];
        app.xy_on = [false; 4];
    }
}

pub fn tab_look(app: &mut App, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Background").default_open(true).show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut on = app.look.bg.is_some();
            let mut c = app.look.bg.unwrap_or(app.bg_last);
            ui.checkbox(&mut on, "Background");
            ui.color_edit_button_srgb(&mut c);
            app.bg_last = c;
            app.look.bg = on.then_some(c);
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.look.sky, "Sky");
            ui.add(egui::TextEdit::singleline(&mut app.look.sky_name).hint_text("map default").desired_width(130.0));
        });
        ui.add_enabled(app.look.sky, egui::Slider::new(&mut app.look.sky_fov, 30.0..=150.0).text("sky fov"));
        ui.add_enabled(app.look.sky, egui::Slider::new(&mut app.look.sky_pitch, -45.0..=60.0).text("sky pitch"));
    });
    egui::CollapsingHeader::new("Textures").default_open(true).show(ui, |ui| {
        if ui.checkbox(&mut app.look.nearest, "Pixelated textures").changed() {
            app.rebuild_renderer();
        }
    });
    egui::CollapsingHeader::new("Effects").default_open(true).show(ui, |ui| {
        ui.checkbox(&mut app.look.cull, "Cutaway (back-face cull)");
    });
}

pub fn tab_camera(app: &mut App, ui: &mut egui::Ui) {
    ui.add(egui::Slider::new(&mut app.iso.pitch, 5.0..=89.0).text("pitch"));
    ui.horizontal(|ui| {
        if ui.button("true iso").clicked() {
            app.iso.pitch = 35.264;
        }
        if ui.button("2:1").clicked() {
            app.iso.pitch = 30.0;
        }
    });
    ui.horizontal(|ui| {
        ui.label("yaw");
        ui.add(egui::DragValue::new(&mut app.yaw).speed(1.0).range(0.0..=360.0));
        for y in [45.0, 135.0, 225.0, 315.0] {
            if ui.button(format!("{y:.0}")).clicked() {
                app.yaw = y;
            }
        }
    });
    if ui.button("Reset view").clicked() {
        app.zoom = 1.0;
        app.pan = glam::DVec3::ZERO;
    }
    ui.weak("Pitch applies to the preview, isometric and animation exports.");
}
