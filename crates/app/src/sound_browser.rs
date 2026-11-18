use crate::{Studio, content};
use audio_engine::{
    Notice, Player,
    assets::{CatalogEntry, CatalogSource, Resolver, audio_extension, wave_prefix},
    decode::{Clip, LoopMarker, Metadata},
};
use eframe::egui::{self, Ui};
use soundscape_core::Script;
use std::{
    collections::BTreeMap,
    fs, io,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::Duration,
};

const MAX_ENTRIES: usize = 150_000;
const AUDITION_BUDGET: usize = 64 * 1024 * 1024;

pub(super) enum Pick {
    Loop,
    Variant,
    Replace(Range<usize>),
}

struct Target {
    path: PathBuf,
    revision: u64,
    layer: [usize; 2],
    pick: Pick,
}

impl Target {
    fn apply(self, doc: &mut Script, waves: &[String]) -> io::Result<()> {
        if doc.path != self.path || doc.revision() != self.revision {
            return Err(io::Error::other(
                "The soundscape changed. Open Sound Browser again.",
            ));
        }
        let Some(wave) = waves.first() else {
            return Ok(());
        };
        let with_prefix = |previous: &str| format!("{}{wave}", wave_prefix(previous));
        match self.pick {
            Pick::Variant => doc.add_variants(&self.layer, waves),
            Pick::Loop => {
                let previous = doc
                    .entry_at(&self.layer)?
                    .named(doc, "wave")
                    .next()
                    .and_then(|entry| entry.text(doc))
                    .unwrap_or_default();
                let wave = with_prefix(&previous);
                doc.set_property(&self.layer, "wave", &wave)
            }
            Pick::Replace(range) => {
                let token = soundscape_core::keyvalues::token(doc.bytes(), range.clone());
                let wave = with_prefix(&token);
                doc.edit_token(range, &wave)
            }
        }
    }
}

struct Worker<T> {
    rx: Receiver<T>,
    cancel: Arc<AtomicBool>,
}

impl<T: Send + 'static> Worker<T> {
    fn start(
        ctx: &egui::Context,
        work: impl FnOnce(&AtomicBool, SyncSender<T>, egui::Context) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            work(&stop, tx, ctx.clone());
            ctx.request_repaint();
        });
        Self { rx, cancel }
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Relaxed);
    }
}

enum Scan {
    Entries(Vec<CatalogEntry>),
    Done(Vec<String>),
}

fn start_scan(ctx: &egui::Context, resolver: Resolver) -> Worker<Scan> {
    Worker::start(ctx, move |stop, tx, ctx| {
        let mut pending = Vec::with_capacity(256);
        let mut count = 0;
        let mut warnings = resolver.scan_catalog(
            || stop.load(Relaxed),
            |entry| {
                count += 1;
                pending.push(entry);
                if pending.len() == 256 {
                    let batch = std::mem::replace(&mut pending, Vec::with_capacity(256));
                    if tx.send(Scan::Entries(batch)).is_err() {
                        return false;
                    }
                    ctx.request_repaint();
                }
                count < MAX_ENTRIES
            },
        );
        if count == MAX_ENTRIES {
            warnings.push(format!("Catalog limited to {MAX_ENTRIES} files."));
        }
        if !pending.is_empty() {
            let _ = tx.send(Scan::Entries(pending));
            ctx.request_repaint();
        }
        let _ = tx.send(Scan::Done(warnings));
        ctx.request_repaint();
    })
}

#[derive(Default)]
struct Folder(BTreeMap<String, Folder>);

impl Folder {
    fn add(&mut self, path: &str) {
        if let Some((folder, _)) = path.rsplit_once('/') {
            let mut node = self;
            for part in folder.split('/') {
                node = node.0.entry(part.into()).or_default();
            }
        }
    }

    fn show(&self, ui: &mut Ui, parent: &str, selected: &mut String) {
        for (name, children) in &self.0 {
            let path = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}/{name}")
            };
            if children.0.is_empty() {
                if ui.selectable_label(*selected == path, name).clicked() {
                    *selected = path;
                }
            } else {
                let response = egui::CollapsingHeader::new(name)
                    .id_salt(&path)
                    .show(ui, |ui| {
                        if ui
                            .selectable_label(*selected == path, "All in folder")
                            .clicked()
                        {
                            *selected = path.clone();
                        }
                        children.show(ui, &path, selected);
                    });
                response.header_response.on_hover_text(&path);
            }
        }
    }
}

struct Row {
    asset: CatalogEntry,
    search: String,
    metadata: Option<Result<Metadata, String>>,
}

impl Row {
    fn name(&self) -> &str {
        self.asset
            .wave
            .rsplit('/')
            .next()
            .unwrap_or(&self.asset.wave)
    }

    fn format(&self) -> &str {
        audio_extension(Path::new(&self.asset.wave)).unwrap_or("")
    }

