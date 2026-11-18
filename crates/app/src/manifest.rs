use crate::{Studio, confirm_discard};
use audio_engine::assets::case_path;
use eframe::egui;
use soundscape_core::{Script, keyvalues::Entry};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

const NAME: &str = "soundscapes_manifest.txt";

#[derive(Default)]
pub(super) struct Manifests {
    pub(super) docs: Vec<Script>,
    selected: usize,
    text: bool,
    registration: Option<PathBuf>,
}

fn root(path: &Path) -> Option<&Path> {
    path.parent()?.parent()
}

fn sibling(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|parent| {
            parent
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("scripts"))
        })
        .map(|parent| parent.join(NAME))
}

fn key(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn entries(doc: &Script) -> Vec<(&Entry, String)> {
    doc.tree
        .entries
        .first()
        .into_iter()
        .flat_map(|block| block.named(doc, "file"))
        .filter_map(|entry| Some((entry, entry.text(doc)?.into_owned())))
        .collect()
}

fn resolve(doc: &Script, reference: &str) -> Option<PathBuf> {
    let path = PathBuf::from(reference.replace('\\', "/"));
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    case_path(&root(&doc.path)?.join(path))
        .ok()
        .filter(|p| p.is_file())
}

fn reference(manifest: &Path, script: &Path) -> Option<String> {
    let base = root(manifest)?;
    let path = script.strip_prefix(base).ok()?;
    if script == manifest
        || path.components().next()?.as_os_str() != "scripts"
            && !path
                .components()
                .next()?
                .as_os_str()
                .eq_ignore_ascii_case("scripts")
    {
        return None;
    }
    Some(path.to_str()?.replace('\\', "/"))
}

impl Manifests {
    pub(super) fn open(&mut self, path: &Path) -> std::io::Result<()> {
        let doc = Script::open(path)?;
        if let Some(index) = self.docs.iter().position(|open| open.path == doc.path) {
            self.selected = index;
        } else {
            self.selected = self.docs.len();
            self.docs.push(doc);
        }
        Ok(())
    }

    pub(super) fn discover(&mut self, script: &Path) -> std::io::Result<()> {
        if let Some(path) = sibling(script).and_then(|path| case_path(&path).ok()) {
            self.open(&path)?;
        }
        Ok(())
    }

    pub(super) fn association<'a>(&'a self, script: &'a Path) -> Option<&'a Path> {
        self.associations(script).next()
    }

    pub(super) fn associations<'a>(&'a self, script: &'a Path) -> impl Iterator<Item = &'a Path> {
        self.docs
            .iter()
            .filter(move |doc| {
                reference(&doc.path, script).is_some_and(|path| {
                    entries(doc)
                        .iter()
                        .any(|(_, entry)| key(entry) == key(&path))
                })
            })
            .map(|doc| doc.path.as_path())
    }

    pub(super) fn offer_registration(
        &mut self,
        source: &crate::source::SourceEditor,
        script: &Path,
    ) -> std::io::Result<()> {
        self.discover(script)?;
        if self.association(script).is_some() {
            return Ok(());
        }
        let mut matching = self
            .docs
            .iter()
            .enumerate()
            .filter(|(_, doc)| reference(&doc.path, script).is_some());
        let Some((index, _)) = matching.next() else {
            return Ok(());
        };
        if matching.next().is_some() {
            self.registration = Some(script.to_owned());
            return Ok(());
        }
        self.register(source, script, index)
    }

    fn register(
        &mut self,
        source: &crate::source::SourceEditor,
        script: &Path,
        index: usize,
    ) -> std::io::Result<()> {
        let doc = &mut self.docs[index];
        let Some(reference) = reference(&doc.path, script) else {
            return Ok(());
        };
        if source.pending(doc) {
            return Err(std::io::Error::other(
                "Apply or discard the manifest draft before registering the new script",
            ));
        }
        if rfd::MessageDialog::new()
            .set_title("Register soundscape script")
            .set_description(format!(
                "Add {reference} to {}? Review and save it in Manifest Editor.",
                doc.path.display()
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
        {
            doc.register_manifest_script(&reference)?;
            self.selected = index;
        }
        Ok(())
    }
}

impl Studio {
    pub(super) fn registration_dialog(&mut self, ui: &mut egui::Ui) {
        let Some(script) = self.manifests.registration.clone() else {
            return;
        };
        let mut open = true;
        let mut selected = None;
        egui::Window::new("Choose a manifest")
            .collapsible(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(script.display().to_string());
                for (index, doc) in self.manifests.docs.iter().enumerate() {
                    if reference(&doc.path, &script).is_some()
                        && ui.button(doc.path.display().to_string()).clicked()
                    {
                        selected = Some(index);
                    }
                }
            });
        if let Some(index) = selected {
            self.manifests.registration = None;
            if let Err(error) = self.manifests.register(&self.source, &script, index) {
                self.errors.push(error.to_string());
            }
        } else if !open {
            self.manifests.registration = None;
        }
    }

    pub(super) fn manifest_panel(&mut self, ui: &mut egui::Ui) {
        let mut open_script = None;
        let mut error = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Manifest Editor");
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open manifest…").clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_file()
                {
                    error = self.manifests.open(&path).err();
                }
                if ui.button("Create manifest…").clicked()
                    && let Some(path) = rfd::FileDialog::new().set_file_name(NAME).save_file()
                {
                    error = Script::create_manifest(&path)
                        .and_then(|()| self.manifests.open(&path))
                        .err();
                }
            });
            if self.manifests.docs.is_empty() {
                ui.weak("Open or create a manifest in the game's scripts folder.");
                return;
            }
            egui::ComboBox::from_id_salt("manifest")
                .selected_text(
                    self.manifests.docs[self.manifests.selected]
                        .path
                        .display()
                        .to_string(),
                )
                .show_ui(ui, |ui| {
                    for (index, doc) in self.manifests.docs.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.manifests.selected,
                            index,
                            doc.path.display().to_string(),
                        );
                    }
                });
            let doc = &mut self.manifests.docs[self.manifests.selected];
            let pending = self.source.pending(doc);
            if pending {
                self.manifests.text = true;
            }
            let mut close = false;
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !pending,
                        egui::Button::new("Entries").selected(!self.manifests.text),
                    )
                    .clicked()
                {
                    self.manifests.text = false;
                }
                ui.selectable_value(&mut self.manifests.text, true, "Source");
                for (label, redo) in [("Undo", false), ("Redo", true)] {
                    if ui
                        .add_enabled(
                            !pending && if redo { doc.can_redo() } else { doc.can_undo() },
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        error = doc.history(redo).err();
                    }
                }
                if ui
                    .add_enabled(
                        !pending && doc.modified(),
                        egui::Button::new("Save manifest"),
                    )
                    .clicked()
                {
                    error = doc.save(&doc.path.clone()).err();
                }
                if ui.button("Close manifest").clicked()
                    && (!(pending || doc.modified()) || confirm_discard())
                {
                    self.source.discard(doc);
                    close = true;
                }
            });
            if close {
                self.manifests.docs.remove(self.manifests.selected);
                self.manifests.selected = self
                    .manifests
                    .selected
                    .min(self.manifests.docs.len().saturating_sub(1));
                return;
            }
            if self.manifests.text {
                self.source.show(ui, doc);
                return;
            }
            if doc.tree.error.is_some() {
                ui.colored_label(
                    egui::Color32::GOLD,
                    "Invalid KeyValues. Correct it in Source.",
                );
                return;
            }
            let mut action = None;
            let mut add = None;
            let list = entries(doc);
            let registered: HashSet<_> = list.iter().map(|(_, entry)| key(entry)).collect();
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Load order");
                let mut seen = HashSet::new();
                for (index, (entry, path)) in list.iter().enumerate() {
                    let found = resolve(doc, path);
                    let duplicate = !seen.insert(key(path));
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{}. {path}", index + 1));
                        if duplicate {
                            ui.colored_label(egui::Color32::GOLD, "Duplicate");
                        }
                        if let Some(path) = found {
                            if ui.small_button("Open").clicked() {
                                open_script = Some(path);
                            }
                        } else {
                            ui.colored_label(egui::Color32::GOLD, "Missing locally");
                        }
                        if index > 0 && ui.small_button("Up").clicked() {
                            action = Some((list[index - 1].0.span(), Some(entry.span())));
                        }
                        if index + 1 < list.len() && ui.small_button("Down").clicked() {
                            action = Some((entry.span(), Some(list[index + 1].0.span())));
                        }
                        if ui.small_button("Remove").clicked() {
                            action = Some((entry.span(), None));
                        }
                    });
                }
                ui.separator();
                ui.heading("Unregistered scripts");
                // This small directory is read on demand so browsing the manifest needs no scan per frame.
                ui.menu_button("Add script…", |ui| {
                    if let Some(parent) = doc.path.parent()
                        && let Ok(files) = std::fs::read_dir(parent)
                    {
                        let mut available: Vec<_> = files
                            .filter_map(Result::ok)
                            .map(|file| file.path())
                            .filter(|path| {
                                let name = path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_ascii_lowercase();
                                (name.starts_with("soundscapes_") || name == "soundscapes.txt")
                                    && name.ends_with(".txt")
                            })
                            .filter_map(|path| reference(&doc.path, &path))
                            .filter(|path| !registered.contains(&key(path)))
                            .collect();
                        available.sort();
                        for path in available {
                            if ui.button(&path).clicked() {
                                add = Some(path);
                                ui.close();
                            }
                        }
                    }
                    if ui.button("Browse…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .set_directory(doc.path.parent().unwrap_or(Path::new(".")))
                            .pick_file()
                    {
                        add = path
                            .canonicalize()
                            .ok()
                            .and_then(|path| reference(&doc.path, &path));
                        if add.is_none() {
                            error = Some(std::io::Error::other(
                                "Select a script under this game's scripts directory",
                            ));
                        }
                        ui.close();
                    }
                });
            });
            if let Some(path) = add {
                error = doc.register_manifest_script(&path).err();
            } else if let Some((a, b)) = action {
                error = match b {
                    Some(b) => doc.swap_entries(a, b),
                    None => doc.remove_entry(a),
                }
                .err();
            }
        });
        if let Some(error) = error {
            self.errors.push(error.to_string());
        }
        if let Some(path) = open_script {
            self.open([path]);
            self.show_manifests = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_and_saves_registration_in_game_relative_order() {
        let game = std::env::temp_dir().join(format!("sss-manifest-{}", std::process::id()));
        let scripts = game.join("scripts");
        let path = scripts.join(NAME);
        let new = scripts.join("soundscapes_new.txt");
        let original = b"soundscaples_manifest\n{\n\t// keep\n\tfile scripts/old.txt\n}\n";
        fs::create_dir_all(&scripts).unwrap();
        fs::write(&path, original).unwrap();
        let created = Script::create(&new).unwrap();
        let mut manifests = Manifests::default();
        manifests.discover(&created.path).unwrap();
        assert!(manifests.association(&created.path).is_none());
        let doc = &mut manifests.docs[0];
        let name = reference(&doc.path, &created.path).unwrap();
        doc.register_manifest_script(&name).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        doc.save(&path).unwrap();
        assert_eq!(
            entries(doc)
                .into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>(),
            ["scripts/old.txt", "scripts/soundscapes_new.txt"]
        );
        assert!(doc.source_text().contains("\t// keep\n"));
        assert!(manifests.association(&created.path).is_some());
        fs::remove_file(new).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(scripts).unwrap();
        fs::remove_dir(game).unwrap();
    }
}
