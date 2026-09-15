#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod chrome;
mod editor;
mod launch_output;
mod markdown;
mod navigation;
mod package_environment;
mod project_drawers;
mod refactor;
mod run_panel;
mod workspace_loading;

use chrome::{ChromeState, Tool};
use workspace_loading::BackgroundLoad;

use anyhow::{Context, Result};
use eframe::egui::{self, Color32, RichText};
use pie_crust_core::{
    Document, FileEntry, FocusTarget, SearchHit, SharedWorkbench, Workbench, WorktreeInfo,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(130, 203, 181);
const MUTED: Color32 = Color32::from_rgb(148, 159, 180);

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            launch_output::show(&format!("Unable to start pie_crust: {error:#}"), true);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut project = None;
    let mut port = 43127;
    let mut mcp_enabled = true;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                launch_output::show(
                    "pie_crust — native Python workbench\n\nUsage: crusty [PROJECT] [--mcp-port PORT] [--no-mcp]\n\nWithout PROJECT, pie_crust opens its welcome screen.\nMCP starts after opening a project and listens on 127.0.0.1:43127 by default.\nCopy its bearer token in the UI, or set PIE_CRUST_MCP_TOKEN\n(at least 32 non-whitespace ASCII characters).",
                    false,
                );
                return Ok(());
            }
            Some("--mcp-port") => {
                port = args
                    .next()
                    .context("--mcp-port requires a port")?
                    .to_str()
                    .context("Port must be UTF-8")?
                    .parse()
                    .context("Invalid port")?;
            }
            Some("--no-mcp") => mcp_enabled = false,
            Some(value) if value.starts_with('-') => anyhow::bail!("Unknown option: {value}"),
            _ => project = Some(PathBuf::from(arg)),
        }
    }
    let token = std::env::var("PIE_CRUST_MCP_TOKEN")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().simple().to_string());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 600.0]),
        #[cfg(feature = "renderer-wgpu")]
        renderer: eframe::Renderer::Wgpu,
        #[cfg(all(feature = "renderer-glow", not(feature = "renderer-wgpu")))]
        renderer: eframe::Renderer::Glow,
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            // Winit's standard Quit menu terminates outside CloseRequested. Route
            // quitting through our own action so unsaved buffers stay protected.
            builder.with_default_menu(false);
        })),
        ..Default::default()
    };
    eframe::run_native(
        "pie_crust",
        options,
        Box::new(move |cc| {
            Ok(Box::new(Launcher::new(
                &cc.egui_ctx,
                project,
                port,
                mcp_enabled,
                token,
            )))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Unable to start the desktop: {error}"))
}

/// Startup has no workspace side effects until the user selects a project.
/// In particular, a Finder launch must not try to index its working directory.
struct Launcher {
    desktop: Option<Desktop>,
    loading: Option<BackgroundLoad<Desktop>>,
    project_path: String,
    error: Option<String>,
    port: u16,
    mcp_enabled: bool,
    token: String,
}

impl Launcher {
    fn new(
        ctx: &egui::Context,
        project: Option<PathBuf>,
        port: u16,
        mcp_enabled: bool,
        token: String,
    ) -> Self {
        Desktop::configure_style(ctx);
        let mut launcher = Self {
            desktop: None,
            loading: None,
            project_path: String::new(),
            error: None,
            port,
            mcp_enabled,
            token,
        };
        if let Some(project) = project {
            launcher.project_path = project.display().to_string();
            launcher.open_project();
        }
        launcher
    }

    fn open_project(&mut self) {
        if self.loading.is_some() {
            return;
        }
        if self.project_path.trim().is_empty() {
            self.error = Some("Saisissez le chemin du dossier de votre projet.".into());
            return;
        }
        self.error = None;
        let path = PathBuf::from(&self.project_path);
        let (port, enabled, token) = (self.port, self.mcp_enabled, self.token.clone());
        self.loading = Some(BackgroundLoad::start(path.clone(), move |progress| {
            let mut desktop =
                Desktop::load_project(&path, token.clone(), |stage| progress.stage(stage))?;
            if enabled {
                progress.stage("Démarrage de la connexion MCP…");
                match pie_crust_mcp::start_server(desktop.shared.clone(), port, token) {
                    Ok(server) => {
                        desktop.endpoint = Some(server.endpoint.clone());
                        desktop._server = Some(server);
                    }
                    Err(error) => desktop.error = Some(format!("MCP : {error:#}")),
                }
            }
            desktop.refresh_index();
            Ok(desktop)
        }));
    }

    fn poll_loading(&mut self) {
        if let Some(result) = self.loading.as_mut().and_then(BackgroundLoad::poll) {
            self.loading = None;
            match result {
                Ok(desktop) => self.desktop = Some(desktop),
                Err(error) => self.error = Some(format!("Impossible d’ouvrir ce projet : {error}")),
            }
        }
    }

    fn render(&mut self, ui: &mut egui::Ui) {
        self.poll_loading();
        if let Some(desktop) = &mut self.desktop {
            desktop.render(ui);
            return;
        }
        if consume_quit_shortcut(ui.ctx()) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space((ui.available_height() * 0.2).max(36.0));
            ui.vertical_centered(|ui| {
                ui.set_max_width(650.0);
                ui.label(
                    RichText::new("P I E _ C R U S T")
                        .strong()
                        .size(30.0)
                        .color(ACCENT),
                );
                ui.add_space(20.0);
                if let Some(loading) = &self.loading {
                    loading.show(ui);
                    ui.add_space(28.0);
                    if ui.small_button("Quitter").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    return;
                }
                ui.heading("Ouvrez votre espace de travail");
                ui.label(
                    RichText::new("Choisissez un projet Python ou un worktree pour commencer.")
                        .color(MUTED),
                );
                ui.add_space(24.0);
                ui.label("Chemin du dossier du projet");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.project_path)
                        .hint_text(if cfg!(target_os = "windows") {
                            "C:\\projets\\mon-projet"
                        } else {
                            "/Users/vous/projets/mon-projet"
                        })
                        .desired_width(580.0),
                );
                ui.add_space(10.0);
                if ui.button("Ouvrir le projet").clicked()
                    || (response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                {
                    self.open_project();
                    ui.ctx().request_repaint();
                }
                if let Some(error) = &self.error {
                    ui.add_space(14.0);
                    ui.colored_label(Color32::from_rgb(241, 163, 148), error);
                }
                ui.add_space(28.0);
                if ui.small_button("Quitter").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new("DEVELOPER PREVIEW  0.1")
                        .size(10.0)
                        .color(MUTED),
                );
            });
        });
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

