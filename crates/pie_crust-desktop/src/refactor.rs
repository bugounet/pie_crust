use super::{Desktop, MUTED};
use eframe::egui::{self, RichText};
use pie_crust_core::python::PythonWorkspace;
use std::{ops::Range, path::PathBuf, sync::Arc};

#[derive(Default)]
pub(super) struct RefactorMenu {
    open: bool,
    return_focus: Option<egui::Id>,
    return_selection: Option<egui::text::CCursorRange>,
    extraction: Option<VariableExtraction>,
}

struct VariableExtraction {
    document: String,
    path: PathBuf,
    source: String,
    selection: Range<usize>,
    snapshot: Arc<PythonWorkspace>,
    suggestions: Vec<String>,
    name: String,
}

const OPERATIONS: [(&str, &str); 7] = [
    (
        "Renommer une fonction…",
        "Renommer la déclaration et ses usages éligibles par typage.",
    ),
    (
        "Déplacer le code…",
        "Déplacer une déclaration vers un autre fichier ou module.",
    ),
    (
        "Réordonner les paramètres…",
        "Changer l’ordre des paramètres d’une fonction et adapter ses appels typés.",
    ),
    (
        "Convertir les arguments en kwargs…",
        "Transformer les arguments positionnels en arguments nommés.",
    ),
    (
        "Réordonner les arguments (sortedargs)…",
        "Réorganiser les arguments d’un appel selon la signature.",
    ),
    (
        "Extraire une fonction de la sélection…",
        "Créer une fonction à partir du code sélectionné.",
    ),
    (
        "Intégrer le corps de la fonction (inline)…",
        "Remplacer un appel par le corps de sa fonction.",
    ),
];

