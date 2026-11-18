use super::Studio;
use audio_engine::{
    Notice, Player,
    assets::{Resolver, Sources, is_vpk, vpk_index_path},
    decode::Info,
    mixer::{Layer, Mode, Range},
};
use eframe::egui::{self, Ui};
use soundscape_core::{
    Script,
    keyvalues::{Entry, Value},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::atomic::Ordering::{Acquire, Relaxed},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) fn settings_path(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))?;
    Some(home.join("SourceSoundscapeStudio").join(name))
}

#[cfg(test)]
mod device_tests;
fn range(e: &Entry, doc: &Script, key: &str, default: f32) -> Range {
    let Some(value) = e.named(doc, key).last().and_then(|p| p.text(doc)) else {
        return Range(default, default);
    };
    let (lo, hi) = value.split_once(',').unwrap_or((&value, &value));
    let parse = |s: &str| {
        s.trim()
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .unwrap_or(default)
    };
    Range(parse(lo), parse(hi))
}
struct Spec {
    layer: Layer,
    volume: Range,
    pitch: Range,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct LayerDraft {
    pub volume: Option<Range>,
    pub pitch: Option<Range>,
    pub interval: Option<Range>,
}
struct Plan {
    tracks: Vec<Spec>,
    warnings: Vec<String>,
    visits: usize,
    seed: u64,
}
impl Plan {
    fn build(
        docs: &[Script],
        di: usize,
        si: usize,
        seed: u64,
        drafts: &HashMap<usize, LayerDraft>,
    ) -> Self {
        let mut p = Plan {
            tracks: Vec::new(),
            warnings: Vec::new(),
            visits: 0,
            seed,
        };
        p.walk(docs, (di, si), None, 1., &mut Vec::new(), drafts);
        p
    }
    fn warn(&mut self, msg: String) {
        if self.warnings.len() < 8 && !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }
    fn walk(
        &mut self,
        docs: &[Script],
        id: (usize, usize),
        group: Option<usize>,
        gain: f32,
        stack: &mut Vec<(usize, usize)>,
        drafts: &HashMap<usize, LayerDraft>,
    ) {
        if stack.contains(&id) || stack.len() >= 16 {
            self.warn("Skipped cyclic or deeply nested reference".into());
            return;
        }
        let Some((doc, scene)) = docs
            .get(id.0)
            .and_then(|d| d.tree.entries.get(id.1).map(|s| (d, s)))
        else {
            return;
        };
        stack.push(id);
        let mut ordinal = 0;
        for e in scene.children() {
            self.visits += 1;
            if self.visits > 32768 || self.tracks.len() >= 128 {
                self.warn("Preview expansion limit reached".into());
                break;
            }
            if !matches!(e.value, Value::Block(_)) {
                continue;
            }
            let kind = e.name(doc).to_ascii_lowercase();
            if !matches!(
                kind.as_str(),
                "playlooping" | "playrandom" | "playsoundscape"
            ) {
                continue;
            }
            let bus = group.unwrap_or(ordinal);
            ordinal += 1;
            let draft = if group.is_none() {
                drafts.get(&bus).copied().unwrap_or_default()
            } else {
                LayerDraft::default()
            };
            let vol = draft.volume.unwrap_or_else(|| range(e, doc, "volume", 1.));
            if kind == "playsoundscape" {
                let target = e
                    .named(doc, "name")
                    .last()
                    .and_then(|n| n.text(doc))
                    .unwrap_or_default();
                let mut matches = docs.iter().enumerate().flat_map(|(d, document)| {
                    document
                        .tree
                        .entries
                        .iter()
                        .enumerate()
                        .filter_map(|(s, entry)| {
                            (matches!(entry.value, Value::Block(_))
                                && entry.name(document).eq_ignore_ascii_case(&target))
                            .then_some((d, s))
                        })
                        .collect::<Vec<_>>()
                });
                if let (Some(child), None) = (matches.next(), matches.next()) {
                    self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let roll = (self.seed >> 40) as f32 / 16777216.;
                    self.walk(docs, child, Some(bus), gain * vol.at(roll), stack, drafts);
                } else {
                    self.warn(format!("Unresolved or ambiguous reference: {target}"));
                }
                continue;
            }
            let (waves, mode) = if kind == "playlooping" {
                (
                    e.named(doc, "wave")
                        .last()
                        .and_then(|p| p.text(doc))
                        .map(|s| s.into_owned())
                        .into_iter()
                        .collect(),
                    Mode::Loop,
                )
            } else {
                let variants = e
                    .named(doc, "rndwave")
                    .flat_map(|g| g.named(doc, "wave"))
                    .filter_map(|p| p.text(doc))
                    .map(|s| s.into_owned())
                    .collect();
                (
                    variants,
                    Mode::Random(draft.interval.unwrap_or_else(|| range(e, doc, "time", 1.))),
                )
            };
            let volume = Range((vol.0 * gain).clamp(0., 4.), (vol.1 * gain).clamp(0., 4.));
            let pitch = draft.pitch.unwrap_or_else(|| range(e, doc, "pitch", 100.));
            self.tracks.push(Spec {
                layer: Layer::new(bus, waves, mode, volume, pitch),
                volume,
                pitch,
            });
        }
        stack.pop();
    }
}
pub(super) struct Preview {
    player: Player,
    pub sources: Sources,
    pub resolver: Resolver,
    detected: Vec<PathBuf>,
    content_path: Option<PathBuf>,
    pub mute: HashSet<usize>,
    pub solo: HashSet<usize>,
    master: f32,
    active: bool,
    following_scene: bool,
    ready: bool,
    individual: Option<String>,
    info: Option<Info>,
    names: Vec<String>,
    notes: Vec<String>,
    tracks: Vec<Layer>,
    scene: Option<(PathBuf, usize)>,
    versions: Vec<(PathBuf, u64)>,
    drafts: HashMap<usize, LayerDraft>,
    draft_revision: Option<u64>,
    drafts_changed: bool,
    seed: u64,
}
impl Default for Preview {
    fn default() -> Self {
        let mut notes = Vec::new();
        let sources = settings_path("sound-roots.txt")
            .and_then(|p| {
                Sources::load(&p)
                    .map_err(|e| notes.push(e.to_string()))
                    .ok()
            })
            .unwrap_or_default();
        Self {
            player: Player::default(),
            resolver: Resolver::new(sources.roots.clone()),
            sources,
            detected: Vec::new(),
            content_path: None,
            mute: Default::default(),
            solo: Default::default(),
            master: 0.65,
            active: false,
            following_scene: false,
            ready: false,
            individual: None,
            info: None,
            names: Vec::new(),
            notes,
            tracks: Vec::new(),
            scene: None,
            versions: Vec::new(),
            drafts: HashMap::new(),
            draft_revision: None,
            drafts_changed: false,
            seed: 1,
        }
    }
}
impl Preview {
    pub(super) fn set_draft(&mut self, revision: u64, group: usize, draft: LayerDraft) -> bool {
        if draft == LayerDraft::default() {
            let changed = self.drafts.remove(&group).is_some();
            self.drafts_changed |= changed;
            return changed;
        }
        self.draft_revision = Some(revision);
        let changed = self.drafts.get(&group) != Some(&draft);
        if changed {
            self.drafts.insert(group, draft);
            self.drafts_changed = true;
        }
        changed
    }

