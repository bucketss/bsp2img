use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use glam::DVec3;

use super::App;
use crate::camera::{Camera, camera_basis, extents, top_down};
use crate::look::Tilt;
use crate::export::{RadarSource, overview_params, radar_params};
use crate::overview::{Overview, Radar};
use crate::grid::{Marker, entity_marks, nice_step};
use crate::overview;
use crate::post;
use crate::radar::{Official, Section};
use crate::render::{Cuts, NO_CLIP, Targets, View, wgpu};
use crate::scene::num;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Iso,
    Top,
    Overview,
    Free,
}

#[derive(Clone, Copy, PartialEq)]
pub enum SpawnShow {
    All,
    Comp,
    None,
}

impl SpawnShow {
    const ALL: [(SpawnShow, &'static str); 3] = [(SpawnShow::All, "All"), (SpawnShow::Comp, "Comp"), (SpawnShow::None, "None")];

    pub fn key(self) -> &'static str {
        SpawnShow::ALL.iter().find(|(s, _)| *s == self).map_or("All", |(_, n)| *n)
    }

    pub fn from_key(k: &str) -> Option<SpawnShow> {
        match k.trim() {
            "true" => Some(SpawnShow::All),
            "false" => Some(SpawnShow::None),
            k => SpawnShow::ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(k)).map(|(s, _)| *s),
        }
    }

    fn step(self, by: usize) -> SpawnShow {
        let i = SpawnShow::ALL.iter().position(|(s, _)| *s == self).unwrap_or(0);
        SpawnShow::ALL[(i + by) % SpawnShow::ALL.len()].0
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Overlay {
    Off,
    Over,
    Under,
}

impl Overlay {
    pub const ALL: [(Overlay, &'static str); 3] = [(Overlay::Off, "off"), (Overlay::Over, "over"), (Overlay::Under, "under")];

    pub fn key(self) -> &'static str {
        Overlay::ALL.iter().find(|(s, _)| *s == self).map_or("off", |(_, n)| *n)
    }

    pub fn from_key(k: &str) -> Option<Overlay> {
        Overlay::ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(k.trim())).map(|(s, _)| *s)
    }
}

#[derive(Clone)]
pub struct RadarView {
    pub frame: Radar,
    pub image: Option<Radar>,
    pub src: RadarSource,
    pub sec: Section,
    pub info: Option<Official>,
}

const FLY_SPEED: f64 = 0.25;
const FRAME_FILL: f64 = 0.92;

pub struct Pick {
    pub pos: Pos2,
    pub focus: bool,
}

pub fn free_input(cam: &mut Camera, ui: &egui::Ui, resp: &egui::Response, rect: Rect, span: f64) -> Option<Pick> {
    let d = resp.drag_delta();
    let (dx, dy) = (d.x as f64, d.y as f64);
    let upp = cam.height() / rect.height().max(1.0) as f64;
    if resp.dragged_by(egui::PointerButton::Primary) {
        cam.orbit(dx, dy);
    } else if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
        cam.pan(dx, dy, upp);
    }
    if resp.hovered() {
        let s = ui.input(|i| i.smooth_scroll_delta.y);
        if s != 0.0 {
            cam.dolly(s as f64);
        }
    }
    let held = ui.input(|i| i.pointer.secondary_down()) && (resp.hovered() || resp.dragged());
    if held {
        use egui::Key;
        let (dt, fast, k) = ui.input(|i| {
            let k = [Key::W, Key::S, Key::D, Key::A, Key::E, Key::Q].map(|k| i.key_down(k) as i32 as f64);
            (i.stable_dt.clamp(0.0, 0.1) as f64, i.modifiers.shift, k)
        });
        let (f, r, u) = (k[0] - k[1], k[2] - k[3], k[4] - k[5]);
        if f != 0.0 || r != 0.0 || u != 0.0 {
            let v = span * FLY_SPEED * dt * if fast { 4.0 } else { 1.0 };
            cam.fly(f * v, r * v, u * v);
            ui.ctx().request_repaint();
        }
    }
    let pos = resp.interact_pointer_pos()?;
    if resp.double_clicked() {
        return Some(Pick { pos, focus: false });
    }
    if resp.clicked() && ui.input(|i| i.modifiers.command) {
        return Some(Pick { pos, focus: true });
    }
    None
}

pub struct Preview {
    t: Targets,
    id: egui::TextureId,
    size: [u32; 2],
}

pub struct Frame {
    rect: Rect,
    view: View,
    upp: f64,
}

pub struct ViewCache {
    key: String,
    view: View,
    upp: f64,
    cuts: Cuts,
    clear: [f64; 4],
    ov: Option<Overview>,
    pub radar: Option<RadarView>,
}

impl App {
    pub(super) fn frame_free(&mut self) {
        let cuts = self.cuts();
        let Some(r) = &self.renderer else { return };
        let aspect = self.cam.aspect;
        self.cam.frame_points(&r.points_in(&cuts), aspect, FRAME_FILL);
    }

