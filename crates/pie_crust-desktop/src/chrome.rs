use super::*;
use crate::run_panel::BottomView;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Files,
    Search,
    Packages,
    Migrations,
    Tests,
    Git,
    Terminal,
    Scratches,
    Settings,
}

impl Tool {
    pub const ALL: [Self; 9] = [
        Self::Files,
        Self::Search,
        Self::Packages,
        Self::Migrations,
        Self::Tests,
        Self::Git,
        Self::Settings,
        Self::Scratches,
        Self::Terminal,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Files => "Fichiers",
            Self::Search => "Recherche",
            Self::Packages => "Paquets",
            Self::Migrations => "Migrations",
            Self::Tests => "Tests",
            Self::Git => "Git",
            Self::Terminal => "Terminal",
            Self::Scratches => "Brouillons",
            Self::Settings => "Paramètres",
        }
    }
    fn number(self) -> usize {
        Self::ALL.iter().position(|tool| *tool == self).unwrap() + 1
    }
}

pub(super) struct ChromeState {
    pub page: CenterPage,
    pub navigation: crate::navigation::NavigationState,
    pub refactor: crate::refactor::RefactorMenu,
    pub active_tool: Option<Tool>,
    pub bottom_view: Option<BottomView>,
    pub follow_editor: bool,
    pub revealed: Option<(String, PathBuf)>,
    pub scratch_name: String,
    pub scratches: Vec<pie_crust_core::ScratchInfo>,
    pub config_text: Option<String>,
    pub config_disk_snapshot: Option<String>,
    pub config_dirty: bool,
    pub config_last_edit: Option<Instant>,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self {
            page: CenterPage::Code,
            navigation: Default::default(),
            refactor: Default::default(),
            active_tool: Some(Tool::Files),
            bottom_view: None,
            follow_editor: true,
            revealed: None,
            scratch_name: "scratch.py".into(),
            scratches: Vec::new(),
            config_text: None,
            config_disk_snapshot: None,
            config_dirty: false,
            config_last_edit: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CenterPage {
    Code,
    Packages,
    Settings,
}

#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
    path: Option<PathBuf>,
}

struct TreeView<'a> {
    worktree: &'a str,
    active: Option<&'a Path>,
    reveal: bool,
    show_all: bool,
}

#[derive(Default)]
struct TreeInteraction {
    open: Option<(PathBuf, bool)>,
    selected: Option<PathBuf>,
    search_directory: Option<PathBuf>,
    focused: bool,
    visible: Vec<(PathBuf, egui::Id)>,
}

impl TreeNode {
    fn insert(&mut self, path: &Path) {
        let mut current = self;
        for component in path.components() {
            current = current
                .children
                .entry(component.as_os_str().to_string_lossy().into_owned())
                .or_default();
        }
        current.path = Some(path.to_owned());
    }
}

impl Desktop {
    pub(super) fn toggle_tool(&mut self, tool: Tool) {
        if self.chrome.active_tool == Some(tool) && matches!(tool, Tool::Settings | Tool::Packages)
        {
            self.chrome.page = CenterPage::Code;
        }
        self.chrome.active_tool = if self.chrome.active_tool == Some(tool) {
            None
        } else {
            Some(tool)
        };
        if self.chrome.active_tool.is_some() {
            match tool {
                Tool::Files => self.chrome.revealed = None,
                Tool::Git => self.load_git(),
                Tool::Terminal => {
                    self.chrome.page = CenterPage::Code;
                    self.chrome.bottom_view = Some(BottomView::Terminal);
                    self.runner.open_terminal_drawer();
                }
                Tool::Tests => {
                    self.chrome.page = CenterPage::Code;
                    self.chrome.bottom_view = Some(BottomView::Tests);
                }
                Tool::Scratches => self.refresh_scratches(),
                Tool::Settings => self.chrome.page = CenterPage::Settings,
                Tool::Packages => {
                    self.chrome.page = CenterPage::Packages;
                    self.open_packages_page(None);
                }
                _ => {}
            }
        }
    }

