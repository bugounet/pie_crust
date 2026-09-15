//! Persistent, line-oriented shells. A terminal owns one shell and its history.
use super::{BottomView, OUTPUT_LIMIT, OutputDecoder, RunRequest, terminate_tree};
use eframe::egui::{self, RichText};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Default)]
pub(super) struct Terminals {
    sessions: Vec<Terminal>,
    selected: Option<u64>,
    next_id: u64,
    drawer_index: usize,
    drawer_focus: bool,
    input_focus: bool,
    close_drawer: bool,
    pub(super) requested_view: bool,
}

impl Terminals {
    pub(super) fn active_selection(&self, ctx: &egui::Context) -> Option<String> {
        let session = self
            .selected
            .and_then(|id| self.sessions.iter().find(|session| session.id == id))?;
        let focused = ctx.memory(|memory| memory.focused())?;
        if focused == terminal_output_id(session.id) {
            selected_text(ctx, focused, &session.output)
        } else if focused == terminal_input_id(session.id) {
            selected_text(ctx, focused, &session.input)
        } else {
            None
        }
    }

    pub(super) fn open_drawer(&mut self) {
        self.drawer_index = self
            .selected
            .and_then(|id| self.sessions.iter().position(|s| s.id == id))
            .unwrap_or(self.sessions.len());
        self.drawer_focus = true;
    }

    pub(super) fn take_close_drawer(&mut self) -> bool {
        std::mem::take(&mut self.close_drawer)
    }

