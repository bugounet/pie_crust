use super::{Desktop, MUTED, code_id, editing};
use eframe::egui::{self, RichText};
use pie_crust_core::python::{
    CallHierarchy, Completion, FixtureMatch, PythonSource, PythonWorkspace, QuickFix, TextEdit,
};
use std::{ops::Range, sync::Arc};

#[derive(Default)]
pub(super) struct AssistanceState {
    pub(super) anchor: Option<egui::Rect>,
    local_snapshot: Option<(String, u64, Arc<PythonWorkspace>)>,
    last_context: Option<(String, u64, usize, usize)>,
    completions: Vec<Completion>,
    selected: usize,
    completion_document: String,
    completion_source: String,
    completion_enabled: bool,
    quick_fixes: Option<FixPopup>,
    hierarchy: Option<Option<CallHierarchy>>,
    fixtures: Vec<FixtureMatch>,
    fixtures_document: String,
    restore_focus: bool,
}

struct FixPopup {
    document: String,
    source: String,
    fixes: Vec<QuickFix>,
    selected: usize,
}

impl Desktop {
    pub(crate) fn clear_python_assistance(&mut self) {
        self.editor_state.assistance = AssistanceState::default();
    }
    /// Explicit transformations refresh all open Python buffers before asking
    /// the engine for byte offsets. Typing only parses the active file when the
    /// background worktree snapshot has not caught up yet.
    pub(crate) fn editor_python_snapshot(
        &mut self,
        index: usize,
        project: bool,
    ) -> anyhow::Result<Arc<PythonWorkspace>> {
        let document = &self.buffers[index];
        let root = self.active_root();
        let excluded = |path: &std::path::Path| {
            root.as_ref()
                .is_some_and(|root| pie_crust_core::is_python_environment_path(root, path))
        };
        if let Some(snapshot) = &self.python_workspace
            && snapshot.source(&document.path) == Some(document.text.as_str())
            && (!project
                || self
                    .buffers
                    .iter()
                    .filter(|buffer| buffer.worktree_id == self.worktree && !excluded(&buffer.path))
                    .all(|buffer| {
                        !matches!(
                            buffer.path.extension().and_then(|value| value.to_str()),
                            Some("py" | "pyi" | "pyw")
                        ) || snapshot.source(&buffer.path) == Some(buffer.text.as_str())
                    }))
        {
            return Ok(snapshot.clone());
        }
        if !project
            && let Some((id, version, snapshot)) = &self.editor_state.assistance.local_snapshot
            && id == &document.id
            && *version == document.version
            && snapshot.source(&document.path) == Some(document.text.as_str())
        {
            return Ok(snapshot.clone());
        }
        let mut sources = if project {
            self.python_workspace
                .as_ref()
                .map_or_else(Vec::new, |snapshot| snapshot.sources())
        } else {
            Vec::new()
        };
        for buffer in &self.buffers {
            if !matches!(
                buffer.path.extension().and_then(|value| value.to_str()),
                Some("py" | "pyi" | "pyw")
            ) {
                continue;
            }
            if buffer.id != document.id && (!project || buffer.worktree_id != self.worktree) {
                continue;
            }
            if buffer.id != document.id && excluded(&buffer.path) {
                continue;
            }
            if let Some(source) = sources.iter_mut().find(|source| source.path == buffer.path) {
                source.text.clone_from(&buffer.text);
            } else {
                sources.push(PythonSource {
                    path: buffer.path.clone(),
                    text: buffer.text.clone(),
                });
            }
        }
        let snapshot = Arc::new(PythonWorkspace::with_source_roots(
            sources,
            &self.python_project.source_roots,
        )?);
        if !project {
            self.editor_state.assistance.local_snapshot =
                Some((document.id.clone(), document.version, snapshot.clone()));
        }
        Ok(snapshot)
    }

