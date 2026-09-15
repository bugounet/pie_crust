use super::{ACCENT, Desktop, MUTED, character_offset, markdown, workspace_loading};
use eframe::egui::{self, Color32, RichText};
use pie_crust_core::{Document, DocumentKind};
use std::{ops::Range, path::Path};

#[path = "editor_assistance.rs"]
mod assistance;
#[path = "editing.rs"]
pub(super) mod editing;

#[derive(Clone)]
struct DraggedTab {
    document: String,
    group: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum EditorLayout {
    #[default]
    Single,
    Columns,
    Rows,
}

#[derive(Default)]
struct EditorGroup {
    tabs: Vec<String>,
    active: Option<String>,
    preview: bool,
}

pub(super) struct EditorState {
    pub(super) layout: EditorLayout,
    pub(super) font_size: f32,
    groups: [EditorGroup; 2],
    active_group: usize,
    split_ratio: f32,
    transfer_focus: Option<(String, usize, usize)>,
    scroll_selection: Option<(String, usize, usize)>,
    assistance: assistance::AssistanceState,
    #[cfg(test)]
    code_rects: [Option<egui::Rect>; 2],
    #[cfg(test)]
    scroll_sizes: [Option<(egui::Vec2, egui::Vec2)>; 2],
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            layout: EditorLayout::Single,
            font_size: 14.0,
            groups: Default::default(),
            active_group: 0,
            split_ratio: 0.5,
            transfer_focus: None,
            scroll_selection: None,
            assistance: Default::default(),
            #[cfg(test)]
            code_rects: [None, None],
            #[cfg(test)]
            scroll_sizes: [None, None],
        }
    }
}

fn is_visible(document: &Document, worktree: &str) -> bool {
    document.worktree_id == worktree || document.kind == DocumentKind::Scratch
}

impl Desktop {
    /// The active code widget, independent of which popup currently has focus.
    pub(super) fn active_editor_id(&self) -> Option<egui::Id> {
        self.active_document
            .as_ref()
            .map(|id| code_id(id, self.editor_state.active_group))
    }

    /// UTF-8 character offsets, matching egui's cursor API (not byte offsets).
    pub(super) fn active_editor_selection(&self, ctx: &egui::Context) -> Option<Range<usize>> {
        let state = egui::TextEdit::load_state(ctx, self.active_editor_id()?)?;
        let range = state.cursor.char_range()?;
        Some(
            range.primary.index.0.min(range.secondary.index.0)
                ..range.primary.index.0.max(range.secondary.index.0),
        )
    }