    pub(super) fn is_running(&self) -> bool {
        self.sessions.iter().any(|session| session.busy)
    }

    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        for session in &mut self.sessions {
            changed |= session.poll();
        }
        changed
    }

    pub(super) fn stop_all(&mut self) {
        for session in &mut self.sessions {
            if session.busy {
                session.stop();
            }
        }
    }

    fn create(&mut self, worktree_id: &str, root: &Path) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.sessions
            .push(Terminal::start(id, worktree_id.to_owned(), root.to_owned()));
        self.activate(id);
        id
    }

    fn activate(&mut self, id: u64) {
        self.selected = Some(id);
        self.close_drawer = true;
        self.drawer_focus = false;
        self.input_focus = true;
        self.requested_view = true;
    }

    pub(super) fn show_drawer(&mut self, ui: &mut egui::Ui, worktree_id: &str, root: &Path) {
        ui.strong("Terminaux");
        ui.label(RichText::new("↑ ↓ choisir · Entrée ouvrir").small().weak());
        let selector = ui.interact(
            ui.max_rect(),
            ui.id().with("terminal_selector"),
            egui::Sense::focusable_noninteractive(),
        );
        if self.drawer_focus {
            selector.request_focus();
            self.drawer_focus = false;
        }
        let focused = selector.has_focus();
        let mut activate = None;
        if focused {
            ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.drawer_index = (self.drawer_index + 1).min(self.sessions.len());
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.drawer_index = self.drawer_index.saturating_sub(1);
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    activate = Some(self.drawer_index);
                }
            });
        }
        egui::ScrollArea::vertical()
            .id_salt("terminal_session_list")
            .show(ui, |ui| {
                for (index, session) in self.sessions.iter().enumerate() {
                    let response = ui
                        .selectable_label(self.drawer_index == index, session.label())
                        .on_hover_text(format!(
                            "{}\n{}",
                            session.root.display(),
                            session.worktree_id
                        ));
                    if response.clicked() {
                        self.drawer_index = index;
                        activate = Some(index);
                    }
                    if focused && self.drawer_index == index {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                }
                if ui
                    .selectable_label(self.drawer_index == self.sessions.len(), "+  New session")
                    .clicked()
                {
                    activate = Some(self.sessions.len());
                }
            });
        if let Some(index) = activate {
            if let Some(session) = self.sessions.get(index) {
                self.activate(session.id);
            } else {
                self.create(worktree_id, root);
            }
        }
    }

    pub(super) fn show_bottom(
        &mut self,
        ui: &mut egui::Ui,
        worktree_id: &str,
        root: &Path,
    ) -> Option<RunRequest> {
        let mut select = None;
        ui.horizontal_wrapped(|ui| {
            for session in &self.sessions {
                if ui
                    .selectable_label(self.selected == Some(session.id), session.label())
                    .clicked()
                {
                    select = Some(session.id);
                }
            }
            if ui
                .button("+")
                .on_hover_text("Nouveau terminal dans le worktree actif")
                .clicked()
            {
                select = Some(self.create(worktree_id, root));
            }
        });
        if let Some(id) = select {
            self.selected = Some(id);
            self.input_focus = true;
        }
        let Some(index) = self
            .selected
            .and_then(|id| self.sessions.iter().position(|session| session.id == id))
        else {
            ui.label("Créez une session avec + ou Ctrl+9, puis Entrée.");
            return None;
        };
        let session = &mut self.sessions[index];
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(session.cwd.display().to_string())
                    .small()
                    .weak(),
            );
            if session.busy {
                ui.spinner();
                ui.label("En cours");
            } else if session.closed {
                ui.label("Session fermée");
            } else if let Some(code) = session.exit_code {
                ui.label(format!("Code {code}"));
            }
            if session.busy
                && ui
                    .small_button("Arrêter")
                    .on_hover_text("Arrêter le processus et fermer ce shell")
                    .clicked()
            {
                session.stop();
            }
            if ui.small_button("Copier").clicked() {
                ui.ctx().copy_text(session.output.clone());
            }
            if ui.small_button("Effacer").clicked() {
                session.output.clear();
            }
            if ui.small_button("Fermer").clicked() {
                close = true;
            }
        });
        let mut request = None;
        ui.horizontal(|ui| {
            let id = terminal_input_id(session.id);
            let previously_focused = ui.memory(|memory| memory.has_focus(id));
            if previously_focused && !session.busy {
                ui.input_mut(|input| {
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        session.previous_command();
                    }
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        session.next_command();
                    }
                });
            }
            let response = ui.add_enabled(
                !session.closed,
                egui::TextEdit::singleline(&mut session.input)
                    .id(id)
                    .hint_text(if session.busy {
                        "Entrée du programme…"
                    } else {
                        "Commande…"
                    })
                    .font(egui::TextStyle::Monospace)
                    .desired_width((ui.available_width() - 100.0).max(120.0)),
            );
            if self.input_focus {
                response.request_focus();
                self.input_focus = false;
            }
            let enter =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if ui
                .add_enabled(
                    !session.closed,
                    egui::Button::new(if session.busy { "Envoyer" } else { "Exécuter" }),
                )
                .clicked()
                || enter
            {
                if session.busy {
                    let line = std::mem::take(&mut session.input);
                    let _ = session
                        .controls
                        .send(TerminalControl::Input(format!("{line}\n")));
                } else if !session.input.trim().is_empty() {
                    request = Some(RunRequest {
                        name: session.input.trim().into(),
                        command: session.input.trim().into(),
                        view: BottomView::Terminal,
                        worktree_id: session.worktree_id.clone(),
                        root: session.root.clone(),
                        terminal_id: Some(session.id),
                    });
                }
                response.request_focus();
            }
        });
        if let Some(error) = &session.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        egui::ScrollArea::both()
            .id_salt(("terminal_output", session.id))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let mut output = session.output.as_str();
                ui.add(
                    egui::TextEdit::multiline(&mut output)
                        .id(terminal_output_id(session.id))
                        .font(egui::TextStyle::Monospace)
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                );
            });
        if close {
            self.sessions.remove(index);
            self.selected = self.sessions.last().map(|session| session.id);
        }
        request
    }

    pub(super) fn start_request(&mut self, request: RunRequest) -> Result<(), String> {
        let id = request
            .terminal_id
            .unwrap_or_else(|| self.create(&request.worktree_id, &request.root));
        let session = self
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or("Le terminal a été fermé.")?;
        if session.root != request.root || session.worktree_id != request.worktree_id {
            return Err("Le contexte du terminal a changé.".into());
        }
        session.execute(request.command)?;
        self.selected = Some(id);
        self.requested_view = true;
        Ok(())
    }
}

