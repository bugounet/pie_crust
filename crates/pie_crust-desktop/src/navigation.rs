use super::*;
use crate::chrome::{CenterPage, Tool};
use regex::{Regex, RegexBuilder};
use std::path::Component;

#[derive(Default)]
pub(super) struct NavigationState {
    pub filename_regex: String,
    pub directory: String,
    pub focus_project_search: bool,
    pub include_environments: bool,
    pub file_include_environments: bool,
    pub environment_files: Vec<FileEntry>,
    pub environment_files_pending: bool,
    pub environment_files_revision: u64,
    pub explorer_selected: Option<PathBuf>,
    pub explorer_focused: bool,
    pub find_open: bool,
    find_query: String,
    find_case: bool,
    find_focus: bool,
    find_index: usize,
    popup: Option<PopupKind>,
    input: String,
    selected: Option<PathBuf>,
    focus_input: bool,
    return_focus: Option<egui::Id>,
    pending_open: Option<(PathBuf, usize, bool)>,
    pending_line: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum PopupKind {
    File,
    Line,
}

pub(super) struct SearchFilter {
    filename: Option<Regex>,
    directory: PathBuf,
}

impl SearchFilter {
    pub fn new(filename: &str, directory: &str) -> Result<Self, String> {
        let filename = if filename.is_empty() {
            None
        } else {
            Some(Regex::new(filename).map_err(|e| format!("Expression du nom de fichier : {e}"))?)
        };
        let directory = PathBuf::from(directory.trim().replace('\\', "/"));
        if directory
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err("Le dossier doit être un chemin relatif au projet, sans '..'.".into());
        }
        let directory = directory
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .collect();
        Ok(Self {
            filename,
            directory,
        })
    }
    pub fn matches(&self, path: &Path) -> bool {
        (self.directory.as_os_str().is_empty() || path.starts_with(&self.directory))
            && self.filename.as_ref().is_none_or(|re| {
                re.is_match(&path.file_name().unwrap_or_default().to_string_lossy())
            })
    }
}

fn shortcut(ctx: &egui::Context, key: egui::Key, extra: egui::Modifiers) -> bool {
    ctx.input_mut(|input| {
        input.consume_shortcut(&egui::KeyboardShortcut::new(
            egui::Modifiers::CTRL | extra,
            key,
        )) || input.consume_shortcut(&egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | extra,
            key,
        ))
    })
}

impl Desktop {
    pub(super) fn navigation_shortcuts(&mut self, ctx: &egui::Context) {
        if shortcut(ctx, egui::Key::F, egui::Modifiers::SHIFT) {
            if let Some(selection) = self.active_search_selection(ctx) {
                self.query = selection;
                self.hits.clear();
                self.query_changed = Some(Instant::now());
            }
            self.chrome.active_tool = Some(Tool::Search);
            self.chrome.navigation.focus_project_search = true;
        } else if shortcut(ctx, egui::Key::F, egui::Modifiers::NONE) {
            self.chrome.page = CenterPage::Code;
            if matches!(
                self.chrome.active_tool,
                Some(Tool::Packages | Tool::Settings)
            ) {
                self.chrome.active_tool = None;
            }
            self.chrome.navigation.find_open = true;
            self.chrome.navigation.find_focus = true;
            if let Some(selection) = self.active_search_selection(ctx) {
                self.chrome.navigation.find_query = selection;
            }
        }
        if shortcut(ctx, egui::Key::O, egui::Modifiers::NONE) {
            self.open_navigation(ctx, PopupKind::File);
        }
        if shortcut(ctx, egui::Key::L, egui::Modifiers::NONE) {
            self.open_navigation(ctx, PopupKind::Line);
        }
        if shortcut(ctx, egui::Key::Comma, egui::Modifiers::NONE) {
            self.chrome.page = CenterPage::Settings;
            self.chrome.active_tool = Some(Tool::Settings);
        }
        let others = shortcut(ctx, egui::Key::W, egui::Modifiers::SHIFT);
        if others || shortcut(ctx, egui::Key::W, egui::Modifiers::NONE) {
            if self.chrome.page != CenterPage::Code {
                self.chrome.page = CenterPage::Code;
                self.chrome.active_tool = None;
            } else {
                self.close_editor_tabs(ctx, others);
            }
        }
    }