enum Event {
    EnvironmentFiles {
        generation: u64,
        worktree: String,
        revision: u64,
        result: Result<Vec<FileEntry>, String>,
    },
    PythonIndexed {
        generation: u64,
        worktree: String,
        revision: u64,
        result: Result<pie_crust_core::PythonWorkspace, String>,
    },
    Indexed {
        generation: u64,
        worktree: String,
        result: Result<(Vec<FileEntry>, String, pie_crust_core::PythonProjectLayout), String>,
    },
    Searched {
        revision: u64,
        generation: u64,
        worktree: String,
        query: String,
        filters: (String, String, bool),
        hits: Vec<SearchHit>,
        done: bool,
        error: Option<String>,
    },
    Git {
        generation: u64,
        worktree: String,
        result: Result<String, String>,
    },
}

#[derive(Clone)]
enum PendingAction {
    Exit,
    OpenProject(PathBuf),
}

struct LoadedWorktree {
    layout: pie_crust_core::PythonProjectLayout,
    runner: run_panel::RunPanel,
    error: Option<String>,
    packages: Option<(PathBuf, Option<pie_crust_core::PythonEnvironment>)>,
}

struct Desktop {
    shared: SharedWorkbench,
    _server: Option<pie_crust_mcp::ServerHandle>,
    endpoint: Option<String>,
    token: String,
    project_path: String,
    worktrees: Vec<WorktreeInfo>,
    worktree: String,
    buffers: Vec<Document>,
    unsynced: HashSet<String>,
    active_document: Option<String>,
    focus_serial: u64,
    pending_focus: Option<FocusTarget>,
    files: Vec<FileEntry>,
    python_project: pie_crust_core::PythonProjectLayout,
    file_filter: String,
    chrome: ChromeState,
    editor_state: editor::EditorState,
    python_workspace: Option<Arc<pie_crust_core::PythonWorkspace>>,
    python_revision: u64,
    python_refresh: Option<Instant>,
    python_building: bool,
    autosave_pending: HashMap<String, Instant>,
    drawer_data: project_drawers::ProjectDrawers,
    runner: run_panel::RunPanel,
    query: String,
    query_changed: Option<Instant>,
    search_pending: bool,
    search_revision: u64,
    hits: Vec<SearchHit>,
    git_log: String,
    git_pending: bool,
    indexing: bool,
    reindex_requested: bool,
    index_status: String,
    error: Option<String>,
    show_connection: bool,
    pending_action: Option<PendingAction>,
    allow_close: bool,
    generation: u64,
    project_loading: Option<BackgroundLoad<Desktop>>,
    worktree_loading: Option<BackgroundLoad<LoadedWorktree>>,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
}

