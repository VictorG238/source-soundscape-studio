#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, RichText};
use soundscape_core::Script;
mod about;
mod content;
mod manifest;
mod preview;
mod sound_browser;
mod source;
mod updates;
mod visual;
use soundscape_core::keyvalues::Value;

use std::path::PathBuf;

const TITLE: &str = "Source Soundscape Studio";

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_icon(about::icon())
            .with_inner_size([1120.0, 700.0])
            .with_min_inner_size([900.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(|cc| {
            let mut theme = egui::Visuals::dark();
            theme.panel_fill = Color32::from_rgb(26, 32, 39);
            theme.selection.bg_fill = Color32::from_rgb(28, 87, 83);
            cc.egui_ctx.set_visuals(theme);
            let mut studio = Studio::default();
            studio.open(std::env::args_os().skip(1).map(PathBuf::from));
            Ok(Box::new(studio))
        }),
    )
}

#[derive(Default)]
struct Studio {
    sound_browser: sound_browser::SoundBrowser,
    scripts: Vec<Script>,
    selected: usize,
    filter: String,
    errors: Vec<String>,
    show_source: bool,
    show_manifests: bool,
    show_about: bool,
    updates: updates::Updates,
    manifests: manifest::Manifests,
    source: source::SourceEditor,
    scene: usize,
    layer: usize,
    rename: Option<(usize, usize, String)>,
    preview: preview::Preview,
    asset_status: std::collections::HashMap<String, Result<String, String>>,
}

fn confirm_discard() -> bool {
    rfd::MessageDialog::new()
        .set_title("Unsaved changes")
        .set_description("Discard unsaved changes and close?")
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

impl Studio {
    fn open(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("soundscapes_manifest.txt"))
            {
                match self.manifests.open(&path) {
                    Ok(()) => self.show_manifests = true,
                    Err(error) => self.errors.push(error.to_string()),
                }
                continue;
            }
            match Script::open(&path) {
                Ok(script) => {
                    if let Err(error) = self.manifests.discover(&script.path) {
                        self.errors.push(error.to_string());
                    }
                    self.scene = 0;
                    self.layer = 0;
                    self.rename = None;
                    if let Some(i) = self.scripts.iter().position(|s| s.path == script.path) {
                        self.selected = i;
                    } else {
                        self.selected = self.scripts.len();
                        self.scripts.push(script);
                    }
                }
                Err(error) => self.errors.push(format!("{} — {error}", path.display())),
            }
        }
    }
    fn browse(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("Soundscape scripts", &["txt"])
            .pick_files()
        {
            self.open(paths);
        }
    }
}