    pub(super) fn select_in_active_editor(&mut self, ctx: &egui::Context, selection: Range<usize>) {
        let Some(id) = self.active_editor_id() else {
            return;
        };
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(selection.start),
                egui::text::CCursor::new(selection.end),
            )));
        egui::TextEdit::store_state(ctx, id, state);
        ctx.memory_mut(|memory| memory.request_focus(id));
        if let Some(document) = &self.active_document {
            self.editor_state.scroll_selection = Some((
                document.clone(),
                self.editor_state.active_group,
                selection.end,
            ));
        }
    }

    pub(super) fn open_document_beside(&mut self, path: &Path, line: usize, column: usize) {
        let single = self.editor_state.layout == EditorLayout::Single;
        self.set_editor_layout(EditorLayout::Columns);
        self.editor_state.active_group = if single {
            1
        } else {
            1 - self.editor_state.active_group
        };
        self.open_document(path, line, column);
    }

    /// Save before closing; a failed save leaves every requested tab in place.
    pub(super) fn close_editor_tabs(&mut self, ctx: &egui::Context, others: bool) -> bool {
        self.sync_editor_groups();
        let active = self.active_document.clone();
        if active.is_none() {
            return true;
        }
        let group = self.editor_state.active_group;
        let candidates: Vec<_> = if others {
            self.editor_state
                .groups
                .iter()
                .flat_map(|group| group.tabs.iter())
                .collect()
        } else {
            self.editor_state.groups[group].tabs.iter().collect()
        };
        let mut closing: Vec<_> = candidates
            .into_iter()
            .filter(|id| (Some(*id) == active.as_ref()) != others)
            .cloned()
            .collect();
        closing.sort();
        closing.dedup();
        if closing.iter().any(|id| self.unsynced.contains(id)) {
            self.error = Some(
                "La page contient une saisie en conflit ; conservez-la avant de la fermer.".into(),
            );
            return false;
        }
        let result = (|| -> anyhow::Result<()> {
            let mut state = self
                .shared
                .lock()
                .map_err(|_| anyhow::anyhow!("Workbench unavailable"))?;
            for id in &closing {
                if state.document(id).is_some_and(|document| document.dirty) {
                    state.save_document(id)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(format!("Fermeture interrompue : {error:#}"));
            return false;
        }
        self.poll();
        if others {
            for group in &mut self.editor_state.groups {
                group.tabs.retain(|id| Some(id) == active.as_ref());
                group.active = group.tabs.first().cloned();
            }
        } else {
            self.editor_state.groups[group]
                .tabs
                .retain(|id| !closing.contains(id));
        }
        if !others {
            self.editor_state.groups[group].active =
                self.editor_state.groups[group].tabs.last().cloned();
        }
        self.activate_editor_group(group);
        self.pending_focus = None;
        if others {
            self.set_editor_layout(EditorLayout::Single);
        }
        if self.editor_state.groups[group].tabs.is_empty()
            && self.editor_state.layout != EditorLayout::Single
        {
            self.activate_editor_group(1 - group);
            self.set_editor_layout(EditorLayout::Single);
        }
        if let Some(id) = self.active_editor_id() {
            ctx.memory_mut(|memory| memory.request_focus(id));
        }
        true
    }

    fn move_editor_tab(&mut self, document: &str, from: usize, to: usize) {
        if from == to {
            return;
        }
        self.editor_state.groups[from]
            .tabs
            .retain(|id| id != document);
        if self.editor_state.groups[from].active.as_deref() == Some(document) {
            self.editor_state.groups[from].active =
                self.editor_state.groups[from].tabs.last().cloned();
        }
        if !self.editor_state.groups[to]
            .tabs
            .iter()
            .any(|id| id == document)
        {
            self.editor_state.groups[to].tabs.push(document.to_owned());
        }
        self.editor_state.groups[to].active = Some(document.to_owned());
        self.editor_state.groups[to].preview = false;
        self.editor_state.transfer_focus = Some((document.to_owned(), from, to));
        self.pending_focus = None;
        self.activate_editor_group(to);
    }

    pub(super) fn set_editor_layout(&mut self, layout: EditorLayout) {
        self.sync_editor_groups();
        if layout == self.editor_state.layout {
            return;
        }
        if layout == EditorLayout::Single {
            if self.editor_state.active_group != 0
                && let Some(id) = &self.active_document
            {
                self.editor_state.transfer_focus =
                    Some((id.clone(), self.editor_state.active_group, 0));
            }
            let other_tabs = self.editor_state.groups[1].tabs.clone();
            for id in other_tabs {
                if !self.editor_state.groups[0].tabs.contains(&id) {
                    self.editor_state.groups[0].tabs.push(id);
                }
            }
            self.editor_state.groups[0].active = self.active_document.clone();
            self.editor_state.groups[0].preview =
                self.editor_state.groups[self.editor_state.active_group].preview;
            self.editor_state.active_group = 0;
            self.editor_state.groups[1] = EditorGroup::default();
        } else if self.editor_state.layout == EditorLayout::Single {
            // A split starts with a second view of the current document. Each
            // group can then select and open its own tabs, sharing the buffer.
            let current = self.active_document.clone();
            if let Some(id) = &current
                && !self.editor_state.groups[1].tabs.contains(id)
            {
                self.editor_state.groups[1].tabs.push(id.clone());
            }
            self.editor_state.groups[1].active = current;
        }
        self.editor_state.layout = layout;
    }

    fn sync_editor_groups(&mut self) {
        for group in &mut self.editor_state.groups {
            group
                .tabs
                .retain(|id| self.buffers.iter().any(|buffer| &buffer.id == id));
            if !self.buffers.iter().any(|buffer| {
                group.active.as_ref() == Some(&buffer.id) && is_visible(buffer, &self.worktree)
            }) {
                group.active = group.tabs.iter().find_map(|id| {
                    self.buffers
                        .iter()
                        .find(|buffer| &buffer.id == id && is_visible(buffer, &self.worktree))
                        .map(|buffer| buffer.id.clone())
                });
            }
        }
        if let Some(id) = &self.active_document
            && self
                .buffers
                .iter()
                .any(|buffer| &buffer.id == id && is_visible(buffer, &self.worktree))
        {
            let group = &mut self.editor_state.groups[self.editor_state.active_group];
            if !group.tabs.contains(id) {
                group.tabs.push(id.clone());
            }
            if group.active.as_ref() != Some(id) || self.pending_focus.is_some() {
                group.preview = false;
            }
            group.active = Some(id.clone());
        }
    }

    fn activate_editor_group(&mut self, group: usize) {
        self.editor_state.active_group = group;
        self.active_document = self.editor_state.groups[group].active.clone();
    }

    fn handle_editor_input(
        &mut self,
        ctx: &egui::Context,
        index: usize,
        group: usize,
        python: bool,
    ) -> bool {
        use egui::TextBuffer as _;
        let id = code_id(&self.buffers[index].id, group);
        if self.pending_focus.as_ref().is_some_and(|focus| {
            focus.document_id != self.buffers[index].id || self.editor_state.active_group != group
        }) || !ctx.memory(|memory| memory.has_focus(id))
        {
            return false;
        }
        self.activate_editor_group(group);
        if !python {
            self.clear_python_assistance();
            // Reserve IDE commands here as well: egui otherwise treats Ctrl+H
            // as backspace, which would turn navigation into a destructive edit.
            ctx.input_mut(|input| {
                for key in [egui::Key::H, egui::Key::Enter] {
                    for modifiers in [egui::Modifiers::COMMAND, egui::Modifiers::CTRL] {
                        let _ =
                            input.consume_shortcut(&egui::KeyboardShortcut::new(modifiers, key));
                    }
                }
            });
        }
        if python && self.handle_assistance_keys(ctx, index) {
            return false;
        }
        // Composition belongs to egui's IME state machine.
        if ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Ime(_)))
        }) {
            return false;
        }
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return false;
        };
        let Some(mut cursor) = state.cursor.char_range() else {
            return false;
        };
        let before = self.buffers[index].text.clone();
        let style = editing::TextStyle::detect(&before);
        let font = egui::FontId::monospace(self.editor_state.font_size);
        let now = ctx.input(|input| input.time);
        let mut undoer = state.undoer();
        undoer.feed_state(now, &(cursor, before.clone()));
        let text = &mut self.buffers[index].text;
        let events = ctx.input_mut(|input| std::mem::take(&mut input.events));
        let mut remaining = Vec::new();
        let mut handled = false;
        for event in events {
            let selection = cursor.primary.index.0.min(cursor.secondary.index.0)
                ..cursor.primary.index.0.max(cursor.secondary.index.0);
            let mut next = None;
            let mut consumed = false;
            match &event {
                egui::Event::Text(value) | egui::Event::Paste(value) if value.is_empty() => {
                    consumed = true;
                }
                egui::Event::Text(value) if value == "\n" || value == "\r" => {
                    consumed = true;
                }
                egui::Event::Text(value) | egui::Event::Paste(value) => {
                    if python && value == "(" {
                        next = editing::open_signature(text, selection.clone());
                    }
                    if python && value == ")" && next.is_none() {
                        next = editing::skip_closing_signature(text, selection.clone());
                    }
                    if next.is_none() {
                        next = Some(editing::replace(text, selection, &style.insertion(value)));
                    }
                    consumed = true;
                }
                egui::Event::Copy | egui::Event::Cut => {
                    if !cursor.is_empty() {
                        ctx.copy_text(cursor.slice_str(text).to_owned());
                        if matches!(event, egui::Event::Cut) {
                            next = Some(editing::replace(text, selection, ""));
                        }
                    }
                    consumed = true;
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let command = modifiers.command || modifiers.ctrl;
                    if command
                        && (*key == egui::Key::Y || (*key == egui::Key::Z && modifiers.shift))
                    {
                        if let Some((previous, value)) =
                            undoer.redo(&(cursor, text.clone())).cloned()
                        {
                            cursor = previous;
                            *text = value;
                        }
                        consumed = true;
                    } else if command && *key == egui::Key::Z {
                        if let Some((previous, value)) =
                            undoer.undo(&(cursor, text.clone())).cloned()
                        {
                            cursor = previous;
                            *text = value;
                        }
                        consumed = true;
                    } else if !command && *key == egui::Key::Tab && !modifiers.alt {
                        next = Some(editing::tab(
                            text,
                            selection.clone(),
                            modifiers.shift,
                            python,
                        ));
                        consumed = true;
                    } else if !command && *key == egui::Key::Enter {
                        next = Some(editing::enter(text, selection.clone(), python));
                        consumed = true;
                    } else if python
                        && modifiers.alt
                        && matches!(key, egui::Key::ArrowLeft | egui::Key::ArrowRight)
                        && let Some(reordered) = editing::reorder_parameter(
                            text,
                            selection.clone(),
                            *key == egui::Key::ArrowRight,
                        )
                    {
                        next = Some(reordered);
                        consumed = true;
                    } else if matches!(key, egui::Key::Backspace | egui::Key::Delete)
                        && !(*key == egui::Key::Delete
                            && modifiers.shift
                            && ctx.os() == egui::os::OperatingSystem::Windows)
                    {
                        let backwards = *key == egui::Key::Backspace;
                        if !command && !modifiers.alt {
                            next = editing::delete_crlf(text, selection.clone(), backwards);
                        }
                        if next.is_none() {
                            let deleted = if !cursor.is_empty() {
                                text.delete_selected(&cursor)
                            } else if modifiers.mac_cmd {
                                let galley = ctx.fonts_mut(|fonts| {
                                    fonts.layout_no_wrap(text.clone(), font.clone(), Color32::WHITE)
                                });
                                if backwards {
                                    text.delete_paragraph_before_cursor(&galley, &cursor)
                                } else {
                                    text.delete_paragraph_after_cursor(&galley, &cursor)
                                }
                            } else if modifiers.ctrl || modifiers.alt {
                                if backwards {
                                    text.delete_previous_word(cursor.primary)
                                } else {
                                    text.delete_next_word(cursor.primary)
                                }
                            } else if backwards {
                                text.delete_previous_char(cursor.primary)
                            } else {
                                text.delete_next_char(cursor.primary)
                            };
                            next = Some(deleted.index.0..deleted.index.0);
                        }
                        consumed = true;
                    } else if modifiers.ctrl && matches!(key, egui::Key::K | egui::Key::U) {
                        let galley = ctx.fonts_mut(|fonts| {
                            fonts.layout_no_wrap(text.clone(), font.clone(), Color32::WHITE)
                        });
                        let deleted = if *key == egui::Key::U {
                            text.delete_paragraph_before_cursor(&galley, &cursor)
                        } else {
                            text.delete_paragraph_after_cursor(&galley, &cursor)
                        };
                        next = Some(deleted.index.0..deleted.index.0);
                        consumed = true;
                    } else if matches!(
                        key,
                        egui::Key::ArrowLeft
                            | egui::Key::ArrowRight
                            | egui::Key::ArrowUp
                            | egui::Key::ArrowDown
                            | egui::Key::Home
                            | egui::Key::End
                    ) || (command && *key == egui::Key::A)
                        || (ctx.os() == egui::os::OperatingSystem::Mac
                            && modifiers.ctrl
                            && matches!(
                                key,
                                egui::Key::P
                                    | egui::Key::N
                                    | egui::Key::B
                                    | egui::Key::F
                                    | egui::Key::E
                            ))
                    {
                        let galley = ctx.fonts_mut(|fonts| {
                            fonts.layout_no_wrap(text.clone(), font.clone(), Color32::WHITE)
                        });
                        let mut modifiers = *modifiers;
                        modifiers.command |=
                            modifiers.ctrl && ctx.os() != egui::os::OperatingSystem::Mac;
                        consumed = cursor.on_key_press(ctx.os(), &galley, &modifiers, *key);
                    }
                }
                _ => {}
            }
            if let Some(selection) = next {
                cursor = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(selection.start),
                    egui::text::CCursor::new(selection.end),
                );
            }
            handled |= consumed;
            if !consumed {
                remaining.push(event);
            }
        }
        ctx.input_mut(|input| input.events = remaining);
        if handled {
            undoer.feed_state(now, &(cursor, text.clone()));
            state.set_undoer(undoer);
            state.cursor.set_char_range(Some(cursor));
            egui::TextEdit::store_state(ctx, id, state);
            self.editor_state.scroll_selection = Some((
                self.buffers[index].id.clone(),
                group,
                cursor.primary.index.0,
            ));
        }
        self.buffers[index].text != before
    }

    pub(super) fn editor(&mut self, ui: &mut egui::Ui) {
        self.sync_editor_groups();
        if let Some((document, from, to)) = self.editor_state.transfer_focus.take() {
            let previous = code_id(&document, from);
            let next = code_id(&document, to);
            if let Some(state) = egui::TextEdit::load_state(ui.ctx(), previous) {
                egui::TextEdit::store_state(ui.ctx(), next, state);
            }
            ui.memory_mut(|memory| memory.request_focus(next));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(ui.visuals().extreme_bg_color))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                if self.editor_state.layout == EditorLayout::Single {
                    self.editor_group(ui, 0);
                    return;
                }
                let columns = self.editor_state.layout == EditorLayout::Columns;
                let extent = if columns { rect.width() } else { rect.height() };
                let origin = if columns { rect.left() } else { rect.top() };
                let offset = origin + extent * self.editor_state.split_ratio;
                let divider = if columns {
                    egui::Rect::from_x_y_ranges(offset - 3.0..=offset + 3.0, rect.y_range())
                } else {
                    egui::Rect::from_x_y_ranges(rect.x_range(), offset - 3.0..=offset + 3.0)
                };
                let response = ui
                    .interact(divider, ui.id().with("editor_split"), egui::Sense::drag())
                    .on_hover_cursor(if columns {
                        egui::CursorIcon::ResizeHorizontal
                    } else {
                        egui::CursorIcon::ResizeVertical
                    });
                if response.dragged()
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let coordinate = if columns { pointer.x } else { pointer.y };
                    self.editor_state.split_ratio =
                        ((coordinate - origin) / extent.max(1.0)).clamp(0.2, 0.8);
                }
                ui.painter().rect_filled(
                    divider,
                    0.0,
                    if response.hovered() || response.dragged() {
                        ACCENT
                    } else {
                        ui.visuals().widgets.noninteractive.bg_stroke.color
                    },
                );
                let mut first = rect;
                let mut second = rect;
                if columns {
                    first.max.x = offset - 3.0;
                    second.min.x = offset + 3.0;
                } else {
                    first.max.y = offset - 3.0;
                    second.min.y = offset + 3.0;
                }
                for (index, bounds) in [first, second].into_iter().enumerate() {
                    let mut pane = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("editor_group", index))
                            .max_rect(bounds)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    pane.set_clip_rect(bounds.intersect(ui.clip_rect()));
                    self.editor_group(&mut pane, index);
                }
                ui.allocate_rect(rect, egui::Sense::hover());
            });
        self.show_editor_assistance(ui.ctx());
    }

    fn editor_group(&mut self, ui: &mut egui::Ui, group_index: usize) {
        ui.style_mut().text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::monospace(self.editor_state.font_size),
        );
        if ui.rect_contains_pointer(ui.max_rect()) && ui.input(|input| input.pointer.any_pressed())
        {
            self.activate_editor_group(group_index);
        }
        let tabs = self.editor_state.groups[group_index].tabs.clone();
        let mut selected = None;
        let mut split = None;
        let mut close = None;
        let mut move_to = None;
        if ui.rect_contains_pointer(ui.max_rect())
            && let Some(payload) = egui::DragAndDrop::payload::<DraggedTab>(ui.ctx())
            && payload.group != group_index
        {
            ui.painter().rect_stroke(
                ui.max_rect().shrink(2.0),
                0.0,
                egui::Stroke::new(2.0, ACCENT),
                egui::StrokeKind::Inside,
            );
            if ui.input(|input| input.pointer.any_released()) {
                egui::DragAndDrop::clear_payload(ui.ctx());
                self.move_editor_tab(&payload.document, payload.group, group_index);
            }
        }
        egui::Frame::NONE
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::horizontal()
                    .id_salt(("document_tabs", group_index))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for id in &tabs {
                                let Some(buffer) = self.buffers.iter().find(|buffer| {
                                    &buffer.id == id && is_visible(buffer, &self.worktree)
                                }) else {
                                    continue;
                                };
                                let name = buffer
                                    .path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy();
                                let label = format!(
                                    "{}{}{}",
                                    if buffer.kind == DocumentKind::Scratch {
                                        "✎ "
                                    } else {
                                        ""
                                    },
                                    name,
                                    if buffer.dirty { " •" } else { "" }
                                );
                                let active = self.editor_state.groups[group_index].active.as_ref()
                                    == Some(id);
                                let response = ui
                                    .add(
                                        egui::Button::selectable(active, label)
                                            .sense(egui::Sense::click_and_drag()),
                                    )
                                    .on_hover_text(buffer.path.display().to_string());
                                response.dnd_set_drag_payload(DraggedTab {
                                    document: id.clone(),
                                    group: group_index,
                                });
                                response.context_menu(|ui| {
                                    if ui.button("Scinder verticalement").clicked() {
                                        selected = Some(id.clone());
                                        split = Some(EditorLayout::Columns);
                                        ui.close();
                                    }
                                    if ui.button("Scinder horizontalement").clicked() {
                                        selected = Some(id.clone());
                                        split = Some(EditorLayout::Rows);
                                        ui.close();
                                    }
                                    if self.editor_state.layout != EditorLayout::Single
                                        && ui.button("Déplacer vers l’autre groupe").clicked()
                                    {
                                        move_to = Some(id.clone());
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button("Fermer · Ctrl+W").clicked() {
                                        selected = Some(id.clone());
                                        close = Some(false);
                                        ui.close();
                                    }
                                    if ui.button("Fermer les autres · Ctrl+Shift+W").clicked() {
                                        selected = Some(id.clone());
                                        close = Some(true);
                                        ui.close();
                                    }
                                });
                                if active && self.editor_state.active_group == group_index {
                                    ui.painter().hline(
                                        response.rect.x_range(),
                                        response.rect.bottom() + 3.0,
                                        egui::Stroke::new(2.0, ACCENT),
                                    );
                                }
                                if response.clicked() {
                                    selected = Some(id.clone());
                                }
                            }
                            if tabs.is_empty() {
                                ui.label(RichText::new("Éditeur").color(MUTED));
                            }
                            ui.menu_button("⋮", |ui| {
                                if ui.button("Scinder verticalement").clicked() {
                                    split = Some(EditorLayout::Columns);
                                    ui.close();
                                }
                                if ui.button("Scinder horizontalement").clicked() {
                                    split = Some(EditorLayout::Rows);
                                    ui.close();
                                }
                                if self.editor_state.layout != EditorLayout::Single
                                    && ui.button("Réunir les groupes").clicked()
                                {
                                    split = Some(EditorLayout::Single);
                                    ui.close();
                                }
                            });
                        });
                    });
            });
        if let Some(id) = selected {
            ui.memory_mut(|memory| memory.request_focus(code_id(&id, group_index)));
            self.editor_state.groups[group_index].active = Some(id);
            self.editor_state.groups[group_index].preview = false;
            self.pending_focus = None;
            self.activate_editor_group(group_index);
        }
        if let Some(layout) = split {
            self.activate_editor_group(group_index);
            self.set_editor_layout(layout);
        }
        if let Some(others) = close {
            self.close_editor_tabs(ui.ctx(), others);
        }
        if let Some(document) = move_to {
            self.move_editor_tab(&document, group_index, 1 - group_index);
        }
        let Some(index) = self.buffers.iter().position(|buffer| {
            Some(&buffer.id) == self.editor_state.groups[group_index].active.as_ref()
                && is_visible(buffer, &self.worktree)
        }) else {
            ui.add_space((ui.available_height() * 0.3).max(24.0));
            if self.indexing {
                workspace_loading::show_loading(
                    ui,
                    &self.active_root().unwrap_or_default(),
                    "Indexation des fichiers…",
                    None,
                );
                return;
            }
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Votre code, votre espace.")
                        .size(24.0)
                        .color(ACCENT),
                );
                ui.add_space(12.0);
                ui.label("Ouvrez un fichier depuis l’explorateur.");
            });
            return;
        };
        let extension = self.buffers[index]
            .path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase();
        let is_markdown = matches!(extension.as_str(), "md" | "markdown");
        let is_python = matches!(extension.as_str(), "py" | "pyi" | "pyw");
        let path = self.buffers[index].path.display().to_string();
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            let remaining =
                (ui.available_width() - if is_markdown { 230.0 } else { 100.0 }).max(40.0);
            ui.add_sized(
                [remaining, 20.0],
                egui::Label::new(RichText::new(path).size(12.0).color(MUTED)).truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Enregistrer").clicked() {
                    self.activate_editor_group(group_index);
                    self.save(false);
                }
                if is_markdown
                    && ui
                        .toggle_value(
                            &mut self.editor_state.groups[group_index].preview,
                            "Aperçu Markdown",
                        )
                        .clicked()
                {
                    self.activate_editor_group(group_index);
                }
            });
        });
        if is_python {
            self.show_fixture_strip(ui, index);
        }
        let document_id = self.buffers[index].id.clone();
        if self.unsynced.contains(&document_id) {
            ui.colored_label(
                Color32::LIGHT_RED,
                "Saisie en conflit conservée en mémoire. Elle n’a pas remplacé la version partagée.",
            );
            if ui
                .button("Conserver ma copie et recharger la version partagée")
                .clicked()
            {
                match self.recover_text(&self.buffers[index].text) {
                    Ok(path) => {
                        self.unsynced.remove(&document_id);
                        self.error = Some(format!(
                            "Votre copie a été conservée dans {}",
                            path.display()
                        ));
                        if let Ok(state) = self.shared.lock()
                            && let Some(document) = state.document(&document_id)
                        {
                            self.buffers[index] = document.clone();
                        }
                    }
                    Err(error) => {
                        self.error = Some(format!("Impossible de conserver la copie : {error:#}"))
                    }
                }
            }
        }
        if self.editor_state.groups[group_index].preview && is_markdown {
            egui::ScrollArea::vertical()
                .id_salt(("markdown_preview", group_index, &document_id))
                .show(ui, |ui| markdown::show(ui, &self.buffers[index].text));
            return;
        }
        let input_changed = self.handle_editor_input(
            ui.ctx(),
            index,
            group_index,
            matches!(extension.as_str(), "py" | "pyi" | "pyw"),
        );
        let occurrences = if is_python {
            self.editor_occurrences(index, ui.ctx(), group_index)
        } else {
            Vec::new()
        };
        let buffer = &mut self.buffers[index];
        let editor_id = code_id(&buffer.id, group_index);
        // Inactive panes must leave a pending navigation request untouched,
        // including when both panes display the exact same document.
        let target = if self.editor_state.active_group == group_index
            && self
                .pending_focus
                .as_ref()
                .is_some_and(|focus| focus.document_id == buffer.id)
        {
            self.pending_focus.take()
        } else {
            None
        };
        let cursor_index = target
            .as_ref()
            .map(|focus| character_offset(&buffer.text, focus.line, focus.column));
        if let Some(position) = cursor_index {
            let mut state = egui::TextEdit::load_state(ui.ctx(), editor_id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(position),
                )));
            egui::TextEdit::store_state(ui.ctx(), editor_id, state);
            ui.memory_mut(|memory| memory.request_focus(editor_id));
        }
        let scroll_index = cursor_index.or_else(|| {
            if self
                .editor_state
                .scroll_selection
                .as_ref()
                .is_some_and(|(id, group, _)| id == &buffer.id && *group == group_index)
            {
                self.editor_state
                    .scroll_selection
                    .take()
                    .map(|(_, _, position)| position)
            } else {
                None
            }
        });
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
            let mut job = highlighted_code(ui, text.as_str(), &extension);
            highlight_occurrences(&mut job, &occurrences);
            ui.fonts_mut(|fonts| fonts.layout_job(job))
        };
        let mut changed = input_changed;
        let mut focused = false;
        let viewport_size = ui.available_size();
        let digits = buffer
            .text
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            .saturating_add(1)
            .to_string()
            .len()
            .max(3);
        let gutter_width = digits as f32 * font.size * 0.65 + 24.0;
        let scroll = egui::ScrollArea::both()
            .id_salt(("editor_scroll", group_index, &buffer.id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.horizontal_top(|ui| {
                    let gutter = ui.allocate_space(egui::vec2(gutter_width, 1.0)).1;
                    let output = egui::TextEdit::multiline(&mut buffer.text)
                        .id(editor_id)
                        .code_editor()
                        .font(font.clone())
                        .desired_width((viewport_size.x - gutter_width - 12.0).max(80.0))
                        .desired_rows(1)
                        .min_size(egui::vec2(0.0, viewport_size.y.max(0.0)))
                        .layouter(&mut layouter)
                        .frame(egui::Frame::NONE)
                        .show(ui);
                    changed |= output.response.changed();
                    focused = output.response.has_focus();
                    if focused {
                        self.editor_state.assistance.anchor = output.cursor_range.map(|range| {
                            output
                                .galley
                                .pos_from_cursor(range.primary)
                                .translate(output.galley_pos.to_vec2())
                        });
                    }
                    #[cfg(test)]
                    {
                        self.editor_state.code_rects[group_index] = Some(output.response.rect);
                    }
                    for (line, row) in output.galley.rows.iter().enumerate() {
                        let y = output.galley_pos.y + row.pos.y;
                        if y + row.size.y < ui.clip_rect().top() || y > ui.clip_rect().bottom() {
                            continue;
                        }
                        ui.painter().text(
                            egui::pos2(gutter.right() - 14.0, y),
                            egui::Align2::RIGHT_TOP,
                            (line + 1).to_string(),
                            font.clone(),
                            MUTED,
                        );
                    }
                    if let Some(position) = scroll_index {
                        let rect = output
                            .galley
                            .pos_from_cursor(egui::text::CCursor::new(position))
                            .translate(output.galley_pos.to_vec2());
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                    }
                });
            });
        #[cfg(test)]
        {
            self.editor_state.scroll_sizes[group_index] =
                Some((scroll.content_size, scroll.inner_rect.size()));
        }
        #[cfg(not(test))]
        let _ = scroll;
        if focused && self.pending_focus.is_none() {
            self.activate_editor_group(group_index);
        }
        if changed {
            self.commit_buffer_edit(index);
        }
        if focused && is_python {
            self.update_editor_assistance(ui.ctx(), index, changed);
        } else if focused {
            self.clear_python_assistance();
        }
    }
}

