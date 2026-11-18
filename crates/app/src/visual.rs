use super::{Studio, Value, sound_browser::Pick};
use crate::preview::LayerDraft;
use audio_engine::mixer::Range as AudioRange;
use eframe::egui::{self, Color32, RichText, Ui};
use soundscape_core::{Script, keyvalues::Entry};
use std::{collections::HashMap, io, ops::Range};

const TEAL: Color32 = Color32::from_rgb(107, 215, 197);
const GOLD: Color32 = Color32::from_rgb(233, 184, 100);
const LILAC: Color32 = Color32::from_rgb(181, 160, 238);
const CARD: Color32 = Color32::from_rgb(35, 42, 51);

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Loop,
    Random,
    Nested,
}
impl Kind {
    const ALL: [Self; 3] = [Self::Loop, Self::Random, Self::Nested];
    fn key(self) -> &'static str {
        match self {
            Self::Loop => "playlooping",
            Self::Random => "playrandom",
            Self::Nested => "playsoundscape",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Loop => "Looping",
            Self::Random => "Random",
            Self::Nested => "Nested soundscape",
        }
    }
    fn icon(self, ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(26., 24.), egui::Sense::hover());
        let p = ui.painter();
        let pt = |x, y| rect.min + egui::vec2(x, y);
        let stroke = egui::Stroke::new(1.8, self.color());
        p.rect_filled(rect, 5., self.color().gamma_multiply(0.2));
        match self {
            Self::Loop => {
                p.circle_stroke(pt(13., 12.), 6., stroke);
                p.line_segment([pt(17., 5.), pt(19., 10.)], stroke);
                p.line_segment([pt(19., 10.), pt(14., 9.)], stroke);
            }
            Self::Random => {
                for (x, h) in [(5., 2.), (9., 5.), (13., 8.), (17., 3.), (21., 6.)] {
                    p.line_segment([pt(x, 12. - h), pt(x, 12. + h)], stroke);
                }
            }
            Self::Nested => {
                p.line_segment([pt(8., 7.), pt(8., 17.)], stroke);
                p.line_segment([pt(8., 17.), pt(18., 17.)], stroke);
                p.circle_filled(pt(8., 7.), 3., self.color());
                p.circle_filled(pt(18., 17.), 3., self.color());
            }
        }
    }
    fn color(self) -> Color32 {
        match self {
            Self::Loop => TEAL,
            Self::Random => GOLD,
            Self::Nested => LILAC,
        }
    }
    fn of(doc: &Script, e: &Entry) -> Option<Self> {
        Self::ALL.into_iter().find(|k| {
            matches!(e.value, Value::Block(_)) && e.name(doc).eq_ignore_ascii_case(k.key())
        })
    }
}