    fn metadata(&self) -> Option<Metadata> {
        self.metadata.as_ref()?.as_ref().ok().copied()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Sort {
    #[default]
    Folder,
    Name,
    Duration,
}

#[derive(Clone, Default, PartialEq)]
struct Filters {
    query: String,
    folder: String,
    format: &'static str,
    source: Option<usize>,
    channels: usize,
    loop_only: bool,
    min_seconds: f64,
    max_seconds: f64,
    sort: Sort,
}

impl Filters {
    fn needs_metadata(&self) -> bool {
        self.sort == Sort::Duration
            || self.channels > 0
            || self.loop_only
            || self.min_seconds > 0.
            || self.max_seconds > 0.
    }

    fn base_matches(&self, row: &Row, words: &[&str]) -> bool {
        (self.folder.is_empty()
            || row
                .search
                .strip_prefix(&self.folder)
                .is_some_and(|tail| tail.starts_with('/')))
            && self.source.is_none_or(|source| row.asset.source == source)
            && (self.format.is_empty() || row.format() == self.format)
            && words.iter().all(|word| row.search.contains(word))
    }

    fn matches_metadata(&self, row: &Row) -> bool {
        let metadata = row.metadata();
        (self.channels == 0 || metadata.and_then(|meta| meta.channels) == Some(self.channels))
            && (!self.loop_only
                || metadata.is_some_and(|meta| meta.loop_marker == Some(LoopMarker::Cue)))
            && ((self.min_seconds == 0. && self.max_seconds == 0.)
                || metadata
                    .and_then(|meta| meta.duration)
                    .is_some_and(|seconds| {
                        seconds >= self.min_seconds
                            && (self.max_seconds == 0. || seconds <= self.max_seconds)
                    }))
    }
}

struct Decoded {
    clip: Arc<Clip>,
    waveform: Vec<[[f32; 2]; 2]>,
}

type ProbeResult = (usize, Result<Metadata, String>);

#[derive(Default)]
pub(super) struct SoundBrowser {
    target: Option<Target>,
    resolver: Resolver,
    sources: Vec<CatalogSource>,
    rows: Vec<Row>,
    folders: Folder,
    scan: Option<Worker<Scan>>,
    probes: Option<Worker<ProbeResult>>,
    skip_auto_probe: bool,
    filters: Filters,
    visible: Vec<usize>,
    selected: Vec<usize>,
    external: Vec<String>,
    reveal: Option<String>,
    import: Option<PathBuf>,
    focused: Option<usize>,
    loading: Option<Worker<Result<Decoded, String>>>,
    decode_gate: Arc<Mutex<()>>,
    decoded: Option<Decoded>,
    player: Player,
    preview_status: String,
    playing: bool,
    play_on_load: bool,
    repeat: bool,
    auto_preview: bool,
    volume: Option<f32>,
    offset: f64,
    zoom: f32,
    view_start: f32,
    error: String,
    scan_warnings: Vec<String>,
}

impl SoundBrowser {
    pub(super) fn open(&mut self, doc: &Script, layer: [usize; 2], pick: Pick) {
        self.target = Some(Target {
            path: doc.path.clone(),
            revision: doc.revision(),
            layer,
            pick,
        });
        self.error.clear();
    }

    fn multiple(&self) -> bool {
        self.target
            .as_ref()
            .is_some_and(|target| matches!(target.pick, Pick::Variant))
    }

    fn reset_catalog(&mut self, ctx: &egui::Context, resolver: &Resolver) {
        self.close_preview();
        self.probes = None;
        self.skip_auto_probe = false;
        self.scan = None;
        self.resolver = resolver.clone();
        self.sources = resolver.catalog_sources();
        self.rows.clear();
        self.visible.clear();
        self.selected.clear();
        self.external.clear();
        self.reveal = None;
        self.folders = Folder::default();
        self.filters.source = None;
        self.filters.folder.clear();
        self.scan_warnings.clear();
        self.scan = Some(start_scan(ctx, resolver.clone()));
    }

    fn close_preview(&mut self) {
        self.player.stop();
        self.playing = false;
        self.loading = None;
        self.decoded = None;
        self.focused = None;
        self.preview_status.clear();
    }

    fn close(&mut self) {
        self.close_preview();
        self.target = None;
        self.selected.clear();
        self.external.clear();
        self.reveal = None;
        self.import = None;
        self.probes = None;
        if self.scan.take().is_some() {
            self.resolver = Resolver::default();
        }
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(scan) = &self.scan {
            for _ in 0..8 {
                match scan.rx.try_recv() {
                    Ok(Scan::Entries(batch)) => {
                        self.rows.extend(batch.into_iter().map(|asset| {
                            let search = asset.wave.to_ascii_lowercase();
                            self.folders.add(&search);
                            Row {
                                asset,
                                search,
                                metadata: None,
                            }
                        }));
                        changed = true;
                    }
                    Ok(Scan::Done(warnings)) => {
                        self.scan_warnings = warnings;
                        self.scan = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.scan = None;
                        break;
                    }
                }
            }
        }
        if let Some(probes) = &self.probes {
            for _ in 0..64 {
                match probes.rx.try_recv() {
                    Ok((id, value)) => {
                        if self.rows[id].metadata.is_none() {
                            self.rows[id].metadata = Some(value);
                            changed = true;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.probes = None;
                        changed = true;
                        break;
                    }
                }
            }
        }
        if let Some(job) = &self.loading {
            match job.rx.try_recv() {
                Ok(result) => {
                    self.loading = None;
                    match result {
                        Ok(decoded) => {
                            if let Some(id) = self.focused {
                                self.rows[id].metadata = Some(Ok(decoded.clip.info.metadata()));
                            }
                            self.decoded = Some(decoded);
                            self.preview_status.clear();
                            if self.play_on_load {
                                self.play();
                            }
                        }
                        Err(error) => {
                            if let Some(id) = self.focused {
                                self.rows[id].metadata = Some(Err(error.clone()));
                            }
                            self.preview_status = error;
                        }
                    }
                    changed = true;
                }
                Err(TryRecvError::Disconnected) => {
                    self.loading = None;
                    self.preview_status = "Sound preview worker stopped.".into();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        for notice in self.player.notices() {
            if let Notice::Error(error) | Notice::Warning(error) = notice {
                self.preview_status = error;
            }
        }
        changed
    }

    fn filter(&mut self) {
        let query = self.filters.query.to_ascii_lowercase().replace('\\', "/");
        let words: Vec<_> = query.split_whitespace().collect();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                self.filters.base_matches(row, &words) && self.filters.matches_metadata(row)
            })
            .map(|(i, _)| i)
            .collect();
        self.visible.sort_unstable_by(|&i, &j| {
            let (a, b) = (&self.rows[i], &self.rows[j]);
            let duration = |row: &Row| {
                row.metadata()
                    .and_then(|meta| meta.duration)
                    .unwrap_or(f64::INFINITY)
            };
            match self.filters.sort {
                Sort::Name => a
                    .search
                    .rsplit('/')
                    .next()
                    .cmp(&b.search.rsplit('/').next()),
                Sort::Duration => duration(a).total_cmp(&duration(b)),
                Sort::Folder => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.search.cmp(&b.search))
            .then_with(|| a.asset.source.cmp(&b.asset.source))
        });
    }

    fn analyze(&mut self, ctx: &egui::Context) {
        let query = self.filters.query.to_ascii_lowercase().replace('\\', "/");
        let words: Vec<_> = query.split_whitespace().collect();
        let entries: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.asset.available
                    && row.metadata.is_none()
                    && self.filters.base_matches(row, &words)
            })
            .take(512)
            .map(|(id, row)| (id, row.asset.clone()))
            .collect();
        if entries.is_empty() {
            return;
        }
        let resolver = self.resolver.clone();
        self.probes = Some(Worker::start(ctx, move |stop, tx, ctx| {
            for (id, entry) in entries {
                if stop.load(Relaxed) {
                    break;
                }
                let result = resolver
                    .probe_entry(&entry)
                    .map_err(|error| error.to_string());
                if tx.send((id, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        }));
    }

    fn select(&mut self, id: usize, checked: bool) {
        let wave = &self.rows[id].asset.wave;
        self.external
            .retain(|name| !name.eq_ignore_ascii_case(wave));
        if checked {
            if !self.multiple() {
                self.selected.clear();
                self.external.clear();
            }
            if !self.selected.contains(&id) {
                self.selected.push(id);
            }
        } else {
            self.selected.retain(|&selected| selected != id);
        }
    }

    fn focus(&mut self, ctx: &egui::Context, id: usize, play: bool) {
        if self.focused == Some(id) {
            if play {
                self.play_on_load = true;
                self.play();
            }
            return;
        }
        self.close_preview();
        self.focused = Some(id);
        if !self.rows[id].asset.available {
            self.preview_status = "This VPK entry is missing its payload.".into();
            return;
        }
        let resolver = self.resolver.clone();
        let asset = self.rows[id].asset.clone();
        self.load_clip(ctx, play, move |stop| {
            resolver.decode_entry(&asset, AUDITION_BUDGET, || stop.load(Relaxed))
        });
    }

    fn load_clip(
        &mut self,
        ctx: &egui::Context,
        play: bool,
        load: impl FnOnce(&AtomicBool) -> io::Result<Clip> + Send + 'static,
    ) {
        self.offset = 0.;
        self.zoom = 1.;
        self.view_start = 0.;
        self.play_on_load = play || self.auto_preview;
        self.preview_status = "Loading audio…".into();
        let gate = self.decode_gate.clone();
        self.loading = Some(Worker::start(ctx, move |stop, tx, ctx| {
            let Ok(_guard) = gate.lock() else { return };
            if stop.load(Relaxed) {
                return;
            }
            let clip = load(stop);
            if !stop.load(Relaxed) {
                let result = clip
                    .map(|clip| {
                        let waveform = clip.waveform();
                        Decoded {
                            clip: Arc::new(clip),
                            waveform,
                        }
                    })
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
                ctx.request_repaint();
            }
        }));
    }

    fn play(&mut self) {
        if let Some(data) = &self.decoded {
            if self.playing && !self.player.transport.finished.load(Relaxed) {
                self.player.transport.paused.store(false, Relaxed);
                return;
            }
            if self.offset >= data.clip.info.duration {
                self.offset = 0.;
            }
            self.player.play_clip(
                data.clip.clone(),
                self.volume.unwrap_or(0.65),
                self.offset,
                self.repeat,
            );
            self.playing = true;
            self.preview_status.clear();
        }
    }

    fn wave_selection(&self) -> Vec<String> {
        let mut waves: Vec<_> = self
            .selected
            .iter()
            .map(|&id| self.rows[id].asset.wave.clone())
            .collect();
        for wave in &self.external {
            if !waves
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(wave))
            {
                waves.push(wave.clone());
            }
        }
        waves
    }

    fn controls(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filters.query)
                    .hint_text("Search sounds, paths or extensions")
                    .desired_width(300.),
            );
            egui::ComboBox::from_id_salt("format")
                .selected_text(if self.filters.format.is_empty() {
                    "All formats".into()
                } else {
                    self.filters.format.to_ascii_uppercase()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.format, "", "All formats");
                    for ext in ["wav", "mp3", "ogg", "flac"] {
                        ui.selectable_value(
                            &mut self.filters.format,
                            ext,
                            ext.to_ascii_uppercase(),
                        );
                    }
                });
            let sort = match self.filters.sort {
                Sort::Folder => "Folder",
                Sort::Name => "Name",
                Sort::Duration => "Duration",
            };
            egui::ComboBox::from_id_salt("sort")
                .selected_text(sort)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.sort, Sort::Folder, "Folder");
                    ui.selectable_value(&mut self.filters.sort, Sort::Name, "Name");
                    ui.selectable_value(&mut self.filters.sort, Sort::Duration, "Duration");
                });
            if ui.button("Import external audio…").clicked() {
                self.pick_external(ui.ctx());
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (count, label) in [(0, "Any channels"), (1, "Mono"), (2, "Stereo")] {
                ui.selectable_value(&mut self.filters.channels, count, label);
            }
            ui.checkbox(&mut self.filters.loop_only, "WAV cue loop");
            ui.label("Duration (s)");
            ui.add(egui::DragValue::new(&mut self.filters.min_seconds).range(0.0..=86_400.).speed(0.1).prefix("Min "));
            ui.add(egui::DragValue::new(&mut self.filters.max_seconds).range(0.0..=86_400.).speed(0.1).prefix("Max "))
                .on_hover_text("0 means no upper limit");
            if self.probes.is_some() {
                if ui.button("Cancel analysis").clicked() {
                    self.probes = None;
                    self.skip_auto_probe = true;
                }
            } else if ui.button("Analyze matches").on_hover_text("Read headers for up to 512 unexamined matches. Channel, duration and cue filters use known metadata.").clicked() {
                self.analyze(ui.ctx());
            }
        });
    }

    fn content_tree(&mut self, ui: &mut Ui, height: f32) {
        ui.strong("Game Content");
        egui::ScrollArea::vertical()
            .id_salt("content-tree")
            .max_height(height)
            .show(ui, |ui| {
                if ui
                    .selectable_label(self.filters.folder.is_empty(), "All Sounds")
                    .clicked()
                {
                    self.filters.folder.clear();
                    self.filters.source = None;
                }
                self.folders.show(ui, "", &mut self.filters.folder);
                for (packed, label) in [(false, "Loose sources"), (true, "VPK Sources")] {
                    egui::CollapsingHeader::new(label).show(ui, |ui| {
                        for (id, source) in self
                            .sources
                            .iter()
                            .enumerate()
                            .filter(|(_, source)| source.packed == packed)
                        {
                            if ui
                                .selectable_label(
                                    self.filters.source == Some(id),
                                    source_name(source),
                                )
                                .on_hover_text(source.path.display().to_string())
                                .clicked()
                            {
                                self.filters.source =
                                    (self.filters.source != Some(id)).then_some(id);
                            }
                        }
                    });
                }
            });
    }

    fn results(&mut self, ui: &mut Ui, height: f32) {
        ui.horizontal(|ui| {
            ui.strong("Sound Results");
            ui.weak(format!("{} of {}", self.visible.len(), self.rows.len()));
            if self.scan.is_some() {
                ui.spinner();
                ui.label("Indexing…");
            }
            if !self.scan_warnings.is_empty() {
                ui.label("Source warnings")
                    .on_hover_text(self.scan_warnings.join("\n"));
            }
        });
        if self.visible.is_empty() {
            ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                ui.weak(if self.scan.is_some() {
                    "Scanning mounted audio…"
                } else if self.sources.is_empty() {
                    "Add a game folder or VPK from Preview → Audio sources."
                } else {
                    "No matches. Adjust the filters or analyze matches for audio metadata."
                });
            });
            return;
        }
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt("sound-results")
            .max_height(height)
            .auto_shrink([false, false])
            .show_rows(ui, 24., self.visible.len(), |ui, range| {
                for position in range {
                    let id = self.visible[position];
                    let row = &self.rows[id];
                    let asset = &row.asset;
                    ui.push_id(id, |ui| {
                        let focused = self.focused == Some(id);
                        let queued = self.selected.contains(&id)
                            || (asset.effective
                                && self
                                    .external
                                    .iter()
                                    .any(|wave| wave.eq_ignore_ascii_case(&asset.wave)));
                        let highlight =
                            (focused || queued).then(|| ui.painter().add(egui::Shape::Noop));
                        let row_response = ui.horizontal(|ui| {
                            let mut checked = queued;
                            if ui
                                .add_enabled(
                                    asset.effective,
                                    egui::Checkbox::without_text(&mut checked),
                                )
                                .on_disabled_hover_text(
                                    "A higher-priority source wins, or the payload is missing.",
                                )
                                .on_hover_text("Select for Add Selected")
                                .changed()
                            {
                                action = Some((id, Some(checked), false));
                            }
                            if ui
                                .add_enabled(asset.available, egui::Button::new("Play"))
                                .on_hover_text("Preview this file")
                                .clicked()
                            {
                                action = Some((id, None, true));
                            }
                            let fixed = 70. + 76. + 150.;
                            let name_width =
                                (ui.available_width() - fixed - 4. * ui.spacing().item_spacing.x)
                                    .max(80.);
                            if ui
                                .add_sized(
                                    [name_width, 23.],
                                    egui::Label::new(if focused {
                                        egui::RichText::new(row.name()).strong()
                                    } else {
                                        egui::RichText::new(row.name())
                                    })
                                    .halign(egui::Align::Min)
                                    .truncate()
                                    .sense(egui::Sense::click()),
                                )
                                .on_hover_text(&asset.wave)
                                .clicked()
                            {
                                action = Some((
                                    id,
                                    (!self.multiple() && asset.effective).then_some(true),
                                    false,
                                ));
                            }
                            let duration = row
                                .metadata()
                                .and_then(|meta| meta.duration)
                                .map_or_else(|| "–".into(), |seconds| format!("{seconds:.1} s"));
                            ui.add_sized([70., 23.], egui::Label::new(duration).truncate());
                            let cue = match row.metadata().and_then(|meta| meta.loop_marker) {
                                Some(LoopMarker::Cue) => " · Cue",
                                Some(LoopMarker::Sampler) => " · Loop",
                                None => "",
                            };
                            ui.add_sized(
                                [76., 23.],
                                egui::Label::new(format!(
                                    "{}{cue}",
                                    row.format().to_ascii_uppercase()
                                ))
                                .truncate(),
                            );
                            let source = &self.sources[asset.source];
                            let label = if !asset.available {
                                "Missing payload".to_owned()
                            } else if !asset.effective {
                                format!("{} · shadowed", source_name(source))
                            } else {
                                source_name(source)
                            };
                            ui.add_sized([150., 23.], egui::Label::new(label).truncate())
                                .on_hover_text(source.path.display().to_string());
                        });
                        if let Some(highlight) = highlight {
                            let color = ui.visuals().selection.bg_fill;
                            let color = if focused {
                                color
                            } else {
                                color.gamma_multiply(0.45)
                            };
                            ui.painter().set(
                                highlight,
                                egui::Shape::rect_filled(row_response.response.rect, 4., color),
                            );
                        }
                    });
                }
            });
        if let Some((id, state, play)) = action {
            if let Some(checked) = state {
                self.select(id, checked);
            }
            self.focus(ui.ctx(), id, play);
        }
    }

    fn preview(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Audio Preview");
            ui.checkbox(&mut self.auto_preview, "Auto Preview");
            if ui.checkbox(&mut self.repeat, "Loop").changed() {
                self.player.transport.repeat(self.repeat);
            }
            let volume = self.volume.get_or_insert(0.65);
            if ui
                .add(egui::Slider::new(volume, 0.0..=1.0).text("Volume"))
                .changed()
            {
                self.player.transport.volume(*volume);
            }
        });
        let (name, source_path) = if let Some(id) = self.focused {
            let row = &self.rows[id];
            let source = &self.sources[row.asset.source];
            let origin = if source.packed {
                "VPK"
            } else if self
                .target
                .as_ref()
                .and_then(|target| content::game_root(&target.path))
                .is_some_and(|root| source.path.starts_with(root))
            {
                "Loose file · Mod"
            } else {
                "Loose file · Mounted content"
            };
            (
                format!(
                    "Previewing {} · {origin} · {} KiB",
                    row.asset.wave,
                    row.asset.size.div_ceil(1024)
                ),
                source.path.display().to_string(),
            )
        } else if let Some(file) = &self.import {
            (
                format!(
                    "Previewing {}",
                    file.file_name().unwrap_or_default().to_string_lossy()
                ),
                file.display().to_string(),
            )
        } else {
            ("Choose a sound to preview".into(), String::new())
        };
        ui.add(egui::Label::new(name).truncate())
            .on_hover_text(source_path);
        if self.playing {
            let transport = &self.player.transport;
            if transport.rate.load(Relaxed) > 0 {
                self.offset = transport.seconds();
            }
            if transport.finished.load(Relaxed) {
                self.playing = false;
            }
        }
        let ready = self.decoded.is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new("Play")).clicked() {
                self.play();
            }
            if ui
                .add_enabled(self.playing, egui::Button::new("Pause"))
                .clicked()
            {
                self.player.transport.paused.store(true, Relaxed);
            }
            if ui.add_enabled(ready, egui::Button::new("Stop")).clicked() {
                self.player.stop();
                self.playing = false;
                self.offset = 0.;
            }
            ui.add_enabled(
                ready,
                egui::Slider::new(&mut self.zoom, 1.0..=16.0).text("Zoom"),
            );
            ui.add_enabled(
                ready && self.zoom > 1.,
                egui::Slider::new(&mut self.view_start, 0.0..=1.0).text("View offset"),
            );
        });
        let details = self.decoded.as_ref().map_or_else(
            || " ".to_owned(),
            |decoded| {
                let info = &decoded.clip.info;
                format!(
                    "{:.2} / {:.2} s · {} Hz · {} · {}",
                    self.offset,
                    info.duration,
                    info.rate,
                    if info.channels == 1 { "Mono" } else { "Stereo" },
                    info.codec
                )
            },
        );
        ui.add(egui::Label::new(details).truncate());
        let status = if !self.preview_status.is_empty() {
            self.preview_status.as_str()
        } else if let Some(decoded) = &self.decoded {
            let cue = decoded
                .clip
                .info
                .loop_points
                .as_ref()
                .is_some_and(|points| points.marker == LoopMarker::Cue);
            match self.target.as_ref().map(|target| &target.pick) {
                Some(Pick::Loop) if !cue => {
                    "No Source WAV cue loop detected. Use a WAV with a cue for a portable loop."
                }
                Some(Pick::Variant | Pick::Replace(_)) if cue => {
                    "This WAV has a loop cue. Prefer one-shot sounds in Random layers."
                }
                _ if self
                    .focused
                    .is_some_and(|id| matches!(self.rows[id].format(), "ogg" | "flac")) =>
                {
                    "Preview supported. Verify this format in the target Source branch."
                }
                _ => " ",
            }
        } else {
            " "
        };
        ui.add(egui::Label::new(egui::RichText::new(status).weak()).truncate())
            .on_hover_text(status);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 64.),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4., egui::Color32::from_gray(22));
        let Some(decoded) = &self.decoded else { return };
        let info = &decoded.clip.info;
        let duration = info.duration;
        let view_span = 1. / self.zoom;
        let start = self.view_start * (1. - view_span);
        let height = rect.height() / info.channels as f32;
        let buckets = decoded.waveform.len();
        let x_for = |fraction: f32| rect.left() + (fraction - start) / view_span * rect.width();
        let first = (start * buckets as f32) as usize;
        let last = (((start + view_span) * buckets as f32).ceil() as usize).min(buckets);
        for (i, channels) in decoded.waveform.iter().enumerate().take(last).skip(first) {
            let x = x_for((i as f32 + 0.5) / buckets as f32);
            for (channel, &[low, high]) in channels.iter().take(info.channels).enumerate() {
                let center = rect.top() + (channel as f32 + 0.5) * height;
                painter.line_segment(
                    [
                        egui::pos2(x, center - high.clamp(-1., 1.) * height * 0.46),
                        egui::pos2(x, center - low.clamp(-1., 1.) * height * 0.46),
                    ],
                    (1., egui::Color32::from_rgb(107, 215, 197)),
                );
            }
        }
        if let Some(points) = &info.loop_points {
            for frame in [points.frames.start, points.frames.end] {
                let x = x_for(frame as f32 / decoded.clip.frames.len() as f32);
                if rect.x_range().contains(x) {
                    painter.vline(x, rect.y_range(), (1., egui::Color32::GOLD));
                }
            }
        }
        let cursor = x_for((self.offset / duration) as f32);
        if rect.x_range().contains(cursor) {
            painter.vline(cursor, rect.y_range(), (1., egui::Color32::WHITE));
        }
        if (response.clicked() || response.dragged())
            && let Some(point) = response.interact_pointer_pos()
        {
            let relative = ((point.x - rect.left()) / rect.width()).clamp(0., 1.);
            self.offset = duration * f64::from(start + relative * view_span);
            self.player.transport.seek(self.offset);
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        if self.playing && !self.player.transport.paused.load(Relaxed) {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }

    fn pick_external(&mut self, ctx: &egui::Context) {
        let Some(file) = rfd::FileDialog::new()
            .add_filter("Audio files", &["wav", "mp3", "ogg", "flac"])
            .pick_file()
        else {
            return;
        };
        if audio_extension(&file).is_none() {
            self.error = "Select WAV, MP3, Ogg or FLAC audio.".into();
        } else if let Some(wave) = self.resolver.wave_for_file(&file) {
            if let Some(id) = self.find_wave(&wave) {
                self.select(id, true);
                self.focus(ctx, id, false);
            } else {
                self.reveal = Some(wave.clone());
                self.add_external(wave);
            }
        } else {
            self.close_preview();
            self.import = Some(file.clone());
            self.load_clip(ctx, false, move |stop| {
                audio_engine::decode::decode(&file, AUDITION_BUDGET, || stop.load(Relaxed))
            });
        }
    }

    fn add_external(&mut self, wave: String) {
        if !self.multiple() {
            self.selected.clear();
            self.external.clear();
        }
        if !self
            .external
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&wave))
        {
            self.external.push(wave);
        }
        self.import = None;
    }

    fn find_wave(&self, wave: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.asset.effective && row.asset.wave.eq_ignore_ascii_case(wave))
    }

    fn draw(&mut self, ctx: &egui::Context, changed: bool) -> BrowserAction {
        let viewport = ctx.content_rect();
        let width = (viewport.width() - 48.).clamp(700., 1080.);
        let height = (viewport.height() - 340.).clamp(145., 475.);
        let modal = egui::Modal::new(egui::Id::unique("sound-browser")).show(ctx, |ui| {
            ui.set_width(width);
            ui.heading("Sound Browser");
            let previous = self.filters.clone();
            self.controls(ui);
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(165., height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(165.);
                        self.content_tree(ui, height);
                    },
                );
                ui.add_space(8.);
                if self.filters != previous {
                    self.skip_auto_probe = false;
                    self.probes = None;
                }
                if changed || self.filters != previous {
                    self.filter();
                    if self.probes.is_none()
                        && self.filters.needs_metadata()
                        && !self.skip_auto_probe
                    {
                        self.analyze(ctx);
                    }
                }
                ui.vertical(|ui| {
                    ui.set_width(width - 190.);
                    self.results(ui, height);
                });
            });
            ui.separator();
            self.preview(ui);
            let mut action = BrowserAction::None;
            if let Some(file) = self.import.clone() {
                ui.add(egui::Label::new(format!("External Audio · {}", file.display())).truncate());
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.target
                                .as_ref()
                                .and_then(|target| content::game_root(&target.path))
                                .is_some(),
                            egui::Button::new("Copy to mod sound/…"),
                        )
                        .clicked()
                    {
                        action = BrowserAction::Import(file.clone(), true);
                    }
                    if ui.button("Choose destination…").clicked() {
                        action = BrowserAction::Import(file.clone(), false);
                    }
                    if ui.button("Cancel import").clicked() {
                        self.import = None;
                        if self.focused.is_none() {
                            self.close_preview();
                        }
                    }
                });
            }
            if !self.error.is_empty() {
                ui.colored_label(egui::Color32::GOLD, &self.error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let count = self.selected.len() + self.external.len();
                ui.label(format!("{count} selected to add"));
                if !self.external.is_empty() {
                    ui.weak("Includes external audio");
                }
                if ui.button("Clear").clicked() {
                    self.selected.clear();
                    self.external.clear();
                }
                if ui
                    .add_enabled(count > 0, egui::Button::new("Add Selected"))
                    .clicked()
                {
                    action = BrowserAction::Apply(self.wave_selection());
                }
                if ui.button("Cancel").clicked() {
                    action = BrowserAction::Close;
                }
            });
            action
        });
        if modal.should_close() {
            BrowserAction::Close
        } else {
            modal.inner
        }
    }
}

