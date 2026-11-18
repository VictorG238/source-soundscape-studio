use eframe::egui::{
    self,
    text::{CCursor, CCursorRange},
};
use soundscape_core::Script;
use std::{collections::HashMap, path::PathBuf};

#[derive(Default)]
pub(super) struct SourceEditor {
    buffers: HashMap<PathBuf, Buffer>,
}

struct Buffer {
    text: String,
    revision: u64,
    dirty: bool,
    error: Option<String>,
    query: String,
    next_match: usize,
}

impl Buffer {
    fn new(doc: &Script) -> Self {
        Self {
            text: doc.source_text(),
            revision: doc.revision(),
            dirty: false,
            error: None,
            query: String::new(),
            next_match: 0,
        }
    }
}

impl SourceEditor {
    pub(super) fn pending(&self, doc: &Script) -> bool {
        self.buffers
            .get(&doc.path)
            .is_some_and(|buffer| buffer.dirty)
    }

    pub(super) fn has_drafts(&self) -> bool {
        self.buffers.values().any(|buffer| buffer.dirty)
    }

    pub(super) fn discard(&mut self, doc: &Script) {
        self.buffers.remove(&doc.path);
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, doc: &mut Script) {
        let buffer = self
            .buffers
            .entry(doc.path.clone())
            .or_insert_with(|| Buffer::new(doc));
        if !buffer.dirty && buffer.revision != doc.revision() {
            buffer.text = doc.source_text();
            buffer.revision = doc.revision();
            buffer.next_match = 0;
        }
        let mut found = None;
        let find_id = ui.make_persistent_id((&doc.path, "source-find"));
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            ui.memory_mut(|memory| memory.request_focus(find_id));
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(buffer.dirty, egui::Button::new("Apply changes"))
                .clicked()
            {
                let result = if buffer.revision == doc.revision() {
                    doc.apply_source(&buffer.text)
                } else {
                    Err(std::io::Error::other(
                        "The document changed while this draft was open. Copy your edits before discarding the draft.",
                    ))
                };
                match result {
                    Ok(()) => {
                        buffer.text = doc.source_text();
                        buffer.dirty = false;
                        buffer.error = None;
                        buffer.revision = doc.revision();
                    }
                    Err(error) => buffer.error = Some(error.to_string()),
                }
            }
            if ui
                .add_enabled(buffer.dirty, egui::Button::new("Discard draft"))
                .clicked()
            {
                *buffer = Buffer::new(doc);
            }
            let query = ui.add(
                egui::TextEdit::singleline(&mut buffer.query)
                    .id(find_id)
                    .hint_text("Find text")
                    .desired_width(160.),
            );
            if query.changed() {
                buffer.next_match = 0;
            }
            let clicked = ui
                .add_enabled(!buffer.query.is_empty(), egui::Button::new("Find next"))
                .clicked();
            if !buffer.query.is_empty()
                && (clicked
                    || (query.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))))
            {
                let at = buffer
                    .text
                    .get(buffer.next_match..)
                    .and_then(|tail| tail.find(&buffer.query))
                    .map(|offset| buffer.next_match + offset)
                    .or_else(|| buffer.text.find(&buffer.query));
                if let Some(at) = at {
                    let end = at + buffer.query.len();
                    buffer.next_match = end;
                    found = Some((at, end));
                } else {
                    ui.weak("No matches");
                }
            }
        });
        if let Some(error) = &buffer.error {
            ui.colored_label(egui::Color32::GOLD, error);
        } else if buffer.dirty {
            ui.weak("Apply changes to update the document and its Undo history.");
        } else if let Some(error) = &doc.tree.error {
            let (line, col) = doc.location(error.offset);
            ui.colored_label(
                egui::Color32::GOLD,
                format!("Line {line}, column {col} · {}", error.message),
            );
        }
        let id = ui.make_persistent_id((&doc.path, buffer.revision, "source-edit"));
        if let Some((at, end)) = found {
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            let start = CCursor::new(buffer.text[..at].chars().count());
            let finish = CCursor::new(buffer.text[..end].chars().count());
            state
                .cursor
                .set_char_range(Some(CCursorRange::two(start, finish)));
            state.store(ui.ctx(), id);
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let output = egui::TextEdit::multiline(&mut buffer.text)
                    .id(id)
                    .code_editor()
                    .desired_width(ui.available_width())
                    .show(ui);
                if output.response.changed() {
                    buffer.dirty = buffer.text != doc.source_text();
                    buffer.error = None;
                    buffer.next_match = 0;
                }
                if found.is_some()
                    && let Some(range) = output.cursor_range
                {
                    let rect = output
                        .galley
                        .pos_from_cursor(range.primary)
                        .translate(output.galley_pos.to_vec2());
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                }
            });
    }
}