impl Desktop {
    fn begin_variable_extraction(&mut self) {
        let result = (|| -> anyhow::Result<VariableExtraction> {
            let index = self
                .buffers
                .iter()
                .position(|document| Some(&document.id) == self.active_document.as_ref())
                .ok_or_else(|| anyhow::anyhow!("Ouvrez un fichier Python"))?;
            let selection = self
                .chrome
                .refactor
                .return_selection
                .ok_or_else(|| anyhow::anyhow!("Sélectionnez une expression Python"))?;
            let selection = crate::editor::editing::byte_range(
                &self.buffers[index].text,
                selection.primary.index.0.min(selection.secondary.index.0)
                    ..selection.primary.index.0.max(selection.secondary.index.0),
            );
            let snapshot = self.editor_python_snapshot(index, true)?;
            let document = &self.buffers[index];
            let plan = snapshot.extract_variable(&document.path, selection.clone(), None)?;
            Ok(VariableExtraction {
                document: document.id.clone(),
                path: document.path.clone(),
                source: document.text.clone(),
                selection,
                snapshot,
                name: plan
                    .suggested_names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "value".into()),
                suggestions: plan.suggested_names,
            })
        })();
        match result {
            Ok(extraction) => self.chrome.refactor.extraction = Some(extraction),
            Err(error) => self.error = Some(format!("Extraction de variable : {error:#}")),
        }
    }

    pub(super) fn open_refactor_menu(&mut self, ctx: &egui::Context) {
        if !self.chrome.refactor.open {
            self.chrome.refactor.return_focus = ctx.memory(|memory| memory.focused());
            self.chrome.refactor.return_selection = self
                .chrome
                .refactor
                .return_focus
                .and_then(|id| egui::TextEdit::load_state(ctx, id))
                .and_then(|state| state.cursor.char_range());
        }
        self.chrome.refactor.open = true;
    }

    pub(super) fn show_refactor_menu(&mut self, ctx: &egui::Context) {
        if !self.chrome.refactor.open {
            // egui keeps the modal layer active until the closing frame ends.
            // Restore focus on the next frame, after that layer is gone.
            if let Some(id) = self.chrome.refactor.return_focus.take() {
                if let Some(selection) = self.chrome.refactor.return_selection.take()
                    && let Some(mut state) = egui::TextEdit::load_state(ctx, id)
                {
                    state.cursor.set_char_range(Some(selection));
                    egui::TextEdit::store_state(ctx, id, state);
                }
                ctx.memory_mut(|memory| memory.request_focus(id));
            }
            return;
        }
        let mut extract = false;
        let mut apply = None;
        let mut extraction = self.chrome.refactor.extraction.take();
        let response = egui::Modal::new(egui::Id::new("refactor_menu")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(if extraction.is_some() {
                "Extraire une variable"
            } else {
                "Refactoring"
            });
            if let Some(document) = self
                .buffers
                .iter()
                .find(|doc| Some(&doc.id) == self.active_document.as_ref())
            {
                ui.label(
                    RichText::new(document.path.display().to_string())
                        .small()
                        .color(MUTED),
                );
            } else {
                ui.label(RichText::new("Aucun fichier actif").small().color(MUTED));
            }
            ui.separator();
            if let Some(extraction) = &mut extraction {
                ui.label("Nom de la nouvelle variable");
                ui.add(
                    egui::TextEdit::singleline(&mut extraction.name)
                        .id(egui::Id::new("extract_variable_name")),
                );
                ui.horizontal_wrapped(|ui| {
                    for suggestion in &extraction.suggestions {
                        if ui
                            .selectable_label(&extraction.name == suggestion, suggestion)
                            .clicked()
                        {
                            extraction.name.clone_from(suggestion);
                        }
                    }
                });
                match extraction.snapshot.extract_variable(
                    &extraction.path,
                    extraction.selection.clone(),
                    Some(&extraction.name),
                ) {
                    Ok(plan) => {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Déclaration insérée avant l’instruction :").color(MUTED),
                        );
                        if let Some(edit) = plan.edits.iter().find(|edit| edit.range.is_empty()) {
                            ui.code(edit.replacement.trim_end());
                        }
                        if ui.button("Extraire la variable").clicked() {
                            apply = Some((
                                extraction.document.clone(),
                                extraction.source.clone(),
                                plan.edits,
                            ));
                        }
                    }
                    Err(error) => {
                        ui.colored_label(egui::Color32::LIGHT_RED, error.to_string());
                    }
                }
                ui.separator();
                return ui.button("Annuler · Échap").clicked();
            }
            let has_selection = self
                .chrome
                .refactor
                .return_selection
                .is_some_and(|selection| !selection.is_empty())
                && self.buffers.iter().any(|document| {
                    Some(&document.id) == self.active_document.as_ref()
                        && matches!(
                            document.path.extension().and_then(|value| value.to_str()),
                            Some("py" | "pyi" | "pyw")
                        )
                });
            if ui
                .add_enabled(
                    has_selection,
                    egui::Button::new("Extraire une variable…")
                        .min_size(egui::vec2(ui.available_width(), 30.0)),
                )
                .on_disabled_hover_text("Sélectionnez une expression Python complète.")
                .clicked()
            {
                extract = true;
            }
            ui.separator();
            for (label, description) in OPERATIONS {
                // The menu exposes the planned operations without pretending that
                // a semantic transformation is available in the current engine.
                ui.add_enabled(
                    false,
                    egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 30.0)),
                )
                .on_disabled_hover_text(description);
            }
            ui.separator();
            ui.label("Ces sept opérations ne sont pas encore disponibles dans cette version.");
            ui.add_space(4.0);
            ui.button("Fermer · Échap").clicked()
        });
        self.chrome.refactor.extraction = extraction;
        if extract {
            self.begin_variable_extraction();
        }
        if let Some((document, source, edits)) = apply
            && self.apply_editor_edits(ctx, &document, &source, &edits)
        {
            self.chrome.refactor.open = false;
            self.chrome.refactor.extraction = None;
            self.chrome.refactor.return_selection = self
                .chrome
                .refactor
                .return_focus
                .and_then(|id| egui::TextEdit::load_state(ctx, id))
                .and_then(|state| state.cursor.char_range());
            ctx.request_repaint();
        }
        if response.inner || response.should_close() {
            self.chrome.refactor.open = false;
            self.chrome.refactor.extraction = None;
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn frame(
        app: &mut Desktop,
        ctx: &egui::Context,
        shortcut: Option<(egui::Modifiers, egui::Key)>,
    ) -> String {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        if let Some((modifiers, key)) = shortcut {
            input.events = vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ];
        }
        let output = ctx.run_ui(input, |ui| app.render(ui));
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
        let mut text = String::new();
        for clipped in &output.shapes {
            text_of(&clipped.shape, &mut text);
        }
        output.drop_without_applying_deltas();
        text
    }

    #[test]
    fn refactor_shortcut_lists_operations_without_opening_run_or_editing_and_restores_focus() {
        for (modifiers, split) in [
            (egui::Modifiers::CTRL, false),
            (egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = "def charge(amount: int) -> int:\n    return amount\n";
            std::fs::write(root.path().join("billing.py"), source).unwrap();
            let shared = Arc::new(Mutex::new(
                pie_crust_core::Workbench::open(root.path()).unwrap(),
            ));
            let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
            let ctx = egui::Context::default();
            Desktop::configure_style(&ctx);
            app.open_document(Path::new("billing.py"), 2, 5);
            frame(&mut app, &ctx, None);
            let id = app.active_document.clone().unwrap();
            if split {
                app.set_editor_layout(crate::editor::EditorLayout::Columns);
                frame(&mut app, &ctx, None);
                ctx.memory_mut(|memory| {
                    memory.request_focus(egui::Id::new(("code", &id, 1_usize)))
                });
                frame(&mut app, &ctx, None);
            }
            let focus = ctx.memory(|memory| memory.focused());
            assert!(focus.is_some());
            if split {
                assert_eq!(focus, Some(egui::Id::new(("code", &id, 1_usize))));
            }
            let mut editor = egui::TextEdit::load_state(&ctx, focus.unwrap()).unwrap();
            editor
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(4),
                    egui::text::CCursor::new(10),
                )));
            egui::TextEdit::store_state(&ctx, focus.unwrap(), editor);
            let editor_before = egui::TextEdit::load_state(&ctx, focus.unwrap())
                .unwrap()
                .cursor
                .char_range();
            frame(
                &mut app,
                &ctx,
                Some((modifiers | egui::Modifiers::SHIFT, egui::Key::R)),
            );
            let text = frame(&mut app, &ctx, None);
            assert!(app.chrome.refactor.open);
            assert!(text.contains("Refactoring"));
            for (label, _) in OPERATIONS {
                assert!(text.contains(label), "Missing operation: {label}");
            }
            assert!(!text.contains("Exécuter une commande"));
            assert!(!app.runner.is_running());
            assert!(text.contains("pas encore disponibles"));
            frame(
                &mut app,
                &ctx,
                Some((egui::Modifiers::NONE, egui::Key::Escape)),
            );
            frame(&mut app, &ctx, None);
            assert!(!app.chrome.refactor.open);
            assert_eq!(ctx.memory(|memory| memory.focused()), focus);
            assert_eq!(
                egui::TextEdit::load_state(&ctx, focus.unwrap())
                    .unwrap()
                    .cursor
                    .char_range(),
                editor_before
            );
            let state = shared.lock().unwrap();
            assert_eq!(state.document(&id).unwrap().text, source);
            assert!(!state.document(&id).unwrap().dirty);
            drop(state);
            assert_eq!(
                std::fs::read_to_string(root.path().join("billing.py")).unwrap(),
                source
            );
            frame(&mut app, &ctx, Some((modifiers, egui::Key::R)));
            let text = frame(&mut app, &ctx, None);
            assert!(text.contains("Exécuter une commande"));
            assert!(!app.chrome.refactor.open);
        }
    }

    #[test]
    fn refactor_menu_also_opens_without_a_document() {
        let root = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(
            pie_crust_core::Workbench::open(root.path()).unwrap(),
        ));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        let ctx = egui::Context::default();
        Desktop::configure_style(&ctx);
        frame(
            &mut app,
            &ctx,
            Some((egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::R)),
        );
        let text = frame(&mut app, &ctx, None);
        assert!(text.contains("Aucun fichier actif"));
        for (label, _) in OPERATIONS {
            assert!(text.contains(label));
        }
    }

    #[test]
    fn variable_extraction_suggests_return_type_and_commits_a_crlf_preserving_edit() {
        let root = tempfile::tempdir().unwrap();
        let source = "class Invoice:\r\n  pass\r\n\r\ndef build_invoice() -> Invoice:\r\n  return Invoice()\r\n\r\ndef checkout() -> Invoice:\r\n  return build_invoice()\r\n";
        std::fs::write(root.path().join("billing.py"), source).unwrap();
        let shared = Arc::new(Mutex::new(
            pie_crust_core::Workbench::open(root.path()).unwrap(),
        ));
        let mut app = Desktop::from_workbench(shared.clone(), None, String::new(), None);
        let ctx = egui::Context::default();
        app.open_document(Path::new("billing.py"), 8, 10);
        frame(&mut app, &ctx, None);
        let start = source.rfind("build_invoice()").unwrap();
        app.select_in_active_editor(&ctx, start..start + "build_invoice()".len());
        app.open_refactor_menu(&ctx);
        app.begin_variable_extraction();
        let extraction = app.chrome.refactor.extraction.as_ref().unwrap();
        assert!(extraction.suggestions.iter().any(|name| name == "invoice"));
        let plan = extraction
            .snapshot
            .extract_variable(
                &extraction.path,
                extraction.selection.clone(),
                Some("invoice"),
            )
            .unwrap();
        let document = extraction.document.clone();
        let text = extraction.source.clone();
        assert!(app.apply_editor_edits(&ctx, &document, &text, &plan.edits));
        let changed = &app.buffers[0].text;
        assert!(changed.ends_with("  invoice = build_invoice()\r\n  return invoice\r\n"));
        assert!(!changed.replace("\r\n", "").contains('\n'));
        assert_eq!(
            shared.lock().unwrap().document(&document).unwrap().text,
            *changed
        );
    }
}