impl Desktop {
    fn configure_style(ctx: &egui::Context) {
        // Setting visuals alone leaves egui following the system theme. The
        // first native input could then select an unconfigured light style.
        ctx.set_theme(egui::Theme::Dark);
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(23, 27, 35);
        visuals.window_fill = Color32::from_rgb(30, 35, 45);
        visuals.extreme_bg_color = Color32::from_rgb(18, 22, 29);
        visuals.override_text_color = Some(Color32::from_rgb(214, 220, 231));
        visuals.weak_text_color = Some(MUTED);
        visuals.code_bg_color = Color32::from_rgb(35, 43, 51);
        visuals.selection.bg_fill = Color32::from_rgb(48, 86, 84);
        visuals.selection.stroke.color = Color32::WHITE;
        ctx.set_visuals_of(egui::Theme::Dark, visuals);
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing = egui::vec2(9.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
            style
                .text_styles
                .insert(egui::TextStyle::Monospace, egui::FontId::monospace(14.0));
        });
    }

    #[cfg(test)]
    fn from_workbench(
        shared: SharedWorkbench,
        server: Option<pie_crust_mcp::ServerHandle>,
        token: String,
        error: Option<String>,
    ) -> Self {
        Self::from_workbench_with_progress(shared, server, token, error, |_| {})
    }

    fn load_project(
        path: &Path,
        token: String,
        progress: impl FnMut(&'static str),
    ) -> Result<Self, String> {
        let workbench = Workbench::open(path).map_err(|error| format!("{error:#}"))?;
        Ok(Self::from_workbench_with_progress(
            Arc::new(Mutex::new(workbench)),
            None,
            token,
            None,
            progress,
        ))
    }

    fn from_workbench_with_progress(
        shared: SharedWorkbench,
        server: Option<pie_crust_mcp::ServerHandle>,
        token: String,
        error: Option<String>,
        mut progress: impl FnMut(&'static str),
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let state = shared.lock().expect("new workbench mutex poisoned");
        let project_path = state.project_root().display().to_string();
        let worktree = state.active_worktree_id().to_owned();
        let worktrees = state.worktrees().to_vec();
        let root = state.project_root().to_owned();
        drop(state);
        progress("Découverte des projets Python…");
        let layout = worktrees
            .iter()
            .find(|tree| tree.id == worktree)
            .map(|tree| pie_crust_core::PythonProjectLayout::discover(&tree.root))
            .transpose();
        let (python_project, error) = match layout {
            Ok(layout) => (layout.unwrap_or_default(), error),
            Err(discovery_error) => (
                pie_crust_core::PythonProjectLayout::default(),
                error.or_else(|| Some(format!("Découverte Python : {discovery_error:#}"))),
            ),
        };
        progress("Préparation de l’environnement et des commandes…");
        let mut runner = run_panel::RunPanel::new(root);
        if let Some(tree) = worktrees.iter().find(|tree| tree.id == worktree) {
            runner.set_worktree(worktree.clone(), tree.root.clone());
        }
        let endpoint = server.as_ref().map(|server| server.endpoint.clone());
        Self {
            shared,
            _server: server,
            endpoint,
            token,
            project_path,
            worktree,
            worktrees,
            buffers: Vec::new(),
            unsynced: HashSet::new(),
            active_document: None,
            focus_serial: 0,
            pending_focus: None,
            files: Vec::new(),
            python_project,
            file_filter: String::new(),
            chrome: ChromeState::default(),
            editor_state: editor::EditorState::default(),
            python_workspace: None,
            python_revision: 0,
            python_refresh: None,
            python_building: false,
            autosave_pending: HashMap::new(),
            drawer_data: project_drawers::ProjectDrawers::default(),
            runner,
            query: String::new(),
            query_changed: None,
            search_pending: false,
            search_revision: 0,
            hits: Vec::new(),
            git_log: String::new(),
            git_pending: false,
            indexing: false,
            reindex_requested: false,
            index_status: "Index en préparation".into(),
            error,
            show_connection: false,
            pending_action: None,
            allow_close: false,
            generation: 0,
            project_loading: None,
            worktree_loading: None,
            sender,
            receiver,
        }
    }

    fn rebuild_index(&mut self) {
        self.update_index(true);
    }

    fn refresh_index(&mut self) {
        self.update_index(false);
    }

    fn update_index(&mut self, verify_hashes: bool) {
        if self.indexing {
            self.reindex_requested = true;
            return;
        }
        let Some(root) = self.active_root() else {
            return;
        };
        let request = self
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
            .and_then(|state| state.index_request(&self.worktree));
        match request {
            Ok(request) => {
                self.indexing = true;
                self.index_status = "Indexation en cours…".into();
                let sender = self.sender.clone();
                let worktree = self.worktree.clone();
                let generation = self.generation;
                std::thread::spawn(move || {
                    let result = if verify_hashes {
                        request.rebuild()
                    } else {
                        request.refresh()
                    }
                    .and_then(|stats| {
                        let message = if !verify_hashes && stats.updated == 0 && stats.removed == 0
                        {
                            format!("{} fichiers · index en cache", stats.files)
                        } else {
                            format!(
                                "{} fichiers · {} actualisés · {} retirés",
                                stats.files, stats.updated, stats.removed
                            )
                        };
                        Ok((
                            request.files()?,
                            message,
                            pie_crust_core::PythonProjectLayout::discover(&root)?,
                        ))
                    })
                    .map_err(|error| format!("{error:#}"));
                    let _ = sender.send(Event::Indexed {
                        generation,
                        worktree,
                        result,
                    });
                });
            }
            Err(error) => self.error = Some(format!("Indexation : {error:#}")),
        }
    }

    fn search(&mut self) {
        self.search_revision += 1;
        let revision = self.search_revision;
        self.query_changed = None;
        if self.query.is_empty() {
            self.hits.clear();
            self.search_pending = false;
            return;
        }
        self.hits.clear();
        self.search_pending = true;
        let shared = self.shared.clone();
        let sender = self.sender.clone();
        let worktree = self.worktree.clone();
        let query = self.query.clone();
        let filters = (
            self.chrome.navigation.filename_regex.clone(),
            self.chrome.navigation.directory.clone(),
            self.chrome.navigation.include_environments,
        );
        let generation = self.generation;
        std::thread::spawn(move || {
            let snapshot = shared
                .lock()
                .map_err(|_| "Workbench unavailable".to_string())
                .and_then(|state| {
                    state
                        .search_snapshot(&worktree)
                        .map_err(|error| format!("{error:#}"))
                });
            let result = snapshot.and_then(|(request, documents)| {
                let filter = navigation::SearchFilter::new(&filters.0, &filters.1)
                    .map_err(|error| error.to_string())?;
                let progress_sender = sender.clone();
                let progress_worktree = worktree.clone();
                let progress_query = query.clone();
                let progress_filters = filters.clone();
                request
                    .search_filtered_with_environments_streaming(
                        &query,
                        300,
                        documents.iter(),
                        |path| filter.matches(path),
                        filters.2,
                        |hits| {
                            let _ = progress_sender.send(Event::Searched {
                                revision,
                                generation,
                                worktree: progress_worktree.clone(),
                                query: progress_query.clone(),
                                filters: progress_filters.clone(),
                                hits: hits.to_vec(),
                                done: false,
                                error: None,
                            });
                        },
                    )
                    .map(|_| ())
                    .map_err(|error| format!("{error:#}"))
            });
            let _ = sender.send(Event::Searched {
                revision,
                generation,
                worktree,
                query,
                filters,
                hits: Vec::new(),
                done: true,
                error: result.err(),
            });
        });
    }

    fn load_git(&mut self) {
        if self.git_pending {
            return;
        }
        let Some(root) = self.active_root() else {
            return;
        };
        self.git_pending = true;
        let sender = self.sender.clone();
        let worktree = self.worktree.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let mut command = std::process::Command::new("git");
            command.arg("-C").arg(root).args([
                "log",
                "--graph",
                "--oneline",
                "--decorate",
                "--all",
                "-n",
                "100",
            ]);
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let result = command
                .output()
                .map_err(|error| error.to_string())
                .and_then(|output| {
                    if output.status.success() {
                        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
                    } else {
                        Err(String::from_utf8_lossy(&output.stderr).into_owned())
                    }
                });
            let _ = sender.send(Event::Git {
                generation,
                worktree,
                result,
            });
        });
    }

    fn active_root(&self) -> Option<PathBuf> {
        self.worktrees
            .iter()
            .find(|tree| tree.id == self.worktree)
            .map(|tree| tree.root.clone())
    }

    fn refresh_python_project(&mut self) {
        let Some(root) = self.active_root() else {
            return;
        };
        match pie_crust_core::PythonProjectLayout::discover(&root) {
            Ok(layout) => self.set_python_project(layout),
            Err(error) => self.error = Some(format!("Découverte Python : {error:#}")),
        }
    }

    fn set_python_project(&mut self, layout: pie_crust_core::PythonProjectLayout) {
        if self.python_project != layout {
            self.python_project = layout;
            self.python_workspace = None;
            self.clear_python_assistance();
            self.python_revision += 1;
            self.python_refresh = Some(Instant::now());
        }
    }

    fn poll(&mut self) {
        self.runner.poll();
        if let Some(view) = self.runner.requested_view() {
            self.chrome.bottom_view = Some(view);
        }
        if self.runner.take_close_drawer() {
            self.chrome.active_tool = None;
        }
        if self.endpoint.is_some()
            && let Some(server) = &self._server
            && !server.is_running()
        {
            self.endpoint = None;
            self.error = Some(format!(
                "Serveur MCP arrêté : {}",
                server
                    .error()
                    .unwrap_or_else(|| "connexion interrompue".into())
            ));
        }
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::EnvironmentFiles {
                    generation,
                    worktree,
                    revision,
                    result,
                } if generation == self.generation
                    && worktree == self.worktree
                    && revision == self.chrome.navigation.environment_files_revision =>
                {
                    self.chrome.navigation.environment_files_pending = false;
                    match result {
                        Ok(files) => self.chrome.navigation.environment_files = files,
                        Err(error) => {
                            self.error = Some(format!("Recherche dans les venv : {error}"))
                        }
                    }
                }
                Event::PythonIndexed {
                    generation,
                    worktree,
                    revision,
                    result,
                } if generation == self.generation && worktree == self.worktree => {
                    self.python_building = false;
                    if revision == self.python_revision {
                        match result {
                            Ok(workspace) => self.python_workspace = Some(Arc::new(workspace)),
                            Err(error) => self.error = Some(format!("Analyse Python : {error}")),
                        }
                    } else {
                        self.python_refresh = Some(Instant::now());
                    }
                }
                Event::Indexed {
                    generation,
                    worktree,
                    result,
                } if generation == self.generation && worktree == self.worktree => {
                    self.indexing = false;
                    match result {
                        Ok((files, status, layout)) => {
                            self.files = files;
                            self.set_python_project(layout);
                            self.python_revision += 1;
                            self.python_refresh = Some(Instant::now());
                            self.index_status = status;
                            if !self.query.is_empty() {
                                self.search();
                            }
                        }
                        Err(error) => {
                            self.index_status = "Échec de l’indexation".into();
                            self.error = Some(error);
                        }
                    }
                    if self.reindex_requested {
                        self.reindex_requested = false;
                        self.rebuild_index();
                    }
                }
                Event::Searched {
                    revision,
                    generation,
                    worktree,
                    query,
                    filters,
                    hits,
                    done,
                    error,
                } if generation == self.generation
                    && revision == self.search_revision
                    && worktree == self.worktree
                    && query == self.query
                    && filters
                        == (
                            self.chrome.navigation.filename_regex.clone(),
                            self.chrome.navigation.directory.clone(),
                            self.chrome.navigation.include_environments,
                        ) =>
                {
                    self.hits.extend(hits);
                    if done {
                        self.search_pending = false;
                    }
                    if let Some(error) = error {
                        self.error = Some(error);
                    }
                }
                Event::Git {
                    generation,
                    worktree,
                    result,
                } if generation == self.generation && worktree == self.worktree => {
                    self.git_pending = false;
                    match result {
                        Ok(log) => self.git_log = log,
                        Err(error) => self.error = Some(error),
                    }
                }
                _ => {}
            }
        }
        if self
            .query_changed
            .is_some_and(|time| time.elapsed() >= Duration::from_millis(250))
        {
            self.search();
        }
        let mut changed_worktree = false;
        if let Ok(state) = self.shared.try_lock() {
            if state.active_worktree_id() != self.worktree {
                self.worktree = state.active_worktree_id().to_owned();
                changed_worktree = true;
            }
            for info in state.documents() {
                if self.unsynced.contains(&info.id) {
                    continue;
                }
                if let Some(local) = self
                    .buffers
                    .iter_mut()
                    .find(|document| document.id == info.id)
                    && (local.version != info.version || local.dirty != info.dirty)
                    && let Some(document) = state.document(&info.id)
                {
                    *local = document.clone();
                }
            }
            if let Some(focus) = state.focus()
                && focus.serial != self.focus_serial
            {
                self.focus_serial = focus.serial;
                if !self
                    .buffers
                    .iter()
                    .any(|buffer| buffer.id == focus.document_id)
                    && let Some(document) = state.document(&focus.document_id)
                {
                    self.buffers.push(document.clone());
                }
                self.active_document = Some(focus.document_id.clone());
                self.chrome.page = chrome::CenterPage::Code;
                if matches!(
                    self.chrome.active_tool,
                    Some(Tool::Packages | Tool::Settings)
                ) {
                    self.chrome.active_tool = None;
                }
                self.pending_focus = Some(focus);
            }
        }
        if changed_worktree {
            self.worktree_changed();
        }
        self.autosave_documents();
        self.poll_packages_autosave();
        if self.chrome.config_dirty
            && self
                .chrome
                .config_last_edit
                .is_some_and(|time| time.elapsed() >= Duration::from_millis(100))
        {
            self.chrome.config_last_edit = None;
            self.save_project_settings();
        }
        self.refresh_python_workspace();
    }

    fn worktree_changed(&mut self) {
        self.reset_worktree();
        let Some(root) = self.active_root() else {
            return;
        };
        let show_packages = self.chrome.page == chrome::CenterPage::Packages;
        let remembered_packages = show_packages
            .then(|| self.remembered_packages_path())
            .flatten();
        let prepare_runner = self
            .runner
            .prepare_worktree(self.worktree.clone(), root.clone());
        self.worktree_loading = Some(BackgroundLoad::start(root.clone(), move |progress| {
            progress.stage("Découverte des projets Python…");
            let (layout, error) = match pie_crust_core::PythonProjectLayout::discover(&root) {
                Ok(layout) => (layout, None),
                Err(error) => (
                    Default::default(),
                    Some(format!("Découverte Python : {error:#}")),
                ),
            };
            progress.stage("Préparation de l’environnement et des commandes…");
            let runner = prepare_runner();
            let packages = show_packages.then(|| {
                let path = remembered_packages.unwrap_or_else(|| {
                    layout
                        .primary_manifest()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| layout.primary_source_root.join("pyproject.toml"))
                });
                let source_root = path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let environment = pie_crust_core::discover_python_environment(&root, source_root);
                (path, environment)
            });
            Ok(LoadedWorktree {
                layout,
                runner,
                error,
                packages,
            })
        }));
    }

    fn reset_worktree(&mut self) {
        self.chrome.navigation.include_environments = false;
        self.chrome.navigation.file_include_environments = false;
        self.chrome.navigation.environment_files.clear();
        self.chrome.navigation.environment_files_pending = false;
        self.chrome.navigation.environment_files_revision += 1;
        self.worktree_loading = None;
        self.python_workspace = None;
        self.clear_python_assistance();
        self.python_revision += 1;
        self.python_building = false;
        self.python_refresh = None;
        self.generation += 1;
        self.indexing = false;
        self.reindex_requested = false;
        self.search_pending = false;
        self.git_pending = false;
        self.files.clear();
        self.python_project = pie_crust_core::PythonProjectLayout::default();
        self.hits.clear();
        self.git_log.clear();
        if !self.buffers.iter().any(|buffer| {
            Some(&buffer.id) == self.active_document.as_ref()
                && (buffer.worktree_id == self.worktree
                    || buffer.kind == pie_crust_core::DocumentKind::Scratch)
        }) {
            self.active_document = self
                .buffers
                .iter()
                .find(|buffer| {
                    buffer.worktree_id == self.worktree
                        || buffer.kind == pie_crust_core::DocumentKind::Scratch
                })
                .map(|buffer| buffer.id.clone());
        }
        self.chrome.revealed = None;
        if self.chrome.active_tool == Some(Tool::Git) {
            self.load_git();
        }
    }

    fn poll_workspace_loading(&mut self) {
        if let Some(result) = self.project_loading.as_mut().and_then(BackgroundLoad::poll) {
            self.project_loading = None;
            match result {
                Ok(prepared) => {
                    // Keep the shared workbench identity used by the MCP server.
                    let installed = match (self.shared.lock(), prepared.shared.lock()) {
                        (Ok(mut current), Ok(mut next)) => {
                            std::mem::swap(&mut *current, &mut *next);
                            true
                        }
                        _ => false,
                    };
                    if !installed {
                        self.error = Some("Projet : espace de travail indisponible.".into());
                        return;
                    }
                    self.project_path = prepared.project_path;
                    self.chrome = ChromeState::default();
                    self.editor_state = editor::EditorState::default();
                    self.autosave_pending.clear();
                    self.drawer_data = project_drawers::ProjectDrawers::default();
                    self.worktrees = prepared.worktrees;
                    self.worktree = prepared.worktree;
                    self.buffers.clear();
                    self.unsynced.clear();
                    self.active_document = None;
                    self.focus_serial = 0;
                    self.pending_focus = None;
                    self.error = prepared.error;
                    self.reset_worktree();
                    self.python_project = prepared.python_project;
                    self.runner.adopt_project(prepared.runner);
                    self.refresh_index();
                }
                Err(error) => self.error = Some(format!("Projet : {error}")),
            }
        }
        if let Some(result) = self
            .worktree_loading
            .as_mut()
            .and_then(BackgroundLoad::poll)
        {
            self.worktree_loading = None;
            match result {
                Ok(loaded) => {
                    self.python_project = loaded.layout;
                    self.runner.adopt_project(loaded.runner);
                    if let Some(error) = loaded.error {
                        self.error = Some(error);
                    }
                    self.refresh_index();
                    if let Some((path, environment)) = loaded.packages {
                        self.open_prepared_packages_page(path, environment);
                    }
                }
                Err(error) => {
                    self.index_status = "Échec du chargement".into();
                    self.error = Some(error);
                }
            }
        }
    }

    fn open_document(&mut self, path: &Path, line: usize, column: usize) {
        self.chrome.page = chrome::CenterPage::Code;
        if matches!(
            self.chrome.active_tool,
            Some(Tool::Packages | Tool::Settings)
        ) {
            self.chrome.active_tool = None;
        }
        let result = self
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
            .and_then(|mut state| state.focus_document(&self.worktree, path, line, column));
        if let Err(error) = result {
            self.error = Some(format!("Ouverture : {error:#}"));
        }
        self.poll();
    }

    fn save(&mut self, all: bool) -> bool {
        if all && self.packages_have_changes() && !self.save_packages_changes() {
            return false;
        }
        if all && self.chrome.config_dirty && !self.save_project_settings() {
            return false;
        }
        if !self.unsynced.is_empty() {
            self.error = Some("Une saisie en conflit est encore en mémoire. Conservez sa copie depuis l’éditeur avant d’enregistrer.".into());
            return false;
        }
        let result = (|| -> Result<()> {
            let mut state = self
                .shared
                .lock()
                .map_err(|_| anyhow::anyhow!("Workbench unavailable"))?;
            let ids: Vec<_> = state
                .documents()
                .into_iter()
                .filter(|doc| doc.dirty && (all || Some(&doc.id) == self.active_document.as_ref()))
                .map(|doc| doc.id)
                .collect();
            for id in ids {
                state.save_document(&id)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.poll();
                self.rebuild_index();
                true
            }
            Err(error) => {
                self.error = Some(format!("Enregistrement interrompu : {error:#}"));
                false
            }
        }
    }

    fn has_dirty_documents(&self) -> bool {
        self.packages_have_changes()
            || self.chrome.config_dirty
            || !self.unsynced.is_empty()
            || self
                .shared
                .lock()
                .map(|state| state.documents().iter().any(|document| document.dirty))
                .unwrap_or(true)
    }

    fn process_run_requests(&mut self) {
        while let Some(request) = self.runner.take_run_request() {
            let result = (|| -> Result<bool> {
                let mut state = self
                    .shared
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Workbench unavailable"))?;
                let belongs = state
                    .worktrees()
                    .iter()
                    .any(|tree| tree.id == request.worktree_id && tree.root == request.root);
                // Persistent terminals retain their own shell after a project switch.
                // RunPanel validates that the terminal id still owns this exact context.
                if !belongs && request.terminal_id.is_some() {
                    return Ok(false);
                }
                anyhow::ensure!(belongs, "Le projet de cette commande n’est plus ouvert");
                let relevant = |doc: &pie_crust_core::DocumentInfo| {
                    doc.worktree_id == request.worktree_id
                        || doc.kind == pie_crust_core::DocumentKind::Scratch
                };
                let documents = state.documents();
                anyhow::ensure!(
                    !documents
                        .iter()
                        .filter(|doc| relevant(doc))
                        .any(|doc| self.unsynced.contains(&doc.id)),
                    "Une saisie en conflit reste dans l’éditeur. Conservez sa copie et résolvez le conflit avant de lancer la commande."
                );
                let ids = documents
                    .iter()
                    .filter(|doc| relevant(doc) && doc.dirty)
                    .map(|doc| doc.id.clone())
                    .collect::<Vec<_>>();
                for id in &ids {
                    state.save_document(id)?;
                }
                Ok(!ids.is_empty())
            })();
            match result {
                Ok(saved) => {
                    self.runner.start_request(request);
                    if saved {
                        self.poll();
                        self.rebuild_index();
                    }
                }
                Err(error) => {
                    let message = format!("Lancement interrompu : {error:#}");
                    self.runner.set_launch_error(message.clone());
                    self.error = Some(message);
                }
            }
        }
    }

    fn request_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        if self.has_dirty_documents() {
            self.pending_action = Some(action);
        } else {
            self.perform_action(action, ctx);
        }
    }

    fn perform_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        match action {
            PendingAction::Exit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            PendingAction::OpenProject(path) => {
                if self.project_loading.is_some() {
                    return;
                }
                let token = self.token.clone();
                self.project_loading = Some(BackgroundLoad::start(path.clone(), move |progress| {
                    Self::load_project(&path, token, |stage| progress.stage(stage))
                }));
                ctx.request_repaint();
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let python_pending = self.python_building || self.python_refresh.is_some();
                if self.indexing || python_pending {
                    ui.spinner();
                } else {
                    ui.colored_label(ACCENT, "●");
                }
                ui.label(RichText::new(&self.index_status).size(11.0).color(MUTED));
                if python_pending {
                    ui.label(
                        RichText::new("Analyse Python en cours…")
                            .size(11.0)
                            .color(MUTED),
                    );
                }
                if ui
                    .add_enabled(
                        !self.indexing,
                        egui::Button::new("Actualiser l’index").small(),
                    )
                    .clicked()
                {
                    self.rebuild_index();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(if cfg!(target_os = "macos") {
                            "⌘S enregistrer"
                        } else {
                            "Ctrl+S enregistrer"
                        })
                        .size(11.0)
                        .color(MUTED),
                    );
                });
            });
            if let Some(error) = self.error.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::from_rgb(241, 163, 148), error);
                    if ui.small_button("Fermer").clicked() {
                        self.error = None;
                    }
                });
            }
        });
    }

    fn commit_buffer_edit(&mut self, index: usize) {
        let buffer = &self.buffers[index];
        let result = self
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("Workbench unavailable"))
            .and_then(|mut state| {
                state.edit_document(&buffer.id, buffer.version, buffer.text.clone())
            });
        match result {
            Ok(version) => {
                let buffer = &mut self.buffers[index];
                buffer.version = version;
                buffer.dirty = true;
                self.autosave_pending
                    .insert(buffer.id.clone(), Instant::now());
                self.python_revision += 1;
                self.python_refresh = Some(Instant::now());
                if self.unsynced.remove(&buffer.id) {
                    self.error = None;
                }
            }
            Err(error) => {
                // Every rejected edit remains visibly unsaved, even if the shared version
                // did not change (for example a NUL byte or an oversized paste).
                self.unsynced.insert(self.buffers[index].id.clone());
                self.buffers[index].dirty = true;
                self.error = Some(match self.recover_text(&self.buffers[index].text) {
                    Ok(path) => format!(
                        "Édition refusée : {error:#}. Votre saisie reste dans l’éditeur ; une copie est conservée dans {}",
                        path.display()
                    ),
                    Err(recovery_error) => format!(
                        "Édition refusée : {error:#}. Échec de récupération : {recovery_error}. Votre saisie reste en mémoire."
                    ),
                });
            }
        }
    }

    fn recover_text(&self, text: &str) -> Result<PathBuf> {
        let root = self
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("Workbench unavailable"))?
            .project_root()
            .join(".pie_crust/recovery");
        std::fs::create_dir_all(&root)?;
        let path = root.join(format!("edit-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, text)?;
        Ok(path)
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.show_connection {
            egui::Window::new("Connexion MCP").open(&mut self.show_connection).resizable(false).show(ctx, |ui| {
                if let Some(endpoint) = &self.endpoint {
                    ui.label("Connectez votre LLM à cette adresse :");
                    ui.monospace(endpoint);
                    if ui.button("Copier l’adresse").clicked() { ui.ctx().copy_text(endpoint.clone()); }
                    ui.separator();
                    ui.label("Authentification : Authorization: Bearer <token>");
                    if ui.button("Copier le token d’accès").clicked() { ui.ctx().copy_text(self.token.clone()); }
                    ui.label(RichText::new("Token de cette session. Utilisez PIE_CRUST_MCP_TOKEN pour un token stable.").size(11.0).color(MUTED));
                } else { ui.label("Serveur désactivé ou indisponible. Consultez le message d’erreur."); }
            });
        }
        if let Some(action) = self.pending_action.clone() {
            egui::Modal::new(egui::Id::new("unsaved_changes")).show(ctx, |ui| {
                ui.set_width(440.0);
                ui.heading("Des modifications ne sont pas enregistrées");
                ui.label("Enregistrez les documents et paramètres modifiés avant de continuer, ou abandonnez explicitement leurs modifications.");
                if self.chrome.config_dirty { ui.monospace(".pie_crust/config.toml"); }
                if self.packages_have_changes() { ui.label(format!("Page Paquets · {}", self.active_packages_path().unwrap_or(Path::new("pyproject.toml")).display())); }
                if let Ok(state) = self.shared.try_lock() {
                    for document in state.documents().iter().filter(|doc| doc.dirty).take(8) { ui.monospace(document.path.display().to_string()); }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Tout enregistrer").clicked() && self.save(true) {
                        self.pending_action = None;
                        self.perform_action(action.clone(), ctx);
                    }
                    if ui.button("Abandonner").clicked() {
                        self.pending_action = None;
                        self.perform_action(action, ctx);
                    }
                    if ui.button("Annuler").clicked() { self.pending_action = None; }
                });
            });
        }
    }
}