    fn active_search_selection(&self, ctx: &egui::Context) -> Option<String> {
        self.runner
            .active_terminal_selection(ctx)
            .and_then(valid_search_selection)
            .or_else(|| {
                let range = self.active_editor_selection(ctx)?;
                let document = self
                    .buffers
                    .iter()
                    .find(|doc| Some(&doc.id) == self.active_document.as_ref())?;
                valid_search_selection(
                    document
                        .text
                        .chars()
                        .skip(range.start)
                        .take(range.end - range.start)
                        .collect(),
                )
            })
    }

    fn open_navigation(&mut self, ctx: &egui::Context, popup: PopupKind) {
        let state = &mut self.chrome.navigation;
        state.popup = Some(popup);
        state.input.clear();
        state.selected = None;
        state.focus_input = true;
        state.file_include_environments = false;
        state.environment_files.clear();
        state.environment_files_pending = false;
        state.environment_files_revision += 1;
        state.return_focus = ctx.memory(|memory| memory.focused());
    }

    fn load_environment_files(&mut self) {
        let state = &mut self.chrome.navigation;
        state.environment_files_revision += 1;
        let revision = state.environment_files_revision;
        state.environment_files_pending = true;
        let shared = self.shared.clone();
        let sender = self.sender.clone();
        let generation = self.generation;
        let worktree = self.worktree.clone();
        std::thread::spawn(move || {
            let result = shared
                .lock()
                .map_err(|_| "Workbench unavailable".to_string())
                .and_then(|state| {
                    state
                        .index_request(&worktree)
                        .map_err(|error| format!("{error:#}"))
                })
                .and_then(|request| {
                    request
                        .environment_files()
                        .map_err(|error| format!("{error:#}"))
                });
            let _ = sender.send(Event::EnvironmentFiles {
                generation,
                worktree,
                revision,
                result,
            });
        });
    }

    pub(super) fn navigation_popups(&mut self, ctx: &egui::Context) {
        let Some(kind) = self.chrome.navigation.popup else {
            if let Some((path, line, beside)) = self.chrome.navigation.pending_open.take() {
                self.chrome.navigation.return_focus = None;
                if beside {
                    self.open_document_beside(&path, line, 1);
                } else {
                    self.open_document(&path, line, 1);
                }
            } else if let Some(line) = self.chrome.navigation.pending_line.take() {
                self.chrome.navigation.return_focus = None;
                self.jump_active_line(line);
            } else if let Some(id) = self.chrome.navigation.return_focus.take() {
                ctx.memory_mut(|m| m.request_focus(id));
            }
            return;
        };
        let mut accept = false;
        let mut load_environments = false;
        let response = egui::Modal::new(egui::Id::new("navigation_popup")).show(ctx, |ui| {
            ui.set_width(610.0);
            ui.heading(if kind == PopupKind::File {
                "Aller au fichier"
            } else {
                "Aller à la ligne"
            });
            let down = kind == PopupKind::File
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            let up = kind == PopupKind::File
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let state = &mut self.chrome.navigation;
            let input = ui.add(
                egui::TextEdit::singleline(&mut state.input)
                    .id_salt("navigation_input")
                    .desired_width(f32::INFINITY)
                    .hint_text(if kind == PopupKind::File {
                        "Nom du fichier ou chemin:ligne"
                    } else {
                        "Numéro de ligne"
                    }),
            );
            if state.focus_input {
                input.request_focus();
                state.focus_input = false;
            }
            if kind == PopupKind::File {
                if ui.checkbox(&mut state.file_include_environments, "Inclure les venv pour cette recherche")
                    .on_hover_text("Parcourt les bibliothèques à la demande, même ignorées par Git, sans les indexer.")
                    .changed() && state.file_include_environments {
                    load_environments = true;
                }
                if state.file_include_environments && state.environment_files_pending {
                    ui.spinner();
                }
                let (query, _) = split_line_suffix(&state.input);
                let mut files = self.files.clone();
                if state.file_include_environments {
                    files.extend(state.environment_files.iter().cloned());
                }
                let candidates = ranked_files(&files, query);
                if state
                    .selected
                    .as_ref()
                    .is_none_or(|selected| !candidates.contains(selected))
                {
                    state.selected = candidates.first().cloned();
                }
                let mut selected = state
                    .selected
                    .as_ref()
                    .and_then(|path| candidates.iter().position(|p| p == path))
                    .unwrap_or(0);
                if !candidates.is_empty() {
                    if down {
                        selected = (selected + 1) % candidates.len();
                    }
                    if up {
                        selected = (selected + candidates.len() - 1) % candidates.len();
                    }
                    state.selected = Some(candidates[selected].clone());
                }
                ui.label(
                    RichText::new("↑ ↓ pour choisir · continuez à saisir · Entrée pour ouvrir")
                        .small()
                        .color(MUTED),
                );
                egui::ScrollArea::vertical()
                    .max_height(340.0)
                    .show(ui, |ui| {
                        for (index, path) in candidates.iter().enumerate() {
                            let row =
                                ui.selectable_label(index == selected, path.display().to_string());
                            if index == selected && (down || up) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                            if row.clicked() {
                                state.selected = Some(path.clone());
                                input.request_focus();
                            }
                            if row.double_clicked() {
                                accept = true;
                            }
                        }
                        if candidates.is_empty() {
                            ui.label("Aucun fichier correspondant.");
                        }
                    });
            } else if !state.input.is_empty()
                && state
                    .input
                    .parse::<usize>()
                    .ok()
                    .is_none_or(|line| line == 0)
            {
                ui.label("Saisissez un numéro de ligne supérieur à zéro.");
            }
            if enter {
                accept = true;
            }
            if ui.button("Ouvrir").clicked() {
                accept = true;
            }
        });
        if load_environments {
            self.load_environment_files();
        }
        if accept {
            let state = &mut self.chrome.navigation;
            if kind == PopupKind::File {
                if let Some(path) = state.selected.clone() {
                    let line = split_line_suffix(&state.input).1.unwrap_or(1);
                    state.pending_open = Some((path, line, false));
                    state.popup = None;
                }
            } else if let Ok(line) = state.input.parse::<usize>()
                && line > 0
            {
                state.pending_line = Some(line);
                state.popup = None;
            }
            ctx.request_repaint();
        } else if response.should_close() {
            self.chrome.navigation.popup = None;
            ctx.request_repaint();
        }
    }

