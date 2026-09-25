use eframe::egui;

use super::preview::Mode;
use super::{App, Tab};
use crate::camera::{Camera, ISO_PITCH};
use crate::export::overview_params;
use crate::look::{BLUEPRINT_BG, Style, Tilt};
use crate::overview;

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
        ui.checkbox(&mut app.look.anim_textures, "Animated textures and water");
        if app.look.anim_textures && app.renderer.as_ref().is_some_and(|r| !r.animated()) {
            ui.weak("no +0..+9 sequences in this map; water still warps");
        }
    });
    egui::CollapsingHeader::new("Effects").default_open(true).show(ui, |ui| {
        let l = &mut app.look;
        ui.checkbox(&mut l.cull, "Cutaway (back-face cull)");
        ui.horizontal(|ui| {
            ui.label("Style");
            if ui.button("None").clicked() {
                l.clear_effects();
                if l.bg == Some(BLUEPRINT_BG) {
                    l.bg = None;
                }
            }
            if ui.button("Blueprint").clicked() {
                l.apply_style(Style::Blueprint);
            }
            if ui.button("Comic").clicked() {
                l.apply_style(Style::Comic);
            }
        });
        ui.checkbox(&mut l.ao, "Ambient occlusion");
        ui.add_enabled(l.ao, egui::Slider::new(&mut l.ao_strength, 0.0..=3.0).text("AO strength"));
        ui.add_enabled(l.ao, egui::Slider::new(&mut l.ao_radius, 8.0..=256.0).text("AO radius (units)"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut l.ink, "Ink outlines");
            ui.add_enabled_ui(l.ink, |ui| ui.color_edit_button_srgb(&mut l.ink_color).on_hover_text("ink colour"));
        });
        ui.add_enabled(l.ink, egui::Slider::new(&mut l.ink_width, 0.5..=6.0).text("ink width px"));
        ui.add(egui::Slider::new(&mut l.saturation, 0.0..=2.0).text("saturation"));
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut l.tint_amount, 0.0..=1.0).text("tint"));
            ui.color_edit_button_srgb(&mut l.tint);
        });
        ui.add(egui::Slider::new(&mut l.contrast, 0.5..=2.0).text("contrast"));
    });
    egui::CollapsingHeader::new("Tilt-shift").default_open(true).show(ui, |ui| {
        let l = &mut app.look;
        ui.horizontal(|ui| {
            ui.radio_value(&mut l.tilt, Tilt::Off, "Off");
            ui.radio_value(&mut l.tilt, Tilt::Shift, "Tilt-shift");
            ui.radio_value(&mut l.tilt, Tilt::Dof, "Depth of field");
        });
        let on = l.tilt != Tilt::Off;
        let a = ui.add_enabled(on, egui::Slider::new(&mut l.focus_y, 0.0..=1.0).text("focus y"));
        let b = ui.add_enabled(on, egui::Slider::new(&mut l.band, 0.0..=1.0).text("sharp band"));
        app.band_drag = a.dragged() || b.dragged();
        ui.add_enabled(on, egui::Slider::new(&mut l.blur, 1.0..=32.0).text("blur px"));
        ui.add_enabled_ui(l.tilt == Tilt::Dof, |ui| {
            ui.horizontal(|ui| {
                ui.label("focus distance");
                ui.add(egui::DragValue::new(&mut l.focus_dist).speed(8.0).range(0.0..=1e6));
                ui.weak("0 = camera target");
            });
        });
        if ui.button("Miniature").on_hover_text("tilt-shift, saturation +25%, contrast +10%").clicked() {
            l.miniature();
        }
        ui.weak("Depth of field needs the Free view in perspective; elsewhere it uses the band. Free view: ctrl+click sets the focus.");
    });
}

pub fn tab_camera(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("Isometric");
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
    ui.separator();
    free_camera(app, ui);
}