impl eframe::App for Studio {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let dropped = ui.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect::<Vec<_>>()
        });
        self.open(dropped);
        if ui.input(|i| i.viewport().close_requested())
            && (self
                .scripts
                .iter()
                .chain(&self.manifests.docs)
                .any(Script::modified)
                || self.source.has_drafts())
            && !confirm_discard()
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(TITLE)
                        .strong()
                        .color(Color32::from_rgb(127, 219, 202)),
                );
                ui.separator();
                ui.menu_button("File", |ui| {
                    if ui.button("Create script…").clicked() {
                        ui.close();
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("soundscapes_custom.txt")
                            .save_file()
                        {
                            match Script::create(&path) {
                                Ok(script) => {
                                    self.selected = self.scripts.len();
                                    if let Err(error) = self
                                        .manifests
                                        .offer_registration(&self.source, &script.path)
                                    {
                                        self.errors.push(error.to_string());
                                    }
                                    self.show_manifests = false;
                                    self.scripts.push(script);
                                    self.scene = 0;
                                    self.layer = 0;
                                    self.rename = None;
                                }
                                Err(error) => self.errors.push(error.to_string()),
                            }
                        }
                    }
                    if ui.button("Open scripts…").clicked() {
                        ui.close();
                        self.rename = None;
                        self.browse();
                    }
                    if ui.button("Open manifest…").clicked() {
                        ui.close();
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            match self.manifests.open(&path) {
                                Ok(()) => self.show_manifests = true,
                                Err(error) => self.errors.push(error.to_string()),
                            }
                        }
                    }
                    if !self.show_manifests
                        && let Some(script) = self.scripts.get_mut(self.selected)
                    {
                        let pending = self.source.pending(script);
                        if ui
                            .add_enabled(!pending, egui::Button::new("Add soundscape"))
                            .clicked()
                        {
                            self.rename = None;
                            match script.add_soundscape() {
                                Ok(()) => {
                                    self.scene = script.tree.entries.len() - 1;
                                    self.layer = 0;
                                }
                                Err(e) => self.errors.push(e.to_string()),
                            }
                            ui.close();
                        }
                        for (label, save_as) in [("Save", false), ("Save As…", true)] {
                            if ui
                                .add_enabled(!pending, egui::Button::new(label))
                                .on_disabled_hover_text("Apply or discard the Source draft first")
                                .clicked()
                            {
                                let path = if save_as {
                                    rfd::FileDialog::new()
                                        .add_filter("Soundscape script", &["txt"])
                                        .save_file()
                                } else {
                                    Some(script.path.clone())
                                };
                                if let Some(path) = path
                                    && let Err(e) = script.save(&path)
                                {
                                    self.errors.push(e.to_string());
                                }
                                ui.close();
                            }
                        }
                    }
                    if ui
                        .add_enabled(
                            !self.show_manifests && !self.scripts.is_empty(),
                            egui::Button::new("Close script"),
                        )
                        .clicked()
                    {
                        let script = &self.scripts[self.selected];
                        if (script.modified() || self.source.pending(script)) && !confirm_discard()
                        {
                            return;
                        }
                        self.rename = None;
                        self.scene = 0;
                        self.layer = 0;
                        self.source.discard(script);
                        self.scripts.remove(self.selected);
                        self.selected = self.selected.min(self.scripts.len().saturating_sub(1));
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About Source Soundscape Studio").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                    if !about::REPOSITORY.is_empty() {
                        ui.hyperlink_to("GitHub", about::REPOSITORY);
                    }
                    if ui.button("Check for updates").clicked() {
                        self.updates.check_now(ui.ctx());
                        ui.close();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    if ui.button("Settings…").clicked() {
                        self.updates.open_settings();
                        ui.close();
                    }
                });
                ui.selectable_value(&mut self.show_manifests, true, "Manifest Editor");
                if self.show_manifests && ui.button("Soundscapes").clicked() {
                    self.show_manifests = false;
                }
                if !self.show_manifests
                    && let Some(script) = self.scripts.get_mut(self.selected)
                {
                    for (label, redo) in [("Undo", false), ("Redo", true)] {
                        let available = if redo {
                            script.can_redo()
                        } else {
                            script.can_undo()
                        };
                        if ui
                            .add_enabled(
                                available && !self.source.pending(script),
                                egui::Button::new(label),
                            )
                            .clicked()
                        {
                            self.rename = None;
                            if let Err(e) = script.history(redo) {
                                self.errors.push(e.to_string());
                            }
                        }
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.source.pending(script),
                            egui::Button::new("Visual Editor").selected(!self.show_source),
                        )
                        .on_disabled_hover_text("Apply or discard the Source draft first")
                        .clicked()
                    {
                        self.show_source = false;
                    }
                    ui.selectable_value(&mut self.show_source, true, "Source");
                    if script.modified() {
                        ui.label("Modified");
                    }
                }
            });
        });
        if self.show_about && about::show(ui.ctx(), &mut self.show_about) {
            self.updates.check_now(ui.ctx());
        }
        self.updates.show(ui.ctx());
        self.registration_dialog(ui);
        if self.show_manifests {
            if !self.errors.is_empty() {
                egui::Panel::bottom("manifest-errors").show(ui, |ui| {
                    if ui.button("Dismiss errors").clicked() {
                        self.errors.clear();
                    }
                    for error in &self.errors {
                        ui.colored_label(Color32::GOLD, error);
                    }
                });
            }
            self.manifest_panel(ui);
        } else {
            self.workspace(ui);
        }
        self.show_sound_browser(ui.ctx());
    }
}