fn code_id(document: &str, group: usize) -> egui::Id {
    if group == 0 {
        egui::Id::new(("code", document))
    } else {
        egui::Id::new(("code", document, group))
    }
}

fn highlighted_code(ui: &egui::Ui, text: &str, extension: &str) -> egui::text::LayoutJob {
    // Resolve the font and palette from the active UI style, including its
    // scaling. Use Python's grammar explicitly for both sources and stubs.
    let language = if matches!(extension, "py" | "pyi" | "pyw") {
        "Python"
    } else {
        extension
    };
    let theme = egui_extras::syntax_highlighting::CodeTheme::from_style(ui.style());
    let mut job =
        egui_extras::syntax_highlighting::highlight(ui.ctx(), ui.style(), &theme, text, language);
    job.wrap.max_width = f32::INFINITY;
    job
}

fn highlight_occurrences(job: &mut egui::text::LayoutJob, occurrences: &[Range<usize>]) {
    if occurrences.is_empty() {
        return;
    }
    let mut sections = Vec::new();
    for section in &job.sections {
        let mut boundaries = vec![section.byte_range.start.0, section.byte_range.end.0];
        for occurrence in occurrences {
            if occurrence.start < section.byte_range.end.0
                && occurrence.end > section.byte_range.start.0
            {
                boundaries.push(occurrence.start.max(section.byte_range.start.0));
                boundaries.push(occurrence.end.min(section.byte_range.end.0));
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        for pair in boundaries.windows(2) {
            let mut part = section.clone();
            part.byte_range.start.0 = pair[0];
            part.byte_range.end.0 = pair[1];
            if occurrences
                .iter()
                .any(|range| range.start <= pair[0] && pair[1] <= range.end)
            {
                part.format.background = Color32::from_rgb(53, 73, 81);
            }
            sections.push(part);
        }
    }
    job.sections = sections;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pie_crust_core::Workbench;
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };

    fn render(app: &mut Desktop, ctx: &egui::Context, events: Vec<egui::Event>) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.editor(ui),
        )
        .drop_without_applying_deltas();
    }

    fn render_full(app: &mut Desktop, ctx: &egui::Context, events: Vec<egui::Event>) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.render(ui),
        )
        .drop_without_applying_deltas();
    }

    fn click_code(app: &Desktop, group: usize) -> Vec<egui::Event> {
        let position =
            app.editor_state.code_rects[group].unwrap().left_top() + egui::vec2(8.0, 8.0);
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
        ]
    }

    #[test]
    fn text_navigation_backspace_and_enter_in_one_frame_keep_event_order() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("example.py"), "x").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        app.open_document(Path::new("example.py"), 1, 2);
        render(&mut app, &ctx, vec![]);
        let mut events = vec![egui::Event::Text("a".into())];
        events.extend(key(egui::Key::Enter, egui::Modifiers::NONE));
        render(&mut app, &ctx, events);
        assert_eq!(app.buffers[0].text, "xa\n");
        let mut events = key(egui::Key::ArrowLeft, egui::Modifiers::NONE);
        events.push(egui::Event::Text("z".into()));
        events.extend(key(egui::Key::Backspace, egui::Modifiers::NONE));
        events.push(egui::Event::Text("b".into()));
        render(&mut app, &ctx, events);
        assert_eq!(app.buffers[0].text, "xab\n");
    }

    #[test]
    fn closed_tabs_do_not_reappear_when_splitting_a_merged_group() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one.py"), "one = 1\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        app.open_document(Path::new("one.py"), 1, 1);
        render(&mut app, &ctx, vec![]);
        app.set_editor_layout(EditorLayout::Columns);
        app.set_editor_layout(EditorLayout::Single);
        assert!(app.close_editor_tabs(&ctx, false));
        app.set_editor_layout(EditorLayout::Columns);
        assert!(
            app.editor_state
                .groups
                .iter()
                .all(|group| group.tabs.is_empty())
        );
    }

    #[test]
    fn typing_signature_tab_return_body_and_paste_preserve_two_spaces_and_crlf() {
        let directory = tempfile::tempdir().unwrap();
        let source = "class C:\r\n  def compute";
        std::fs::write(directory.path().join("example.py"), source).unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        app.open_document(Path::new("example.py"), 2, 14);
        render(&mut app, &ctx, vec![]);
        app.select_in_active_editor(&ctx, source.chars().count()..source.chars().count());
        render(&mut app, &ctx, vec![egui::Event::Text("(".into())]);
        assert_eq!(
            app.buffers[0].text,
            "class C:\r\n  def compute(self, ) -> None:"
        );
        render(&mut app, &ctx, vec![egui::Event::Text("value: int".into())]);
        render(&mut app, &ctx, key(egui::Key::Tab, egui::Modifiers::NONE));
        let selection = app.active_editor_selection(&ctx).unwrap();
        assert_eq!(
            &app.buffers[0].text[editing::byte_range(&app.buffers[0].text, selection)],
            "None"
        );
        render(&mut app, &ctx, key(egui::Key::Enter, egui::Modifiers::NONE));
        assert!(app.buffers[0].text.ends_with("\r\n    "));
        render(
            &mut app,
            &ctx,
            vec![egui::Event::Paste("return value\n  # pasted".into())],
        );
        let text = &app.buffers[0].text;
        assert!(text.ends_with("return value\r\n  # pasted"));
        assert!(!text.replace("\r\n", "").contains('\n'));
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .document(&app.buffers[0].id)
                .unwrap()
                .text,
            *text
        );
    }

    #[test]
    fn close_saves_dirty_buffer_and_refuses_external_conflict() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one.py"), "one = 1\n").unwrap();
        std::fs::write(directory.path().join("two.py"), "two = 2\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        app.open_document(Path::new("one.py"), 1, 1);
        render(&mut app, &ctx, vec![]);
        let first = app.active_document.clone().unwrap();
        app.open_document(Path::new("two.py"), 1, 1);
        render(&mut app, &ctx, vec![]);
        app.buffers[1].text = "two = 20\n".into();
        app.commit_buffer_edit(1);
        assert!(app.close_editor_tabs(&ctx, false));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("two.py")).unwrap(),
            "two = 20\n"
        );
        assert_eq!(app.active_document, Some(first));
        app.buffers[0].text = "one = 10\n".into();
        app.commit_buffer_edit(0);
        std::fs::write(directory.path().join("one.py"), "one = 100\n").unwrap();
        assert!(!app.close_editor_tabs(&ctx, false));
        assert_eq!(app.editor_state.groups[0].tabs.len(), 1);
        assert_eq!(app.buffers[0].text, "one = 10\n");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("one.py")).unwrap(),
            "one = 100\n"
        );
    }

    #[test]
    fn dropping_tab_into_other_pane_moves_view_and_keeps_shared_buffer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one.py"), "one = 1\n").unwrap();
        std::fs::write(directory.path().join("two.py"), "two = 2\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        app.open_document(Path::new("one.py"), 1, 1);
        render(&mut app, &ctx, vec![]);
        let first = app.active_document.clone().unwrap();
        app.open_document_beside(Path::new("two.py"), 1, 1);
        render(&mut app, &ctx, vec![]);
        let target = app.editor_state.code_rects[1].unwrap().center();
        egui::DragAndDrop::set_payload(
            &ctx,
            DraggedTab {
                document: first.clone(),
                group: 0,
            },
        );
        render(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(target),
                egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(!app.editor_state.groups[0].tabs.contains(&first));
        assert!(app.editor_state.groups[1].tabs.contains(&first));
        assert_eq!(app.active_document, Some(first));
        assert_eq!(app.buffers.len(), 2);
    }

    #[test]
    fn close_other_pages_keeps_only_current_document_across_split_groups() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one.py"), "one = 1\n").unwrap();
        std::fs::write(directory.path().join("two.py"), "two = 2\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        app.open_document(Path::new("one.py"), 1, 1);
        render(&mut app, &ctx, vec![]);
        app.open_document_beside(Path::new("two.py"), 1, 1);
        render(&mut app, &ctx, vec![]);
        let current = app.active_document.clone().unwrap();
        assert!(app.close_editor_tabs(&ctx, true));
        render(&mut app, &ctx, vec![]);
        assert_eq!(app.editor_state.layout, EditorLayout::Single);
        assert_eq!(app.editor_state.groups[0].tabs, vec![current.clone()]);
        assert!(
            app.editor_state.groups[1]
                .tabs
                .iter()
                .all(|id| id == &current)
        );
        assert_eq!(app.active_document, Some(current));
    }

    #[test]
    fn python_grammar_colors_comments_strings_keywords_and_stubs() {
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        ctx.run_ui(Default::default(), |ui| {
            let source = "# annotation\ndef charge(value: str) -> str:\n    return \"invoice\"\n";
            let python = highlighted_code(ui, source, "py");
            let stub = highlighted_code(ui, source, "pyi");
            assert_eq!(python, stub);
            let color_at = |needle: &str| {
                let index = source.find(needle).unwrap();
                python
                    .sections
                    .iter()
                    .find(|section| {
                        section.byte_range.start.0 <= index && index < section.byte_range.end.0
                    })
                    .unwrap()
                    .format
                    .color
            };
            assert_ne!(color_at("#"), color_at("def"));
            assert_ne!(color_at("def"), color_at("invoice"));
            assert_ne!(color_at("invoice"), color_at("charge"));
            assert!(python.sections.iter().all(|section| {
                section.format.font_id == egui::TextStyle::Monospace.resolve(ui.style())
                    && section.format.color.a() == 255
            }));
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn split_views_share_edits_and_route_navigation_to_the_active_group() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one.py"), "first = 1\n").unwrap();
        std::fs::write(directory.path().join("two.py"), "second = 2\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        app.open_document(Path::new("one.py"), 1, 1);
        let first = app.active_document.clone().unwrap();
        render(&mut app, &ctx, vec![]);
        app.set_editor_layout(EditorLayout::Columns);
        render(&mut app, &ctx, vec![]);
        let pane = app.editor_state.code_rects[1].unwrap();
        let position = pane.left_top() + egui::vec2(8.0, 8.0);
        render(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        render(&mut app, &ctx, vec![egui::Event::Text("typed".into())]);
        assert_eq!(app.editor_state.active_group, 1);
        assert!(
            shared
                .lock()
                .unwrap()
                .document(&first)
                .unwrap()
                .text
                .contains("typed")
        );
        app.open_document(Path::new("two.py"), 1, 4);
        let second = app.active_document.clone().unwrap();
        render(&mut app, &ctx, vec![]);
        assert_eq!(app.editor_state.groups[0].active.as_ref(), Some(&first));
        assert_eq!(app.editor_state.groups[1].active.as_ref(), Some(&second));
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(code_id(&second, 1))
        );
        app.set_editor_layout(EditorLayout::Rows);
        render(&mut app, &ctx, vec![]);
        assert!(
            app.editor_state.code_rects[0].unwrap().top()
                < app.editor_state.code_rects[1].unwrap().top()
        );
        app.set_editor_layout(EditorLayout::Single);
        render(&mut app, &ctx, vec![]);
        assert_eq!(app.editor_state.active_group, 0);
        assert_eq!(app.active_document.as_ref(), Some(&second));
        assert!(app.editor_state.groups[0].tabs.contains(&first));
        assert!(app.editor_state.groups[0].tabs.contains(&second));
    }

    #[test]
    fn explorer_follows_mouse_focus_between_two_distinct_editor_groups() {
        let directory = tempfile::tempdir().unwrap();
        let paths = [Path::new("app/first.py"), Path::new("tests/second.py")];
        for path in paths {
            std::fs::create_dir_all(directory.path().join(path.parent().unwrap())).unwrap();
            std::fs::write(directory.path().join(path), "value = 1\n").unwrap();
        }
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.files = paths
            .iter()
            .map(|path| pie_crust_core::FileEntry {
                path: (*path).to_owned(),
            })
            .collect();
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        ctx.style_mut_of(egui::Theme::Dark, |style| style.animation_time = 0.0);
        app.open_document(paths[0], 1, 1);
        render_full(&mut app, &ctx, vec![]);
        app.set_editor_layout(EditorLayout::Columns);
        render_full(&mut app, &ctx, vec![]);
        let events = click_code(&app, 1);
        render_full(&mut app, &ctx, events);
        app.open_document(paths[1], 1, 1);
        render_full(&mut app, &ctx, vec![]);
        for group in [0, 1] {
            let events = click_code(&app, group);
            render_full(&mut app, &ctx, events);
            // The explorer precedes the editor in a frame, so reveal is
            // applied on the next repaint after the editor receives focus.
            render_full(&mut app, &ctx, vec![]);
            assert_eq!(app.editor_state.active_group, group);
            assert_eq!(
                app.chrome.revealed,
                Some((app.worktree.clone(), paths[group].to_owned()))
            );
            let active = app
                .buffers
                .iter()
                .find(|document| Some(&document.id) == app.active_document.as_ref())
                .unwrap();
            assert_eq!(active.path, paths[group]);
        }
    }

    #[test]
    fn short_scratches_fill_the_pane_without_extra_vertical_content() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        {
            let mut state = shared.lock().unwrap();
            state.create_scratch("notes.pyi", "value: str\n").unwrap();
            state.focus_scratch("notes.pyi", 1, 1).unwrap();
        }
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.poll();
        let scratch = app
            .buffers
            .iter()
            .find(|document| document.kind == DocumentKind::Scratch)
            .unwrap()
            .clone();
        assert!(is_visible(&scratch, "another-worktree"));
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        app.editor_state.font_size = 22.0;
        for layout in [
            EditorLayout::Single,
            EditorLayout::Columns,
            EditorLayout::Rows,
        ] {
            app.set_editor_layout(layout);
            for _ in 0..2 {
                render(&mut app, &ctx, vec![]);
            }
            let count = if layout == EditorLayout::Single { 1 } else { 2 };
            for group in 0..count {
                assert_eq!(
                    app.editor_state.groups[group].active.as_ref(),
                    Some(&scratch.id)
                );
                let (content, viewport) = app.editor_state.scroll_sizes[group].unwrap();
                assert!(
                    content.y <= viewport.y + 1.0,
                    "Short scratch introduced extra vertical content: {content:?} in {viewport:?}"
                );
                assert!(app.editor_state.code_rects[group].unwrap().height() > 100.0);
            }
        }
    }
}