#[cfg(test)]
#[path = "sound_browser/tests.rs"]
mod tests;

fn source_name(source: &CatalogSource) -> String {
    let name = if source.packed {
        source.path.file_name()
    } else {
        source
            .path
            .parent()
            .and_then(Path::file_name)
            .or_else(|| source.path.file_name())
    };
    name.map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.path.display().to_string())
}

#[derive(Debug)]
enum BrowserAction {
    None,
    Close,
    Apply(Vec<String>),
    Import(PathBuf, bool),
}

fn copy_audio(source: &Path, destination: &Path) -> io::Result<()> {
    let temp = destination.with_file_name(format!(".sss-import-{}.tmp", std::process::id()));
    let mut input = fs::File::open(source)?;
    let mut output = fs::File::create_new(&temp)?;
    let result = (|| {
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        fs::hard_link(&temp, destination)
    })();
    let _ = fs::remove_file(temp);
    result
}

impl Studio {
    pub(super) fn show_sound_browser(&mut self, ctx: &egui::Context) {
        let browser = &mut self.sound_browser;
        if browser.target.is_none() {
            return;
        }
        if !browser.resolver.same_sources(&self.preview.resolver) {
            browser.reset_catalog(ctx, &self.preview.resolver);
        }
        let changed = browser.poll();
        if changed
            && let Some(id) = browser
                .reveal
                .as_deref()
                .and_then(|wave| browser.find_wave(wave))
        {
            browser.reveal = None;
            browser.focus(ctx, id, false);
        }
        match browser.draw(ctx, changed) {
            BrowserAction::None => {}
            BrowserAction::Close => browser.close(),
            BrowserAction::Apply(waves) => {
                let target = browser.target.take().unwrap();
                let result = self
                    .scripts
                    .iter_mut()
                    .find(|doc| doc.path == target.path)
                    .filter(|doc| !self.source.pending(doc))
                    .ok_or_else(|| {
                        io::Error::other("The target script is unavailable or has a Source draft")
                    })
                    .and_then(|doc| target.apply(doc, &waves));
                if let Err(error) = result {
                    self.errors.push(error.to_string());
                }
                browser.close();
                ctx.request_repaint();
            }
            BrowserAction::Import(source, default_location) => {
                if let Err(error) = self.import_audio(ctx, &source, default_location) {
                    self.sound_browser.error = error.to_string();
                }
            }
        }
    }

