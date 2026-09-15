use super::{Desktop, MUTED};

use eframe::egui::{self, RichText};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[path = "packages.rs"]
mod packages;

#[derive(Default)]
pub(super) struct ProjectDrawers {
    migrations: Option<MigrationSnapshot>,
    package_pages: packages::PackagePages,
}

struct Manifest {
    path: PathBuf,
    dependencies: Vec<(String, String)>,
    note: Option<String>,
}

struct MigrationSnapshot {
    key: u64,
    nodes: Vec<Migration>,
    notes: Vec<String>,
    incomplete: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct Migration {
    key: String,
    group: String,
    label: String,
    path: PathBuf,
    parents: Vec<String>,
    depth: usize,
    head: bool,
    partial: bool,
}

impl Desktop {
    pub(super) fn migrations_drawer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Arbre des migrations");
            if ui.small_button("Actualiser").clicked() {
                self.drawer_data.migrations = None;
            }
        });
        let key = snapshot_key(self);
        if self
            .drawer_data
            .migrations
            .as_ref()
            .is_none_or(|snapshot| snapshot.key != key)
        {
            let paths = self
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>();
            self.drawer_data.migrations = self
                .active_root()
                .map(|root| read_migrations(key, &root, &paths));
        }
        ui.label(
            RichText::new("Graphe local des fichiers indexés · clic pour relire")
                .size(12.0)
                .color(MUTED),
        );
        let mut open = None;
        if let Some(snapshot) = &self.drawer_data.migrations {
            egui::ScrollArea::vertical().id_salt("migration_graph").show(ui, |ui| {
                if snapshot.nodes.is_empty() {
                    ui.label("Aucune migration Django ou Alembic détectée dans les fichiers indexés.");
                }
                for note in &snapshot.notes {
                    ui.label(RichText::new(note).size(12.0).color(MUTED));
                }
                let groups = snapshot.nodes.iter().map(|node| &node.group).collect::<BTreeSet<_>>();
                for group in groups {
                    ui.add_space(10.0);
                    ui.strong(group);
                    let nodes = snapshot.nodes.iter().filter(|node| &node.group == group).collect::<Vec<_>>();
                    if snapshot.incomplete.contains(group) {
                        ui.label(RichText::new("Graphe partiel : les heads ne peuvent pas être déterminées.").size(12.0).color(MUTED));
                    } else {
                        let heads = nodes.iter().filter(|node| node.head).count();
                        ui.label(RichText::new(if heads > 1 {
                            format!("{heads} heads locales sans merge commun")
                        } else {
                            format!("{heads} head locale")
                        }).size(12.0).color(MUTED));
                    }
                    for node in nodes {
                        ui.push_id(&node.key, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space((node.depth.min(6) * 12) as f32);
                                ui.label(RichText::new(if node.parents.len() > 1 { "◇" } else { "○" }).color(MUTED));
                                let response = ui.link(&node.label).on_hover_text(node.path.display().to_string());
                                if response.clicked() {
                                    open = Some(node.path.clone());
                                }
                                if node.head && !snapshot.incomplete.contains(&node.group) {
                                    ui.label(RichText::new("HEAD").small().strong().color(super::ACCENT));
                                }
                                if node.partial {
                                    ui.label(RichText::new("partiel").small().color(MUTED));
                                }
                            });
                            if !node.parents.is_empty() {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space((node.depth.min(6) * 12 + 18) as f32);
                                    ui.label(RichText::new("←").color(MUTED));
                                    for parent in &node.parents {
                                        if let Some(target) = snapshot.nodes.iter().find(|target| &target.key == parent) {
                                            if ui.link(RichText::new(&target.label).small()).clicked() {
                                                open = Some(target.path.clone());
                                            }
                                        } else {
                                            ui.label(RichText::new(format!("{} (hors graphe)", parent.rsplit(':').next().unwrap_or(parent))).small().color(MUTED));
                                        }
                                    }
                                });
                            }
                        });
                    }
                }
            });
        }
        if let Some(path) = open {
            self.open_document(&path, 1, 1);
        }
    }
}