    pub(super) fn clear_drafts(&mut self) {
        self.drafts_changed |= !self.drafts.is_empty();
        self.draft_revision = None;
        self.drafts.clear();
    }
    pub(super) fn reload_content(&mut self, script: &std::path::Path) {
        self.detected = crate::content::discover(script);
        self.content_path = Some(script.to_owned());
        self.refresh_sources();
    }
    fn refresh_sources(&mut self) {
        let mut paths = self.sources.roots.clone();
        for path in &self.detected {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        self.resolver = Resolver::new(paths);
    }
    pub fn roots_changed(&mut self) {
        self.refresh_sources();
        self.stop();
        if let Err(e) = settings_path("sound-roots.txt")
            .ok_or_else(|| std::io::Error::other("No settings directory"))
            .and_then(|p| self.sources.save(&p))
        {
            self.notes.push(format!("Audio sources not saved: {e}"));
        }
    }
    fn stop(&mut self) {
        self.player.stop();
        self.active = false;
        self.following_scene = false;
        self.ready = false;
    }
    fn launch(&mut self, layers: Vec<Layer>, individual: Option<String>) {
        self.launch_with_pause(layers, individual, false);
    }
    fn launch_with_pause(&mut self, layers: Vec<Layer>, individual: Option<String>, paused: bool) {
        self.tracks = layers;
        self.individual = individual;
        self.info = None;
        self.names.clear();
        self.active = true;
        self.following_scene = self.individual.is_none();
        self.ready = false;
        self.apply_bus();
        self.player.play_resolved_with_pause(
            self.tracks.clone(),
            self.resolver.clone(),
            self.master,
            self.individual.is_some(),
            paused,
        );
    }
    pub fn audition(&mut self, path: String) {
        self.notes.clear();
        self.launch(
            vec![Layer::new(
                0,
                vec![path.clone()],
                Mode::Once,
                Range(1., 1.),
                Range(100., 100.),
            )],
            Some(path),
        );
    }
    pub fn apply_bus(&self) {
        for layer in &self.tracks {
            layer.levels.audible.store(
                self.individual.is_some()
                    || (!self.mute.contains(&layer.group)
                        && (self.solo.is_empty() || self.solo.contains(&layer.group))),
                Relaxed,
            );
        }
    }
    fn start_scene(&mut self, docs: &[Script], di: usize, si: usize) {
        self.seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let p = Plan::build(docs, di, si, self.seed, &self.drafts);
        self.notes = p.warnings;
        self.launch(p.tracks.into_iter().map(|s| s.layer).collect(), None);
        self.drafts_changed = false;
        self.versions = docs
            .iter()
            .map(|d| (d.path.clone(), d.revision()))
            .collect();
    }
    fn sync(&mut self, docs: &[Script], di: usize, si: usize) {
        let current = docs.get(di).map(|d| (d.path.clone(), si));
        if current != self.scene {
            self.stop();
            self.individual = None;
            self.info = None;
            self.mute.clear();
            self.solo.clear();
            self.clear_drafts();
            self.scene = current;
        }
        if self.draft_revision.is_some()
            && self.draft_revision != docs.get(di).map(Script::revision)
        {
            self.clear_drafts();
        }
        if !self.following_scene {
            return;
        }
        let versions_changed = docs.len() != self.versions.len()
            || docs
                .iter()
                .zip(&self.versions)
                .any(|(d, (p, r))| d.path != *p || d.revision() != *r);
        if versions_changed || std::mem::take(&mut self.drafts_changed) {
            self.drafts_changed = false;
            let p = Plan::build(docs, di, si, self.seed, &self.drafts);
            if p.tracks.len() == self.tracks.len()
                && p.tracks.iter().zip(&self.tracks).all(|(s, l)| {
                    s.layer.group == l.group
                        && s.layer.waves == l.waves
                        && (s.layer.mode == l.mode
                            || matches!((s.layer.mode, l.mode), (Mode::Random(_), Mode::Random(_))))
                })
            {
                for (s, l) in p.tracks.iter().zip(&self.tracks) {
                    l.levels.set(s.volume, s.pitch);
                    if let Mode::Random(interval) = s.layer.mode {
                        l.levels.set_interval(interval);
                    }
                }
            } else {
                let paused = self.player.transport.paused.load(Relaxed);
                self.notes = p.warnings;
                self.launch_with_pause(
                    p.tracks.into_iter().map(|spec| spec.layer).collect(),
                    None,
                    paused,
                );
            }
            if versions_changed {
                self.versions = docs
                    .iter()
                    .map(|d| (d.path.clone(), d.revision()))
                    .collect();
            }
        }
    }
}
impl Studio {
    pub(super) fn preview_panel(&mut self, ui: &mut Ui) {
        let p = &mut self.preview;
        let script = self.scripts.get(self.selected).map(|doc| &doc.path);
        if script != p.content_path.as_ref() {
            p.content_path = script.cloned();
            p.detected = script
                .map(|path| crate::content::discover(path))
                .unwrap_or_default();
            p.refresh_sources();
            self.asset_status.clear();
        }
        p.sync(&self.scripts, self.selected, self.scene);
        for msg in p.player.notices() {
            match msg {
                Notice::Ready { names, info } => {
                    p.names = names;
                    p.info = info;
                    p.ready = true
                }
                Notice::Error(m) => {
                    p.active = false;
                    p.notes.push(m)
                }
                Notice::Warning(m) => p.notes.push(m),
            }
        }
        if p.active && p.player.transport.finished.load(Relaxed) {
            p.active = false;
        }
        if p.active {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        egui::Panel::bottom("preview")
            .default_size(150.)
            .min_size(85.)
            .resizable(true)
            .show(ui, |ui| {
                // Keep the dragged height even when the preview has little content.
                ui.set_min_height(ui.available_height());
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.heading("Preview");
                        if ui
                            .add_enabled(
                                !self.scripts.is_empty(),
                                egui::Button::new("Play soundscape"),
                            )
                            .clicked()
                        {
                            p.start_scene(&self.scripts, self.selected, self.scene);
                        }
                        if let Some(name) = p.individual.clone()
                            && ui.small_button("Replay sound").clicked()
                        {
                            p.audition(name);
                        }
                        let paused = p.player.transport.paused.load(Relaxed);
                        if ui
                            .add_enabled(
                                p.active,
                                egui::Button::new(if paused { "Resume" } else { "Pause" }),
                            )
                            .clicked()
                        {
                            p.player.transport.paused.store(!paused, Relaxed);
                        }
                        if ui
                            .add_enabled(p.active, egui::Button::new("Stop"))
                            .clicked()
                        {
                            p.stop();
                        }
                        if ui
                            .add(egui::Slider::new(&mut p.master, 0.0..=1.0).text("Master"))
                            .changed()
                        {
                            p.player.transport.volume(p.master);
                        }
                        ui.menu_button(
                            format!(
                                "Audio sources ({})",
                                p.sources.roots.len() + p.detected.len()
                            ),
                            |ui| {
                                let mut remove = None;
                                let mut up = None;
                                if !p.sources.roots.is_empty() || !p.detected.is_empty() {
                                    ui.set_width(480.);
                                    egui::ScrollArea::vertical()
                                        .max_height(280.)
                                        .auto_shrink([false, true])
                                        .show(ui, |ui| {
                                            // Menu labels are justified by default, which stretches wrapped paths.
                                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                                for (i, root) in p.sources.roots.iter().enumerate() {
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(egui::Align::TOP),
                                                        |ui| {
                                                            if ui.small_button("Remove").clicked() {
                                                                remove = Some(i);
                                                            }
                                                            if i > 0 && ui.small_button("Up").clicked() {
                                                                up = Some(i);
                                                            }
                                                            ui.add(
                                                                egui::Label::new(root.display().to_string())
                                                                    .halign(egui::Align::Min)
                                                                    .wrap(),
                                                            );
                                                        },
                                                    );
                                                }
                                                if !p.detected.is_empty() {
                                                    ui.label("Detected for this script");
                                                    for path in &p.detected {
                                                        ui.add(egui::Label::new(path.display().to_string()).wrap());
                                                    }
                                                }
                                            });
                                        });
                                    ui.separator();
                                }
                                let mut changed = false;
                                if let Some(i) = remove {
                                    p.sources.roots.remove(i);
                                    changed = true;
                                } else if let Some(i) = up {
                                    p.sources.roots.swap(i, i - 1);
                                    changed = true;
                                }
                                if ui.button("Add sound folder…").clicked()
                                    && let Some(root) = rfd::FileDialog::new().pick_folder()
                                    && !p.sources.roots.contains(&root)
                                {
                                    p.sources.roots.push(root);
                                    changed = true;
                                }
                                if ui.button("Add game or mod folder…").clicked()
                                    && let Some(game) = rfd::FileDialog::new().pick_folder()
                                {
                                    let mounts = crate::content::game_sources(&game);
                                    if mounts.is_empty() {
                                        p.notes.push(format!(
                                            "No audio sources found in {}. Select the folder containing gameinfo.txt, sound/ or the game's VPK files.",
                                            game.display()
                                        ));
                                    }
                                    for path in mounts {
                                        if !p.sources.roots.contains(&path) {
                                            p.sources.roots.push(path);
                                            changed = true;
                                        }
                                    }
                                    ui.close();
                                }
                                if ui.button("Add VPK sources…").clicked()
                                    && let Some(files) = rfd::FileDialog::new()
                                        .add_filter("Valve packages", &["vpk"])
                                        .pick_files()
                                {
                                    for file in files {
                                        let index = vpk_index_path(&file);
                                        if is_vpk(&index) && !p.sources.roots.contains(&index) {
                                            p.sources.roots.push(index);
                                            changed = true;
                                        }
                                    }
                                }
                                if changed {
                                    p.roots_changed();
                                    self.asset_status.clear();
                                }
                                if ui.button("Refresh file status").clicked() {
                                    p.detected = script
                                        .map(|path| crate::content::discover(path))
                                        .unwrap_or_default();
                                    p.refresh_sources();
                                    self.asset_status.clear();
                                    ui.close();
                                }
                            },
                        );
                    });
                    if p.sources.roots.is_empty() && p.detected.is_empty() && script.is_some() {
                        ui.weak("No audio sources. Add a game or mod folder, sound folder or VPK in Audio sources.");
                    }
                    let t = &p.player.transport;
                    let state = if !p.active {
                        "Stopped"
                    } else if !p.ready {
                        "Loading"
                    } else if t.paused.load(Relaxed) {
                        "Paused"
                    } else {
                        "Playing"
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(state);
                        ui.label(format!("{:.2} s", t.seconds()));
                        ui.label(format!(
                            "{} active voices",
                            if p.active { t.voices.load(Relaxed) } else { 0 }
                        ));
                        if let Some(name) = &p.individual {
                            ui.label(name);
                        }
                    });
                    if let Some(info) = &p.info {
                        ui.label(format!(
                            "{} · {} Hz · {} ch · {:.2} s",
                            info.codec, info.rate, info.channels, info.duration
                        ));
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 28.),
                            egui::Sense::hover(),
                        );
                        let paint = ui.painter();
                        paint.rect_filled(rect, 2., egui::Color32::from_gray(25));
                        for (i, amp) in info.wave.iter().enumerate() {
                            let x = rect.left() + rect.width() * i as f32 / info.wave.len() as f32;
                            let h = 13. * amp.clamp(0., 1.);
                            paint.line_segment(
                                [
                                    egui::pos2(x, rect.center().y - h),
                                    egui::pos2(x, rect.center().y + h),
                                ],
                                egui::Stroke::new(2., egui::Color32::LIGHT_BLUE),
                            );
                        }
                        let x = rect.left()
                            + rect.width() * (t.seconds() / info.duration).clamp(0., 1.) as f32;
                        paint.line_segment(
                            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                            egui::Stroke::new(2., egui::Color32::WHITE),
                        );
                    } else if p.ready {
                        let count = t.event_count.load(Acquire);
                        for n in (count.saturating_sub(3)..count).rev() {
                            let event = t.events[n as usize % 8].load(Relaxed);
                            if let Some(name) = p.names.get(event as u32 as usize) {
                                ui.small(format!("{} s  ·  {name}", event >> 32));
                            }
                        }
                    }
                    for note in p.notes.iter().rev().take(3) {
                        ui.colored_label(egui::Color32::LIGHT_YELLOW, note);
                    }
                    if let Some(err) = self.errors.last() {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                        if ui.small_button("Dismiss").clicked() {
                            self.errors.clear();
                        }
                    }
                });
            });
    }
}