enum Edit {
    AddScene,
    CopyScene,
    Add(Kind),
    Remove(Range<usize>),
    Copy(Range<usize>),
    Move(Range<usize>, Range<usize>, usize),
    Set(&'static str, String),
    Go(usize, usize),
}

fn short(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
fn compact(v: f64) -> String {
    let raw = format!("{v:.5}");
    raw.trim_end_matches('0').trim_end_matches('.').to_owned()
}
fn summary(doc: &Script, e: &Entry, key: &str) -> String {
    let mut found = e.named(doc, key);
    match (found.next(), found.next()) {
        (None, None) => "Inherited".into(),
        (Some(p), None) => p
            .text(doc)
            .map(|t| {
                if matches!(key, "volume" | "pitch" | "time") {
                    t.split(',')
                        .map(|s| {
                            s.trim()
                                .parse::<f64>()
                                .ok()
                                .filter(|n| n.is_finite())
                                .map(compact)
                                .unwrap_or_else(|| s.to_owned())
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    t.into_owned()
                }
            })
            .unwrap_or_else(|| "See Source".into()),
        _ => "Multiple · see Source".into(),
    }
}
fn asset(
    ui: &mut Ui,
    path: &str,
    preview: &mut crate::preview::Preview,
    cache: &mut HashMap<String, Result<String, String>>,
) {
    let status = cache.entry(path.to_owned()).or_insert_with(|| {
        preview
            .resolver
            .describe(path)
            .map_err(|error| error.to_string())
    });
    let name = if path.is_empty() {
        "Unassigned"
    } else {
        short(path)
    };
    let text = RichText::new(name).color(if status.is_ok() { TEAL } else { GOLD });
    let detail = match status {
        Ok(location) | Err(location) => location.as_str(),
    };
    if ui
        .add_enabled(!path.is_empty(), egui::Button::new(text).frame(false))
        .on_hover_text(format!("{path}\n{detail}"))
        .clicked()
    {
        preview.audition(path.to_owned());
    }
}

impl Studio {
    pub(super) fn workspace(&mut self, ui: &mut Ui) {
        self.preview_panel(ui);
        let mut edit = None;
        egui::Panel::left("browser")
            .default_size(210.)
            .min_size(175.)
            .max_size(310.)
            .resizable(true)
            .show(ui, |ui| {
                ui.heading("Soundscapes");
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Search files or soundscapes"),
                );
                if ui
                    .add_enabled(
                        !self.show_source && !self.scripts.is_empty(),
                        egui::Button::new("+ Soundscape"),
                    )
                    .clicked()
                {
                    edit = Some(Edit::AddScene);
                }
                ui.separator();
                let search = self.filter.to_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut groups = std::collections::BTreeMap::<_, Vec<_>>::new();
                    for (di, doc) in self.scripts.iter().enumerate() {
                        groups
                            .entry(self.manifests.association(&doc.path))
                            .or_default()
                            .push((di, doc));
                    }
                    let mut show_script = |ui: &mut Ui, di, doc: &Script| {
                        let file = doc.path.file_name().unwrap_or_default().to_string_lossy();
                        let all = file.to_lowercase().contains(&search);
                        let matches = |e: &Entry| {
                            matches!(e.value, Value::Block(_))
                                && (all || e.name(doc).to_lowercase().contains(&search))
                        };
                        if !all && !doc.tree.entries.iter().any(matches) {
                            return;
                        }
                        let response = egui::CollapsingHeader::new(file.as_ref())
                            .id_salt((&doc.path, &search))
                            .default_open(true)
                            .show(ui, |ui| {
                                for (si, e) in doc
                                    .tree
                                    .entries
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, e)| matches(e))
                                {
                                    let name = e.name(doc);
                                    let row = ui.selectable_label(
                                        self.selected == di && self.scene == si,
                                        name.as_ref(),
                                    );
                                    if row.clicked() {
                                        self.selected = di;
                                        self.scene = si;
                                        self.layer = 0;
                                        self.rename = None;
                                    }
                                    if !self.show_source && !self.source.pending(doc) {
                                        row.context_menu(|ui| {
                                            for label in
                                                ["Rename…", "Duplicate", "Delete soundscape"]
                                            {
                                                if ui.button(label).clicked() {
                                                    self.selected = di;
                                                    self.scene = si;
                                                    self.layer = 0;
                                                    match label {
                                                        "Rename…" => {
                                                            self.rename =
                                                                Some((di, si, name.to_string()))
                                                        }
                                                        "Duplicate" => edit = Some(Edit::CopyScene),
                                                        _ => edit = Some(Edit::Remove(e.span())),
                                                    }
                                                    ui.close();
                                                }
                                            }
                                        });
                                    }
                                }
                            });
                        let associations = self
                            .manifests
                            .associations(&doc.path)
                            .map(|manifest| manifest.display().to_string())
                            .collect::<Vec<_>>();
                        let mut header = response.header_response;
                        if !associations.is_empty() {
                            header = header.on_hover_text(format!(
                                "Registered in\n{}",
                                associations.join("\n")
                            ));
                        }
                        if header.clicked() {
                            self.selected = di;
                            self.scene = 0;
                            self.layer = 0;
                            self.rename = None;
                        }
                    };
                    for (manifest, scripts) in groups {
                        let show_group = |ui: &mut Ui| {
                            for (di, doc) in scripts {
                                show_script(ui, di, doc);
                            }
                        };
                        if let Some(path) = manifest {
                            let label = path.file_name().unwrap_or_default().to_string_lossy();
                            let caption = path
                                .parent()
                                .and_then(std::path::Path::parent)
                                .and_then(std::path::Path::file_name)
                                .map_or_else(
                                    || label.to_string(),
                                    |game| format!("{} / {label}", game.to_string_lossy()),
                                );
                            egui::CollapsingHeader::new(caption)
                                .id_salt(path)
                                .default_open(true)
                                .show(ui, show_group)
                                .header_response
                                .on_hover_text(path.display().to_string());
                        } else {
                            show_group(ui);
                        }
                    }
                });
            });
        if let Some(doc) = self.scripts.get_mut(self.selected)
            && (self.show_source || self.source.pending(doc))
        {
            self.show_source = true;
            self.preview.clear_drafts();
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("Source");
                self.source.show(ui, doc);
            });
            return;
        }
        let Some(doc) = self.scripts.get(self.selected) else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(85.);
                    ui.heading("Open a soundscape script");
                    ui.label("Drop .txt files here to edit their layers.");
                    if ui.button("Open scripts…").clicked() {
                        self.browse();
                    }
                });
            });
            return;
        };
        if !doc
            .tree
            .entries
            .get(self.scene)
            .is_some_and(|e| matches!(e.value, Value::Block(_)))
        {
            self.scene = doc
                .tree
                .entries
                .iter()
                .position(|e| matches!(e.value, Value::Block(_)))
                .unwrap_or(0);
        }
        let scene = doc
            .tree
            .entries
            .get(self.scene)
            .filter(|e| matches!(e.value, Value::Block(_)));
        let layers: Vec<_> = scene
            .into_iter()
            .flat_map(Entry::children)
            .enumerate()
            .filter_map(|(i, e)| Kind::of(doc, e).map(|kind| (i, e, kind)))
            .collect();
        self.layer = self.layer.min(layers.len().saturating_sub(1));
        let mut pick = None;
        if !self.show_source {
            egui::Panel::right("inspector")
                .default_size(285.)
                .min_size(248.)
                .max_size(390.)
                .resizable(true)
                .show(ui, |ui| {
                    ui.heading("Inspector");
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("controls")
                        .show(ui, |ui| {
                            if let Some((_, e, kind)) = layers.get(self.layer) {
                                ui.push_id((&doc.path, self.scene, self.layer, kind.key()), |ui| {
                                    ui.colored_label(
                                        kind.color(),
                                        RichText::new(kind.label()).strong(),
                                    );
                                    let draft = inspector(
                                        ui,
                                        doc,
                                        e,
                                        *kind,
                                        &self.scripts,
                                        &mut edit,
                                        &mut pick,
                                    );
                                    if self.preview.set_draft(doc.revision(), self.layer, draft) {
                                        ui.ctx().request_repaint();
                                    }
                                });
                            } else {
                                ui.weak("Select or add a layer.");
                            }
                        });
                });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(error) = &doc.tree.error {
                let (line, col) = doc.location(error.offset);
                ui.colored_label(
                    GOLD,
                    format!("Line {line}, column {col} · {}", error.message),
                );
            }
            let Some(scene) = scene else {
                ui.label("Add a soundscape to start editing.");
                return;
            };
            ui.heading(scene.name(doc).as_ref());
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!("{} layers", layers.len()));
                ui.separator();
                ui.menu_button("+ Add layer", |ui| {
                    for k in Kind::ALL {
                        if ui.button(k.label()).clicked() {
                            edit = Some(Edit::Add(k));
                            ui.close();
                        }
                    }
                });
            });
            if scene.children().len() > layers.len()
                || doc
                    .tree
                    .entries
                    .iter()
                    .any(|e| matches!(e.value, Value::Text(_)))
            {
                ui.small("Additional KeyValues preserved in Source.");
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt((&doc.path, self.scene))
                .show(ui, |ui| {
                    for (n, (_, e, k)) in layers.iter().enumerate() {
                        let selected = n == self.layer;
                        let card = egui::Frame::new()
                            .fill(if selected {
                                Color32::from_rgb(37, 51, 58)
                            } else {
                                CARD
                            })
                            .stroke(egui::Stroke::new(
                                1.,
                                if selected {
                                    k.color()
                                } else {
                                    Color32::from_gray(60)
                                },
                            ))
                            .corner_radius(6)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    k.icon(ui);
                                    if ui
                                        .selectable_label(
                                            selected,
                                            RichText::new(k.label())
                                                .size(15.)
                                                .strong()
                                                .color(k.color()),
                                        )
                                        .on_hover_text("Inspect this layer")
                                        .clicked()
                                    {
                                        self.layer = n;
                                    }
                                    for (label, set) in [
                                        ("Mute", &mut self.preview.mute),
                                        ("Solo", &mut self.preview.solo),
                                    ] {
                                        if ui.selectable_label(set.contains(&n), label).clicked()
                                            && !set.remove(&n)
                                        {
                                            set.insert(n);
                                        }
                                    }
                                    if ui.small_button("Copy").clicked() {
                                        self.layer = n;
                                        edit = Some(Edit::Copy(e.span()));
                                    }
                                    if ui.small_button("Delete").clicked() {
                                        self.layer = n;
                                        edit = Some(Edit::Remove(e.span()));
                                    }
                                    if n > 0
                                        && ui.small_button("Up").on_hover_text("Move up").clicked()
                                    {
                                        edit = Some(Edit::Move(
                                            layers[n - 1].1.span(),
                                            e.span(),
                                            n - 1,
                                        ));
                                    }
                                    if n + 1 < layers.len()
                                        && ui
                                            .small_button("Down")
                                            .on_hover_text("Move down")
                                            .clicked()
                                    {
                                        edit = Some(Edit::Move(
                                            e.span(),
                                            layers[n + 1].1.span(),
                                            n + 1,
                                        ));
                                    }
                                });
                                match k {
                                    Kind::Loop => {
                                        for wave in e.named(doc, "wave") {
                                            if let Some(path) = wave.text(doc) {
                                                asset(
                                                    ui,
                                                    &path,
                                                    &mut self.preview,
                                                    &mut self.asset_status,
                                                );
                                            }
                                        }
                                    }
                                    Kind::Random => {
                                        for g in e.named(doc, "rndwave") {
                                            for wave in g.named(doc, "wave") {
                                                if let Some(path) = wave.text(doc) {
                                                    ui.horizontal_wrapped(|ui| {
                                                        asset(
                                                            ui,
                                                            &path,
                                                            &mut self.preview,
                                                            &mut self.asset_status,
                                                        );
                                                        if ui
                                                            .small_button("−")
                                                            .on_hover_text("Remove this variant")
                                                            .clicked()
                                                        {
                                                            self.layer = n;
                                                            edit = Some(Edit::Remove(wave.span()));
                                                        }
                                                    });
                                                }
                                            }
                                        }
                                        if ui.small_button("+ Sound…").clicked() {
                                            self.layer = n;
                                            pick = Some(Pick::Variant);
                                        }
                                        ui.weak(format!("Interval  {} s", summary(doc, e, "time")));
                                    }
                                    Kind::Nested => {
                                        let name = summary(doc, e, "name");
                                        let targets = references(&self.scripts, &name);
                                        ui.horizontal_wrapped(|ui| {
                                            ui.strong(&name);
                                            match targets.as_slice() {
                                                [(d, s)] => {
                                                    if ui.link("Open reference").clicked() {
                                                        edit = Some(Edit::Go(*d, *s));
                                                    }
                                                }
                                                [] => {
                                                    ui.colored_label(GOLD, "Unresolved");
                                                }
                                                _ => {
                                                    ui.colored_label(GOLD, "Ambiguous");
                                                }
                                            }
                                        });
                                    }
                                }
                                ui.horizontal_wrapped(|ui| {
                                    for (label, key) in [
                                        ("Volume", "volume"),
                                        ("Pitch", "pitch"),
                                        ("Position", "position"),
                                    ] {
                                        if key != "pitch" || *k != Kind::Nested {
                                            ui.label(
                                                RichText::new(format!(
                                                    "{label}  {}",
                                                    summary(doc, e, key)
                                                ))
                                                .size(12.)
                                                .color(Color32::from_gray(192)),
                                            );
                                        }
                                    }
                                });
                            });
                        if card.response.contains_pointer()
                            && ui.input(|i| i.pointer.primary_clicked())
                            && edit.is_none()
                            && pick.is_none()
                        {
                            self.layer = n;
                        }
                        ui.add_space(8.);
                    }
                });
        });
        self.preview.apply_bus();
        let path = layers.get(self.layer).map(|(idx, _, _)| [self.scene, *idx]);
        if let Some(pick) = pick
            && let Some(path) = path
        {
            self.sound_browser.open(doc, path, pick);
        }
        if let Some(edit) = edit {
            self.rename = None;
            let doc = &mut self.scripts[self.selected];
            let result = match edit {
                Edit::Go(d, s) => {
                    self.selected = d;
                    self.scene = s;
                    self.layer = 0;
                    Ok(())
                }
                Edit::AddScene => doc.add_soundscape().map(|()| {
                    self.scene = doc.tree.entries.len() - 1;
                    self.layer = 0;
                }),
                Edit::CopyScene => doc.copy_soundscape(self.scene).map(|()| {
                    self.scene += 1;
                    self.layer = 0;
                }),
                Edit::Add(k) => {
                    let key = doc.tree.entries[self.scene].key.clone();
                    doc.add_layer(&key, k.key())
                        .map(|()| self.layer = usize::MAX)
                }
                Edit::Copy(r) => doc.duplicate_entry(r).map(|()| self.layer += 1),
                Edit::Remove(r) => doc.remove_entry(r),
                Edit::Move(a, b, n) => doc.swap_entries(a, b).map(|()| self.layer = n),
                Edit::Set(k, v) => path
                    .ok_or_else(|| io::Error::other("Select a layer"))
                    .and_then(|p| doc.set_property(&p, k, &v)),
            };
            if let Err(e) = result {
                self.errors.push(e.to_string());
            } else {
                ui.ctx().request_repaint();
            }
        }
        if let Some((d, s, name)) = &mut self.rename {
            let mut close = false;
            egui::Window::new("Rename soundscape")
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.text_edit_singleline(name);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!name.trim().is_empty(), egui::Button::new("Rename"))
                            .clicked()
                        {
                            let doc = &mut self.scripts[*d];
                            let range = doc.tree.entries[*s].key.clone();
                            match doc.edit_token(range, name) {
                                Ok(()) => close = true,
                                Err(e) => self.errors.push(e.to_string()),
                            }
                        }
                        close |= ui.button("Cancel").clicked();
                    });
                });
            if close {
                self.rename = None;
            }
        }
    }
}
fn references(scripts: &[Script], target: &str) -> Vec<(usize, usize)> {
    scripts
        .iter()
        .enumerate()
        .flat_map(|(d, doc)| {
            doc.tree
                .entries
                .iter()
                .enumerate()
                .filter_map(move |(s, e)| {
                    (matches!(e.value, Value::Block(_)) && e.name(doc).eq_ignore_ascii_case(target))
                        .then_some((d, s))
                })
        })
        .collect()
}