fn snapshot_key(app: &Desktop) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    app.worktree.hash(&mut hasher);
    // A different project may have the same worktree label.
    app.project_path.hash(&mut hasher);
    for file in &app.files {
        if is_migration_candidate(&file.path) {
            file.path.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn read_local(root: &Path, path: &Path) -> Result<String, String> {
    let resolved = root
        .join(path)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !resolved.starts_with(root) {
        return Err("Le fichier sort du worktree.".into());
    }
    let metadata = std::fs::metadata(&resolved).map_err(|error| error.to_string())?;
    if metadata.len() > 2 * 1024 * 1024 {
        return Err("Fichier trop volumineux pour cet aperçu.".into());
    }
    std::fs::read_to_string(resolved).map_err(|error| error.to_string())
}

fn is_manifest(path: &Path) -> bool {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    filename == "pyproject.toml"
        || (matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("txt" | "in")
        ) && (filename.starts_with("requirements")
            || path
                .components()
                .any(|component| component.as_os_str() == "requirements")))
}

fn read_manifest(root: &Path, path: &Path) -> Manifest {
    let mut manifest = Manifest {
        path: path.to_owned(),
        dependencies: Vec::new(),
        note: None,
    };
    let source = match read_local(root, path) {
        Ok(source) => source,
        Err(error) => {
            manifest.note = Some(error);
            return manifest;
        }
    };
    if path
        .file_name()
        .is_some_and(|name| name == "pyproject.toml")
    {
        let value = match toml::from_str::<toml::Value>(&source) {
            Ok(value) => value,
            Err(error) => {
                manifest.note = Some(format!("TOML invalide : {error}"));
                return manifest;
            }
        };
        let mut push_array = |group: &str, value: Option<&toml::Value>| {
            if let Some(array) = value.and_then(toml::Value::as_array) {
                for item in array {
                    manifest.dependencies.push((
                        group.to_owned(),
                        item.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| item.to_string()),
                    ));
                }
            }
        };
        push_array(
            "Projet",
            value
                .get("project")
                .and_then(|project| project.get("dependencies")),
        );
        if let Some(groups) = value
            .get("project")
            .and_then(|project| project.get("optional-dependencies"))
            .and_then(toml::Value::as_table)
        {
            for (group, dependencies) in groups {
                push_array(&format!("Extra · {group}"), Some(dependencies));
            }
        }
        if let Some(groups) = value
            .get("dependency-groups")
            .and_then(toml::Value::as_table)
        {
            for (group, dependencies) in groups {
                push_array(&format!("Groupe · {group}"), Some(dependencies));
            }
        }
        let poetry = value.get("tool").and_then(|tool| tool.get("poetry"));
        if let Some(dependencies) = poetry
            .and_then(|poetry| poetry.get("dependencies"))
            .and_then(toml::Value::as_table)
        {
            for (name, constraint) in dependencies {
                manifest.dependencies.push((
                    "Poetry".into(),
                    format!(
                        "{name} {}",
                        constraint
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| constraint.to_string())
                    ),
                ));
            }
        }
        if let Some(groups) = poetry
            .and_then(|poetry| poetry.get("group"))
            .and_then(toml::Value::as_table)
        {
            for (group, value) in groups {
                if let Some(dependencies) =
                    value.get("dependencies").and_then(toml::Value::as_table)
                {
                    for (name, constraint) in dependencies {
                        manifest.dependencies.push((
                            format!("Poetry · {group}"),
                            format!(
                                "{name} {}",
                                constraint
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| constraint.to_string())
                            ),
                        ));
                    }
                }
            }
        }
    } else {
        for line in source
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            manifest
                .dependencies
                .push(("Requirements".into(), line.to_owned()));
        }
        manifest.note = Some(
            "Déclarations brutes ; les inclusions et options restent visibles dans le manifeste."
                .into(),
        );
    }
    if manifest.dependencies.is_empty() {
        manifest.note = Some("Aucune dépendance statique déclarée dans ce manifeste.".into());
    }
    manifest
}

fn is_migration_candidate(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "py")
        && path.file_name().is_some_and(|name| name != "__init__.py")
        && path.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("migrations" | "versions")
            )
        })
}