impl Desktop {
    fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_workspace_loading();
        if self.project_loading.is_none() {
            self.poll();
        } else {
            self.runner.poll();
        }
        if consume_quit_shortcut(&ctx) && !self.allow_close {
            self.request_action(PendingAction::Exit, &ctx);
        }
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_action(PendingAction::Exit, &ctx);
        }
        if self.project_loading.is_some() || self.worktree_loading.is_some() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.add_space((ui.available_height() * 0.25).max(24.0));
                if let Some(loading) = &self.project_loading {
                    loading.show(ui);
                } else if let Some(loading) = &self.worktree_loading {
                    loading.show(ui);
                }
            });
            self.dialogs(&ctx);
            return;
        }
        if ctx.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::S,
            ))
        }) {
            if self.chrome.page == chrome::CenterPage::Packages {
                self.save_packages_changes();
            } else if self.chrome.config_dirty
                && ctx.memory(|memory| memory.focused())
                    == Some(egui::Id::new("settings_config_editor"))
            {
                self.save_project_settings();
            } else {
                self.save(false);
            }
        }
        self.chrome_shortcuts(&ctx);
        self.top_bar(ui);
        self.status_bar(ui);
        self.activity_bar(ui);
        self.drawer(ui);
        self.bottom_panel(ui);
        match self.chrome.page {
            chrome::CenterPage::Code => {
                self.file_find_bar(ui);
                self.editor(ui);
            }
            chrome::CenterPage::Packages => {
                egui::CentralPanel::default().show(ui, |ui| self.packages_page(ui));
            }
            chrome::CenterPage::Settings => {
                egui::CentralPanel::default().show(ui, |ui| self.settings_page(ui));
            }
        }
        self.navigation_popups(&ctx);
        self.runner.show_run_popup(&ctx);
        self.show_refactor_menu(&ctx);
        self.dialogs(&ctx);
        self.process_run_requests();
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl eframe::App for Desktop {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

fn consume_quit_shortcut(ctx: &egui::Context) -> bool {
    ctx.input_mut(|input| {
        input.consume_shortcut(&egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND,
            egui::Key::Q,
        ))
    })
}