    fn pick(&mut self, p: Pick, ppp: f32) {
        let (Some(r), Some(pv), Some(f)) = (&self.renderer, &self.preview, &self.frame) else { return };
        let x = ((p.pos.x - f.rect.min.x) * ppp).max(0.0) as u32;
        let y = ((p.pos.y - f.rect.min.y) * ppp).max(0.0) as u32;
        let hit = r.read_depth(&pv.t, x, y).and_then(|d| r.unproject(&f.view, pv.size[0], pv.size[1], x as f64, y as f64, d));
        let Some((pt, z)) = hit else {
            self.status = "no geometry under the cursor".into();
            return;
        };
        if p.focus {
            self.look.focus_y = ((p.pos.y - f.rect.min.y) / f.rect.height()).clamp(0.0, 1.0) as f64;
            if f.view.persp.is_some() {
                self.look.focus_dist = z.round();
            }
            self.status = format!("focus {:.0} units", z);
        } else {
            self.cam.refocus(pt);
            self.status = format!("target {:.0} {:.0} {:.0}", pt.x, pt.y, pt.z);
        }
    }

    pub(super) fn sync_sky(&mut self) {
        let want = if self.look.sky {
            self.scene.as_ref().map(|s| s.sky_name(Some(self.look.sky_name.trim())))
        } else {
            None
        };
        if want == self.sky_loaded {
            if let Some(r) = &mut self.renderer {
                r.set_sky_angles(self.look.sky_fov, self.look.sky_pitch);
            }
            return;
        }
        let (Some(scene), Some(r)) = (&self.scene, &mut self.renderer) else { return };
        match &want {
            Some(n) => match scene.load_sky(n) {
                Some(f) => {
                    r.set_sky(Some(f), self.look.sky_fov, self.look.sky_pitch);
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

    pub(super) fn ensure_preview(&mut self, w: u32, h: u32) {
        let fx = post::needed(&self.look);
        if self.preview.as_ref().is_some_and(|p| p.size == [w, h] && p.t.has_post() == fx) {
            return;
        }
        let Some(r) = &self.renderer else { return };
        let t = match r.make_targets(w, h, fx) {
            Ok(t) => t,
            Err(e) => {
                self.log.push(format!("error: {e:#}"));
                return;
            }
        };
        let mut er = self.egui_rend.write();
        let id = match &self.preview {
            Some(p) => {
                er.update_egui_texture_from_wgpu_texture(&self.gpu.device, &t.color_view, wgpu::FilterMode::Linear, p.id);
                p.id
            }
            None => er.register_native_texture(&self.gpu.device, &t.color_view, wgpu::FilterMode::Linear),
        };
        drop(er);
        self.preview = Some(Preview { t, id, size: [w, h] });
        self.last_key.clear();
    }

    pub(super) fn is_cs2(&self) -> bool {
        self.scene.as_ref().is_some_and(|s| s.bsp.is_cs2())
    }

    fn radar_texture(&mut self, ctx: &egui::Context, sec: &str) -> Option<egui::TextureId> {
        let s = self.scene.clone()?;
        let key = format!("{:p}|{sec}", std::sync::Arc::as_ptr(&s));
        if self.radar_tex.as_ref().is_none_or(|(k, _)| *k != key) {
            let x = s.bsp.cs2.as_ref()?;
            let map = s.bsp.name().to_lowercase();
            let t = match crate::s2::radar::image(&x.fs, &map, sec, 2048) {
                Ok(Some((n, im))) => {
                    self.log.push(format!("official radar: {n} {}x{}", im.w, im.h));
                    let img = egui::ColorImage::from_rgba_unmultiplied([im.w, im.h], &im.rgba);
                    Some(ctx.load_texture("official-radar", img, egui::TextureOptions::LINEAR))
                }
                Ok(None) => {
                    self.log.push(format!("no official radar image for {map} section {sec}"));
                    None
                }
                Err(e) => {
                    self.log.push(format!("official radar: {e:#}"));
                    None
                }
            };
            self.radar_tex = Some((key, t));
        }
        self.radar_tex.as_ref()?.1.as_ref().map(|t| t.id())
    }

    fn radar_bar(&mut self, ui: &mut egui::Ui) {
        let (info, has_official) = match &self.scene {
            Some(s) => (self.view_cache.as_ref().and_then(|c| c.radar.as_ref()).and_then(|r| r.info.clone()), crate::export::official(&s.bsp).is_some()),
            None => (None, false),
        };
        ui.horizontal(|ui| {
            ui.label("Framing");
            let cur = if self.ov.from_txt.is_some() {
                RadarSource::Txt
            } else if self.ov.fit || !has_official {
                RadarSource::Fitted
            } else {
                RadarSource::Official
            };
            let off = ui.add_enabled(has_official, egui::Button::selectable(cur == RadarSource::Official, "Official"));
            if off.on_disabled_hover_text("this map has no resource/overviews txt").clicked() {
                (self.ov.fit, self.ov.from_txt) = (false, None);
            }
            if ui.selectable_label(cur == RadarSource::Fitted, "Fitted").clicked() {
                (self.ov.fit, self.ov.from_txt) = (true, None);
            }
            let txt = ui.selectable_label(cur == RadarSource::Txt, "From txt");
            if txt.clicked()
                && let Some(f) = rfd::FileDialog::new().add_filter("overview", &["txt"]).pick_file()
            {
                self.ov.from_txt = Some(f);
            }
            if let Some(f) = &self.ov.from_txt {
                txt.on_hover_text(f.display().to_string());
            }
            ui.separator();
            ui.label("Section");
            let secs: Vec<Section> = info.map(|i| i.sections).unwrap_or_else(|| vec![Section::all()]);
            if !secs.iter().any(|s| s.name.eq_ignore_ascii_case(&self.radar_sec)) {
                self.radar_sec = secs[0].name.clone();
            }
            egui::ComboBox::from_id_salt("radar_sec").selected_text(self.radar_sec.clone()).show_ui(ui, |ui| {
                for s in &secs {
                    let what = if s.frame.is_some() { "own framing".to_string() } else { s.range() };
                    ui.selectable_value(&mut self.radar_sec, s.name.clone(), format!("{}  ({what})", s.name));
                }
            });
            ui.separator();
            ui.label("Official radar");
            for (o, n) in Overlay::ALL {
                ui.selectable_value(&mut self.radar_overlay, o, n);
            }
            ui.add_enabled(self.radar_overlay != Overlay::Off, egui::Slider::new(&mut self.radar_opacity, 0.0..=1.0).text("opacity"));
            ui.checkbox(&mut self.radar_icons, "Icons").on_hover_text("rings: the txt's CTSpawn/TSpawn/bombA/bombB keys; dots and boxes: this map's spawns and bombsites in the section");
        });
    }

    pub(super) fn central(&mut self, ui: &mut egui::Ui) {
        let cs2 = self.is_cs2();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Iso, "Isometric");
            ui.selectable_value(&mut self.mode, Mode::Top, "Top");
            ui.selectable_value(&mut self.mode, Mode::Overview, "Overview");
            ui.selectable_value(&mut self.mode, Mode::Free, "Free");
            ui.separator();
            if self.mode == Mode::Iso {
                for y in [45.0, 135.0, 225.0, 315.0] {
                    if ui.button(format!("{y:.0}")).clicked() {
                        self.yaw = y;
                    }
                }
                ui.label(format!("yaw {:.0}", self.yaw.rem_euclid(360.0)));
            }
            if self.mode == Mode::Free {
                ui.radio_value(&mut self.cam.ortho, false, "Persp");
                ui.radio_value(&mut self.cam.ortho, true, "Ortho");
            }
            if ui.button("Reset view").clicked() {
                self.zoom = 1.0;
                self.pan = DVec3::ZERO;
                if self.mode == Mode::Free {
                    self.frame_free();
                }
            }
            if self.mode == Mode::Top {
                let b = ui
                    .button(format!("Spawns: {}", self.top_spawns.key()))
                    .on_hover_text("click: next, right-click: previous. Comp = the lowest-priority spawns each team fills first (CS2); a black dot marks them");
                if b.clicked() {
                    self.top_spawns = self.top_spawns.step(1);
                } else if b.secondary_clicked() {
                    self.top_spawns = self.top_spawns.step(2);
                }
                ui.checkbox(&mut self.top_grid, "Grid");
                ui.checkbox(&mut self.top_objectives, "Objectives");
            }
            ui.weak(match self.mode {
                Mode::Iso => "drag rotate, right-drag pan, wheel zoom",
                Mode::Top => "drag pan, shift+drag XY box, wheel zoom",
                Mode::Overview if cs2 => "exact radar framing (1024x1024)",
                Mode::Overview => "exact overview framing (1024x768)",
                Mode::Free => "drag orbit, right-drag pan, wheel dolly, double-click target, hold right + WASD/QE fly, ctrl+click focus",
            });
        });
        if self.mode == Mode::Overview && cs2 {
            self.radar_bar(ui);
        }
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
        if self.mode == Mode::Overview && cs2 {
            let s = avail.width().min(avail.height());
            rect = Rect::from_center_size(avail.center(), Vec2::splat(s));
        } else if self.mode == Mode::Overview {
            let s = (avail.width() / 4.0).min(avail.height() / 3.0);
            rect = Rect::from_center_size(avail.center(), Vec2::new(s * 4.0, s * 3.0));
        }
        let (wpx, hpx) = (((rect.width() * ppp) as u32).max(16), ((rect.height() * ppp) as u32).max(16));
        if wpx > self.gpu.max_dim || hpx > self.gpu.max_dim {
            return;
        }
        let want = matches!(self.mode, Mode::Iso | Mode::Free).then(|| self.explode_planes()).flatten();
        if let (Some(r), Some(s)) = (&mut self.renderer, &self.scene) {
            r.set_explode(&s.mesh, want.as_ref());
        }
        if self.mode == Mode::Free {
            self.cam.aspect = wpx as f64 / hpx as f64;
            if self.frame_cam {
                self.frame_cam = false;
                self.frame_free();
            }
        }

        self.handle_input(ui, &resp, rect);

        let time = if self.look.anim_textures {
            ui.ctx().request_repaint();
            (ui.input(|i| i.time) * 30.0).round() / 30.0
        } else {
            0.0
        };
        let vkey = format!(
            "{:?}{:?}{:?}{:?}{}{:?}{}{}{:?}{}{}{:?}{:?}",
            want,
            cuts,
            (self.mode as u8, self.yaw, self.zoom, self.pan.to_array()),
            (self.mode == Mode::Free).then_some(self.cam),
            self.iso.pitch,
            self.look,
            wpx,
            hpx,
            self.sky_loaded,
            self.ov.margin,
            self.renderer.as_ref().map(|r| r.points.len()).unwrap_or(0),
            self.renderer.as_ref().map(|r| std::sync::Arc::as_ptr(r.scene())),
            (self.ov.fit, &self.ov.from_txt, &self.radar_sec, self.radar_overlay as u8),
        );
        let key = format!("{vkey}{time}");
        self.ensure_preview(wpx, hpx);
        let Some(r) = &self.renderer else { return };
        if self.view_cache.as_ref().is_none_or(|c| c.key != vkey) {
            let mut ov = None;
            let mut radar = None;
            let (view, upp, cuts, clear) = match self.mode {
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
                        persp: None,
                    };
                    let bg = match self.look.bg {
                        Some(c) => c.map(|v| v as f64 / 255.0),
                        None => [0.094, 0.094, 0.1],
                    };
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
                        persp: None,
                    };
                    (v, upp, rc, [0.11, 0.11, 0.11, 1.0])
                }
                Mode::Overview if cs2 => {
                    let Some(s) = &self.scene else { return };
                    let (main, src, info) = match radar_params(r, &s.bsp, &cuts, &self.ov) {
                        Ok(x) => x,
                        Err(e) => {
                            self.status = format!("overview: {e:#}");
                            return;
                        }
                    };
                    let sec = info.as_ref().and_then(|i| i.section(&self.radar_sec)).cloned().unwrap_or_else(Section::all);
                    let frame = sec.frame.unwrap_or(main);
                    let v = overview::radar_view(&frame);
                    let upp = v.w / rect.width() as f64;
                    let clear = match (self.radar_overlay, self.look.bg) {
                        (Overlay::Under, _) | (_, None) => [0.0; 4],
                        (_, Some(c)) => [c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0, 1.0],
                    };
                    let rc = sec.cuts(&cuts);
                    let image = crate::export::official(&s.bsp).map(|o| o.section(&sec.name).map_or(o.frame, |x| o.frame_of(x)));
                    radar = Some(RadarView { frame, image, src, sec, info });
                    (v, upp, rc, clear)
                }
                Mode::Overview => {
                    let Ok(o) = overview_params(r, &cuts, &self.ov) else { return };
                    let v = overview::view(&o);
                    ov = Some(o);
                    let upp = v.w / rect.width() as f64;
                    (v, upp, cuts, [0.0, 1.0, 0.0, 1.0])
                }
                Mode::Free => {
                    let v = self.cam.view(wpx, hpx);
                    let upp = v.h / rect.height() as f64;
                    let bg = match self.look.bg {
                        Some(c) => c.map(|v| v as f64 / 255.0),
                        None => [0.094, 0.094, 0.1],
                    };
                    (v, upp, cuts, [bg[0], bg[1], bg[2], 1.0])
                }
            };
            self.view_cache = Some(ViewCache { key: vkey, view, upp, cuts, clear, ov, radar });
        }
        let c = self.view_cache.as_ref().unwrap();
        let (view, upp, rcuts, clear) = (c.view, c.upp, c.cuts, c.clear);
        if key != self.last_key {
            let p = self.preview.as_ref().unwrap();
            let mut enc = self.gpu.device.create_command_encoder(&Default::default());
            r.draw(&mut enc, &p.t, &view, &rcuts, &self.look, 1, time, clear);
            self.gpu.queue.submit([enc.finish()]);
            self.last_key = key;
        }
        let id = self.preview.as_ref().unwrap().id;
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        let rv = self.view_cache.as_ref().and_then(|c| c.radar.clone()).filter(|_| self.mode == Mode::Overview && cs2);
        let tex = match &rv {
            Some(rv) if self.radar_overlay != Overlay::Off => self.radar_texture(ui.ctx(), &rv.sec.name),
            _ => None,
        };
        let tint = Color32::from_white_alpha((self.radar_opacity.clamp(0.0, 1.0) * 255.0) as u8);
        let fr = Frame { rect, view, upp };
        let over = rv.as_ref().and_then(|v| v.image).map_or(rect, |i| Rect::from_two_pos(to_screen(&fr, i.pos_x, i.pos_y), to_screen(&fr, i.pos_x + i.size(), i.pos_y - i.size())));
        let clipped = painter.with_clip_rect(rect.intersect(avail));
        match (tex, self.radar_overlay) {
            (Some(t), Overlay::Under) => {
                clipped.image(t, over, uv, tint);
                painter.image(id, rect, uv, Color32::WHITE);
            }
            (Some(t), _) => {
                painter.image(id, rect, uv, Color32::WHITE);
                clipped.image(t, over, uv, tint);
            }
            (None, _) => {
                painter.image(id, rect, uv, Color32::WHITE);
            }
        }
        self.frame = Some(fr);
        if self.band_drag && self.look.tilt != Tilt::Off {
            let h = rect.height() as f64;
            let (fy, b) = (self.look.focus_y, self.look.band);
            let band = Rect::from_x_y_ranges(
                rect.x_range(),
                (rect.top() + ((fy - b / 2.0) * h) as f32).max(rect.top())..=(rect.top() + ((fy + b / 2.0) * h) as f32).min(rect.bottom()),
            );
            painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(255, 200, 0, 40));
            painter.rect_stroke(band, 0.0, Stroke::new(1.0, Color32::from_rgb(255, 200, 0)), StrokeKind::Inside);
        }
        match self.mode {
            Mode::Top => self.overlay_top(&painter, &resp, &cuts),
            Mode::Overview if cs2 => {
                if let Some(rv) = &rv {
                    self.overlay_radar(&painter, &resp, rv);
                }
            }
            Mode::Overview => {
                if let Some(c) = &self.view_cache {
                    if let Some(ov) = c.ov {
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
            Mode::Iso | Mode::Free => {}
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
            Mode::Free => {
                let span = self.map_span;
                if let Some(p) = free_input(&mut self.cam, ui, resp, rect, span) {
                    self.pick(p, ui.ctx().pixels_per_point());
                }
                return;
            }
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
        while self.top_grid && gx < x1 {
            let p = to_screen(f, gx, 0.0);
            let major = gx == 0.0 || gx % (step * 4.0) == 0.0;
            let c = if major { Color32::from_rgba_unmultiplied(255, 230, 0, 160) } else { Color32::from_white_alpha(50) };
            painter.line_segment([Pos2::new(p.x, rect.top()), Pos2::new(p.x, rect.bottom())], Stroke::new(1.0, c));
            painter.text(Pos2::new(p.x + 3.0, rect.top() + 3.0), Align2::LEFT_TOP, format!("x {gx:.0}"), font.clone(), Color32::WHITE);
            gx += step;
        }
        let mut gy = (y0 / step).ceil() * step;
        while self.top_grid && gy < y1 {
            let p = to_screen(f, 0.0, gy);
            let major = gy == 0.0 || gy % (step * 4.0) == 0.0;
            let c = if major { Color32::from_rgba_unmultiplied(255, 230, 0, 160) } else { Color32::from_white_alpha(50) };
            painter.line_segment([Pos2::new(rect.left(), p.y), Pos2::new(rect.right(), p.y)], Stroke::new(1.0, c));
            painter.text(Pos2::new(rect.left() + 3.0, p.y + 3.0), Align2::LEFT_TOP, format!("y {gy:.0}"), font.clone(), Color32::WHITE);
            gy += step;
        }
        if let Some(s) = &self.scene {
            let shown = |m: &Marker| match (m.spawn, self.top_spawns) {
                (false, _) => self.top_objectives,
                (true, SpawnShow::All) => true,
                (true, SpawnShow::Comp) => m.comp != Some(false),
                (true, SpawnShow::None) => false,
            };
            for m in entity_marks(&s.bsp).into_iter().filter(shown) {
                let c = Color32::from_rgb(m.color[0], m.color[1], m.color[2]);
                if let Some(b) = m.bbox {
                    let r = Rect::from_two_pos(to_screen(f, b[0], b[1]), to_screen(f, b[2], b[3]));
                    painter.rect_stroke(r, 0.0, Stroke::new(2.0, c), StrokeKind::Inside);
                    painter.text(r.center(), Align2::CENTER_CENTER, m.label, font.clone(), c);
                } else {
                    let p = to_screen(f, m.pos.x, m.pos.y);
                    painter.circle(p, 4.0, c, Stroke::new(1.0, Color32::BLACK));
                    if m.comp == Some(true) {
                        painter.circle_filled(p, 1.6, Color32::BLACK);
                    }
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

impl App {
    fn overlay_radar(&self, painter: &egui::Painter, resp: &egui::Response, rv: &RadarView) {
        let Some(f) = &self.frame else { return };
        let Some(s) = &self.scene else { return };
        let rect = f.rect;
        let font = FontId::monospace(13.0);
        let shadow = |p: Pos2, a: Align2, t: &str, c: Color32| {
            painter.text(p + Vec2::splat(1.0), a, t, font.clone(), Color32::BLACK);
            painter.text(p, a, t, font.clone(), c);
        };
        let fr = rv.frame;
        let sec = &rv.sec;
        let what = if sec.frame.is_some() { "own framing".to_string() } else { sec.range() };
        let label = format!(
            "pos_x {}  pos_y {}  scale {}  ({})  section: {} ({what})",
            num(fr.pos_x),
            num(fr.pos_y),
            num(fr.scale),
            rv.src.label(),
            sec.name
        );
        shadow(rect.left_top() + Vec2::new(6.0, 6.0), Align2::LEFT_TOP, &label, Color32::WHITE);
        if let Some(e) = s.bsp.cs2.as_ref().and_then(|x| x.radar_err.as_ref()) {
            shadow(rect.left_top() + Vec2::new(6.0, 24.0), Align2::LEFT_TOP, &format!("overview txt: {e}"), Color32::from_rgb(255, 120, 90));
        }
        if let Some(p) = resp.hover_pos().filter(|p| rect.contains(*p)) {
            let (wx, wy) = to_world(f, p);
            let (px, py) = fr.world_to_px(wx, wy, overview::RADAR as f64);
            shadow(rect.right_bottom() - Vec2::new(8.0, 8.0), Align2::RIGHT_BOTTOM, &format!("{wx:.0}, {wy:.0}  radar px {px:.0} {py:.0}"), Color32::WHITE);
        }
        if !self.radar_icons {
            return;
        }
        let small = FontId::monospace(11.0);
        let in_sec = |z: f64| sec.frame.is_some() || rv.info.as_ref().and_then(|i| i.section_for(z)).is_none_or(|x| x.name.eq_ignore_ascii_case(&sec.name));
        for m in entity_marks(&s.bsp).into_iter().filter(|m| (m.spawn || m.label == "BOMB") && in_sec(m.pos.z)) {
            let c = Color32::from_rgb(m.color[0], m.color[1], m.color[2]);
            match m.bbox {
                Some(b) => {
                    let r = Rect::from_two_pos(to_screen(f, b[0], b[1]), to_screen(f, b[2], b[3]));
                    painter.rect_stroke(r, 0.0, Stroke::new(2.0, c), StrokeKind::Inside);
                }
                None => {
                    painter.circle(to_screen(f, m.pos.x, m.pos.y), 3.0, c, Stroke::new(1.0, Color32::BLACK));
                }
            }
        }
        let Some(info) = &rv.info else { return };
        for (n, _) in &info.icons {
            let Some((x, y)) = info.icon_world(n) else { continue };
            let l = n.to_lowercase();
            let (c, t) = match l.as_str() {
                "ctspawn" => (crate::grid::CT, "CT".to_string()),
                "tspawn" => (crate::grid::T, "T".to_string()),
                _ if l.starts_with("bomb") => ([255, 140, 0], n[4..].to_uppercase()),
                _ if l.starts_with("hostage") => (crate::grid::HOSTAGE, format!("H{}", &n[7..])),
                _ => ([220, 220, 220], n.clone()),
            };
            let c = Color32::from_rgb(c[0], c[1], c[2]);
            let p = to_screen(f, x, y);
            painter.circle_stroke(p, 9.0, Stroke::new(3.0, Color32::BLACK));
            painter.circle_stroke(p, 9.0, Stroke::new(2.0, c));
            painter.text(p + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, t, small.clone(), c);
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, RawInput, TouchPhase};

    struct H {
        ctx: egui::Context,
        t: f64,
        cam: Camera,
        picks: Vec<Pick>,
    }

    impl H {
        fn new() -> H {
            H { ctx: egui::Context::default(), t: 0.0, cam: Camera::default(), picks: Vec::new() }
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.t += 1.0 / 60.0;
            let raw = RawInput {
                time: Some(self.t),
                predicted_dt: 1.0 / 60.0,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                events,
                focused: true,
                ..Default::default()
            };
            let (cam, picks) = (&mut self.cam, &mut self.picks);
            let _ = self.ctx.run_ui(raw, |ui| {
                let rect = ui.max_rect();
                let resp = ui.allocate_rect(rect, Sense::click_and_drag());
                if let Some(p) = free_input(cam, ui, &resp, rect, 4000.0) {
                    picks.push(p);
                }
            });
        }

        fn mv(&mut self, x: f32, y: f32) {
            self.frame(vec![Event::PointerMoved(Pos2::new(x, y))]);
        }

        fn button(&mut self, b: PointerButton, pressed: bool, m: Modifiers) {
            self.frame(vec![Event::PointerButton { pos: Pos2::new(400.0, 300.0), button: b, pressed, modifiers: m }]);
        }

        fn drag(&mut self, b: PointerButton) {
            self.mv(400.0, 300.0);
            self.button(b, true, Modifiers::NONE);
            for i in 1..=10 {
                self.mv(400.0 + i as f32 * 4.0, 300.0 + i as f32 * 2.0);
            }
            self.frame(vec![Event::PointerButton {
                pos: Pos2::new(440.0, 320.0),
                button: b,
                pressed: false,
                modifiers: Modifiers::NONE,
            }]);
        }
    }

    #[test]
    fn free_orbit_pan_dolly() {
        let mut h = H::new();
        h.drag(PointerButton::Primary);
        assert!(h.cam.yaw > 25.0 && h.cam.yaw < 40.0, "yaw {}", h.cam.yaw);
        assert!(h.cam.pitch > 35.264 + 3.0, "pitch {}", h.cam.pitch);
        let (yaw, t0) = (h.cam.yaw, h.cam.target);
        h.drag(PointerButton::Secondary);
        assert_eq!(h.cam.yaw, yaw);
        let moved = h.cam.target - t0;
        assert!(moved.dot(h.cam.basis().r) < -1.0, "pan {moved}");
        let d0 = h.cam.dist;
        h.mv(400.0, 300.0);
        h.frame(vec![Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, 120.0),
            phase: TouchPhase::Move,
            modifiers: Modifiers::NONE,
        }]);
        for _ in 0..30 {
            h.mv(400.0, 300.0);
        }
        assert!(h.cam.dist < d0 * 0.9, "dolly {} -> {}", d0, h.cam.dist);
    }

    #[test]
    fn free_fly() {
        let mut h = H::new();
        h.mv(400.0, 300.0);
        h.button(PointerButton::Secondary, true, Modifiers::NONE);
        let key = |pressed| Event::Key { key: Key::W, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
        h.frame(vec![key(true)]);
        let t0 = h.cam.target;
        for _ in 0..20 {
            h.frame(vec![]);
        }
        let fwd = (h.cam.target - t0).dot(h.cam.basis().f);
        assert!(fwd > 50.0 && fwd < 1000.0, "fly {fwd}");
        h.frame(vec![key(false)]);
        h.button(PointerButton::Secondary, false, Modifiers::NONE);
        let t1 = h.cam.target;
        h.frame(vec![key(true)]);
        h.frame(vec![]);
        assert_eq!(h.cam.target, t1);
    }

    #[test]
    fn free_picks() {
        let mut h = H::new();
        h.mv(400.0, 300.0);
        for _ in 0..2 {
            h.button(PointerButton::Primary, true, Modifiers::NONE);
            h.button(PointerButton::Primary, false, Modifiers::NONE);
        }
        assert!(h.picks.iter().any(|p| !p.focus), "double-click");
        h.picks.clear();
        for _ in 0..30 {
            h.mv(400.0, 300.0);
        }
        h.frame(vec![Event::ModifiersChanged(Modifiers::COMMAND)]);
        h.button(PointerButton::Primary, true, Modifiers::COMMAND);
        h.button(PointerButton::Primary, false, Modifiers::COMMAND);
        assert!(h.picks.iter().any(|p| p.focus), "ctrl+click");
    }
}