    pub(super) fn chrome_shortcuts(&mut self, ctx: &egui::Context) {
        self.navigation_shortcuts(ctx);
        let keys = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        for (tool, key) in Tool::ALL.into_iter().zip(keys) {
            if consume_control_shortcut(ctx, key) {
                self.toggle_tool(tool);
            }
        }
        // Consume the more specific combination first: egui shortcuts allow
        // additional Shift/Alt modifiers when matching the simpler combination.
        if consume_control_shortcut_with(ctx, egui::Key::R, egui::Modifiers::SHIFT) {
            self.open_refactor_menu(ctx);
        } else if consume_control_shortcut(ctx, egui::Key::R) {
            self.runner.open_run_popup();
        }
    }

    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("P I E _ C R U S T").strong().color(ACCENT));
                ui.separator();
                let active_name = self
                    .worktrees
                    .iter()
                    .find(|tree| tree.id == self.worktree)
                    .map(|tree| tree.name.as_str())
                    .unwrap_or("Projet");
                egui::ComboBox::from_id_salt("worktree_picker")
                    .selected_text(active_name)
                    .width(165.0)
                    .show_ui(ui, |ui| {
                        for tree in self.worktrees.clone() {
                            let label = match tree.branch.as_deref() {
                                Some(branch) => format!("{} · {branch}", tree.name),
                                None => tree.name.clone(),
                            };
                            if ui
                                .selectable_label(tree.id == self.worktree, label)
                                .on_hover_text(display_path(&tree.root))
                                .clicked()
                                && let Ok(mut state) = self.shared.lock()
                            {
                                let _ = state.focus_worktree(&tree.id);
                            }
                        }
                    });
                if let Some(branch) = self
                    .worktrees
                    .iter()
                    .find(|tree| tree.id == self.worktree)
                    .and_then(|tree| tree.branch.as_ref())
                {
                    ui.label(RichText::new(branch).color(MUTED));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.runner.show_run_controls(ui);
                });
            });
            ui.add_space(3.0);
        });
    }

    pub(super) fn activity_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("activity_bar")
            .exact_size(46.0)
            .resizable(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 5.0;
                for tool in Tool::ALL.into_iter().take(6) {
                    self.activity_button(ui, tool);
                }
                self.activity_button(ui, Tool::Terminal);
                ui.add_space((ui.available_height() - 92.0).max(4.0));
                self.activity_button(ui, Tool::Scratches);
                self.activity_button(ui, Tool::Settings);
            });
    }

    fn activity_button(&mut self, ui: &mut egui::Ui, tool: Tool) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(32.0, 36.0), egui::Sense::click());
        let selected = self.chrome.active_tool == Some(tool);
        if selected || response.hovered() {
            ui.painter().rect_filled(
                rect,
                4.0,
                if selected {
                    Color32::from_rgb(48, 66, 70)
                } else {
                    Color32::from_rgb(37, 42, 51)
                },
            );
        }
        if selected {
            ui.painter().vline(
                rect.left() - 3.0,
                rect.y_range(),
                egui::Stroke::new(2.5, ACCENT),
            );
        }
        draw_icon(
            ui.painter(),
            rect.center(),
            tool,
            if selected { ACCENT } else { MUTED },
        );
        let response = response.on_hover_text(format!("{} · Ctrl+{}", tool.title(), tool.number()));
        if response.clicked() {
            self.toggle_tool(tool);
        }
    }

    pub(super) fn drawer(&mut self, ui: &mut egui::Ui) {
        let Some(tool) = self.chrome.active_tool else {
            return;
        };
        if matches!(tool, Tool::Settings | Tool::Packages) {
            return;
        }
        egui::Panel::left("tool_drawer")
            .default_size(310.0)
            .size_range(220.0..=650.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(tool.title());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("×")
                            .on_hover_text("Replier le panneau")
                            .clicked()
                        {
                            self.chrome.active_tool = None;
                        }
                        ui.label(
                            RichText::new(format!("Ctrl+{}", tool.number()))
                                .small()
                                .color(MUTED),
                        );
                    });
                });
                ui.separator();
                match tool {
                    Tool::Files => self.files_drawer(ui),
                    Tool::Search => self.search_drawer(ui),
                    Tool::Git => self.git_drawer(ui),
                    Tool::Terminal => {
                        self.runner.show_drawer(ui, BottomView::Terminal);
                        ui.separator();
                        if ui.button("Ouvrir le terminal système").clicked()
                            && let Some(root) = self.active_root()
                            && let Err(error) = open_terminal(&root)
                        {
                            self.error = Some(format!("Terminal : {error:#}"));
                        }
                    }
                    Tool::Tests => {
                        self.runner.show_drawer(ui, BottomView::Tests);
                        ui.separator();
                        ui.strong("Fichiers de tests");
                        let files: Vec<_> = self
                            .files
                            .iter()
                            .filter(|file| {
                                let name =
                                    file.path.file_name().unwrap_or_default().to_string_lossy();
                                name.starts_with("test_") || name.ends_with("_test.py")
                            })
                            .map(|file| file.path.clone())
                            .collect();
                        self.source_list(ui, "test_files", &files);
                    }
                    Tool::Scratches => self.scratches_drawer(ui),
                    Tool::Settings | Tool::Packages => {}
                    Tool::Migrations => self.migrations_drawer(ui),
                }
            });
    }

    fn files_drawer(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(format!(
                "Racine des sources : {}",
                self.python_project.primary_source_root.display()
            ))
            .small()
            .color(MUTED),
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.file_filter)
                .hint_text("Filtrer les fichiers…")
                .desired_width(f32::INFINITY),
        );
        let active = self
            .buffers
            .iter()
            .find(|doc| {
                Some(&doc.id) == self.active_document.as_ref()
                    && doc.kind == pie_crust_core::DocumentKind::Source
                    && doc.worktree_id == self.worktree
            })
            .map(|doc| doc.path.clone());
        let marker = active
            .as_ref()
            .map(|path| (self.worktree.clone(), path.clone()));
        let code_focused = self
            .active_editor_id()
            .is_some_and(|id| ui.memory(|memory| memory.has_focus(id)));
        let reveal = self.chrome.follow_editor
            && marker.is_some()
            && (marker != self.chrome.revealed
                || (code_focused && self.chrome.navigation.explorer_selected != active));
        if reveal {
            self.file_filter.clear();
        }
        let mut tree = TreeNode::default();
        let query = self.file_filter.to_lowercase();
        for file in &self.files {
            if file.path.to_string_lossy().to_lowercase().contains(&query) {
                tree.insert(&file.path);
            }
        }
        // Keep an unsaved, externally deleted source visible while it is open.
        if let Some(path) = &active
            && query.is_empty()
        {
            tree.insert(path);
        }
        let mut interaction = TreeInteraction {
            selected: self
                .chrome
                .navigation
                .explorer_selected
                .clone()
                .or_else(|| active.clone()),
            ..Default::default()
        };
        if reveal {
            interaction.selected = active.clone();
        }
        let worktree = self.worktree.clone();
        let show_all = !query.is_empty();
        egui::ScrollArea::vertical()
            .id_salt(("file_tree", &worktree))
            // Keep the viewport sized to the drawer: with the default auto-shrink
            // behaviour, the vertical scrollbar stays beside the tree contents
            // after the drawer is widened instead of following its right edge.
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let view = TreeView {
                    worktree: &worktree,
                    active: active.as_deref(),
                    reveal,
                    show_all,
                };
                draw_tree(ui, &tree, Path::new(""), &view, &mut interaction);
            });
        if reveal {
            self.chrome.revealed = marker;
        }
        if self.files.is_empty() && !self.indexing {
            ui.label("Aucun fichier dans le périmètre du projet.");
        }
        self.chrome.navigation.explorer_focused = interaction.focused;
        if interaction.focused && !interaction.visible.is_empty() {
            let mut index = interaction
                .visible
                .iter()
                .position(|(path, _)| Some(path) == interaction.selected.as_ref())
                .unwrap_or(0);
            let down = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            if down || up {
                index = if down {
                    (index + 1).min(interaction.visible.len() - 1)
                } else {
                    index.saturating_sub(1)
                };
                interaction.selected = Some(interaction.visible[index].0.clone());
                ui.memory_mut(|memory| memory.request_focus(interaction.visible[index].1));
            }
        }
        self.chrome.navigation.explorer_selected = interaction.selected;
        if let Some(directory) = interaction.search_directory {
            self.chrome.navigation.directory = directory.display().to_string();
            self.chrome.active_tool = Some(Tool::Search);
            self.chrome.navigation.focus_project_search = true;
            self.query_changed = Some(Instant::now());
        }
        if let Some((path, beside)) = interaction.open {
            if beside {
                self.open_document_beside(&path, 1, 1);
            } else if path
                .file_name()
                .is_some_and(|name| name == "pyproject.toml")
            {
                self.chrome.page = CenterPage::Packages;
                self.chrome.active_tool = Some(Tool::Packages);
                self.open_packages_page(Some(path));
            } else {
                self.open_document(&path, 1, 1);
            }
        }
    }

    fn search_drawer(&mut self, ui: &mut egui::Ui) {
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("Rechercher dans le code…")
                .desired_width(f32::INFINITY),
        );
        if self.chrome.navigation.focus_project_search {
            response.request_focus();
            self.chrome.navigation.focus_project_search = false;
        }
        if response.changed() {
            self.query_changed = Some(Instant::now());
            self.hits.clear();
        }
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.search();
        }
        let filename = ui.add(
            egui::TextEdit::singleline(&mut self.chrome.navigation.filename_regex)
                .hint_text("Nom de fichier (regex), ex. \\.py$")
                .desired_width(f32::INFINITY),
        );
        let directory = ui.add(
            egui::TextEdit::singleline(&mut self.chrome.navigation.directory)
                .hint_text("Dossier, ex. app/services")
                .desired_width(f32::INFINITY),
        );
        let environments = ui.checkbox(
            &mut self.chrome.navigation.include_environments,
            "Inclure les venv pour cette recherche",
        ).on_hover_text("Parcourt les bibliothèques à la demande, même ignorées par Git, sans les indexer ni les ajouter à l’analyse du projet.");
        if filename.changed() || directory.changed() || environments.changed() {
            self.hits.clear();
            self.query_changed = Some(Instant::now());
        }
        if let Err(error) = crate::navigation::SearchFilter::new(
            &self.chrome.navigation.filename_regex,
            &self.chrome.navigation.directory,
        ) {
            ui.colored_label(Color32::LIGHT_RED, error);
            return;
        }
        ui.label(
            RichText::new("Texte exact · sensible à la casse")
                .small()
                .color(MUTED),
        );
        if self.search_pending {
            ui.spinner();
        }
        ui.label(format!(
            "{} résultat(s){}",
            self.hits.len(),
            if self.hits.len() == 300 {
                " · limite 300"
            } else {
                ""
            }
        ));
        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt("search_results")
            .show(ui, |ui| {
                for hit in &self.hits {
                    ui.group(|ui| {
                        if ui
                            .link(format!("{}:{}", hit.path.display(), hit.line))
                            .clicked()
                        {
                            open = Some((hit.path.clone(), hit.line, hit.column));
                        }
                        ui.label(RichText::new(hit.preview.trim()).monospace().size(12.0));
                    });
                }
            });
        if let Some((path, line, column)) = open {
            self.open_document(&path, line, column);
        }
    }

    fn git_drawer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("100 derniers commits");
            if ui.small_button("Actualiser").clicked() {
                self.load_git();
            }
        });
        if self.git_pending {
            ui.spinner();
        }
        egui::ScrollArea::both().id_salt("git_log").show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(&self.git_log).monospace().size(12.0))
                    .selectable(true)
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
    }

    fn source_list(&mut self, ui: &mut egui::Ui, salt: &str, files: &[PathBuf]) {
        let mut open = None;
        egui::ScrollArea::vertical().id_salt(salt).show(ui, |ui| {
            for path in files {
                if ui
                    .selectable_label(false, path.display().to_string())
                    .clicked()
                {
                    open = Some(path.clone());
                }
            }
            if files.is_empty() {
                ui.label("Aucun fichier trouvé.");
            }
        });
        if let Some(path) = open {
            self.open_document(&path, 1, 1);
        }
    }

    pub(super) fn refresh_scratches(&mut self) {
        let result = self
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
            .and_then(|state| state.list_scratches());
        match result {
            Ok(files) => self.chrome.scratches = files,
            Err(error) => self.error = Some(format!("Brouillons : {error:#}")),
        }
    }

    fn scratches_drawer(&mut self, ui: &mut egui::Ui) {
        ui.label("Des fichiers libres, partagés entre les worktrees de ce projet.");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.chrome.scratch_name)
                    .hint_text("scratch.py")
                    .desired_width((ui.available_width() - 66.0).max(80.0)),
            );
            if ui.button("Créer").clicked() {
                let name = self.chrome.scratch_name.trim().to_owned();
                let result = self
                    .shared
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
                    .and_then(|mut state| {
                        state.create_scratch(&name, "")?;
                        state.focus_scratch(&name, 1, 1)
                    });
                match result {
                    Ok(_) => {
                        self.poll();
                        self.refresh_scratches();
                    }
                    Err(error) => self.error = Some(format!("Brouillon : {error:#}")),
                }
            }
        });
        if ui.small_button("Actualiser").clicked() {
            self.refresh_scratches();
        }
        ui.separator();
        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt("scratches")
            .show(ui, |ui| {
                for scratch in &self.chrome.scratches {
                    let selected = self.buffers.iter().any(|doc| {
                        Some(&doc.id) == self.active_document.as_ref()
                            && doc.kind == pie_crust_core::DocumentKind::Scratch
                            && doc.path == scratch.path
                    });
                    if ui.selectable_label(selected, &scratch.name).clicked() {
                        open = Some(scratch.name.clone());
                    }
                }
            });
        if let Some(name) = open {
            let result = self
                .shared
                .lock()
                .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
                .and_then(|mut state| state.focus_scratch(&name, 1, 1));
            if let Err(error) = result {
                self.error = Some(format!("Brouillon : {error:#}"));
            }
            self.poll();
        }
    }

    pub(super) fn save_project_settings(&mut self) -> bool {
        let result = (|| -> Result<()> {
            let text = self
                .chrome
                .config_text
                .as_ref()
                .context("Configuration non chargée")?;
            pie_crust_core::validate_config_text(text)?;
            let root = self
                .shared
                .lock()
                .map_err(|_| anyhow::anyhow!("Workbench unavailable"))?
                .project_root()
                .to_owned();
            let storage = root.join(".pie_crust").canonicalize()?;
            anyhow::ensure!(
                storage.starts_with(&root),
                "Le dossier des paramètres sort du projet"
            );
            let path = storage.join("config.toml");
            let current = match std::fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            anyhow::ensure!(
                current == self.chrome.config_disk_snapshot,
                "La configuration a changé sur disque. Votre saisie est conservée ; rouvrez le projet après avoir copié vos changements."
            );
            let mut file = tempfile::NamedTempFile::new_in(storage)?;
            use std::io::Write;
            file.write_all(text.as_bytes())?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.chrome.config_disk_snapshot = self.chrome.config_text.clone();
                self.chrome.config_dirty = false;
                self.rebuild_index();
                true
            }
            Err(error) => {
                self.error = Some(format!("Configuration : {error:#}"));
                false
            }
        }
    }

    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Paramètres");
            if ui.button("Revenir au code").clicked() {
                self.chrome.page = CenterPage::Code;
                self.chrome.active_tool = None;
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("settings").show(ui, |ui| {
            ui.strong("Éditeur");
            ui.add(egui::Slider::new(&mut self.editor_state.font_size, 11.0..=24.0).text("Taille du code"));
            if ui.checkbox(&mut self.chrome.follow_editor, "Suivre le fichier actif dans l’explorateur").changed() { self.chrome.revealed = None; }
            ui.label(RichText::new("Thème sombre · échelle native de l’écran").small().color(MUTED));
            ui.separator();
            ui.strong("Projet");
            ui.add(egui::TextEdit::singleline(&mut self.project_path).desired_width(f32::INFINITY));
            if ui.button("Ouvrir ce projet").clicked() { self.request_action(PendingAction::OpenProject(PathBuf::from(&self.project_path)), ui.ctx()); }
            ui.separator();
            ui.strong("Recherche et indexation");
            if self.chrome.config_text.is_none() {
                let path = self.shared.lock().ok().map(|state| state.project_root().join(".pie_crust/config.toml"));
                self.chrome.config_disk_snapshot = path.and_then(|path| std::fs::read_to_string(path).ok());
                self.chrome.config_text = Some(self.chrome.config_disk_snapshot.clone().unwrap_or_else(|| "[search]\nrespect_gitignore = true\nexclude = [\"**/.venv/**\", \"**/__pycache__/**\", \"**/target/**\"]\n\n[index]\nrespect_gitignore = true\nexclude = [\"**/.venv/**\", \"**/__pycache__/**\", \"**/target/**\"]\n".into()));
            }
            ui.label("Les exclusions de recherche et d’indexation sont indépendantes.");
            ui.label("Les venv sont exclus de l’index et de l’analyse du projet dans tous les worktrees, même avec une liste d’exclusions vide. La recherche de fichiers et de texte propose une option temporaire pour les inclure.");
            let text = self.chrome.config_text.as_mut().unwrap();
            if ui.add(egui::TextEdit::multiline(text).id(egui::Id::new("settings_config_editor")).code_editor().desired_width(f32::INFINITY).desired_rows(13)).changed() { self.chrome.config_dirty = true; self.chrome.config_last_edit = Some(Instant::now()); }
            if ui.add_enabled(self.chrome.config_dirty, egui::Button::new("Enregistrer et réindexer")).clicked() {
                self.save_project_settings();
            }
            ui.separator();
            ui.strong("Raccourcis");
            for tool in Tool::ALL { ui.label(format!("Ctrl+{}  {}", tool.number(), tool.title())); }
            ui.label("Ctrl+R  Lancer une commande");
            ui.label("Ctrl+Shift+R  Menu de refactoring");
            ui.label("Ctrl+S / ⌘S  Enregistrer");
            ui.label("Sauvegarde automatique après 100 ms d’inactivité dans le code");
            ui.label("Ctrl+,  Paramètres");
            ui.label("Ctrl+F / Ctrl+Shift+F  Recherche fichier / projet");
            ui.label("Ctrl+O / Ctrl+L  Aller au fichier / à la ligne");
            ui.label("Ctrl+W / Ctrl+Shift+W  Fermer l’onglet / les autres onglets");
            ui.label("Ctrl+Entrée  Corrections · Ctrl+H  Appelants typés");
            ui.separator();
            if ui.button("Connexion MCP").clicked() { self.show_connection = true; }
            if ui.button("Quitter pie_crust").clicked() { self.request_action(PendingAction::Exit, ui.ctx()); }
        });
    }

    pub(super) fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        if !matches!(self.chrome.page, CenterPage::Code | CenterPage::Packages) {
            return;
        }
        let Some(view) = self.chrome.bottom_view else {
            egui::Panel::bottom("bottom_tabs_closed").show(ui, |ui| {
                self.bottom_tabs(ui, None);
            });
            return;
        };
        egui::Panel::bottom("bottom_output")
            .default_size(235.0)
            .size_range(125.0..=650.0)
            .resizable(true)
            .show(ui, |ui| {
                self.bottom_tabs(ui, Some(view));
                ui.separator();
                self.runner.show_bottom_content(ui, view);
            });
    }

    fn bottom_tabs(&mut self, ui: &mut egui::Ui, active: Option<BottomView>) {
        ui.horizontal(|ui| {
            for (view, label) in [
                (BottomView::Terminal, "Terminal"),
                (BottomView::Tests, "Tests"),
                (BottomView::Run, "Exécution"),
            ] {
                if ui.selectable_label(active == Some(view), label).clicked() {
                    self.chrome.bottom_view = if active == Some(view) {
                        None
                    } else {
                        Some(view)
                    };
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if active.is_some()
                    && ui
                        .small_button("×")
                        .on_hover_text("Replier la sortie")
                        .clicked()
                {
                    self.chrome.bottom_view = None;
                }
                if self.runner.is_running() {
                    ui.spinner();
                }
            });
        });
    }
}

