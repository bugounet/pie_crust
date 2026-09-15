//! Editable pyproject pages share source buffers and their conflict-safe save path.
use super::super::{Desktop, MUTED};
use crate::package_environment::{
    DependencyStatus, inspection_args, package_manager_args, parse_inspection,
};
use crate::run_panel::BottomView;
use anyhow::{Context, Result, bail, ensure};
use eframe::egui::{self, RichText};
use pie_crust_core::{Document, Workbench};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use toml_edit::{Array, DocumentMut, Item, Table, Value, value};

#[derive(Default)]
pub(super) struct PackagePages {
    forms: BTreeMap<(String, PathBuf), PackageEditor>,
    active: BTreeMap<String, PathBuf>,
    error: Option<String>,
    outdated: BTreeMap<String, OutdatedResult>,
    observed_upgrades: std::collections::BTreeSet<u64>,
    environments: BTreeMap<(String, PathBuf), Option<pie_crust_core::PythonEnvironment>>,
    inspections: BTreeMap<(String, PathBuf), EnvironmentInspection>,
}

struct EnvironmentInspection {
    dependencies: Vec<String>,
    previous_execution: Option<u64>,
    pending: bool,
    packages: Vec<DependencyStatus>,
    error: Option<String>,
}

struct OutdatedResult {
    execution_id: u64,
    packages: Vec<OutdatedPackage>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct OutdatedPackage {
    name: String,
    version: String,
    latest_version: String,
}

struct PackageEditor {
    document_id: String,
    document_version: u64,
    original: String,
    document: DocumentMut,
    form: PackageForm,
    initial_form: PackageForm,
    source: String,
    advanced: bool,
    poetry: bool,
    error: Option<String>,
    last_seen_form: PackageForm,
    last_seen_source: String,
    edited_at: Option<Instant>,
    autosave_attempted: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct PackageForm {
    name: String,
    version: String,
    description: String,
    requires_python: String,
    dependencies: Vec<String>,
    scripts: Vec<(String, String)>,
}

impl PackageEditor {
    fn from_document(document: Document) -> Result<Self> {
        let source = document.text.replace("\r\n", "\n");
        let parsed = source
            .parse::<DocumentMut>()
            .context("Le pyproject.toml contient une erreur TOML")?;
        let poetry = parsed.get("project").is_none()
            && parsed
                .get("tool")
                .and_then(|item| item.get("poetry"))
                .is_some();
        let form = PackageForm::read(&parsed, poetry);
        Ok(Self {
            document_id: document.id,
            document_version: document.version,
            original: document.text,
            document: parsed,
            initial_form: form.clone(),
            last_seen_form: form.clone(),
            form,
            last_seen_source: source.clone(),
            source,
            advanced: false,
            poetry,
            error: None,
            edited_at: None,
            autosave_attempted: false,
        })
    }

    fn note_edit(&mut self) {
        if self.form != self.last_seen_form || self.source != self.last_seen_source {
            self.last_seen_form.clone_from(&self.form);
            self.last_seen_source.clone_from(&self.source);
            self.edited_at = Some(Instant::now());
            self.autosave_attempted = false;
            self.error = None;
        }
    }

    fn dirty(&self) -> bool {
        self.form != self.initial_form || self.source != self.original.replace("\r\n", "\n")
    }

    fn rendered(&self) -> Result<String> {
        let mut document = if self.source != self.original.replace("\r\n", "\n") {
            self.source
                .parse::<DocumentMut>()
                .context("Le TOML modifié n’est pas valide")?
        } else {
            self.document.clone()
        };
        self.form
            .apply(&self.initial_form, &mut document, self.poetry)?;
        let rendered = document.to_string();
        rendered
            .parse::<DocumentMut>()
            .context("Le résultat contient une erreur TOML")?;
        // The form preserves the original newline convention, including inserted fields.
        Ok(if self.original.contains("\r\n") {
            rendered.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            rendered
        })
    }