    pub(crate) fn apply_editor_edits(
        &mut self,
        ctx: &egui::Context,
        document: &str,
        source: &str,
        edits: &[TextEdit],
    ) -> bool {
        let Some(index) = self.buffers.iter().position(|buffer| buffer.id == document) else {
            return false;
        };
        let shared_matches = self.shared.lock().ok().is_some_and(|state| {
            state.document(document).is_some_and(|shared| {
                shared.version == self.buffers[index].version && shared.text == source
            })
        });
        if self.buffers[index].text != source
            || self.unsynced.contains(document)
            || !shared_matches
            || self.active_document.as_deref() != Some(document)
        {
            self.error = Some("Le fichier a changé depuis la proposition. Rouvrez le menu pour recalculer la modification.".into());
            return false;
        }
        let mut edits = edits.to_vec();
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let mut previous = 0;
        for edit in &edits {
            if edit.range.start < previous
                || edit.range.start > edit.range.end
                || edit.range.end > source.len()
                || !source.is_char_boundary(edit.range.start)
                || !source.is_char_boundary(edit.range.end)
            {
                self.error = Some(
                    "La proposition contient des positions incompatibles avec le fichier.".into(),
                );
                return false;
            }
            previous = edit.range.end;
        }
        let style = editing::TextStyle::detect(source);
        let editor = self.active_editor_id();
        let original = editor.and_then(|id| egui::TextEdit::load_state(ctx, id));
        let mut text = source.to_owned();
        for edit in edits.iter().rev() {
            text.replace_range(edit.range.clone(), &style.insertion(&edit.replacement));
        }
        let end = edits.first().map_or(0, |edit| {
            edit.range.start + style.insertion(&edit.replacement).len()
        });
        let cursor = editing::char_offset(&text, end);
        self.buffers[index].text = text;
        self.commit_buffer_edit(index);
        if let (Some(id), Some(mut state)) = (editor, original) {
            let mut undoer = state.undoer();
            let previous_cursor = state.cursor.char_range().unwrap_or_default();
            let next_cursor = egui::text::CCursorRange::one(egui::text::CCursor::new(cursor));
            undoer.add_undo(&(previous_cursor, source.to_owned()));
            undoer.add_undo(&(next_cursor, self.buffers[index].text.clone()));
            state.set_undoer(undoer);
            state.cursor.set_char_range(Some(next_cursor));
            egui::TextEdit::store_state(ctx, id, state);
        }
        self.select_in_active_editor(ctx, cursor..cursor);
        self.editor_state.assistance.completions.clear();
        self.editor_state.assistance.completion_enabled = false;
        self.editor_state.assistance.last_context = None;
        true
    }

    pub(super) fn editor_occurrences(
        &mut self,
        index: usize,
        ctx: &egui::Context,
        group: usize,
    ) -> Vec<Range<usize>> {
        let id = code_id(&self.buffers[index].id, group);
        if self.editor_state.active_group != group {
            return Vec::new();
        }
        let Some(cursor) =
            egui::TextEdit::load_state(ctx, id).and_then(|state| state.cursor.char_range())
        else {
            return Vec::new();
        };
        let byte = editing::byte_offset(&self.buffers[index].text, cursor.primary.index.0);
        self.editor_python_snapshot(index, false).map_or_else(
            |_| Vec::new(),
            |snapshot| snapshot.occurrences(&self.buffers[index].path, byte),
        )
    }