fn character_offset(text: &str, line: usize, column: usize) -> usize {
    text.split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(|line| line.chars().count())
        .sum::<usize>()
        + column.saturating_sub(1)
}

fn open_terminal(root: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        if std::process::Command::new("wt.exe")
            .arg("-d")
            .arg(root)
            .spawn()
            .is_err()
        {
            std::process::Command::new("powershell.exe")
                .arg("-NoExit")
                .current_dir(root)
                .creation_flags(0x00000010)
                .spawn()?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-a", "Terminal"])
            .arg(root)
            .spawn()?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("x-terminal-emulator")
            .current_dir(root)
            .spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(mut finished: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !finished() {
            assert!(
                Instant::now() < deadline,
                "Workspace loading did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn finish_indexing(app: &mut Desktop) {
        wait_for(|| {
            app.poll();
            !app.indexing && !app.python_building && app.python_refresh.is_none()
        });
    }

    #[test]
    fn native_theme_changes_keep_the_configured_style_and_native_scale() {
        // Backends can report the OS theme either before app creation or with
        // the first frame. Both startup sequences must use the same UI style.
        for initial_theme in [None, Some(egui::Theme::Light)] {
            let ctx = egui::Context::default();
            if let Some(theme) = initial_theme {
                ctx.run_ui(
                    egui::RawInput {
                        system_theme: Some(theme),
                        ..Default::default()
                    },
                    |_| {},
                )
                .drop_without_applying_deltas();
            }
            let mut welcome = Launcher::new(&ctx, None, 0, false, String::new());
            for (theme, scale) in [
                (egui::Theme::Light, 1.0),
                (egui::Theme::Dark, 1.5),
                (egui::Theme::Light, 2.0),
            ] {
                let mut input = egui::RawInput {
                    system_theme: Some(theme),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    ..Default::default()
                };
                input
                    .viewports
                    .get_mut(&egui::ViewportId::ROOT)
                    .unwrap()
                    .native_pixels_per_point = Some(scale);
                let output = ctx.run_ui(input, |ui| {
                    assert_eq!(ctx.system_theme(), Some(theme));
                    assert_eq!(ctx.theme(), egui::Theme::Dark);
                    assert!(ui.visuals().dark_mode);
                    assert_eq!(ui.visuals().panel_fill, Color32::from_rgb(23, 27, 35));
                    assert_eq!(ui.visuals().text_color(), Color32::from_rgb(214, 220, 231));
                    assert_eq!(ui.spacing().item_spacing, egui::vec2(9.0, 8.0));
                    assert_eq!(ui.spacing().button_padding, egui::vec2(10.0, 6.0));
                    assert_eq!(
                        egui::TextStyle::Body.resolve(ui.style()),
                        egui::FontId::proportional(14.0)
                    );
                    assert_eq!(ctx.zoom_factor(), 1.0);
                    assert_eq!(ctx.pixels_per_point(), scale);
                    welcome.render(ui);
                });
                assert!(!output.shapes.is_empty());
                output.drop_without_applying_deltas();
            }
        }
    }

    #[test]
    fn welcome_renders_without_a_project_and_survives_initial_open_failure() {
        let ctx = egui::Context::default();
        let mut welcome = Launcher::new(&ctx, None, 0, false, String::new());
        assert!(welcome.desktop.is_none());
        assert!(welcome.project_path.is_empty());
        assert!(welcome.error.is_none());
        let output = ctx.run_ui(Default::default(), |ui| welcome.render(ui));
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();

        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing-project");
        let mut invalid = Launcher::new(&ctx, Some(missing), 0, false, String::new());
        assert!(invalid.desktop.is_none());
        assert!(invalid.loading.is_some());
        wait_for(|| {
            invalid.poll_loading();
            invalid.loading.is_none()
        });
        assert!(invalid.error.is_some());
        let output = ctx.run_ui(Default::default(), |ui| invalid.render(ui));
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();
        assert!(!directory.path().join(".pie_crust").exists());

        // Retrying from the welcome screen starts another worker and clears the error.
        std::fs::create_dir_all(directory.path().join("backend")).unwrap();
        std::fs::write(
            directory.path().join("backend/pyproject.toml"),
            "[project]\nname = 'demo'\n",
        )
        .unwrap();
        invalid.project_path = directory.path().display().to_string();
        invalid.open_project();
        assert!(invalid.loading.is_some());
        assert!(invalid.error.is_none());
        wait_for(|| {
            invalid.poll_loading();
            invalid.loading.is_none()
        });
        let app = invalid
            .desktop
            .as_mut()
            .expect("The valid retry opens the desktop");
        assert_eq!(
            app.python_project.primary_manifest(),
            Some(Path::new("backend/pyproject.toml"))
        );
        finish_indexing(app);
        assert!(
            app.files
                .iter()
                .any(|file| file.path == Path::new("backend/pyproject.toml"))
        );
    }

    #[test]
    fn async_project_switch_keeps_current_buffers_on_failure_and_ignores_old_results() {
        let ctx = egui::Context::default();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("first.py"), "value = 1\n").unwrap();
        std::fs::write(second.path().join("second.py"), "value = 2\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(first.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        app.open_document(Path::new("first.py"), 1, 1);
        app.buffers[0].text = "value = 3\n".into();
        app.commit_buffer_edit(0);
        let document = app.buffers[0].id.clone();
        let first_worktree = app.worktree.clone();
        let generation = app.generation;

        app.perform_action(
            PendingAction::OpenProject(first.path().join("missing")),
            &ctx,
        );
        assert!(app.project_loading.is_some());
        assert_eq!(app.worktree, first_worktree);
        wait_for(|| {
            app.poll_workspace_loading();
            app.project_loading.is_none()
        });
        assert!(app.error.is_some());
        assert_eq!(app.worktree, first_worktree);
        assert_eq!(app.buffers[0].text, "value = 3\n");
        assert!(shared.lock().unwrap().document(&document).unwrap().dirty);

        // Exercise the existing save-before-switch action, then its asynchronous adoption.
        assert!(app.save(true));
        app.request_action(PendingAction::OpenProject(second.path().to_owned()), &ctx);
        assert!(app.project_loading.is_some());
        assert_eq!(app.worktree, first_worktree);
        wait_for(|| {
            app.poll_workspace_loading();
            app.project_loading.is_none()
        });
        assert!(Arc::ptr_eq(&shared, &app.shared));
        assert_eq!(
            shared.lock().unwrap().project_root(),
            second.path().canonicalize().unwrap()
        );
        assert!(app.buffers.is_empty());
        assert!(app.generation > generation);
        app.sender
            .send(Event::Indexed {
                generation,
                worktree: first_worktree,
                result: Ok((
                    vec![FileEntry {
                        path: PathBuf::from("stale.py"),
                    }],
                    "stale index".into(),
                    Default::default(),
                )),
            })
            .unwrap();
        finish_indexing(&mut app);
        assert!(
            app.files
                .iter()
                .any(|file| file.path == Path::new("second.py"))
        );
        assert!(
            !app.files
                .iter()
                .any(|file| file.path == Path::new("stale.py"))
        );
        assert_ne!(app.index_status, "stale index");
    }

    #[test]
    fn worktree_preparation_keeps_packages_ready_before_indexing() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("backend")).unwrap();
        std::fs::write(
            directory.path().join("backend/pyproject.toml"),
            "[project]\nname = 'demo'\n",
        )
        .unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.chrome.page = chrome::CenterPage::Packages;
        app.worktree_changed();
        assert!(app.worktree_loading.is_some());
        assert!(!app.python_building);
        assert!(app.python_refresh.is_none());
        wait_for(|| {
            app.poll_workspace_loading();
            app.worktree_loading.is_none()
        });
        assert_eq!(
            app.active_packages_path(),
            Some(Path::new("backend/pyproject.toml"))
        );
        assert!(app.indexing);
        finish_indexing(&mut app);
    }

    #[test]
    fn focus_uses_unicode_scalar_positions_with_crlf() {
        let text = "été\r\n函数(π)\nend";
        let offset = character_offset(text, 2, 4);
        assert_eq!(text.chars().nth(offset), Some('π'));
        assert_eq!(character_offset(text, 1, 1), 0);
    }

    #[test]
    fn headless_editor_focus_edit_and_close_preserve_the_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("example.py");
        let original = "# été\r\nvalue = 'π'\n";
        std::fs::write(&source, original).unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        app.open_document(Path::new("example.py"), 2, 10);
        let id = app.active_document.clone().unwrap();
        let editor_id = egui::Id::new(("code", &id));
        let ctx = egui::Context::default();
        let screen = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 800.0),
        ));
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: screen,
                ..Default::default()
            },
            |ui| app.render(ui),
        );
        output.drop_without_applying_deltas();
        let cursor = egui::TextEdit::load_state(&ctx, editor_id)
            .unwrap()
            .cursor
            .char_range()
            .unwrap();
        assert_eq!(cursor.primary.index.0, character_offset(original, 2, 10));
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(editor_id));

        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: screen,
                events: vec![egui::Event::Text("typed".into())],
                ..Default::default()
            },
            |ui| app.render(ui),
        );
        output.drop_without_applying_deltas();
        let edited = shared.lock().unwrap().document(&id).unwrap().clone();
        assert!(edited.text.contains("typed"));
        assert!(edited.dirty);
        let mac_command = egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: screen,
                events: vec![
                    egui::Event::ModifiersChanged(mac_command),
                    egui::Event::Key {
                        key: egui::Key::Q,
                        physical_key: Some(egui::Key::Q),
                        pressed: true,
                        repeat: false,
                        modifiers: mac_command,
                    },
                ],
                ..Default::default()
            },
            |ui| app.render(ui),
        );
        output.drop_without_applying_deltas();
        assert!(matches!(app.pending_action, Some(PendingAction::Exit)));
        assert!(!app.allow_close);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), original);

        std::fs::write(&source, "# external change\n").unwrap();
        assert!(!app.save(false));
        assert_eq!(
            shared.lock().unwrap().document(&id).unwrap().text,
            edited.text
        );
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "# external change\n"
        );
    }

    #[test]
    fn run_saves_edited_sources_and_blocks_on_disk_or_buffer_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("example.py");
        std::fs::write(&source, "value = 1\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.open_document(Path::new("example.py"), 1, 1);
        app.buffers[0].text = "value = 2\n".into();
        app.commit_buffer_edit(0);
        std::fs::write(&source, "# external edit\n").unwrap();
        app.runner.run_command(
            "Conflict",
            "echo should-not-run",
            run_panel::BottomView::Run,
        );
        app.process_run_requests();
        assert!(!app.runner.is_running());
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "# external edit\n"
        );
        assert!(app.error.as_ref().unwrap().contains("Lancement interrompu"));
        // Restoring the original disk version resolves the save conflict.
        std::fs::write(&source, "value = 1\n").unwrap();
        app.buffers[0].text = "invalid\0paste".into();
        app.commit_buffer_edit(0);
        app.runner.run_command(
            "Rejected edit",
            "echo should-not-run",
            run_panel::BottomView::Run,
        );
        app.process_run_requests();
        assert!(!app.runner.is_running());
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "value = 1\n");
        app.buffers[0].text = "value = 3\n".into();
        app.commit_buffer_edit(0);
        app.runner
            .run_command("Saved", "echo saved", run_panel::BottomView::Run);
        app.process_run_requests();
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "value = 3\n");
        assert!(!app.buffers[0].dirty);
    }

    #[test]
    fn rejected_edit_stays_unsaved_until_explicit_recovery_or_valid_correction() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("example.py"), "value = 1\n").unwrap();
        let shared = Arc::new(Mutex::new(Workbench::open(directory.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        app.open_document(Path::new("example.py"), 1, 1);
        let id = app.active_document.clone().unwrap();
        let before = shared.lock().unwrap().document(&id).unwrap().clone();
        app.buffers[0].text = "invalid\0paste".into();
        app.commit_buffer_edit(0);
        let rejected = shared.lock().unwrap().document(&id).unwrap().clone();
        assert_eq!(rejected.version, before.version);
        assert!(!rejected.dirty);
        assert!(app.unsynced.contains(&id));
        assert!(app.buffers[0].dirty);
        assert!(app.has_dirty_documents());
        app.poll();
        assert_eq!(app.buffers[0].text, "invalid\0paste");
        assert!(!app.save(false));
        assert!(
            std::fs::read_dir(directory.path().join(".pie_crust/recovery"))
                .unwrap()
                .next()
                .is_some()
        );
        app.request_action(PendingAction::Exit, &egui::Context::default());
        assert!(!app.allow_close);
        assert!(app.pending_action.is_some());

        app.buffers[0].text = "value = 2\n".into();
        app.commit_buffer_edit(0);
        assert!(!app.unsynced.contains(&id));
        let corrected = shared.lock().unwrap().document(&id).unwrap().clone();
        assert_eq!(corrected.text, "value = 2\n");
        assert!(corrected.dirty);
    }
}
