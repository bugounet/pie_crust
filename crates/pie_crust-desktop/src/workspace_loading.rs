//! Workspace preparation runs off the UI thread so the loading screen can animate.

use super::{ACCENT, MUTED};
use eframe::egui::{self, RichText};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

enum LoadEvent<T> {
    Stage(&'static str),
    Finished(Result<T, String>),
}

pub(super) struct LoadProgress<T> {
    sender: mpsc::Sender<LoadEvent<T>>,
}

impl<T> LoadProgress<T> {
    pub(super) fn stage(&self, stage: &'static str) {
        let _ = self.sender.send(LoadEvent::Stage(stage));
    }
}

pub(super) struct BackgroundLoad<T> {
    receiver: mpsc::Receiver<LoadEvent<T>>,
    path: PathBuf,
    stage: &'static str,
    started: Instant,
}

impl<T: Send + 'static> BackgroundLoad<T> {
    pub(super) fn start(
        path: PathBuf,
        load: impl FnOnce(&LoadProgress<T>) -> Result<T, String> + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let progress = LoadProgress { sender };
        std::thread::spawn(move || {
            let result = load(&progress);
            let _ = progress.sender.send(LoadEvent::Finished(result));
        });
        Self {
            receiver,
            path,
            stage: "Ouverture du projet…",
            started: Instant::now(),
        }
    }

    pub(super) fn poll(&mut self) -> Option<Result<T, String>> {
        loop {
            match self.receiver.try_recv() {
                Ok(LoadEvent::Stage(stage)) => self.stage = stage,
                Ok(LoadEvent::Finished(result)) => return Some(result),
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Some(Err("Le chargement du workspace a été interrompu.".into()));
                }
            }
        }
    }

    pub(super) fn show(&self, ui: &mut egui::Ui) {
        show_loading(ui, &self.path, self.stage, Some(self.started.elapsed()));
    }
}

pub(super) fn show_loading(ui: &mut egui::Ui, path: &Path, stage: &str, elapsed: Option<Duration>) {
    ui.vertical_centered(|ui| {
        ui.add(egui::Spinner::new().size(32.0).color(ACCENT));
        ui.add_space(16.0);
        ui.heading("Chargement du workspace");
        ui.add_space(8.0);
        ui.label(RichText::new(stage).color(ACCENT));
        ui.label(
            RichText::new(path.display().to_string())
                .small()
                .color(MUTED),
        );
        ui.add_space(8.0);
        ui.label(RichText::new("Les grands projets peuvent prendre un peu de temps.").color(MUTED));
        if let Some(elapsed) = elapsed {
            ui.label(
                RichText::new(format!("{} s écoulées", elapsed.as_secs()))
                    .small()
                    .color(MUTED),
            );
        }
    });
    ui.ctx().request_repaint_after(Duration::from_millis(50));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish<T: Send + 'static>(loading: &mut BackgroundLoad<T>) -> Result<T, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = loading.poll() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "The loading worker did not finish"
            );
            std::thread::yield_now();
        }
    }

    fn text_of(shape: &egui::Shape, text: &mut String) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_of(shape, text);
                }
            }
            egui::Shape::Text(shape) => text.push_str(&shape.galley.job.text),
            _ => {}
        }
    }

    #[test]
    fn pending_worker_keeps_rendering_its_stage_until_released() {
        let (ready, prepared) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let path = PathBuf::from("large-python-project");
        let mut loading = BackgroundLoad::start(path.clone(), move |progress| {
            progress.stage("Découverte des projets Python…");
            ready.send(()).unwrap();
            released.recv().unwrap();
            Ok(42)
        });
        prepared.recv_timeout(Duration::from_secs(5)).unwrap();

        let ctx = egui::Context::default();
        for frame in 0..3 {
            assert!(loading.poll().is_none());
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(frame as f64 * 0.1),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| loading.show(ui),
            );
            let mut text = String::new();
            for clipped in &output.shapes {
                text_of(&clipped.shape, &mut text);
            }
            assert!(text.contains("Chargement du workspace"));
            assert!(text.contains("Découverte des projets Python…"));
            assert!(text.contains(&path.display().to_string()));
            output.drop_without_applying_deltas();
        }

        release.send(()).unwrap();
        assert_eq!(finish(&mut loading), Ok(42));
    }

    #[test]
    fn failed_worker_returns_its_error_after_the_last_stage() {
        let mut loading =
            BackgroundLoad::<()>::start(PathBuf::from("missing-project"), |progress| {
                progress.stage("Découverte des worktrees…");
                Err("Le dossier du projet est introuvable.".into())
            });

        assert_eq!(
            finish(&mut loading),
            Err("Le dossier du projet est introuvable.".into())
        );
        assert_eq!(loading.stage, "Découverte des worktrees…");
    }

    #[test]
    fn disconnected_worker_reports_interruption_instead_of_loading_forever() {
        let (sender, receiver) = mpsc::channel::<LoadEvent<()>>();
        let mut loading = BackgroundLoad {
            receiver,
            path: PathBuf::from("interrupted-project"),
            stage: "Ouverture du projet…",
            started: Instant::now(),
        };
        sender
            .send(LoadEvent::Stage("Préparation Python…"))
            .unwrap();
        drop(sender);

        assert_eq!(
            loading.poll(),
            Some(Err("Le chargement du workspace a été interrompu.".into()))
        );
        assert_eq!(loading.stage, "Préparation Python…");
    }
}