fn read_migrations(key: u64, root: &Path, paths: &[PathBuf]) -> MigrationSnapshot {
    let mut snapshot = MigrationSnapshot {
        key,
        nodes: Vec::new(),
        notes: Vec::new(),
        incomplete: BTreeSet::new(),
    };
    for path in paths.iter().filter(|path| is_migration_candidate(path)) {
        match read_local(root, path) {
            Ok(source) => {
                if let Some(node) = parse_migration(path, &source) {
                    if node.partial {
                        snapshot.incomplete.insert(node.group.clone());
                    }
                    snapshot.nodes.push(node);
                }
            }
            Err(error) => snapshot.notes.push(format!("{} : {error}", path.display())),
        }
    }
    let mut parents = BTreeMap::new();
    for node in &snapshot.nodes {
        if parents
            .insert(node.key.clone(), node.parents.clone())
            .is_some()
        {
            snapshot.incomplete.insert(node.group.clone());
        }
    }
    let referenced = snapshot
        .nodes
        .iter()
        .flat_map(|node| node.parents.iter().cloned())
        .collect::<BTreeSet<_>>();
    let depths = migration_depths(&parents);
    for node in &mut snapshot.nodes {
        if node
            .parents
            .iter()
            .any(|parent| !parents.contains_key(parent))
        {
            snapshot.incomplete.insert(node.group.clone());
        }
        match depths.get(&node.key) {
            Some(depth) => node.depth = *depth,
            None => {
                snapshot.incomplete.insert(node.group.clone());
            }
        }
        node.head = !referenced.contains(&node.key);
    }
    if !snapshot.notes.is_empty() {
        snapshot
            .incomplete
            .extend(snapshot.nodes.iter().map(|node| node.group.clone()));
    }
    // A dynamic dependency may point into another app or version directory.
    // Its unknown edges also make the other group's head count uncertain.
    for framework in ["Django", "Alembic"] {
        if snapshot
            .incomplete
            .iter()
            .any(|group| group.starts_with(framework))
        {
            snapshot.incomplete.extend(
                snapshot
                    .nodes
                    .iter()
                    .filter(|node| node.group.starts_with(framework))
                    .map(|node| node.group.clone()),
            );
        }
    }
    snapshot.nodes.sort_by(|left, right| {
        (&left.group, left.depth, &left.label).cmp(&(&right.group, right.depth, &right.label))
    });
    snapshot
}