    fn save(&mut self, workbench: &mut Workbench) -> Result<()> {
        let text = self.rendered()?;
        let current = workbench
            .document(&self.document_id)
            .context("Le fichier n’est plus ouvert")?
            .clone();
        if !current.dirty {
            workbench.read_document(&current.worktree_id, &current.path)?;
        }
        let current = workbench
            .document(&self.document_id)
            .context("Le fichier n’est plus ouvert")?;
        ensure!(
            current.version == self.document_version,
            "Le fichier a été modifié dans l’éditeur depuis l’ouverture du formulaire. Rechargez le formulaire pour reprendre ces modifications."
        );
        self.document_version =
            workbench.edit_document(&self.document_id, self.document_version, text)?;
        workbench.save_document(&self.document_id)?;
        let document = workbench
            .document(&self.document_id)
            .context("Le document a été fermé")?
            .clone();
        let advanced = self.advanced;
        *self = Self::from_document(document)?;
        self.advanced = advanced;
        Ok(())
    }
}

impl PackageForm {
    fn read(document: &DocumentMut, poetry: bool) -> Self {
        let section = if poetry {
            document.get("tool").and_then(|item| item.get("poetry"))
        } else {
            document.get("project")
        };
        let text = |name: &str| {
            section
                .and_then(|section| section.get(name))
                .and_then(Item::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let dependencies = if poetry {
            section
                .and_then(|section| section.get("dependencies"))
                .and_then(Item::as_table_like)
                .map(|table| {
                    table
                        .iter()
                        .filter(|(name, _)| *name != "python")
                        .map(|(name, value)| format!("{name} = {}", value.to_string().trim()))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            section
                .and_then(|section| section.get("dependencies"))
                .and_then(Item::as_array)
                .map(|array| {
                    array
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let scripts = section
            .and_then(|section| section.get("scripts"))
            .and_then(Item::as_table_like)
            .map(|table| {
                table
                    .iter()
                    .filter_map(|(name, value)| {
                        value.as_str().map(|target| (name.into(), target.into()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            name: text("name"),
            version: text("version"),
            description: text("description"),
            requires_python: if poetry {
                section
                    .and_then(|section| section.get("dependencies"))
                    .and_then(|dependencies| dependencies.get("python"))
                    .and_then(Item::as_str)
                    .unwrap_or("")
                    .to_owned()
            } else {
                text("requires-python")
            },
            dependencies,
            scripts,
        }
    }

    fn apply(&self, previous: &Self, document: &mut DocumentMut, poetry: bool) -> Result<()> {
        if self == previous {
            return Ok(());
        }
        if self.name != previous.name {
            ensure!(
                valid_package_name(&self.name),
                "Le nom du paquet accepte lettres ASCII, chiffres, points, tirets et underscores."
            );
        }
        if self.version != previous.version {
            ensure!(
                !self.version.trim().is_empty(),
                "Saisissez une version avant d’enregistrer."
            );
        }
        let section = if poetry {
            &mut document["tool"]["poetry"]
        } else {
            &mut document["project"]
        };
        if section.is_none() {
            *section = Item::Table(Table::new());
        }
        ensure!(
            section.is_table_like(),
            "La section du paquet doit être une table TOML."
        );
        for (key, current, old) in [
            ("name", &self.name, &previous.name),
            ("version", &self.version, &previous.version),
            ("description", &self.description, &previous.description),
        ] {
            if current != old {
                replace_string(&mut section[key], current);
                if !poetry
                    && let Some(dynamic) = section.get_mut("dynamic").and_then(Item::as_array_mut)
                {
                    dynamic.retain(|value| value.as_str() != Some(key));
                }
            }
        }
        if self.requires_python != previous.requires_python {
            if poetry {
                replace_string(
                    &mut section["dependencies"]["python"],
                    &self.requires_python,
                );
            } else {
                replace_string(&mut section["requires-python"], &self.requires_python);
            }
        }
        if self.dependencies != previous.dependencies {
            if poetry {
                let text = self
                    .dependencies
                    .iter()
                    .filter(|line| !line.trim().is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                let parsed = text.parse::<DocumentMut>().context(
                    "Dépendances Poetry : utilisez nom = \"contrainte\" ou une table inline TOML",
                )?;
                ensure!(
                    parsed.as_table().iter().all(|(key, _)| key != "python"),
                    "La version de Python se règle dans le champ Python."
                );
                let table = &mut section["dependencies"];
                if table.is_none() {
                    *table = Item::Table(Table::new());
                }
                let table = table
                    .as_table_like_mut()
                    .context("Les dépendances Poetry doivent être une table")?;
                let remove = table
                    .iter()
                    .filter(|(key, _)| *key != "python" && !parsed.contains_key(key))
                    .map(|(key, _)| key.to_owned())
                    .collect::<Vec<_>>();
                for key in remove {
                    table.remove(&key);
                }
                for (key, entry) in parsed.iter() {
                    if table
                        .get(key)
                        .map(ToString::to_string)
                        .as_deref()
                        .map(str::trim)
                        != Some(entry.to_string().trim())
                    {
                        table.insert(key, entry.clone());
                    }
                }
            } else {
                ensure!(
                    self.dependencies
                        .iter()
                        .all(|dependency| !dependency.trim().is_empty()),
                    "Supprimez les dépendances vides ou saisissez leur nom."
                );
                let old = section
                    .get("dependencies")
                    .and_then(Item::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut array = Array::new();
                for dependency in &self.dependencies {
                    if let Some(value) = old
                        .iter()
                        .find(|value| value.as_str() == Some(dependency.as_str()))
                    {
                        array.push_formatted(value.clone());
                    } else {
                        array.push(dependency.as_str());
                    }
                }
                *array.decor_mut() = old.decor().clone();
                section["dependencies"] = value(array);
            }
        }
        if self.scripts != previous.scripts {
            let table = &mut section["scripts"];
            if table.is_none() {
                *table = Item::Table(Table::new());
            }
            let table = table
                .as_table_like_mut()
                .context("Les scripts doivent être une table TOML")?;
            let mut seen = std::collections::BTreeSet::new();
            for (name, target) in &self.scripts {
                ensure!(
                    !name.trim().is_empty() && !target.trim().is_empty(),
                    "Chaque script doit avoir un nom et une cible module:fonction."
                );
                ensure!(
                    seen.insert(name),
                    "Deux scripts portent le même nom : {name}"
                );
            }
            // Preserve richer Poetry script tables, which the raw TOML editor exposes.
            let remove = previous
                .scripts
                .iter()
                .filter(|(key, _)| !self.scripts.iter().any(|(current, _)| current == key))
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in remove {
                table.remove(&key);
            }
            for (key, target) in &self.scripts {
                if let Some(current) = table.get_mut(key) {
                    replace_string(current, target);
                } else {
                    table.insert(key, value(target));
                }
            }
        }
        Ok(())
    }
}

fn replace_string(item: &mut Item, text: &str) {
    let decor = item.as_value().map(|value| value.decor().clone());
    *item = value(text);
    if let Some(decor) = decor
        && let Some(value) = item.as_value_mut()
    {
        *value.decor_mut() = decor;
    }
}

fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn parse_outdated(text: &str) -> Result<Vec<OutdatedPackage>> {
    for (index, character) in text.char_indices() {
        if character == '['
            && let Some(Ok(packages)) = serde_json::Deserializer::from_str(&text[index..])
                .into_iter::<Vec<OutdatedPackage>>()
                .next()
        {
            return Ok(packages);
        }
    }
    bail!(
        "Le gestionnaire de paquets n’a pas renvoyé de liste JSON valide. Consultez le journal d’exécution."
    )
}

impl Desktop {
    pub(crate) fn remembered_packages_path(&self) -> Option<PathBuf> {
        self.drawer_data
            .package_pages
            .active
            .get(&self.worktree)
            .filter(|path| {
                self.drawer_data
                    .package_pages
                    .forms
                    .get(&(self.worktree.clone(), (*path).clone()))
                    .is_some_and(PackageEditor::dirty)
                    || self
                        .active_root()
                        .is_some_and(|root| root.join(path).is_file())
            })
            .cloned()
    }

    pub(crate) fn open_packages_page(&mut self, path: Option<PathBuf>) {
        self.refresh_python_project();
        let path = path
            .or_else(|| self.remembered_packages_path())
            .unwrap_or_else(|| {
                self.python_project
                    .primary_manifest()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| {
                        let root = &self.python_project.primary_source_root;
                        if root == Path::new(".") {
                            PathBuf::from("pyproject.toml")
                        } else {
                            root.join("pyproject.toml")
                        }
                    })
            });
        let source_root = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let environment = self
            .active_root()
            .and_then(|root| pie_crust_core::discover_python_environment(&root, source_root));
        self.open_prepared_packages_page(path, environment);
    }

    pub(crate) fn open_prepared_packages_page(
        &mut self,
        path: PathBuf,
        environment: Option<pie_crust_core::PythonEnvironment>,
    ) {
        self.drawer_data
            .package_pages
            .active
            .insert(self.worktree.clone(), path.clone());
        let key = (self.worktree.clone(), path.clone());
        if self.drawer_data.package_pages.environments.get(&key) != Some(&environment) {
            self.drawer_data.package_pages.inspections.remove(&key);
        }
        self.drawer_data
            .package_pages
            .environments
            .insert(key.clone(), environment);
        if self
            .drawer_data
            .package_pages
            .forms
            .get(&key)
            .is_some_and(PackageEditor::dirty)
        {
            return;
        }
        let result = (|| -> Result<PackageEditor> {
            let document = self
                .shared
                .lock()
                .map_err(|_| anyhow::anyhow!("Workbench indisponible"))?
                .read_document(&self.worktree, &path)?;
            PackageEditor::from_document(document)
        })();
        match result {
            Ok(editor) => {
                self.drawer_data.package_pages.forms.insert(key, editor);
                self.drawer_data.package_pages.error = None;
            }
            Err(error) => {
                self.drawer_data.package_pages.error = Some(format!("Paquets : {error:#}"))
            }
        }
    }

    pub(crate) fn packages_have_changes(&self) -> bool {
        self.drawer_data
            .package_pages
            .forms
            .values()
            .any(PackageEditor::dirty)
    }

    pub(crate) fn poll_packages_autosave(&mut self) {
        let mut saved = false;
        if let Ok(mut workbench) = self.shared.lock() {
            for editor in self.drawer_data.package_pages.forms.values_mut() {
                if !editor.dirty()
                    || editor.autosave_attempted
                    || !editor
                        .edited_at
                        .is_some_and(|time| time.elapsed() >= Duration::from_millis(100))
                {
                    continue;
                }
                editor.autosave_attempted = true;
                let result = if self.unsynced.contains(&editor.document_id) {
                    Err(anyhow::anyhow!(
                        "Une saisie du TOML reste en conflit dans l’éditeur."
                    ))
                } else {
                    editor.save(&mut workbench)
                };
                match result {
                    Ok(()) => saved = true,
                    Err(error) => editor.error = Some(format!("Brouillon conservé : {error:#}")),
                }
            }
        }
        if saved {
            self.rebuild_index();
        }
    }

    pub(crate) fn save_packages_changes(&mut self) -> bool {
        let result = (|| -> Result<()> {
            let mut workbench = self
                .shared
                .lock()
                .map_err(|_| anyhow::anyhow!("Workbench indisponible"))?;
            for editor in self
                .drawer_data
                .package_pages
                .forms
                .values_mut()
                .filter(|editor| editor.dirty())
            {
                ensure!(
                    !self.unsynced.contains(&editor.document_id),
                    "Le pyproject.toml contient une saisie en conflit dans l’éditeur."
                );
                editor.save(&mut workbench)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.drawer_data.package_pages.error = None;
                self.poll();
                true
            }
            Err(error) => {
                let message = format!("Enregistrement des paquets : {error:#}");
                self.drawer_data.package_pages.error = Some(message.clone());
                self.error = Some(message);
                false
            }
        }
    }

    pub(crate) fn active_packages_path(&self) -> Option<&Path> {
        self.drawer_data
            .package_pages
            .active
            .get(&self.worktree)
            .map(PathBuf::as_path)
    }

    pub(crate) fn packages_page(&mut self, ui: &mut egui::Ui) {
        if !self
            .drawer_data
            .package_pages
            .active
            .contains_key(&self.worktree)
        {
            self.open_packages_page(None);
        }
        let path = self
            .drawer_data
            .package_pages
            .active
            .get(&self.worktree)
            .cloned()
            .unwrap_or_else(|| "pyproject.toml".into());
        let key = (self.worktree.clone(), path.clone());
        let environment = self
            .drawer_data
            .package_pages
            .environments
            .get(&key)
            .and_then(Option::as_ref)
            .map_or_else(
                || "Python système".to_owned(),
                |environment| environment.root.display().to_string(),
            );
        let environment_context = format!("{} · {environment}", path.display());
        let environment_key = format!("{}:{environment_context}", self.worktree);
        let updates_name = format!("pie_crust · mises à jour des paquets · {environment_context}");
        let inspection_name = format!("pie_crust · dépendances · {environment_context}");
        let mut save = false;
        let mut reload = false;
        let mut open_source = false;
        let mut open_other = None;
        let mut create = false;
        ui.horizontal(|ui| {
            ui.heading("Paquets");
            ui.add(
                egui::Label::new(
                    RichText::new(path.display().to_string())
                        .monospace()
                        .color(MUTED),
                )
                .truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Journal d’exécution").clicked() {
                    self.chrome.bottom_view = Some(BottomView::Run);
                }
                if ui.button("Ouvrir le TOML").clicked() {
                    open_source = true;
                }
                let dirty = self
                    .drawer_data
                    .package_pages
                    .forms
                    .get(&key)
                    .is_some_and(PackageEditor::dirty);
                if ui
                    .add_enabled(dirty, egui::Button::new("Enregistrer"))
                    .clicked()
                {
                    save = true;
                }
                if ui
                    .button(if dirty {
                        "Abandonner le formulaire"
                    } else {
                        "Actualiser"
                    })
                    .clicked()
                {
                    reload = true;
                }
            });
        });
        ui.separator();
        let mut manifest_selected = None;
        ui.horizontal_wrapped(|ui| {
            for manifest in &self.python_project.manifests {
                if ui
                    .selectable_label(*manifest == path, manifest.display().to_string())
                    .clicked()
                {
                    manifest_selected = Some(manifest.clone());
                }
            }
        });
        let source_root = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        ui.label(
            RichText::new(format!("Racine des sources : {}", source_root.display()))
                .small()
                .color(MUTED),
        );
        if let Some(error) = &self.drawer_data.package_pages.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let mut update = None;
        let mut inspect = false;
        let mut check_environment = false;
        egui::ScrollArea::vertical().id_salt("packages_full_page").show(ui, |ui| {
            ui.set_max_width(1100.0);
            if let Some(editor) = self.drawer_data.package_pages.forms.get_mut(&key) {
                let source_changed = editor.source != editor.original.replace("\r\n", "\n");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut editor.advanced, false, "Projet, dépendances et scripts");
                    ui.selectable_value(&mut editor.advanced, true, "TOML complet");
                    if editor.dirty() { ui.label(RichText::new("Modifications non enregistrées").color(super::super::ACCENT)); }
                });
                ui.add_space(12.0);
                if editor.advanced {
                    ui.label("Métadonnées, groupes optionnels, outils, build-system… Toutes les sections restent éditables ici.");
                    if editor.form != editor.initial_form { ui.label("Les champs modifiés dans le formulaire seront également appliqués à l’enregistrement."); }
                    ui.add(egui::TextEdit::multiline(&mut editor.source).font(egui::TextStyle::Monospace).code_editor().desired_width(f32::INFINITY).desired_rows(25));
                } else {
                    if source_changed { ui.label("Le TOML complet a été modifié. Enregistrez pour actualiser les champs du formulaire."); }
                    ui.add_enabled_ui(!source_changed, |ui| {
                        let field_width = (ui.available_width() - 170.0).clamp(100.0, 800.0);
                        egui::Grid::new("package_metadata").num_columns(2).spacing([24.0, 12.0]).show(ui, |ui| {
                            for (label, input) in [("Nom du paquet", &mut editor.form.name), ("Version", &mut editor.form.version), ("Description", &mut editor.form.description), ("Python", &mut editor.form.requires_python)] {
                                ui.label(label); ui.add(egui::TextEdit::singleline(input).desired_width(field_width)); ui.end_row();
                            }
                        });
                        ui.add_space(18.0);
                        ui.strong("Dépendances");
                        ui.label(RichText::new(if editor.poetry { "Une entrée TOML Poetry par ligne : django = \"^5.0\"" } else { "Nom et contrainte PEP 508 : django>=5 ; python_version >= '3.10'" }).small().color(MUTED));
                        let mut remove = None;
                        for (index, dependency) in editor.form.dependencies.iter_mut().enumerate() {
                            ui.push_id(("dependency", index), |ui| { ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(dependency).font(egui::TextStyle::Monospace).desired_width((ui.available_width() - 85.0).max(100.0)));
                                if ui.small_button("Retirer").clicked() { remove = Some(index); }
                            }); });
                        }
                        if let Some(index) = remove { editor.form.dependencies.remove(index); }
                        if ui.button("+ Dépendance").clicked() { editor.form.dependencies.push(String::new()); }
                        ui.add_space(18.0);
                        ui.strong("Scripts");
                        ui.label(RichText::new("Entrées installables : nom de commande → module:fonction").small().color(MUTED));
                        let mut remove = None;
                        for (index, (name, target)) in editor.form.scripts.iter_mut().enumerate() {
                            ui.push_id(("script", index), |ui| { ui.horizontal(|ui| {
                                let name_width = (ui.available_width() * 0.28).clamp(80.0, 220.0);
                                ui.add(egui::TextEdit::singleline(name).hint_text("ma-commande").desired_width(name_width));
                                ui.add(egui::TextEdit::singleline(target).hint_text("mon_paquet.cli:main").desired_width((ui.available_width() - 85.0).max(100.0)));
                                if ui.small_button("Retirer").clicked() { remove = Some(index); }
                            }); });
                        }
                        if let Some(index) = remove { editor.form.scripts.remove(index); }
                        if ui.button("+ Script").clicked() { editor.form.scripts.push((String::new(), String::new())); }
                    });
                }
                editor.note_edit();
                if let Some(error) = &editor.error { ui.colored_label(ui.visuals().error_fg_color, error); }
            } else if self.active_root().is_some_and(|root| !root.join(&path).exists()) {
                ui.label("Aucun pyproject.toml trouvé dans ce dossier de sources.");
                if ui.button("Créer pyproject.toml").clicked() { create = true; }
            }
            ui.add_space(18.0);
            egui::CollapsingHeader::new("Autres manifestes et groupes de dépendances").id_salt("other_package_manifests").show(ui, |ui| {
                if let Some(root) = self.active_root() {
                    for file in self.files.iter().filter(|file| super::is_manifest(&file.path)) {
                        let manifest = super::read_manifest(&root, &file.path);
                        if ui.link(manifest.path.display().to_string()).clicked() { open_other = Some(manifest.path); }
                        if let Some(note) = manifest.note { ui.label(RichText::new(note).small().color(MUTED)); }
                        for (group, dependency) in manifest.dependencies { ui.label(RichText::new(format!("{group} · {dependency}")).monospace()); }
                        ui.add_space(8.0);
                    }
                }
            });
            ui.add_space(24.0);
            ui.separator();
            ui.strong("Environnement Python détecté");
            if let Some(Some(environment)) = self.drawer_data.package_pages.environments.get(&key) {
                ui.label(RichText::new(environment.interpreter.display().to_string()).monospace());
            } else {
                ui.label(RichText::new("Aucun venv compatible trouvé ; Python système sera utilisé.").small().color(MUTED));
            }
            ui.label(RichText::new("Compare les dépendances principales du TOML avec les versions installées, sans modifier l’environnement.").small().color(MUTED));
            let checking = self.runner.command_is_pending_or_running(&self.worktree, &inspection_name);
            let can_check = self.drawer_data.package_pages.forms.get(&key).is_some_and(|editor| !editor.poetry && editor.source == editor.original.replace("\r\n", "\n"));
            if ui.add_enabled(can_check && !checking, egui::Button::new("Vérifier les dépendances")).clicked() { check_environment = true; }
            if !can_check { ui.label(RichText::new("Vérification disponible pour les dépendances [project] après enregistrement du TOML complet.").small().color(MUTED)); }
            if checking { ui.spinner(); }
            if let Some(result) = self.drawer_data.package_pages.inspections.get(&key) {
                let current = self.drawer_data.package_pages.forms.get(&key).is_some_and(|editor| can_check && editor.form.dependencies == result.dependencies);
                if !current { ui.label("Le TOML a changé : relancez la vérification."); }
                else if let Some(error) = &result.error { ui.colored_label(ui.visuals().error_fg_color, error); }
                else if !result.pending {
                    egui::Grid::new("installed_dependencies").num_columns(3).striped(true).show(ui, |ui| {
                        for package in &result.packages {
                            ui.label(&package.requirement);
                            ui.label(package.installed.as_deref().unwrap_or("—"));
                            let (label, color) = match package.status.as_str() {
                                "ok" => ("Compatible", super::super::ACCENT),
                                "missing" => ("Manquant", ui.visuals().error_fg_color),
                                "mismatch" => ("Version incompatible", ui.visuals().error_fg_color),
                                "skipped" => ("Non applicable", MUTED),
                                _ => ("À vérifier", MUTED),
                            };
                            ui.colored_label(color, label).on_hover_text(&package.detail);
                            ui.end_row();
                        }
                    });
                    if result.packages.is_empty() { ui.label("Aucune dépendance principale déclarée."); }
                }
            }
            ui.add_space(18.0);
            ui.strong("Mises à jour de l’environnement Python");
            ui.label(RichText::new("Utilise l’interpréteur détecté pour ce manifeste. Les contraintes du TOML restent celles que vous avez saisies.").small().color(MUTED));
            let inspecting = self.runner.command_is_pending_or_running(&self.worktree, &updates_name);
            ui.horizontal(|ui| {
                if ui.add_enabled(!inspecting, egui::Button::new("Rechercher les mises à jour")).clicked() { inspect = true; }
                if inspecting { ui.spinner(); ui.label("Recherche en cours…"); }
            });
            if let Some(result) = self.drawer_data.package_pages.outdated.get(&environment_key) {
                let upgrading = result.packages.iter().any(|package| self.runner.command_is_pending_or_running(&self.worktree, &format!("Mise à jour · {} · {environment_context}", package.name)));
                if let Some(error) = &result.error { ui.colored_label(ui.visuals().error_fg_color, error); }
                else if result.packages.is_empty() { ui.label("Tous les paquets installés sont à jour."); }
                egui::Grid::new("package_updates").num_columns(4).striped(true).show(ui, |ui| {
                    for package in &result.packages {
                        ui.label(&package.name); ui.label(&package.version); ui.label(format!("→ {}", package.latest_version));
                        if ui.add_enabled(!upgrading && !inspecting && valid_package_name(&package.name), egui::Button::new("Mettre à jour")).clicked() { update = Some(package.name.clone()); }
                        ui.end_row();
                    }
                });
            }
        });
        if let Some(path) = manifest_selected {
            self.open_packages_page(Some(path));
        }
        if save {
            self.save_packages_changes();
        }
        if reload {
            self.drawer_data.package_pages.forms.remove(&key);
            self.open_packages_page(None);
        }
        if open_source {
            self.open_document(&path, 1, 1);
        }
        if let Some(path) = open_other {
            self.open_document(&path, 1, 1);
        }
        if create {
            let result = (|| -> Result<()> {
                let root = self.active_root().context("Aucun worktree actif")?;
                let parent = root
                    .join(&path)
                    .parent()
                    .context("Chemin sans dossier")?
                    .canonicalize()?;
                ensure!(
                    parent.starts_with(root.canonicalize()?),
                    "Le manifeste sort du worktree."
                );
                let mut file = tempfile::NamedTempFile::new_in(&parent)?;
                file.write_all(
                    b"[project]\nname = \"mon-projet\"\nversion = \"0.1.0\"\ndependencies = []\n",
                )?;
                file.as_file().sync_all()?;
                file.persist_noclobber(root.join(&path))?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    self.open_packages_page(Some(path.clone()));
                    self.rebuild_index();
                }
                Err(error) => {
                    self.drawer_data.package_pages.error = Some(format!("Création : {error:#}"))
                }
            }
        }
        if let Some(result) = self
            .drawer_data
            .package_pages
            .outdated
            .get(&environment_key)
        {
            for package in &result.packages {
                if let Some((id, success, _)) = self.runner.completed_output_for(
                    &self.worktree,
                    &format!("Mise à jour · {} · {environment_context}", package.name),
                ) && self.drawer_data.package_pages.observed_upgrades.insert(id)
                {
                    if success {
                        inspect = true;
                    } else {
                        self.drawer_data.package_pages.error = Some(format!(
                            "Échec de la mise à jour de {}. Consultez le journal d’exécution.",
                            package.name
                        ));
                    }
                }
            }
        }
        if inspect || update.is_some() || check_environment {
            let current_environment = self
                .active_root()
                .and_then(|root| pie_crust_core::discover_python_environment(&root, &source_root));
            if self.drawer_data.package_pages.environments.get(&key) != Some(&current_environment) {
                self.open_packages_page(Some(path.clone()));
                self.drawer_data.package_pages.error = Some("L’environnement Python a changé. Vérifiez l’interpréteur affiché puis relancez l’action.".into());
                return;
            }
        }
        if inspect {
            self.runner.run_python_command_in_source_root(
                &updates_name,
                &package_manager_args(None),
                BottomView::Run,
                &source_root,
            );
        }
        if let Some(name) = update {
            self.runner.run_python_command_in_source_root(
                &format!("Mise à jour · {name} · {environment_context}"),
                &package_manager_args(Some(&name)),
                BottomView::Run,
                &source_root,
            );
        }
        if let Some((execution_id, success, output)) = self
            .runner
            .completed_output_for(&self.worktree, &updates_name)
            && self
                .drawer_data
                .package_pages
                .outdated
                .get(&environment_key)
                .is_none_or(|result| result.execution_id != execution_id)
        {
            let result = if success {
                parse_outdated(output)
            } else {
                Err(anyhow::anyhow!(
                    "La recherche a échoué. Consultez le journal d’exécution."
                ))
            };
            let (packages, error) = match result {
                Ok(packages) => (packages, None),
                Err(error) => (Vec::new(), Some(error.to_string())),
            };
            self.drawer_data.package_pages.outdated.insert(
                environment_key,
                OutdatedResult {
                    execution_id,
                    packages,
                    error,
                },
            );
        }
        if check_environment && let Some(editor) = self.drawer_data.package_pages.forms.get(&key) {
            let dependencies = editor.form.dependencies.clone();
            let previous_execution = self
                .runner
                .completed_output_for(&self.worktree, &inspection_name)
                .map(|(id, _, _)| id);
            let queued = self.runner.run_python_command_in_source_root(
                &inspection_name,
                &inspection_args(&dependencies),
                BottomView::Run,
                &source_root,
            );
            if queued {
                self.drawer_data.package_pages.inspections.insert(
                    key.clone(),
                    EnvironmentInspection {
                        dependencies,
                        previous_execution,
                        pending: true,
                        packages: Vec::new(),
                        error: None,
                    },
                );
            } else {
                self.drawer_data.package_pages.error = Some("Impossible de lancer la vérification. Vérifiez le dossier de sources et les exécutions en attente.".into());
            }
        }
        if let Some(result) = self.drawer_data.package_pages.inspections.get_mut(&key)
            && result.pending
            && let Some((id, success, output)) = self
                .runner
                .completed_output_for(&self.worktree, &inspection_name)
            && result.previous_execution != Some(id)
        {
            result.pending = false;
            match if success {
                parse_inspection(output)
            } else {
                Err(anyhow::anyhow!(
                    "La vérification a échoué. Consultez le journal d’exécution."
                ))
            } {
                Ok(packages) => result.packages = packages,
                Err(error) => result.error = Some(error.to_string()),
            }
        }
        if let Some(result) = self.drawer_data.package_pages.inspections.get_mut(&key)
            && result.pending
            && !self
                .runner
                .command_is_pending_or_running(&self.worktree, &inspection_name)
        {
            result.pending = false;
            result.error = Some("La vérification n’a pas pu démarrer. Résolvez les erreurs de sauvegarde affichées puis réessayez.".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> (tempfile::TempDir, Workbench, Document) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("pyproject.toml"), text).unwrap();
        let mut workbench = Workbench::open(root.path()).unwrap();
        let id = workbench.active_worktree_id().to_owned();
        let document = workbench
            .read_document(&id, Path::new("pyproject.toml"))
            .unwrap();
        (root, workbench, document)
    }

    #[test]
    fn packages_discover_nested_manifest_before_indexing_and_recover_missing_selection() {
        let root = tempfile::tempdir().unwrap();
        let nested = PathBuf::from("back-end/frigo-recettes/pyproject.toml");
        std::fs::create_dir_all(root.path().join(nested.parent().unwrap())).unwrap();
        std::fs::write(
            root.path().join(&nested),
            "[project]\nname = 'frigo-recettes'\n",
        )
        .unwrap();
        let shared =
            std::sync::Arc::new(std::sync::Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        assert!(app.files.is_empty());
        // Reproduce the stale root selection retained by the former index-based lookup.
        app.drawer_data
            .package_pages
            .active
            .insert(app.worktree.clone(), "pyproject.toml".into());
        app.open_packages_page(None);
        assert_eq!(app.active_packages_path(), Some(nested.as_path()));
        assert_eq!(
            app.python_project.primary_source_root,
            Path::new("back-end/frigo-recettes")
        );
        assert!(app.drawer_data.package_pages.error.is_none());
        let key = (app.worktree.clone(), nested.clone());
        assert_eq!(
            app.drawer_data.package_pages.forms[&key].form.name,
            "frigo-recettes"
        );
        assert!(!root.path().join("pyproject.toml").exists());
    }

    #[test]
    fn packages_refresh_discovers_new_manifest_and_preserves_selected_dirty_form() {
        let root = tempfile::tempdir().unwrap();
        let shared =
            std::sync::Arc::new(std::sync::Mutex::new(Workbench::open(root.path()).unwrap()));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.open_packages_page(None);
        let nested = PathBuf::from("backend/pyproject.toml");
        std::fs::create_dir_all(root.path().join("backend")).unwrap();
        std::fs::write(root.path().join(&nested), "[project]\nname = 'backend'\n").unwrap();
        app.open_packages_page(None);
        assert_eq!(app.active_packages_path(), Some(nested.as_path()));
        let key = (app.worktree.clone(), nested.clone());
        app.drawer_data
            .package_pages
            .forms
            .get_mut(&key)
            .unwrap()
            .form
            .name = "unsaved".into();
        std::fs::write(
            root.path().join("pyproject.toml"),
            "[project]\nname = 'root'\n",
        )
        .unwrap();
        std::fs::remove_file(root.path().join(&nested)).unwrap();
        app.open_packages_page(None);
        assert_eq!(app.active_packages_path(), Some(nested.as_path()));
        assert_eq!(
            app.drawer_data.package_pages.forms[&key].form.name,
            "unsaved"
        );
        assert_eq!(
            app.python_project.primary_manifest(),
            Some(Path::new("pyproject.toml"))
        );
    }

    #[test]
    fn structured_edits_preserve_comments_tools_and_line_endings() {
        let source = "# Header\r\n[project]\r\nname = 'demo' # identity\r\nversion = '1.0'\r\ndependencies = ['django>=5']\r\n[project.scripts]\r\napp = 'demo:main'\r\n[tool.ruff]\r\nline-length = 99 # keep tool setting\r\n";
        let (root, mut workbench, doc) = document(source);
        let mut editor = PackageEditor::from_document(doc).unwrap();
        editor.form.name = "changed".into();
        editor.form.dependencies.push("pytest>=8".into());
        editor
            .form
            .scripts
            .push(("worker".into(), "demo.worker:main".into()));
        assert!(editor.dirty());
        editor.save(&mut workbench).unwrap();
        let written = std::fs::read_to_string(root.path().join("pyproject.toml")).unwrap();
        let parsed = written.parse::<DocumentMut>().unwrap();
        assert_eq!(parsed["project"]["name"].as_str(), Some("changed"));
        assert_eq!(
            parsed["project"]["dependencies"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            parsed["project"]["scripts"]["worker"].as_str(),
            Some("demo.worker:main")
        );
        assert!(written.contains("# identity"));
        assert!(written.contains("line-length = 99 # keep tool setting"));
        assert!(!written.replace("\r\n", "").contains('\n'));
        assert!(!editor.dirty());
    }

    #[test]
    fn conflicting_buffer_or_disk_changes_are_not_overwritten() {
        let (root, mut workbench, doc) = document("[project]\nname = 'demo'\n");
        let mut editor = PackageEditor::from_document(doc.clone()).unwrap();
        editor.form.name = "edited-form".into();
        workbench
            .edit_document(
                &doc.id,
                doc.version,
                "[project]\nname = 'edited-buffer'\n".into(),
            )
            .unwrap();
        assert!(editor.save(&mut workbench).is_err());
        assert!(
            workbench
                .document(&doc.id)
                .unwrap()
                .text
                .contains("edited-buffer")
        );
        let mut editor =
            PackageEditor::from_document(workbench.document(&doc.id).unwrap().clone()).unwrap();
        editor.form.name = "edited-again".into();
        std::fs::write(
            root.path().join("pyproject.toml"),
            "[project]\nname = 'external'\n",
        )
        .unwrap();
        assert!(editor.save(&mut workbench).is_err());
        assert!(
            std::fs::read_to_string(root.path().join("pyproject.toml"))
                .unwrap()
                .contains("external")
        );
        assert!(editor.dirty());
    }

    #[test]
    fn poetry_tables_and_full_toml_remain_editable() {
        let (_, _, doc) = document(
            "[tool.poetry]\nname = 'demo'\n[tool.poetry.dependencies]\npython = '^3.12'\nrequests = {version = '^2', optional = true}\n[tool.poetry.group.dev.dependencies]\npytest = '^8'\n",
        );
        let mut editor = PackageEditor::from_document(doc).unwrap();
        assert!(editor.poetry);
        editor.form.dependencies.push("django = \"^5\"".into());
        let result = editor.rendered().unwrap().parse::<DocumentMut>().unwrap();
        assert_eq!(
            result["tool"]["poetry"]["dependencies"]["django"].as_str(),
            Some("^5")
        );
        assert!(result["tool"]["poetry"]["dependencies"]["requests"].is_inline_table());
        assert_eq!(
            result["tool"]["poetry"]["group"]["dev"]["dependencies"]["pytest"].as_str(),
            Some("^8")
        );
        editor.source = "[project]\nname = [".into();
        assert!(editor.rendered().is_err());
    }

    #[test]
    fn outdated_list_parsing_and_package_validation_do_not_execute_anything() {
        let list = parse_outdated("$ python -m pip list\n[{\"name\":\"Django\",\"version\":\"5.1\",\"latest_version\":\"5.2\"}]\nwarning").unwrap();
        assert_eq!(list[0].name, "Django");
        assert!(parse_outdated("pip failed").is_err());
        assert!(valid_package_name("some-package.name_2"));
        for invalid in ["--index-url", "name; command", "a'b", "a b", ""] {
            assert!(!valid_package_name(invalid));
        }
    }

    #[test]
    fn autosave_waits_for_idle_and_keeps_invalid_drafts_without_repeated_writes() {
        let (root, workbench, _) = document("[project]\nname = 'demo'\n");
        let shared = std::sync::Arc::new(std::sync::Mutex::new(workbench));
        let mut app = Desktop::from_workbench(shared, None, String::new(), None);
        app.open_packages_page(Some("pyproject.toml".into()));
        let key = (app.worktree.clone(), PathBuf::from("pyproject.toml"));
        let editor = app.drawer_data.package_pages.forms.get_mut(&key).unwrap();
        editor.form.name = "renamed".into();
        editor.note_edit();
        app.poll_packages_autosave();
        assert!(
            std::fs::read_to_string(root.path().join("pyproject.toml"))
                .unwrap()
                .contains("'demo'")
        );
        app.drawer_data
            .package_pages
            .forms
            .get_mut(&key)
            .unwrap()
            .edited_at = Some(Instant::now() - Duration::from_millis(200));
        app.poll_packages_autosave();
        assert!(
            std::fs::read_to_string(root.path().join("pyproject.toml"))
                .unwrap()
                .contains("renamed")
        );
        assert!(!app.packages_have_changes());
        let editor = app.drawer_data.package_pages.forms.get_mut(&key).unwrap();
        editor.source = "[project]\nname = [".into();
        editor.note_edit();
        editor.edited_at = Some(Instant::now() - Duration::from_millis(200));
        let version = editor.document_version;
        app.poll_packages_autosave();
        let editor = app.drawer_data.package_pages.forms.get_mut(&key).unwrap();
        assert!(editor.autosave_attempted && editor.error.is_some() && editor.dirty());
        assert_eq!(editor.document_version, version);
        app.poll_packages_autosave();
        assert!(
            std::fs::read_to_string(root.path().join("pyproject.toml"))
                .unwrap()
                .contains("renamed")
        );
        let editor = app.drawer_data.package_pages.forms.get_mut(&key).unwrap();
        editor.source = "[project]\nname = 'corrected'\n".into();
        editor.note_edit();
        editor.edited_at = Some(Instant::now() - Duration::from_millis(200));
        app.poll_packages_autosave();
        assert!(!app.packages_have_changes());
        assert!(
            std::fs::read_to_string(root.path().join("pyproject.toml"))
                .unwrap()
                .contains("corrected")
        );
    }
}