fn terminal_input_id(session: u64) -> egui::Id {
    egui::Id::new(("terminal_input", session))
}

fn terminal_output_id(session: u64) -> egui::Id {
    egui::Id::new(("terminal_output_text", session))
}

fn selected_text(ctx: &egui::Context, id: egui::Id, text: &str) -> Option<String> {
    let range = egui::TextEdit::load_state(ctx, id)?.cursor.char_range()?;
    let start = range.primary.index.0.min(range.secondary.index.0);
    let end = range.primary.index.0.max(range.secondary.index.0);
    (start < end).then(|| text.chars().skip(start).take(end - start).collect())
}

enum TerminalControl {
    Execute(String),
    Input(String),
    Stop,
}
enum TerminalEvent {
    Output(String),
    Completed(i32, PathBuf),
    Error(String),
    Closed,
}

struct Terminal {
    id: u64,
    worktree_id: String,
    root: PathBuf,
    cwd: PathBuf,
    input: String,
    output: String,
    history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,
    busy: bool,
    closed: bool,
    exit_code: Option<i32>,
    error: Option<String>,
    controls: Sender<TerminalControl>,
    events: Option<Receiver<TerminalEvent>>,
    worker: Option<JoinHandle<()>>,
}

impl Terminal {
    fn start(id: u64, worktree_id: String, root: PathBuf) -> Self {
        let (controls, control_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::sync_channel(64);
        let worker_root = root.clone();
        let worker =
            thread::spawn(move || terminal_process(&worker_root, control_receiver, event_sender));
        Self {
            id,
            worktree_id,
            cwd: root.clone(),
            root,
            input: String::new(),
            output: String::new(),
            history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            busy: false,
            closed: false,
            exit_code: None,
            error: None,
            controls,
            events: Some(events),
            worker: Some(worker),
        }
    }

    fn label(&self) -> String {
        let status = if self.busy {
            "▶"
        } else if self.closed {
            "■"
        } else {
            "›"
        };
        format!(
            "{status} {}",
            self.history.last().map_or("New session", String::as_str)
        )
    }

    fn execute(&mut self, command: String) -> Result<(), String> {
        if self.closed {
            return Err("Ce shell est fermé ; créez une nouvelle session.".into());
        }
        if self.busy {
            return Err("Une commande est déjà en cours dans ce terminal.".into());
        }
        if command.len() > 64 * 1024 {
            return Err("Commande trop longue (64 Kio maximum).".into());
        }
        self.controls
            .send(TerminalControl::Execute(command.clone()))
            .map_err(|_| "Le shell est fermé.")?;
        self.append_output(&format!("\n$ {command}\n"));
        self.history.push(command);
        if self.history.len() > 500 {
            self.history.remove(0);
        }
        self.history_cursor = None;
        self.history_draft.clear();
        self.input.clear();
        self.busy = true;
        self.error = None;
        Ok(())
    }

    fn previous_command(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft.clone_from(&self.input);
                self.history.len() - 1
            }
        };
        self.history_cursor = Some(index);
        self.input.clone_from(&self.history[index]);
    }

    fn next_command(&mut self) {
        if let Some(index) = self.history_cursor {
            if index + 1 < self.history.len() {
                self.history_cursor = Some(index + 1);
                self.input.clone_from(&self.history[index + 1]);
            } else {
                self.history_cursor = None;
                self.input.clone_from(&self.history_draft);
            }
        }
    }

    fn append_output(&mut self, text: &str) {
        self.output.push_str(text);
        if self.output.len() > OUTPUT_LIMIT {
            let mut start = self.output.len() - OUTPUT_LIMIT;
            while !self.output.is_char_boundary(start) {
                start += 1;
            }
            self.output.drain(..start);
        }
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..128 {
            let Some(events) = &self.events else {
                break;
            };
            match events.try_recv() {
                Ok(TerminalEvent::Output(output)) => self.append_output(&output),
                Ok(TerminalEvent::Completed(code, cwd)) => {
                    self.busy = false;
                    self.exit_code = Some(code);
                    self.cwd = cwd;
                }
                Ok(TerminalEvent::Closed) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    self.busy = false;
                    self.events.take();
                    break;
                }
                Ok(TerminalEvent::Error(error)) => {
                    self.error = Some(error);
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
            changed = true;
        }
        changed
    }

    fn stop(&mut self) {
        let _ = self.controls.send(TerminalControl::Stop);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.stop();
        self.events.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn persistent_shell(root: &Path) -> Command {
    #[cfg(windows)]
    let mut shell = {
        use std::os::windows::process::CommandExt;
        let mut shell = Command::new("powershell.exe");
        shell.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "-"]);
        shell.creation_flags(0x0800_0000);
        shell
    };
    #[cfg(unix)]
    let mut shell = {
        use std::os::unix::process::CommandExt;
        let mut shell = Command::new("/bin/sh");
        shell.process_group(0);
        shell
    };
    shell
        .current_dir(root)
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    shell
}