fn migration_depths(graph: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, usize> {
    let mut outstanding = BTreeMap::new();
    let mut children = BTreeMap::<String, Vec<String>>::new();
    let mut depth = BTreeMap::<String, usize>::new();
    let mut ready = VecDeque::new();
    for (key, parents) in graph {
        let parents = parents
            .iter()
            .filter(|parent| graph.contains_key(*parent))
            .collect::<BTreeSet<_>>();
        outstanding.insert(key.clone(), parents.len());
        if parents.is_empty() {
            ready.push_back(key.clone());
        }
        for parent in parents {
            children
                .entry(parent.clone())
                .or_default()
                .push(key.clone());
        }
    }
    let mut completed = BTreeMap::new();
    while let Some(key) = ready.pop_front() {
        let current = depth.get(&key).copied().unwrap_or(0);
        completed.insert(key.clone(), current);
        for child in children.get(&key).into_iter().flatten() {
            depth
                .entry(child.clone())
                .and_modify(|depth| *depth = (*depth).max(current + 1))
                .or_insert(current + 1);
            if let Some(remaining) = outstanding.get_mut(child) {
                *remaining -= 1;
                if *remaining == 0 {
                    ready.push_back(child.clone());
                }
            }
        }
    }
    completed
}

fn parse_migration(path: &Path, source: &str) -> Option<Migration> {
    let tokens = lex(source);
    let filename = path.file_stem()?.to_string_lossy().into_owned();
    let revision = assignment(&tokens, "revision", Some(0));
    if revision.is_some() || assignment(&tokens, "down_revision", Some(0)).is_some() {
        let revision = revision.unwrap_or_default();
        let group_path = path.parent()?.display().to_string();
        let group = format!("Alembic · {group_path}");
        let id = match parse_literal(&revision) {
            Some(Literal::Text(id)) => id,
            _ => filename.clone(),
        };
        let revision_valid = matches!(parse_literal(&revision), Some(Literal::Text(_)));
        let ancestors = assignment(&tokens, "down_revision", Some(0))
            .and_then(|tokens| parse_literal(&tokens))
            .and_then(string_sequence);
        let dependencies = assignment(&tokens, "depends_on", Some(0))
            .and_then(|tokens| parse_literal(&tokens))
            .and_then(string_sequence);
        // depends_on has distinct Alembic semantics. Until that mode is selected
        // explicitly, do not claim a complete head calculation for such graphs.
        let has_extra_dependencies = dependencies
            .as_ref()
            .is_some_and(|values| !values.is_empty())
            || (assignment(&tokens, "depends_on", Some(0)).is_some() && dependencies.is_none());
        let parents = ancestors
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|parent| format!("alembic:{group_path}:{parent}"))
            .collect();
        return Some(Migration {
            key: format!("alembic:{group_path}:{id}"),
            group,
            label: format!("{id} · {filename}"),
            path: path.to_owned(),
            parents,
            depth: 0,
            head: false,
            partial: !revision_valid || ancestors.is_none() || has_extra_dependencies,
        });
    }
    let (class_indent, class_tokens) = migration_class_body(&tokens)?;
    let migrations = path.parent()?;
    if migrations.file_name()? != "migrations" {
        return None;
    }
    let app = migrations.parent()?.file_name()?.to_string_lossy();
    let dependencies = assignment(class_tokens, "dependencies", Some(class_indent))
        .and_then(|tokens| parse_literal(&tokens))
        .and_then(django_dependencies);
    let special = ["replaces", "run_before"].iter().any(|name| {
        assignment(class_tokens, name, Some(class_indent)).is_some_and(|tokens| {
            !matches!(parse_literal(&tokens), Some(Literal::Sequence(values)) if values.is_empty())
        })
    });
    Some(Migration {
        key: format!("django:{app}:{filename}"),
        group: format!("Django · {app}"),
        label: filename,
        path: path.to_owned(),
        parents: dependencies
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(app, name)| format!("django:{app}:{name}"))
            .collect(),
        depth: 0,
        head: false,
        partial: dependencies.is_none() || special,
    })
}

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Name(String),
    Text(String),
    Symbol(char),
    Newline,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    column: usize,
}

fn lex(source: &str) -> Vec<Token> {
    let chars = source.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let (mut index, mut column) = (0, 0);
    while index < chars.len() {
        let start = column;
        let value = chars[index];
        if value == '\n' {
            tokens.push(Token {
                kind: TokenKind::Newline,
                column,
            });
            index += 1;
            column = 0;
        } else if value.is_whitespace() {
            index += 1;
            column += 1;
        } else if value == '#' {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
                column += 1;
            }
        } else if matches!(value, '\'' | '"') {
            let triple =
                chars.get(index + 1) == Some(&value) && chars.get(index + 2) == Some(&value);
            let delimiter = if triple { 3 } else { 1 };
            index += delimiter;
            column += delimiter;
            let mut text = String::new();
            let mut valid = true;
            let mut closed = false;
            while index < chars.len() {
                if chars[index] == value
                    && (!triple
                        || (chars.get(index + 1) == Some(&value)
                            && chars.get(index + 2) == Some(&value)))
                {
                    index += delimiter;
                    column += delimiter;
                    closed = true;
                    break;
                }
                if chars[index] == '\\' {
                    valid = false;
                    index += 1;
                    column += 1;
                    if index == chars.len() {
                        break;
                    }
                }
                if chars[index] == '\n' {
                    column = 0;
                } else {
                    column += 1;
                }
                text.push(chars[index]);
                index += 1;
            }
            tokens.push(Token {
                kind: if valid && closed {
                    TokenKind::Text(text)
                } else {
                    TokenKind::Symbol('?')
                },
                column: start,
            });
        } else if value.is_alphanumeric() || value == '_' {
            let begin = index;
            while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                index += 1;
                column += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Name(chars[begin..index].iter().collect()),
                column: start,
            });
        } else {
            tokens.push(Token {
                kind: TokenKind::Symbol(value),
                column,
            });
            index += 1;
            column += 1;
        }
    }
    tokens
}