    fn jump_active_line(&mut self, line: usize) {
        if let Some(doc) = self
            .buffers
            .iter()
            .find(|d| Some(&d.id) == self.active_document.as_ref())
            .cloned()
        {
            if doc.kind == pie_crust_core::DocumentKind::Scratch {
                let name = doc.path.file_name().unwrap_or_default().to_string_lossy();
                let result = self
                    .shared
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
                    .and_then(|mut state| state.focus_scratch(&name, line, 1));
                if let Err(error) = result {
                    self.error = Some(error.to_string());
                }
                self.poll();
            } else {
                self.open_document(&doc.path, line, 1);
            }
        }
    }

    pub(super) fn file_find_bar(&mut self, ui: &mut egui::Ui) {
        if !self.chrome.navigation.find_open {
            return;
        }
        let mut select = None;
        let mut search_focus = None;
        egui::Panel::top("file_find").show(ui, |ui| {
            ui.horizontal(|ui| {
                let state = &mut self.chrome.navigation;
                ui.label("Dans ce fichier");
                let input = ui.add(
                    egui::TextEdit::singleline(&mut state.find_query)
                        .id_salt("file_find_query")
                        .desired_width(280.0),
                );
                if state.find_focus {
                    input.request_focus();
                    state.find_focus = false;
                }
                if input.has_focus() {
                    search_focus = Some(input.id);
                }
                let changed = input.changed() | ui.checkbox(&mut state.find_case, "Aa").changed();
                let matches = self
                    .buffers
                    .iter()
                    .find(|d| Some(&d.id) == self.active_document.as_ref())
                    .map(|doc| literal_ranges(&doc.text, &state.find_query, state.find_case))
                    .unwrap_or_default();
                if changed {
                    state.find_index = 0;
                }
                let previous = ui.button("↑").clicked()
                    || (input.has_focus()
                        && ui.input_mut(|i| {
                            i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter)
                        }));
                let next = ui.button("↓").clicked()
                    || (input.has_focus()
                        && ui
                            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)));
                if !matches.is_empty() {
                    if previous {
                        state.find_index = (state.find_index + matches.len() - 1) % matches.len();
                    }
                    if next {
                        state.find_index = (state.find_index + 1) % matches.len();
                    }
                    state.find_index = state.find_index.min(matches.len() - 1);
                    if changed || previous || next {
                        select = Some(matches[state.find_index].clone());
                    }
                }
                ui.label(format!(
                    "{} / {}",
                    if matches.is_empty() {
                        0
                    } else {
                        state.find_index + 1
                    },
                    matches.len()
                ));
                if ui.button("×").clicked()
                    || (input.has_focus()
                        && ui
                            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
                {
                    state.find_open = false;
                }
            });
        });
        if let Some(range) = select {
            self.select_in_active_editor(ui.ctx(), range);
            if let Some(id) = search_focus {
                ui.memory_mut(|memory| memory.request_focus(id));
            }
        }
    }

    pub(super) fn autosave_documents(&mut self) {
        let ready: Vec<_> = self
            .autosave_pending
            .iter()
            .filter(|(_, since)| since.elapsed() >= Duration::from_millis(100))
            .map(|(id, _)| id.clone())
            .collect();
        let mut saved = false;
        for id in ready {
            self.autosave_pending.remove(&id);
            if self.unsynced.contains(&id) {
                continue;
            }
            let result = self
                .shared
                .lock()
                .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
                .and_then(|mut state| {
                    if state.document(&id).is_some_and(|doc| doc.dirty) {
                        state.save_document(&id)?;
                    }
                    Ok(state.document(&id).cloned())
                });
            match result {
                Ok(Some(doc)) => {
                    if let Some(buffer) = self.buffers.iter_mut().find(|d| d.id == id) {
                        *buffer = doc;
                    }
                    saved = true;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(format!("Sauvegarde automatique interrompue : {error:#}"))
                }
            }
        }
        if saved {
            self.rebuild_index();
        }
    }

    pub(super) fn refresh_python_workspace(&mut self) {
        if self.project_loading.is_some()
            || self.worktree_loading.is_some()
            || self.python_building
            || !self
                .python_refresh
                .is_some_and(|time| time.elapsed() >= Duration::from_millis(180))
        {
            return;
        }
        let Some(root) = self.active_root() else {
            return;
        };
        self.python_refresh = None;
        self.python_building = true;
        let generation = self.generation;
        let revision = self.python_revision;
        let worktree = self.worktree.clone();
        let files = self.files.clone();
        let source_roots = self.python_project.source_roots.clone();
        let buffers: Vec<_> = self
            .buffers
            .iter()
            .filter(|doc| {
                doc.worktree_id == worktree && doc.kind == pie_crust_core::DocumentKind::Source
            })
            .cloned()
            .collect();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<pie_crust_core::PythonWorkspace> {
                let mut sources = std::collections::BTreeMap::new();
                let mut size = 0;
                for file in files {
                    if !is_python(&file.path) { continue; }
                    let Ok(path) = root.join(&file.path).canonicalize() else { continue; };
                    if !path.starts_with(&root) || std::fs::metadata(&path)?.len() > 2*1024*1024 { continue; }
                    if let Ok(text) = std::fs::read_to_string(path) {
                        size += text.len(); anyhow::ensure!(size <= 256*1024*1024, "Le périmètre Python dépasse 256 Mio. Excluez les environnements et fichiers générés.");
                        sources.insert(file.path, text);
                    }
                }
                for buffer in buffers { if is_python(&buffer.path) && !pie_crust_core::is_python_environment_path(&root, &buffer.path) { sources.insert(buffer.path, buffer.text); } }
                pie_crust_core::PythonWorkspace::with_source_roots(sources.into_iter().map(|(path, text)| pie_crust_core::PythonSource { path, text }).collect(), &source_roots)
            })().map_err(|error| format!("{error:#}"));
            let _ = sender.send(Event::PythonIndexed {
                generation,
                worktree,
                revision,
                result,
            });
        });
    }
}