fn command_frame(command: &str, marker: &str, windows: bool) -> String {
    if windows {
        let literal = command
            .replace('\r', "")
            .split('\n')
            .map(|line| super::quote_shell_argument(line, true))
            .collect::<Vec<_>>()
            .join(" + [char]10 + ");
        format!(
            "$global:LASTEXITCODE = 0; try {{ Invoke-Expression ({literal}); $pie_crust_result = if ($?) {{ $LASTEXITCODE }} elseif ($LASTEXITCODE -ne 0) {{ $LASTEXITCODE }} else {{ 1 }} }} catch {{ [Console]::Error.WriteLine($_.ToString()); $pie_crust_result = 1 }}; [Console]::Out.WriteLine('{marker}' + $pie_crust_result + ':' + (Get-Location).Path)\n"
        )
    } else {
        format!(
            "eval {}; pie_crust_result=$?; printf '\\n{marker}%s:%s\\n' \"$pie_crust_result\" \"$PWD\"\n",
            super::quote_shell_argument(command, false)
        )
    }
}

fn terminal_process(
    root: &Path,
    controls: Receiver<TerminalControl>,
    events: SyncSender<TerminalEvent>,
) {
    let mut child = match persistent_shell(root).spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = events.send(TerminalEvent::Error(format!(
                "Lancement du shell : {error}"
            )));
            let _ = events.send(TerminalEvent::Closed);
            return;
        }
    };
    let marker = format!("__PIE_CRUST_{}__:", uuid::Uuid::new_v4().simple());
    let (input, inputs) = mpsc::sync_channel::<String>(16);
    let (reader_done, readers_done) = mpsc::channel();
    if let Some(mut stdin) = child.stdin.take() {
        let events = events.clone();
        thread::spawn(move || {
            #[cfg(windows)]
            if stdin.write_all(b"[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding\n").and_then(|()| stdin.flush()).is_err() { return; }
            while let Ok(text) = inputs.recv() {
                if let Err(error) = stdin
                    .write_all(text.as_bytes())
                    .and_then(|()| stdin.flush())
                {
                    let _ = events.send(TerminalEvent::Error(format!(
                        "Entrée du terminal : {error}"
                    )));
                    break;
                }
            }
        });
    }
    if let Some(stdout) = child.stdout.take() {
        let events = events.clone();
        let marker = marker.clone();
        let done = reader_done.clone();
        thread::spawn(move || {
            terminal_output(stdout, events, Some(&marker));
            let _ = done.send(());
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let events = events.clone();
        let done = reader_done.clone();
        thread::spawn(move || {
            terminal_output(stderr, events, None);
            let _ = done.send(());
        });
    }
    drop(reader_done);
    loop {
        let payload = match controls.recv_timeout(Duration::from_millis(40)) {
            Ok(TerminalControl::Execute(command)) => {
                Some(command_frame(&command, &marker, cfg!(windows)))
            }
            Ok(TerminalControl::Input(line)) if line.len() <= 64 * 1024 => Some(line),
            Ok(TerminalControl::Input(_)) => {
                let _ = events.send(TerminalEvent::Error(
                    "Entrée trop longue (64 Kio maximum).".into(),
                ));
                None
            }
            Ok(TerminalControl::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                terminate_tree(&mut child);
                let _ = child.wait();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => None,
        };
        if let Some(payload) = payload
            && input.try_send(payload).is_err()
        {
            let _ = events.send(TerminalEvent::Error(
                "Le programme ne lit pas son entrée ; la ligne n’a pas été envoyée.".into(),
            ));
        }
        if !matches!(child.try_wait(), Ok(None)) {
            break;
        }
    }
    drop(input);
    let deadline = std::time::Instant::now() + Duration::from_millis(600);
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        if readers_done.recv_timeout(remaining).is_err() {
            break;
        }
    }
    let _ = events.send(TerminalEvent::Closed);
}

fn terminal_output(mut reader: impl Read, events: SyncSender<TerminalEvent>, marker: Option<&str>) {
    let mut buffer = [0_u8; 4096];
    let mut decoder = OutputDecoder::default();
    let mut pending = String::new();
    loop {
        let length = match reader.read(&mut buffer) {
            Ok(length) => length,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => 0,
        };
        pending.push_str(&decoder.push(&buffer[..length], length == 0));
        if let Some(marker) = marker {
            if !drain_terminal_output(&mut pending, marker, length == 0, &events) {
                return;
            }
        } else if !pending.is_empty()
            && events
                .send(TerminalEvent::Output(std::mem::take(&mut pending)))
                .is_err()
        {
            return;
        }
        if length == 0 {
            break;
        }
    }
}

fn drain_terminal_output(
    pending: &mut String,
    marker: &str,
    finished: bool,
    events: &SyncSender<TerminalEvent>,
) -> bool {
    loop {
        if let Some(start) = pending.find(marker) {
            if start > 0
                && events
                    .send(TerminalEvent::Output(pending.drain(..start).collect()))
                    .is_err()
            {
                return false;
            }
            let Some(end) = pending.find('\n') else {
                break;
            };
            let line: String = pending.drain(..=end).collect();
            if let Some((code, cwd)) = line[marker.len()..].trim_end().split_once(':')
                && let Ok(code) = code.parse()
            {
                if events
                    .send(TerminalEvent::Completed(code, cwd.into()))
                    .is_err()
                {
                    return false;
                }
            } else if events.send(TerminalEvent::Output(line)).is_err() {
                return false;
            }
        } else {
            let keep = if finished {
                0
            } else {
                (1..marker.len())
                    .rev()
                    .find(|length| pending.ends_with(&marker[..*length]))
                    .unwrap_or(0)
            };
            let emit = pending.len() - keep;
            if emit > 0
                && events
                    .send(TerminalEvent::Output(pending.drain(..emit).collect()))
                    .is_err()
            {
                return false;
            }
            break;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait_command(session: &mut Terminal) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while session.busy && Instant::now() < deadline {
            session.poll();
            thread::sleep(Duration::from_millis(15));
        }
        session.poll();
        assert!(!session.busy, "{} {:?}", session.output, session.error);
        assert!(!session.closed, "{} {:?}", session.output, session.error);
    }

    #[test]
    fn active_selection_reads_the_focused_terminal_text() {
        let root = tempfile::tempdir().unwrap();
        let mut terminals = Terminals::default();
        let id = terminals.create("worktree", root.path());
        terminals.sessions[0].output = "zero needle end".into();
        let ctx = egui::Context::default();
        let output_id = terminal_output_id(id);
        let mut state = egui::text_edit::TextEditState::default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(5),
                egui::text::CCursor::new(11),
            )));
        egui::TextEdit::store_state(&ctx, output_id, state);
        ctx.memory_mut(|memory| memory.request_focus(output_id));

        assert_eq!(terminals.active_selection(&ctx).as_deref(), Some("needle"));
    }

    #[test]
    fn shells_keep_directory_variables_and_independent_histories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        let mut first = Terminal::start(1, "first".into(), root.path().into());
        let mut second = Terminal::start(2, "first".into(), root.path().into());
        first
            .execute(
                if cfg!(windows) {
                    "$answer = 'remembered'; Set-Location nested"
                } else {
                    "answer=remembered; cd nested"
                }
                .into(),
            )
            .unwrap();
        wait_command(&mut first);
        first
            .execute(
                if cfg!(windows) {
                    "[Console]::WriteLine($answer); [Console]::WriteLine((Get-Location).Path)"
                } else {
                    "printf '%s\\n' \"$answer\"; pwd"
                }
                .into(),
            )
            .unwrap();
        wait_command(&mut first);
        second.execute(if cfg!(windows) { "[Console]::WriteLine('value=' + $answer); [Console]::WriteLine((Get-Location).Path)" } else { "printf 'value=%s\\n' \"$answer\"; pwd" }.into()).unwrap();
        wait_command(&mut second);
        assert!(first.output.contains("remembered"));
        assert_eq!(
            first.cwd.canonicalize().unwrap(),
            root.path().join("nested").canonicalize().unwrap()
        );
        assert_eq!(
            second.cwd.canonicalize().unwrap(),
            root.path().canonicalize().unwrap()
        );
        assert!(!second.output.contains("remembered"));
        assert_eq!(first.history.len(), 2);
        assert_eq!(second.history.len(), 1);
        first.input = "draft".into();
        first.previous_command();
        first.previous_command();
        assert_eq!(first.input, first.history[0]);
        first.next_command();
        first.next_command();
        assert_eq!(first.input, "draft");
    }

    #[test]
    fn completion_frames_survive_split_reads_without_hiding_regular_output() {
        let (sender, events) = mpsc::sync_channel(20);
        let marker = "__PIE_CRUST_test__:";
        let mut pending = "output__PIE_CRUST_te".to_owned();
        assert!(drain_terminal_output(&mut pending, marker, false, &sender));
        assert!(
            matches!(events.try_recv().unwrap(), TerminalEvent::Output(text) if text == "output")
        );
        pending.push_str("st__:7:C:\\project\\sub\nnext");
        assert!(drain_terminal_output(&mut pending, marker, false, &sender));
        assert!(
            matches!(events.try_recv().unwrap(), TerminalEvent::Completed(7, path) if path == Path::new("C:\\project\\sub"))
        );
        assert!(
            matches!(events.try_recv().unwrap(), TerminalEvent::Output(text) if text == "next")
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn terminal_picker_uses_arrows_enter_and_returns_focus_to_the_input() {
        let root = tempfile::tempdir().unwrap();
        let mut terminals = Terminals::default();
        let ctx = egui::Context::default();
        let frame = |terminals: &mut Terminals, key: Option<egui::Key>| {
            let events = key
                .map(|key| {
                    vec![egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]
                })
                .unwrap_or_default();
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::Panel::left("test_terminal_drawer")
                        .show(ui, |ui| terminals.show_drawer(ui, "worktree", root.path()));
                    egui::CentralPanel::default().show(ui, |ui| {
                        terminals.show_bottom(ui, "worktree", root.path());
                    });
                },
            )
            .drop_without_applying_deltas();
        };
        terminals.open_drawer();
        frame(&mut terminals, None);
        let selector_focus = ctx.memory(|memory| memory.focused());
        frame(&mut terminals, Some(egui::Key::Enter));
        assert_eq!(terminals.sessions.len(), 1);
        assert!(terminals.take_close_drawer());
        assert_ne!(ctx.memory(|memory| memory.focused()), selector_focus);
        terminals.open_drawer();
        frame(&mut terminals, None);
        frame(&mut terminals, Some(egui::Key::ArrowDown));
        assert_eq!(terminals.drawer_index, 1);
        frame(&mut terminals, Some(egui::Key::Enter));
        assert_eq!(terminals.sessions.len(), 2);
        terminals.open_drawer();
        frame(&mut terminals, None);
        frame(&mut terminals, Some(egui::Key::ArrowUp));
        frame(&mut terminals, Some(egui::Key::Enter));
        assert_eq!(terminals.selected, Some(terminals.sessions[0].id));
        assert!(terminals.take_close_drawer());
    }
}