fn assignment(tokens: &[Token], name: &str, column: Option<usize>) -> Option<Vec<TokenKind>> {
    let mut result = None;
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Name(name.into())
            || column.is_some_and(|column| token.column != column)
            || (index > 0 && tokens[index - 1].kind != TokenKind::Newline)
        {
            continue;
        }
        let mut cursor = index + 1;
        if !matches!(
            tokens.get(cursor).map(|token| &token.kind),
            Some(TokenKind::Symbol('=' | ':'))
        ) {
            // Includes augmented assignment and calls that mutate a collection.
            return Some(vec![TokenKind::Symbol('?')]);
        }
        while cursor < tokens.len()
            && !matches!(
                tokens[cursor].kind,
                TokenKind::Symbol('=') | TokenKind::Newline
            )
        {
            cursor += 1;
        }
        if cursor == tokens.len() || tokens[cursor].kind != TokenKind::Symbol('=') {
            continue;
        }
        cursor += 1;
        let mut depth = 0_i32;
        let mut value = Vec::new();
        while cursor < tokens.len() {
            let kind = &tokens[cursor].kind;
            if *kind == TokenKind::Newline && depth <= 0 {
                break;
            }
            match kind {
                TokenKind::Symbol('(' | '[' | '{') => depth += 1,
                TokenKind::Symbol(')' | ']' | '}') => depth -= 1,
                _ => {}
            }
            if *kind != TokenKind::Newline {
                value.push(kind.clone());
            }
            cursor += 1;
        }
        // Repeated/reassigned declarations require actual Python evaluation.
        if result.is_some() {
            return Some(vec![TokenKind::Symbol('?')]);
        }
        result = Some(value);
    }
    result
}

fn migration_class_body(tokens: &[Token]) -> Option<(usize, &[Token])> {
    let start = tokens.windows(2).position(|tokens| {
        tokens[0].column == 0
            && tokens[0].kind == TokenKind::Name("class".into())
            && tokens[1].kind == TokenKind::Name("Migration".into())
    })?;
    let mut indent = None;
    let mut in_body = false;
    let mut end = tokens.len();
    for (index, token) in tokens.iter().enumerate().skip(start + 2) {
        if token.kind == TokenKind::Newline {
            in_body = true;
            continue;
        }
        if in_body && index > 0 && tokens[index - 1].kind == TokenKind::Newline {
            if token.column == 0 {
                end = index;
                break;
            }
            indent = Some(indent.map_or(token.column, |current: usize| current.min(token.column)));
        }
    }
    indent.map(|indent| (indent, &tokens[start..end]))
}

#[derive(Clone, Debug)]
enum Literal {
    Text(String),
    None,
    Sequence(Vec<Literal>),
}

fn parse_literal(tokens: &[TokenKind]) -> Option<Literal> {
    fn value(tokens: &[TokenKind], position: &mut usize, depth: usize) -> Option<Literal> {
        if depth > 64 {
            return None;
        }
        let token = tokens.get(*position)?;
        *position += 1;
        match token {
            TokenKind::Text(text) => Some(Literal::Text(text.clone())),
            TokenKind::Name(name) if name == "None" => Some(Literal::None),
            TokenKind::Symbol(open @ ('[' | '(')) => {
                let close = TokenKind::Symbol(if *open == '[' { ']' } else { ')' });
                let mut items = Vec::new();
                while tokens.get(*position) != Some(&close) {
                    items.push(value(tokens, position, depth + 1)?);
                    if tokens.get(*position) == Some(&TokenKind::Symbol(',')) {
                        *position += 1;
                    } else if tokens.get(*position) != Some(&close) {
                        return None;
                    }
                }
                *position += 1;
                Some(Literal::Sequence(items))
            }
            _ => None,
        }
    }
    let mut position = 0;
    let literal = value(tokens, &mut position, 0)?;
    (position == tokens.len()).then_some(literal)
}