fn valid_search_selection(selection: String) -> Option<String> {
    (!selection.is_empty() && selection.len() <= 4096 && !selection.contains('\0'))
        .then_some(selection)
}

fn is_python(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("py" | "pyi" | "pyw")
    )
}

fn split_line_suffix(query: &str) -> (&str, Option<usize>) {
    match query.rsplit_once(':') {
        Some((name, "")) => (name, None),
        Some((name, line)) if line.chars().all(|c| c.is_ascii_digit()) => {
            (name, line.parse::<usize>().ok().map(|line| line.max(1)))
        }
        _ => (query, None),
    }
}

fn ranked_files(files: &[FileEntry], query: &str) -> Vec<PathBuf> {
    let query = query.replace('\\', "/").to_lowercase();
    let mut scored: Vec<_> = files
        .iter()
        .filter_map(|file| {
            let path = file
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase();
            if let Some(start) = path.find(&query) {
                Some((start, path.len(), file.path.clone()))
            } else {
                let mut letters = query.chars();
                let mut next = letters.next();
                let mut score = 0;
                for (offset, ch) in path.chars().enumerate() {
                    if Some(ch) == next {
                        score += offset;
                        next = letters.next();
                        if next.is_none() {
                            break;
                        }
                    }
                }
                next.is_none()
                    .then(|| (1000 + score, path.len(), file.path.clone()))
            }
        })
        .collect();
    scored.sort();
    scored
        .into_iter()
        .take(200)
        .map(|(_, _, path)| path)
        .collect()
}