fn free_camera(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("Free camera");
    let before = app.cam;
    let c = &mut app.cam;
    ui.horizontal(|ui| {
        ui.radio_value(&mut c.ortho, false, "Perspective");
        ui.radio_value(&mut c.ortho, true, "Orthographic");
    });
    egui::Grid::new("cam").num_columns(2).show(ui, |ui| {
        ui.label("yaw");
        ui.add(egui::DragValue::new(&mut c.yaw).speed(1.0).range(-360.0..=720.0));
        ui.end_row();
        ui.label("pitch");
        ui.add(egui::DragValue::new(&mut c.pitch).speed(0.5).range(-90.0..=90.0));
        ui.end_row();
        ui.label("roll");
        ui.add(egui::DragValue::new(&mut c.roll).speed(0.5).range(-180.0..=180.0));
        ui.end_row();
        ui.label("distance");
        ui.add(egui::DragValue::new(&mut c.dist).speed(8.0).range(1.0..=1e6));
        ui.end_row();
        ui.label("fov");
        ui.add(egui::DragValue::new(&mut c.fov).speed(0.5).range(1.0..=170.0).suffix("°"));
        ui.end_row();
        ui.label("target");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut c.target.x).speed(8.0).prefix("x "));
            ui.add(egui::DragValue::new(&mut c.target.y).speed(8.0).prefix("y "));
            ui.add(egui::DragValue::new(&mut c.target.z).speed(8.0).prefix("z "));
        });
        ui.end_row();
    });
    let mut frame = false;
    ui.horizontal_wrapped(|ui| {
        for y in [45.0, 135.0, 225.0, 315.0] {
            if ui.button(format!("iso {y:.0}")).clicked() {
                (c.yaw, c.pitch, c.roll) = (y, ISO_PITCH, 0.0);
                frame = true;
            }
        }
        if ui.button("top").clicked() {
            (c.yaw, c.pitch, c.roll) = (90.0, 90.0, 0.0);
            frame = true;
        }
        if ui.button("frame map").clicked() {
            frame = true;
        }
    });
    let mut ov_preset = false;
    ui.horizontal(|ui| {
        ov_preset = ui.button("overview framing").clicked();
        if ui.button("Save camera...").clicked() {
            if let Some(f) = rfd::FileDialog::new().add_filter("camera", &["cam"]).set_file_name("view.cam").save_file() {
                match app.cam.save(&f) {
                    Ok(()) => app.log.push(format!("camera saved to {}", f.display())),
                    Err(e) => app.log.push(format!("error: {e:#}")),
                }
            }
        }
        if ui.button("Load camera...").clicked() {
            if let Some(f) = rfd::FileDialog::new().add_filter("camera", &["cam"]).pick_file() {
                match Camera::load(&f) {
                    Ok(c) => {
                        app.cam = c;
                        app.mode = Mode::Free;
                    }
                    Err(e) => app.log.push(format!("error: {e:#}")),
                }
            }
        }
    });
    if ov_preset {
        overview_camera(app);
    }
    if frame {
        app.frame_free();
    }
    if app.cam != before {
        app.mode = Mode::Free;
    }
    ui.checkbox(&mut app.cam_export, "Use this camera for exports");
    ui.weak("When on, isometric exports write one image from this camera, and animations orbit its target at its pitch and distance. Size sets the longest side; the shape follows the preview.");
}

fn overview_camera(app: &mut App) {
    let cuts = app.cuts();
    let Some(r) = &app.renderer else { return };
    let Ok(ov) = overview_params(r, &cuts, &app.ov) else { return };
    let (rr, _) = overview::axes(ov.rotated);
    let c = &mut app.cam;
    c.yaw = rr.x.atan2(-rr.y).to_degrees().rem_euclid(360.0);
    (c.pitch, c.roll, c.ortho) = (90.0, 0.0, true);
    c.target = glam::DVec3::from_array(ov.origin);
    c.set_height(6144.0 / ov.zoom);
    app.mode = Mode::Free;
}