    fn import_audio(
        &mut self,
        ctx: &egui::Context,
        file: &Path,
        default_location: bool,
    ) -> io::Result<()> {
        let browser = &mut self.sound_browser;
        let script = browser.target.as_ref().unwrap().path.clone();
        let game = content::game_root(&script);
        let mod_sound = game.map(|game| game.join("sound"));
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("Unsupported audio filename"))?;
        let destination = if default_location {
            mod_sound
                .as_ref()
                .ok_or_else(|| io::Error::other("No mod folder detected"))?
                .join(name)
        } else {
            let mut dialog = rfd::FileDialog::new().set_file_name(name);
            if let Some(directory) = mod_sound.as_ref().filter(|path| path.is_dir()).or_else(|| {
                browser
                    .sources
                    .iter()
                    .find(|source| !source.packed)
                    .map(|source| &source.path)
            }) {
                dialog = dialog.set_directory(directory);
            }
            let Some(path) = dialog.save_file() else {
                return Ok(());
            };
            path
        };
        if destination.exists() {
            return Err(io::Error::other(
                "Choose a new filename. Existing audio will not be replaced.",
            ));
        }
        if audio_extension(&destination) != audio_extension(file) {
            return Err(io::Error::other("Keep the original audio extension."));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("Choose a sound folder"))?;
        let leaf = destination
            .file_name()
            .ok_or_else(|| io::Error::other("Choose an audio filename"))?;
        let physical_parent = parent.canonicalize().or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound && default_location {
                return Ok(parent.to_owned());
            }
            Err(error)
        })?;
        let physical = physical_parent.join(leaf);
        let root = browser.sources.iter().filter(|source| !source.packed)
            .filter_map(|source| source.path.canonicalize().ok())
            .find(|root| physical.starts_with(root))
            .or_else(|| {
                let sound = mod_sound.as_ref()?;
                let root = sound.canonicalize().unwrap_or_else(|_| sound.clone());
                let game = game?.canonicalize().ok()?;
                (root.starts_with(game) && physical.starts_with(&root)).then_some(root)
            })
            .ok_or_else(|| io::Error::other("Choose a location under the current mod's sound folder or a mounted sound folder."))?;
        let wave = physical
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .filter(|wave| !wave.contains(['"', '\r', '\n']) && wave_prefix(wave).is_empty())
            .ok_or_else(|| {
                io::Error::other("The destination cannot be written as a Source wave path.")
            })?
            .replace('\\', "/");
        if browser.resolver.describe(&wave).is_ok() {
            return Err(io::Error::other(
                "That wave path already exists in the mounted content. Choose a different filename.",
            ));
        }
        if rfd::MessageDialog::new()
            .set_title("Copy external audio")
            .set_description(format!(
                "Copy {} to {}?",
                file.display(),
                destination.display()
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            != rfd::MessageDialogResult::Yes
        {
            return Ok(());
        }
        if default_location && !parent.exists() {
            fs::create_dir(parent)?;
        }
        copy_audio(file, &destination)?;
        let previous = if browser.multiple() {
            browser.wave_selection()
        } else {
            Vec::new()
        };
        self.preview.reload_content(&script);
        self.asset_status.clear();
        browser.reset_catalog(ctx, &self.preview.resolver);
        browser.external = previous;
        let resolved = browser
            .resolver
            .wave_for_file(&destination)
            .ok_or_else(|| {
                io::Error::other(
                    "Audio was copied, but its Source path is not available in the current mounts.",
                )
            })?;
        browser.reveal = Some(resolved.clone());
        browser.add_external(resolved);
        browser.error.clear();
        Ok(())
    }
}