fn consume_control_shortcut(ctx: &egui::Context, key: egui::Key) -> bool {
    consume_control_shortcut_with(ctx, key, egui::Modifiers::NONE)
}

fn consume_control_shortcut_with(
    ctx: &egui::Context,
    key: egui::Key,
    extra: egui::Modifiers,
) -> bool {
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

fn draw_tree(
    ui: &mut egui::Ui,
    node: &TreeNode,
    prefix: &Path,
    view: &TreeView<'_>,
    interaction: &mut TreeInteraction,
) {
    let mut entries: Vec<_> = node.children.iter().collect();
    entries.sort_by_key(|(name, node)| (node.path.is_some(), name.to_lowercase()));
    for (name, node) in entries {
        let path = prefix.join(name);
        if let Some(file) = &node.path {
            let selected = interaction.selected.as_deref() == Some(file.as_path());
            let response = ui
                .selectable_label(selected, name)
                .on_hover_text(file.display().to_string());
            if selected && view.reveal {
                response.scroll_to_me(Some(egui::Align::Center));
            }
            interaction.visible.push((file.clone(), response.id));
            if response.clicked() {
                interaction.selected = Some(file.clone());
                response.request_focus();
            }
            interaction.focused |= response.has_focus();
            let beside = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
            let keyboard_beside =
                response.has_focus() && consume_control_shortcut(ui.ctx(), egui::Key::Enter);
            let keyboard_open = response.has_focus()
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            if (response.clicked() && beside)
                || response.double_clicked()
                || keyboard_open
                || keyboard_beside
            {
                interaction.open = Some((file.clone(), beside || keyboard_beside));
            }
        } else {
            let mut header =
                egui::CollapsingHeader::new(name).id_salt(("directory", view.worktree, &path));
            if view.show_all
                || (view.reveal && view.active.is_some_and(|active| active.starts_with(&path)))
            {
                header = header.open(Some(true));
            }
            let header = header.show(ui, |ui| {
                draw_tree(ui, node, &path, view, interaction);
            });
            header.header_response.context_menu(|ui| {
                if ui.button("Rechercher dans ce dossier").clicked() {
                    interaction.search_directory = Some(path.clone());
                    ui.close();
                }
            });
        }
    }
}

fn display_path(path: &Path) -> String {
    path.display()
        .to_string()
        .trim_start_matches("\\\\?\\")
        .to_owned()
}

fn draw_icon(painter: &egui::Painter, center: egui::Pos2, tool: Tool, color: Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let p = |x: f32, y: f32| center + egui::vec2(x, y);
    let line = |a, b| {
        painter.line_segment([a, b], stroke);
    };
    match tool {
        Tool::Files => {
            line(p(-9.0, -5.0), p(-3.0, -5.0));
            line(p(-3.0, -5.0), p(0.0, -2.0));
            line(p(0.0, -2.0), p(9.0, -2.0));
            line(p(9.0, -2.0), p(9.0, 7.0));
            line(p(9.0, 7.0), p(-9.0, 7.0));
            line(p(-9.0, 7.0), p(-9.0, -5.0));
        }
        Tool::Search => {
            painter.circle_stroke(p(-2.0, -2.0), 6.0, stroke);
            line(p(3.0, 3.0), p(9.0, 9.0));
        }
        Tool::Packages => {
            for (a, b) in [
                ((-8.0, -4.0), (0.0, -8.0)),
                ((0.0, -8.0), (8.0, -4.0)),
                ((8.0, -4.0), (0.0, 0.0)),
                ((0.0, 0.0), (-8.0, -4.0)),
                ((-8.0, -4.0), (-8.0, 5.0)),
                ((-8.0, 5.0), (0.0, 9.0)),
                ((0.0, 9.0), (8.0, 5.0)),
                ((8.0, 5.0), (8.0, -4.0)),
                ((0.0, 0.0), (0.0, 9.0)),
            ] {
                line(p(a.0, a.1), p(b.0, b.1));
            }
        }
        Tool::Migrations | Tool::Git => {
            line(p(-5.0, -6.0), p(-5.0, 6.0));
            line(p(-5.0, 2.0), p(6.0, -4.0));
            for pos in [p(-5.0, -7.0), p(-5.0, 7.0), p(7.0, -5.0)] {
                painter.circle_filled(pos, 2.5, color);
            }
        }
        Tool::Tests => {
            line(p(-7.0, 0.0), p(-1.0, 6.0));
            line(p(-1.0, 6.0), p(9.0, -7.0));
        }
        Tool::Terminal => {
            line(p(-8.0, -6.0), p(-2.0, 0.0));
            line(p(-2.0, 0.0), p(-8.0, 6.0));
            line(p(1.0, 6.0), p(9.0, 6.0));
        }
        Tool::Scratches => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-7.0, -9.0), p(7.0, 9.0)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            for y in [-4.0, 0.0, 4.0] {
                line(p(-3.0, y), p(4.0, y));
            }
        }
        Tool::Settings => {
            for (y, x) in [(-6.0, -3.0), (0.0, 4.0), (6.0, -1.0)] {
                line(p(-9.0, y), p(9.0, y));
                painter.circle_filled(p(x, y), 3.0, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(app: &mut Desktop, ctx: &egui::Context, key: Option<egui::Key>) -> egui::FullOutput {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            ..Default::default()
        };
        if let Some(key) = key {
            input.events = vec![
                egui::Event::ModifiersChanged(egui::Modifiers::CTRL),
                egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL,
                },
            ];
        }
        ctx.run_ui(input, |ui| app.render(ui))
    }

    fn text_of(shape: &egui::Shape, text: &mut String) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_of(shape, text);
                }
            }
            egui::Shape::Text(shape) => {
                text.push_str(&shape.galley.job.text);
                text.push('\n');
            }
            _ => {}
        }
    }

    #[test]
    fn packages_page_keeps_the_execution_log_visible() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("pyproject.toml"),
            "[project]\nname = 'demo'\n",
        )
        .unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        app.chrome.page = CenterPage::Packages;
        app.chrome.bottom_view = Some(BottomView::Run);
        frame(&mut app, &ctx, None).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, None);
        let mut text = String::new();
        for clipped in &output.shapes {
            text_of(&clipped.shape, &mut text);
        }
        assert!(text.contains("Journal d’exécution"), "{text}");
        assert!(text.contains("Ctrl+R pour choisir un script"), "{text}");
        assert!(app.chrome.page == CenterPage::Packages);
        output.drop_without_applying_deltas();
    }

    #[test]
    fn numbered_shortcuts_toggle_each_drawer_and_ctrl_r_opens_without_running() {
        let root = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        app.chrome.active_tool = None;
        for (tool, key) in Tool::ALL.into_iter().zip([
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ]) {
            frame(&mut app, &ctx, Some(key)).drop_without_applying_deltas();
            assert_eq!(app.chrome.active_tool, Some(tool));
            frame(&mut app, &ctx, Some(key)).drop_without_applying_deltas();
            assert_eq!(app.chrome.active_tool, None);
        }
        frame(&mut app, &ctx, Some(egui::Key::R)).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, None);
        let mut text = String::new();
        for clipped in &output.shapes {
            text_of(&clipped.shape, &mut text);
        }
        assert!(text.contains("Exécuter une commande"));
        assert!(!app.runner.is_running());
        output.drop_without_applying_deltas();
    }

    #[test]
    fn dirty_settings_block_close_and_validate_before_replacing_disk() {
        let root = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        let path = root.path().join(".pie_crust/config.toml");
        app.chrome.config_disk_snapshot = std::fs::read_to_string(&path).ok();
        app.chrome.config_text = Some("[search]\nexclude = 42\n".into());
        app.chrome.config_dirty = true;
        app.request_action(PendingAction::Exit, &ctx);
        assert!(app.pending_action.is_some());
        assert!(!app.allow_close);
        let before = std::fs::read_to_string(&path).ok();
        assert!(!app.save(true));
        assert_eq!(std::fs::read_to_string(&path).ok(), before);
        assert!(app.chrome.config_dirty);
        app.chrome.config_text = Some("[index]\nexclude = [\"[\"]\n".into());
        assert!(!app.save_project_settings());
        assert_eq!(std::fs::read_to_string(&path).ok(), before);
        app.chrome.config_text = Some("[search]\nexclude = [\"**/generated/**\"]\n".into());
        assert!(app.save_project_settings());
        assert!(!app.chrome.config_dirty);
        assert_eq!(std::fs::read_to_string(&path).ok(), app.chrome.config_text);
        // A second save must replace the existing file atomically on Windows too.
        app.chrome.config_text = Some("[index]\nexclude = []\n".into());
        app.chrome.config_dirty = true;
        assert!(app.save_project_settings());
        std::fs::write(&path, "# external edit\n").unwrap();
        app.chrome.config_text = Some("[search]\nexclude = []\n".into());
        app.chrome.config_dirty = true;
        assert!(!app.save_project_settings());
        assert!(app.chrome.config_dirty);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# external edit\n");
    }

    #[test]
    fn explorer_reveals_active_nested_source_and_retains_it_after_panel_reopens() {
        let root = tempfile::tempdir().unwrap();
        let relative = Path::new("app/services/billing.py");
        std::fs::create_dir_all(root.path().join(relative.parent().unwrap())).unwrap();
        std::fs::write(
            root.path().join(relative),
            "def charge(amount: int) -> int:\n    return amount\n",
        )
        .unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.files = vec![FileEntry {
            path: relative.to_owned(),
        }];
        app.file_filter = "does-not-match".into();
        app.open_document(relative, 2, 5);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        ctx.style_mut_of(egui::Theme::Dark, |style| style.animation_time = 0.0);
        let output = frame(&mut app, &ctx, None);
        assert!(app.file_filter.is_empty());
        assert_eq!(
            app.chrome.revealed,
            Some((app.worktree.clone(), relative.to_owned()))
        );
        let mut text = String::new();
        for clipped in &output.shapes {
            text_of(&clipped.shape, &mut text);
        }
        assert!(
            text.contains("services") && text.contains("billing.py"),
            "The active file's ancestors must be expanded"
        );
        output.drop_without_applying_deltas();
        app.toggle_tool(Tool::Files);
        app.toggle_tool(Tool::Files);
        assert!(app.chrome.revealed.is_none());
        frame(&mut app, &ctx, None).drop_without_applying_deltas();
        assert_eq!(
            app.chrome.revealed,
            Some((app.worktree.clone(), relative.to_owned()))
        );
    }
}