fn scalar<'a>(
    ui: &mut Ui,
    doc: &'a Script,
    e: &Entry,
    key: &str,
) -> Option<Option<std::borrow::Cow<'a, str>>> {
    let mut matches = e.named(doc, key);
    match (matches.next(), matches.next()) {
        (None, None) => Some(None),
        (Some(item), None) if matches!(item.value, Value::Text(_)) => Some(item.text(doc)),
        _ => {
            ui.colored_label(GOLD, format!("{key}: structured or repeated · see Source"));
            None
        }
    }
}

fn numeric(
    ui: &mut Ui,
    doc: &Script,
    e: &Entry,
    field: (&'static str, &str),
    limits: std::ops::RangeInclusive<f64>,
    suggestion: f64,
    edit: &mut Option<Edit>,
) -> Option<AudioRange> {
    let (key, label) = field;
    let raw = scalar(ui, doc, e, key)?;
    let parse = |s: &str| s.trim().parse::<f64>().ok().filter(|v| v.is_finite());
    let initial = match raw.as_deref() {
        None => Some((suggestion, suggestion, false)),
        Some(s) => match s.split_once(',') {
            Some((lo, hi)) => parse(lo).zip(parse(hi)).map(|(lo, hi)| (lo, hi, true)),
            None => parse(s).map(|v| (v, v, false)),
        },
    };
    let Some(initial) =
        initial.filter(|(lo, hi, _)| limits.contains(lo) && limits.contains(hi) && lo <= hi)
    else {
        ui.weak(format!(
            "{label}: {} · preserved in Source",
            raw.as_deref().unwrap_or_default()
        ));
        return None;
    };
    let draft = ui
        .push_id(key, |ui| {
            let id = ui
                .scope_id()
                .with(("draft", doc.revision(), raw.as_deref()));
            let mut values = ui.data(|d| d.get_temp(id)).unwrap_or(initial);
            ui.horizontal_wrapped(|ui| {
                ui.strong(label);
                if raw.is_none() {
                    ui.weak("Unset");
                }
                ui.checkbox(&mut values.2, "Range")
                    .on_hover_text("Store a minimum and maximum");
            });
            ui.spacing_mut().slider_width = (ui.available_width() - 100.).clamp(60., 185.);
            let (drag_speed, log_min) = match key {
                "volume" => (0.001, Some(0.001)),
                "time" => (0.01, Some(0.01)),
                _ => (1., None),
            };
            let number = |ui: &mut Ui, v: &mut f64, caption: &str| {
                ui.horizontal(|ui| {
                    if !caption.is_empty() {
                        ui.small(caption);
                    }
                    let mut slider = egui::Slider::new(v, limits.clone())
                        .show_value(false)
                        .max_decimals(5);
                    if let Some(min) = log_min {
                        slider = slider.logarithmic(true).smallest_positive(min);
                    }
                    ui.add(slider).on_hover_text(label);
                    ui.add(
                        egui::DragValue::new(v)
                            .range(limits.clone())
                            .speed(drag_speed)
                            .max_decimals(5),
                    )
                    .on_hover_text("Click to type an exact value");
                });
            };
            number(ui, &mut values.0, if values.2 { "Min" } else { "" });
            if values.2 {
                number(ui, &mut values.1, "Max");
            }
            let invalid = values.2 && values.0 > values.1;
            if invalid {
                ui.colored_label(GOLD, "Minimum exceeds maximum");
            }
            if (values != initial || raw.is_none())
                && ui
                    .add_enabled(!invalid, egui::Button::new(format!("Apply {label}")))
                    .clicked()
            {
                let value = if values.2 {
                    format!("{},{}", compact(values.0), compact(values.1))
                } else {
                    compact(values.0)
                };
                *edit = Some(Edit::Set(key, value));
                ui.data_mut(|d| d.remove::<(f64, f64, bool)>(id));
            } else if values != initial {
                ui.data_mut(|d| d.insert_temp(id, values));
            } else {
                ui.data_mut(|d| d.remove::<(f64, f64, bool)>(id));
            }
            (!invalid && values != initial).then_some(AudioRange(
                values.0 as f32,
                if values.2 {
                    values.1 as f32
                } else {
                    values.0 as f32
                },
            ))
        })
        .inner;
    ui.add_space(5.);
    draft
}

fn inspector(
    ui: &mut Ui,
    doc: &Script,
    e: &Entry,
    kind: Kind,
    scripts: &[Script],
    edit: &mut Option<Edit>,
    pick: &mut Option<Pick>,
) -> LayerDraft {
    ui.add_space(6.);
    match kind {
        Kind::Loop => {
            if let Some(value) = scalar(ui, doc, e, "wave") {
                let path = value.as_deref().unwrap_or("");
                ui.label(if path.is_empty() {
                    "No sound assigned"
                } else {
                    short(path)
                })
                .on_hover_text(path);
                if ui.button("Choose sound…").clicked() {
                    *pick = Some(Pick::Loop);
                }
            }
        }
        Kind::Nested => {
            if let Some(value) = scalar(ui, doc, e, "name") {
                egui::ComboBox::from_id_salt("reference")
                    .width(ui.available_width() - 10.)
                    .selected_text(
                        value
                            .as_deref()
                            .filter(|v| !v.is_empty())
                            .unwrap_or("Choose reference"),
                    )
                    .show_ui(ui, |ui| {
                        for doc in scripts {
                            for target in &doc.tree.entries {
                                if matches!(target.value, Value::Block(_)) {
                                    let name = target.name(doc);
                                    if ui
                                        .selectable_label(
                                            value.as_deref() == Some(&*name),
                                            name.as_ref(),
                                        )
                                        .clicked()
                                    {
                                        *edit = Some(Edit::Set("name", name.into_owned()));
                                    }
                                }
                            }
                        }
                    });
            }
        }
        Kind::Random => {
            for group in e.named(doc, "rndwave") {
                for wave in group.named(doc, "wave") {
                    if let Value::Text(range) = &wave.value
                        && let Some(path) = wave.text(doc)
                    {
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .small_button(if path.is_empty() {
                                    "Choose sound…"
                                } else {
                                    short(&path)
                                })
                                .on_hover_text(path.as_ref())
                                .clicked()
                            {
                                *pick = Some(Pick::Replace(range.clone()));
                            }
                            if ui
                                .small_button("−")
                                .on_hover_text("Remove variant")
                                .clicked()
                            {
                                *edit = Some(Edit::Remove(wave.span()));
                            }
                        });
                    }
                }
            }
            if ui.button("+ Add variant…").clicked() {
                *pick = Some(Pick::Variant);
            }
        }
    }
    ui.separator();
    let draft = LayerDraft {
        volume: numeric(ui, doc, e, ("volume", "Volume"), 0.0..=1.0, 1., edit),
        pitch: if kind != Kind::Nested {
            numeric(ui, doc, e, ("pitch", "Pitch (%)"), 1.0..=255.0, 100., edit)
        } else {
            None
        },
        interval: if kind == Kind::Random {
            numeric(ui, doc, e, ("time", "Interval (s)"), 0.0..=3600.0, 1., edit)
        } else {
            None
        },
    };
    if let Some(value) = scalar(ui, doc, e, "position") {
        ui.strong(if kind == Kind::Nested {
            "Starting position"
        } else {
            "Position"
        });
        egui::ComboBox::from_id_salt("position")
            .selected_text(value.as_deref().unwrap_or("Inherited"))
            .show_ui(ui, |ui| {
                if let Some(prop) = e.named(doc, "position").next()
                    && ui.selectable_label(false, "Inherited").clicked()
                {
                    *edit = Some(Edit::Remove(prop.span()));
                }
                for i in 0..8 {
                    let pos = i.to_string();
                    if ui
                        .selectable_label(value.as_deref() == Some(&pos), format!("Emitter {i}"))
                        .clicked()
                    {
                        *edit = Some(Edit::Set("position", pos));
                    }
                }
                if kind == Kind::Random
                    && ui
                        .selectable_label(value.as_deref() == Some("random"), "Random position")
                        .clicked()
                {
                    *edit = Some(Edit::Set("position", "random".into()));
                }
            })
            .response
            .on_hover_text("Emitter slots are assigned by the map.");
    }
    if kind != Kind::Nested
        && let Some(raw) = scalar(ui, doc, e, "suppress_on_restore")
    {
        if matches!(raw.as_deref(), None | Some("0" | "1")) {
            let mut enabled = raw.as_deref() == Some("1");
            if ui
                .checkbox(&mut enabled, "Suppress on restore")
                .on_hover_text("Write the Source suppress_on_restore flag")
                .changed()
            {
                *edit = Some(Edit::Set(
                    "suppress_on_restore",
                    if enabled { "1" } else { "0" }.into(),
                ));
            }
        } else {
            ui.weak("Custom restore flag preserved in Source.");
        }
    }
    let extra = e.children().iter().any(|p| {
        let name = p.name(doc).to_ascii_lowercase();
        !match name.as_str() {
            "volume" | "position" => true,
            "pitch" | "suppress_on_restore" => kind != Kind::Nested,
            "wave" => kind == Kind::Loop,
            "name" => kind == Kind::Nested,
            "time" => kind == Kind::Random,
            "rndwave" => {
                kind == Kind::Random
                    && matches!(p.value, Value::Block(_))
                    && e.named(doc, "rndwave").count() == 1
                    && p.children().iter().all(|c| {
                        c.name(doc).eq_ignore_ascii_case("wave")
                            && matches!(c.value, Value::Text(_))
                    })
            }
            _ => false,
        }
    });
    if extra {
        ui.separator();
        ui.weak("Additional properties preserved in Source.");
    }
    draft
}