fn string_sequence(literal: Literal) -> Option<Vec<String>> {
    match literal {
        Literal::None => Some(Vec::new()),
        Literal::Text(text) => Some(vec![text]),
        Literal::Sequence(items) => items
            .into_iter()
            .map(|item| match item {
                Literal::Text(text) => Some(text),
                _ => None,
            })
            .collect(),
    }
}

fn django_dependencies(literal: Literal) -> Option<Vec<(String, String)>> {
    let Literal::Sequence(items) = literal else {
        return None;
    };
    items
        .into_iter()
        .map(|item| {
            let Literal::Sequence(pair) = item else {
                return None;
            };
            if pair.len() != 2 {
                return None;
            }
            let mut pair = pair.into_iter();
            match (pair.next()?, pair.next()?) {
                (Literal::Text(app), Literal::Text(name)) => Some((app, name)),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alembic_branches_and_merge_have_only_current_heads() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("versions")).unwrap();
        let mut paths = Vec::new();
        for (name, text) in [
            ("base", "revision: str = 'base'\ndown_revision = None\n"),
            ("left", "revision = 'left'\ndown_revision = 'base'\n"),
            ("right", "revision = 'right'\ndown_revision = 'base'\n"),
        ] {
            let path = PathBuf::from(format!("versions/{name}.py"));
            std::fs::write(root.join(&path), text).unwrap();
            paths.push(path);
        }
        let branches = read_migrations(0, &root, &paths);
        assert!(branches.incomplete.is_empty());
        assert_eq!(branches.nodes.iter().filter(|node| node.head).count(), 2);
        let merge = PathBuf::from("versions/merge.py");
        std::fs::write(
            root.join(&merge),
            "revision = 'merged'\ndown_revision: tuple[str, str] = ('left', 'right')\n",
        )
        .unwrap();
        paths.push(merge);
        let merged = read_migrations(0, &root, &paths);
        assert!(merged.incomplete.is_empty());
        let heads = merged
            .nodes
            .iter()
            .filter(|node| node.head)
            .collect::<Vec<_>>();
        assert_eq!(heads.len(), 1);
        assert!(heads[0].key.ends_with(":merged"));
    }

    #[test]
    fn django_literals_resolve_and_dynamic_dependencies_remain_partial() {
        let path = Path::new("billing/migrations/0002_charge.py");
        let static_node = parse_migration(path, "from django.db import migrations\nclass Migration(migrations.Migration):\n    dependencies = [\n        ('billing', '0001_initial'), # previous\n    ]\n    operations = []\n").unwrap();
        assert_eq!(static_node.parents, vec!["django:billing:0001_initial"]);
        assert!(!static_node.partial);
        let dynamic = parse_migration(path, "class Migration(migrations.Migration):\n    dependencies = [migrations.swappable_dependency(settings.AUTH_USER_MODEL)]\n").unwrap();
        assert!(dynamic.partial);
        assert!(dynamic.parents.is_empty());
        let quoted = parse_migration(Path::new("versions/a.py"), "\"\"\"\nrevision = 'not_a_revision'\n\"\"\"\nrevision = 'real'\ndown_revision = None\n").unwrap();
        assert!(quoted.key.ends_with(":real"));
    }

    #[test]
    fn manifest_lists_pep621_extras_and_dependency_groups() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("pyproject.toml"), "[project]\ndependencies = ['django>=5']\n[project.optional-dependencies]\npostgres = ['psycopg']\n[dependency-groups]\ntest = ['pytest']\n").unwrap();
        let manifest = read_manifest(&root, Path::new("pyproject.toml"));
        assert!(
            manifest.note.is_none(),
            "{}",
            manifest.note.as_deref().unwrap_or_default()
        );
        assert_eq!(
            manifest.dependencies,
            vec![
                ("Projet".into(), "django>=5".into()),
                ("Extra · postgres".into(), "psycopg".into()),
                ("Groupe · test".into(), "pytest".into())
            ]
        );
    }
}
