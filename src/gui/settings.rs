use eframe::egui;

use super::App;
use crate::wallbang::cache::CacheLoc;

impl App {
    pub(super) fn cache_loc(&self) -> CacheLoc {
        CacheLoc::from_kv(&self.cache_kind, &self.cache_custom)
    }

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            ui.strong("Sim cache location");
            let shown = |l: CacheLoc| l.dir().map_or("(not available)".to_string(), |d| d.display().to_string());
            for (loc, label) in [(CacheLoc::Program, "Program folder"), (CacheLoc::AppData, "%APPDATA%")] {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.cache_kind, loc.key().to_string(), label);
                    ui.weak(shown(loc));
                });
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.cache_kind, CacheLoc::Custom(String::new()).key().to_string(), "Custom");
                let edit = ui.add(egui::TextEdit::singleline(&mut self.cache_custom).desired_width(280.0));
                if edit.gained_focus() || edit.changed() {
                    self.cache_kind = CacheLoc::Custom(String::new()).key().into();
                }
                if ui.button("Browse...").clicked()
                    && let Some(d) = rfd::FileDialog::new().pick_folder()
                {
                    self.cache_custom = d.to_string_lossy().into_owned();
                    self.cache_kind = CacheLoc::Custom(String::new()).key().into();
                }
            });
            match self.cache_loc().dir() {
                Some(d) => ui.weak(format!("wallbang sims are saved in {}", d.join("wallbang").display())),
                None => ui.colored_label(egui::Color32::from_rgb(230, 120, 80), "no folder set; sims will not be cached"),
            };
        });
        self.settings_open &= open;
    }
}