fn literal_ranges(text: &str, query: &str, case_sensitive: bool) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let Ok(re) = RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case_sensitive)
        .build()
    else {
        return Vec::new();
    };
    let mut previous_byte = 0;
    let mut previous_char = 0;
    re.find_iter(text)
        .map(|m| {
            let start = previous_char + text[previous_byte..m.start()].chars().count();
            let end = start + m.as_str().chars().count();
            previous_byte = m.end();
            previous_char = end;
            start..end
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(app: &mut Desktop, ctx: &egui::Context, events: Vec<egui::Event>) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.render(ui),
        )
        .drop_without_applying_deltas();
    }
    fn key(modifiers: egui::Modifiers, key: egui::Key) -> Vec<egui::Event> {
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
    fn library_browsing_is_temporary_and_never_changes_project_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".venv")).unwrap();
        std::fs::write(
            root.path().join(".venv/library.py"),
            "def library(): pass\n",
        )
        .unwrap();
        std::fs::write(root.path().join("app.py"), "def app(): pass\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        app.files = shared.lock().unwrap().files(&app.worktree).unwrap();
        let ctx = egui::Context::default();
        app.open_navigation(&ctx, PopupKind::File);
        assert!(!app.chrome.navigation.file_include_environments);
        app.chrome.navigation.file_include_environments = true;
        app.load_environment_files();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.chrome.navigation.environment_files_pending && Instant::now() < deadline {
            app.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.chrome.navigation.environment_files_pending);
        assert_eq!(app.chrome.navigation.environment_files.len(), 1);
        assert_eq!(
            app.chrome.navigation.environment_files[0].path,
            Path::new(".venv/library.py")
        );
        assert_eq!(app.files.len(), 1);
        assert_eq!(app.files[0].path, Path::new("app.py"));
        let old_revision = app.chrome.navigation.environment_files_revision;
        app.open_navigation(&ctx, PopupKind::File);
        app.sender
            .send(Event::EnvironmentFiles {
                generation: app.generation,
                worktree: app.worktree.clone(),
                revision: old_revision,
                result: Ok(vec![FileEntry {
                    path: ".venv/stale.py".into(),
                }]),
            })
            .unwrap();
        app.poll();
        assert!(!app.chrome.navigation.file_include_environments);
        assert!(app.chrome.navigation.environment_files.is_empty());
        app.chrome.navigation.include_environments = true;
        app.reset_worktree();
        assert!(!app.chrome.navigation.include_environments);
    }

    #[test]
    fn filters_and_literal_ranges_preserve_directory_and_unicode_boundaries() {
        let filter = SearchFilter::new(r"^test_.*\.py$", "app/tests").unwrap();
        assert!(filter.matches(Path::new("app/tests/unit/test_one.py")));
        assert!(!filter.matches(Path::new("app/tests_other/test_one.py")));
        assert!(!filter.matches(Path::new("app/tests/test_one.txt")));
        assert!(SearchFilter::new("[", "").is_err());
        assert!(SearchFilter::new("", "../outside").is_err());
        assert_eq!(
            literal_ranges("éé abc\r\n🙂 ABC", "abc", false),
            [3..6, 10..13]
        );
        assert_eq!(
            split_line_suffix("app/billing.py:1234"),
            ("app/billing.py", Some(1234))
        );
        assert_eq!(
            split_line_suffix("C:/app/billing.py"),
            ("C:/app/billing.py", None)
        );
    }

    #[test]
    fn find_shortcuts_seed_queries_from_the_editor_selection() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("app.py"), "zero needle end\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        app.open_document(Path::new("app.py"), 1, 1);
        frame(&mut app, &ctx, Vec::new());
        app.select_in_active_editor(&ctx, 5..11);

        frame(
            &mut app,
            &ctx,
            key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::F),
        );
        assert_eq!(app.query, "needle");
        assert_eq!(app.chrome.active_tool, Some(Tool::Search));

        app.select_in_active_editor(&ctx, 5..11);
        frame(&mut app, &ctx, key(egui::Modifiers::CTRL, egui::Key::F));
        assert_eq!(app.chrome.navigation.find_query, "needle");
    }
    #[test]
    fn quick_open_keeps_typing_after_keyboard_selection_and_opens_the_requested_line() {
        let root = tempfile::tempdir().unwrap();
        let source = (0..30)
            .map(|i| format!("value_{i} = {i}\n"))
            .collect::<String>();
        for name in ["billing.py", "billing_test.py"] {
            std::fs::write(root.path().join(name), &source).unwrap();
        }
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.files = ["billing.py", "billing_test.py"]
            .into_iter()
            .map(|name| FileEntry { path: name.into() })
            .collect();
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        frame(&mut app, &ctx, key(egui::Modifiers::CTRL, egui::Key::O));
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::ModifiersChanged(egui::Modifiers::NONE),
                egui::Event::Text("bill".into()),
            ],
        );
        frame(
            &mut app,
            &ctx,
            key(egui::Modifiers::NONE, egui::Key::ArrowDown),
        );
        assert_eq!(
            app.chrome.navigation.selected.as_deref(),
            Some(Path::new("billing_test.py"))
        );
        frame(&mut app, &ctx, vec![egui::Event::Text("ing:20".into())]);
        assert_eq!(app.chrome.navigation.input, "billing:20");
        assert_eq!(
            app.chrome.navigation.selected.as_deref(),
            Some(Path::new("billing_test.py"))
        );
        frame(&mut app, &ctx, key(egui::Modifiers::NONE, egui::Key::Enter));
        frame(&mut app, &ctx, Vec::new());
        frame(&mut app, &ctx, Vec::new());
        let doc = app
            .buffers
            .iter()
            .find(|doc| Some(&doc.id) == app.active_document.as_ref())
            .unwrap();
        assert_eq!(doc.path, Path::new("billing_test.py"));
        let selection = app.active_editor_selection(&ctx).unwrap();
        assert_eq!(selection.start, character_offset(&source, 20, 1));
    }
    #[test]
    fn autosave_waits_for_idle_and_never_overwrites_a_disk_conflict() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("billing.py");
        std::fs::write(&path, "value = 1\r\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.open_document(Path::new("billing.py"), 1, 1);
        app.buffers[0].text = "value = 2\r\n".into();
        app.commit_buffer_edit(0);
        app.autosave_documents();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 1\r\n");
        let id = app.buffers[0].id.clone();
        app.autosave_pending
            .insert(id.clone(), Instant::now() - Duration::from_millis(101));
        app.autosave_documents();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 2\r\n");
        assert!(!app.buffers[0].dirty);
        app.buffers[0].text = "value = 3\r\n".into();
        app.commit_buffer_edit(0);
        std::fs::write(&path, "external\r\n").unwrap();
        app.autosave_pending
            .insert(id, Instant::now() - Duration::from_millis(101));
        app.autosave_documents();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external\r\n");
        assert!(app.buffers[0].dirty);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("Sauvegarde automatique interrompue")
        );
    }
}
