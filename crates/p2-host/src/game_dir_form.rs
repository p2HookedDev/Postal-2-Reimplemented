use std::path::Path;

use bevy::ecs::prelude::Resource;
use bevy_egui::egui;

use crate::install::{self, InstallInfo};

#[derive(Resource, Default)]
pub struct GameDirForm {
    pub input: String,
    pub error: Option<String>,
    pub info: Option<InstallInfo>,
}

impl GameDirForm {
    pub fn new(dir: &Path) -> Self {
        let mut form = Self::default();
        form.set(dir);
        form
    }

    pub fn set(&mut self, dir: &Path) {
        self.input = dir.display().to_string();
        self.check();
    }

    pub fn check(&mut self) {
        match install::validate(Path::new(self.input.trim())) {
            Ok(info) => (self.info, self.error) = (Some(info), None),
            Err(error) => (self.info, self.error) = (None, Some(error.to_string())),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add(egui::TextEdit::singleline(&mut self.input).desired_width(400.0)).changed() {
                self.check();
            }
            if ui.button("Browse").clicked()
                && let Some(dir) = rfd::FileDialog::new().set_title("POSTAL 2 install folder").pick_folder()
            {
                self.set(&dir);
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
    }
}