    pub(super) fn handle_assistance_keys(&mut self, ctx: &egui::Context, index: usize) -> bool {
        let shortcut = |modifiers, key| egui::KeyboardShortcut::new(modifiers, key);
        let command = |ctx: &egui::Context, key| {
            ctx.input_mut(|input| {
                input.consume_shortcut(&shortcut(egui::Modifiers::COMMAND, key))
                    || input.consume_shortcut(&shortcut(egui::Modifiers::CTRL, key))
            })
        };
        if command(ctx, egui::Key::Enter) {
            let selection = self.active_editor_selection(ctx).unwrap_or(0..0);
            let byte = editing::byte_offset(&self.buffers[index].text, selection.start);
            match self.editor_python_snapshot(index, true) {
                Ok(snapshot) => {
                    self.editor_state.assistance.quick_fixes = Some(FixPopup {
                        document: self.buffers[index].id.clone(),
                        source: self.buffers[index].text.clone(),
                        fixes: snapshot.quick_fixes(&self.buffers[index].path, byte),
                        selected: 0,
                    });
                    self.editor_state.assistance.completions.clear();
                }
                Err(error) => self.error = Some(format!("Corrections Python : {error:#}")),
            }
        }
        if command(ctx, egui::Key::H) {
            let selection = self.active_editor_selection(ctx).unwrap_or(0..0);
            let byte = editing::byte_offset(&self.buffers[index].text, selection.start);
            match self.editor_python_snapshot(index, true) {
                Ok(snapshot) => {
                    self.editor_state.assistance.hierarchy =
                        Some(snapshot.call_hierarchy(&self.buffers[index].path, byte));
                    self.editor_state.assistance.completions.clear();
                }
                Err(error) => self.error = Some(format!("Appels typés : {error:#}")),
            }
        }
        if self.editor_state.assistance.completions.is_empty() {
            return false;
        }
        let state = &mut self.editor_state.assistance;
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            state.completions.clear();
            state.completion_enabled = false;
            return false;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
            state.selected = (state.selected + 1) % state.completions.len();
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
            state.selected =
                (state.selected + state.completions.len() - 1) % state.completions.len();
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            let item = state.completions[state.selected].clone();
            let source = state.completion_source.clone();
            let document = state.completion_document.clone();
            return self.apply_editor_edits(
                ctx,
                &document,
                &source,
                &[TextEdit {
                    range: item.replace,
                    replacement: item.name,
                }],
            );
        }
        false
    }

    pub(super) fn update_editor_assistance(
        &mut self,
        ctx: &egui::Context,
        index: usize,
        changed: bool,
    ) {
        if changed {
            self.editor_state.assistance.completion_enabled = true;
        }
        let Some(selection) = self.active_editor_selection(ctx) else {
            return;
        };
        let byte = editing::byte_offset(&self.buffers[index].text, selection.end);
        let pointer = self
            .python_workspace
            .as_ref()
            .map_or(0, |snapshot| Arc::as_ptr(snapshot) as usize);
        let context = (
            self.buffers[index].id.clone(),
            self.buffers[index].version,
            byte,
            pointer,
        );
        if self.editor_state.assistance.last_context.as_ref() == Some(&context) {
            return;
        }
        let Ok(snapshot) = self.editor_python_snapshot(index, false) else {
            return;
        };
        let document = &self.buffers[index];
        let state = &mut self.editor_state.assistance;
        state.fixtures = snapshot.fixtures(&document.path, byte);
        state.fixtures_document = document.id.clone();
        if state.completion_enabled && selection.is_empty() {
            state.completions = snapshot.completions(&document.path, byte);
            state.completion_source.clone_from(&document.text);
            state.completion_document.clone_from(&document.id);
        } else {
            state.completions.clear();
        }
        state.selected = 0;
        state.last_context = Some(context);
    }

    pub(super) fn show_fixture_strip(&mut self, ui: &mut egui::Ui, index: usize) {
        if self.editor_state.assistance.fixtures_document != self.buffers[index].id
            || self.editor_state.assistance.fixtures.is_empty()
        {
            return;
        }
        let fixtures = self.editor_state.assistance.fixtures.clone();
        let mut navigate = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Fixtures pytest :").small().color(MUTED));
            for fixture in fixtures {
                let available = fixture.location.is_some();
                let label = if available {
                    fixture.name.clone()
                } else {
                    format!("{} · non localisée", fixture.name)
                };
                let response = ui
                    .add_enabled(available, egui::Button::new(label).small())
                    .on_hover_text(&fixture.reason)
                    .on_disabled_hover_text(format!(
                        "{} · Définition introuvable dans les fichiers indexés",
                        fixture.reason
                    ));
                if response.clicked() {
                    navigate = fixture.location;
                }
            }
        });
        if let Some(location) = navigate {
            self.open_document(&location.path, location.line, location.column);
        }
    }

    pub(super) fn show_editor_assistance(&mut self, ctx: &egui::Context) {
        if self.editor_state.assistance.restore_focus {
            self.editor_state.assistance.restore_focus = false;
            if let Some(id) = self.active_editor_id() {
                ctx.memory_mut(|memory| memory.request_focus(id));
            }
        }
        if let Some(mut popup) = self.editor_state.assistance.quick_fixes.take() {
            let mut chosen = None;
            let response = egui::Modal::new(egui::Id::new("python_quick_fix")).show(ctx, |ui| {
                ui.set_width(500.0);
                ui.heading("Corriger le symbole");
                ui.label("Importer · Créer · Corriger le nom");
                ui.separator();
                if popup.fixes.is_empty() {
                    ui.label("Aucune correction statique disponible à cette position.");
                }
                if !popup.fixes.is_empty() {
                    if ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                    }) {
                        popup.selected = (popup.selected + 1) % popup.fixes.len();
                    }
                    if ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                    }) {
                        popup.selected =
                            (popup.selected + popup.fixes.len() - 1) % popup.fixes.len();
                    }
                    if ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    }) {
                        chosen = Some(popup.selected);
                    }
                }
                for (index, fix) in popup.fixes.iter().enumerate() {
                    if ui
                        .selectable_label(index == popup.selected, &fix.label)
                        .clicked()
                    {
                        chosen = Some(index);
                    }
                }
                ui.separator();
                ui.button("Fermer · Échap").clicked()
            });
            if let Some(index) = chosen {
                self.apply_editor_edits(
                    ctx,
                    &popup.document,
                    &popup.source,
                    &popup.fixes[index].edits,
                );
                self.editor_state.assistance.restore_focus = true;
            } else if response.inner || response.should_close() {
                self.editor_state.assistance.restore_focus = true;
            } else {
                self.editor_state.assistance.quick_fixes = Some(popup);
            }
        }
        if let Some(hierarchy) = self.editor_state.assistance.hierarchy.take() {
            let mut navigate = None;
            let mut open = true;
            egui::Window::new("Appelants et appels typés · Ctrl+H").open(&mut open).default_width(540.0).show(ctx, |ui| {
                if let Some(hierarchy) = &hierarchy {
                    ui.heading(&hierarchy.target.name);
                    ui.label(RichText::new("Relations statiques entre fonctions explicitement typées").color(MUTED));
                    ui.separator();
                    ui.strong(format!("Appelants ({})", hierarchy.callers.len()));
                    for site in &hierarchy.callers {
                        if ui.button(format!("{} · {}:{}", site.caller.name, site.path.display(), site.line)).clicked() { navigate = Some((site.path.clone(), site.line, site.column)); }
                    }
                    ui.separator();
                    ui.strong(format!("Appels ({})", hierarchy.callees.len()));
                    for site in &hierarchy.callees {
                        if ui.button(format!("{} · {}:{}", site.callee.name, site.callee.path.display(), site.callee.line)).clicked() { navigate = Some((site.callee.path.clone(), site.callee.line, site.callee.column)); }
                    }
                } else { ui.label("Placez le curseur sur une fonction dont les paramètres et le retour sont annotés."); }
            });
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                open = false;
            }
            if let Some((path, line, column)) = navigate {
                self.open_document(&path, line, column);
            } else if open {
                self.editor_state.assistance.hierarchy = Some(hierarchy);
            } else {
                self.editor_state.assistance.restore_focus = true;
            }
        }
        let state = &self.editor_state.assistance;
        if state.completions.is_empty()
            || self
                .active_editor_id()
                .is_none_or(|id| !ctx.memory(|memory| memory.has_focus(id)))
        {
            return;
        }
        let anchor = state.anchor.unwrap_or(egui::Rect::from_min_size(
            egui::pos2(350.0, 150.0),
            egui::vec2(1.0, 20.0),
        ));
        let mut chosen = None;
        egui::Area::new(egui::Id::new("python_completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor.left_bottom())
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(400.0);
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for (index, item) in state.completions.iter().enumerate() {
                                let response = ui.selectable_label(
                                    index == state.selected,
                                    format!("{}    {}", item.name, item.detail),
                                );
                                if index == state.selected {
                                    response.scroll_to_me(Some(egui::Align::Center));
                                }
                                if response.clicked() {
                                    chosen = Some(index);
                                }
                            }
                        });
                    ui.label(
                        RichText::new("↑ ↓ choisir · Entrée insérer · Échap fermer")
                            .small()
                            .color(MUTED),
                    );
                });
            });
        if let Some(index) = chosen {
            let state = &self.editor_state.assistance;
            let item = state.completions[index].clone();
            let document = state.completion_document.clone();
            let source = state.completion_source.clone();
            self.apply_editor_edits(
                ctx,
                &document,
                &source,
                &[TextEdit {
                    range: item.replace,
                    replacement: item.name,
                }],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pie_crust_core::Workbench;
    use std::{path::Path, sync::Mutex};

    fn frame(app: &mut Desktop, ctx: &egui::Context, events: Vec<egui::Event>) {
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
    fn open_environment_buffers_do_not_reenter_project_analysis() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("runtime/Lib/site-packages")).unwrap();
        std::fs::write(root.path().join("runtime/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::fs::write(
            root.path().join("runtime/Lib/site-packages/library.py"),
            "def dependency() -> int:\n    return 1\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("app.py"),
            "def project() -> int:\n    return 2\n",
        )
        .unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        for path in ["app.py", "runtime/Lib/site-packages/library.py"] {
            let document = shared
                .lock()
                .unwrap()
                .read_document(&app.worktree, Path::new(path))
                .unwrap();
            app.buffers.push(document);
        }
        let snapshot = app.editor_python_snapshot(0, true).unwrap();
        assert!(snapshot.source(Path::new("app.py")).is_some());
        assert!(
            snapshot
                .source(Path::new("runtime/Lib/site-packages/library.py"))
                .is_none()
        );
        // Explicit local assistance remains available for the file being read.
        assert!(
            app.editor_python_snapshot(1, false)
                .unwrap()
                .source(Path::new("runtime/Lib/site-packages/library.py"))
                .is_some()
        );
    }

    #[test]
    fn typing_offers_completion_enter_applies_and_undo_restores_prefix() {
        let root = tempfile::tempdir().unwrap();
        let source = "def calculate_total(value: int) -> int:\n    return value\n\ncal";
        std::fs::write(root.path().join("example.py"), source).unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        app.open_document(Path::new("example.py"), 4, 4);
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![egui::Event::Text("c".into())]);
        assert!(
            app.editor_state
                .assistance
                .completions
                .iter()
                .any(|item| item.name == "calculate_total")
        );
        frame(&mut app, &ctx, key(egui::Key::Enter, egui::Modifiers::NONE));
        assert!(app.buffers[0].text.ends_with("calculate_total"));
        assert!(app.editor_state.assistance.completions.is_empty());
        frame(
            &mut app,
            &ctx,
            key(
                egui::Key::Z,
                egui::Modifiers::COMMAND | egui::Modifiers::CTRL,
            ),
        );
        assert!(app.buffers[0].text.ends_with("calc"));
    }

    #[test]
    fn quick_fix_shortcut_opens_without_inserting_newline_on_both_platform_modifiers() {
        for modifiers in [
            egui::Modifiers::CTRL,
            egui::Modifiers::COMMAND | egui::Modifiers::MAC_CMD,
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = "invoice_total = 12\nprint(invocie_total)\n";
            std::fs::write(root.path().join("example.py"), source).unwrap();
            let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
            let mut app = Desktop::from_workbench(shared, None, String::new(), None);
            let ctx = egui::Context::default();
            app.open_document(Path::new("example.py"), 2, 9);
            frame(&mut app, &ctx, vec![]);
            frame(&mut app, &ctx, key(egui::Key::Enter, modifiers));
            assert_eq!(app.buffers[0].text, source);
            let popup = app.editor_state.assistance.quick_fixes.as_ref().unwrap();
            assert!(
                popup
                    .fixes
                    .iter()
                    .any(|fix| fix.label.contains("invoice_total"))
            );
        }
    }

    #[test]
    fn non_python_assistance_shortcuts_and_empty_paste_do_not_change_source() {
        for filename in ["pyproject.toml", "README.md"] {
            let root = tempfile::tempdir().unwrap();
            let source = "project = \"demo\"\n";
            std::fs::write(root.path().join(filename), source).unwrap();
            let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
            let mut app = Desktop::from_workbench(shared, None, String::new(), None);
            let ctx = egui::Context::default();
            app.open_document(Path::new(filename), 1, 9);
            frame(&mut app, &ctx, vec![]);
            for modifiers in [
                egui::Modifiers::CTRL,
                egui::Modifiers::COMMAND | egui::Modifiers::MAC_CMD,
            ] {
                for command in [egui::Key::H, egui::Key::Enter] {
                    frame(&mut app, &ctx, key(command, modifiers));
                    assert_eq!(app.buffers[0].text, source);
                    assert!(app.editor_state.assistance.quick_fixes.is_none());
                    assert!(app.editor_state.assistance.hierarchy.is_none());
                }
            }
            app.select_in_active_editor(&ctx, 0..7);
            frame(&mut app, &ctx, vec![egui::Event::Paste(String::new())]);
            assert_eq!(app.buffers[0].text, source);
            assert_eq!(app.active_editor_selection(&ctx), Some(0..7));
        }
    }

    #[test]
    fn applying_proposal_checks_shared_version_before_touching_local_text() {
        let root = tempfile::tempdir().unwrap();
        let source = "café = 1\r\n";
        std::fs::write(root.path().join("example.py"), source).unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        let ctx = egui::Context::default();
        app.open_document(Path::new("example.py"), 1, 1);
        frame(&mut app, &ctx, vec![]);
        let id = app.active_document.clone().unwrap();
        shared
            .lock()
            .unwrap()
            .edit_document(&id, app.buffers[0].version, "café = 2\r\n".into())
            .unwrap();
        assert!(!app.apply_editor_edits(
            &ctx,
            &id,
            source,
            &[TextEdit {
                range: 0..0,
                replacement: "# insertion\n".into()
            }]
        ));
        assert_eq!(app.buffers[0].text, source);
        assert_eq!(
            shared.lock().unwrap().document(&id).unwrap().text,
            "café = 2\r\n"
        );
    }
}
