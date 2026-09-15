//! Static Python assistance over an immutable, worktree-local source snapshot.
//!
//! All ranges are UTF-8 **byte** offsets. No project code is executed. Resolution
//! is lexical and conservative: ambiguous imports, dynamic receivers and missing
//! annotations never produce guessed edges in the typed call hierarchy.

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser, Tree};

#[derive(Clone, Debug)]
pub struct PythonSource {
    pub path: PathBuf,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub name: String,
    pub detail: String,
    pub replace: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolLocation {
    pub name: String,
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSite {
    pub caller: SymbolLocation,
    pub callee: SymbolLocation,
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallHierarchy {
    pub target: SymbolLocation,
    pub callers: Vec<CallSite>,
    pub callees: Vec<CallSite>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureMatch {
    pub name: String,
    /// None means explicitly requested, but its definition is not in the
    /// snapshot: a plugin, an excluded source or an unresolved fixture name.
    pub location: Option<SymbolLocation>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickFix {
    pub label: String,
    /// Edits apply only to the requested file, not to other worktrees/files.
    pub edits: Vec<TextEdit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extraction {
    pub suggested_names: Vec<String>,
    pub edits: Vec<TextEdit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Function,
    Class,
    Variable,
    Parameter,
    Import,
}

#[derive(Clone, Debug)]
struct Import {
    module: String,
    member: Option<String>,
    statement: String,
}

#[derive(Clone, Debug)]
struct Symbol {
    location: SymbolLocation,
    scope: usize,
    kind: Kind,
    annotation: Option<String>,
    import: Option<Import>,
    body_scope: Option<usize>,
    typed: bool,
    parameters: Vec<String>,
    decorators: Vec<Decorator>,
}

#[derive(Clone, Debug, Default)]
struct Decorator {
    name: String,
    strings: Vec<String>,
    keywords: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct Scope {
    range: Range<usize>,
    parent: Option<usize>,
    owner: Option<usize>,
    kind: Kind,
    unsafe_bindings: bool,
}

#[derive(Clone, Debug)]
struct Module {
    text: String,
    tree: Tree,
    name: String,
    source_root: PathBuf,
    scopes: Vec<Scope>,
    symbols: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct PythonWorkspace {
    modules: BTreeMap<PathBuf, Module>,
    symbols: Vec<Symbol>,
}

impl PythonWorkspace {
    pub fn new(sources: Vec<PythonSource>) -> Result<Self> {
        Self::with_source_roots(sources, &[])
    }

    /// Source roots use the same worktree-relative paths as `PythonSource`.
    /// The deepest matching root determines the importable module name;
    /// source and symbol paths remain relative to the whole worktree.
    pub fn with_source_roots(sources: Vec<PythonSource>, source_roots: &[PathBuf]) -> Result<Self> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        let source_roots: Vec<PathBuf> = source_roots
            .iter()
            .map(|root| {
                root.components()
                    .filter(|part| *part != std::path::Component::CurDir)
                    .collect()
            })
            .collect();
        let mut workspace = Self {
            modules: BTreeMap::new(),
            symbols: Vec::new(),
        };
        for source in sources {
            if workspace.modules.contains_key(&source.path) {
                bail!("Duplicate Python source: {}", source.path.display());
            }
            if !matches!(
                source.path.extension().and_then(|ext| ext.to_str()),
                Some("py" | "pyi" | "pyw")
            ) {
                continue;
            }
            let tree = parser
                .parse(&source.text, None)
                .context("Python parser interrupted")?;
            let source_root = source_roots
                .iter()
                .filter(|root| source.path.starts_with(root))
                .max_by_key(|root| root.components().count())
                .cloned()
                .unwrap_or_default();
            let mut module = Module {
                name: module_name(
                    source
                        .path
                        .strip_prefix(&source_root)
                        .unwrap_or(&source.path),
                ),
                source_root,
                scopes: vec![Scope {
                    range: 0..source.text.len(),
                    parent: None,
                    owner: None,
                    kind: Kind::Variable,
                    unsafe_bindings: false,
                }],
                symbols: Vec::new(),
                text: source.text,
                tree: tree.clone(),
            };
            workspace.collect(&source.path, &mut module, tree.root_node(), 0);
            workspace.modules.insert(source.path, module);
        }
        Ok(workspace)
    }

    pub fn source(&self, path: &Path) -> Option<&str> {
        self.modules.get(path).map(|module| module.text.as_str())
    }

    pub fn sources(&self) -> Vec<PythonSource> {
        self.modules
            .iter()
            .map(|(path, module)| PythonSource {
                path: path.clone(),
                text: module.text.clone(),
            })
            .collect()
    }

    pub fn completions(&self, path: &Path, offset: usize) -> Vec<Completion> {
        let Some(module) = self.modules.get(path) else {
            return Vec::new();
        };
        if offset > module.text.len() || !module.text.is_char_boundary(offset) {
            return Vec::new();
        }
        let start = identifier_start(&module.text, offset);
        let end = identifier_end(&module.text, offset);
        let prefix = &module.text[start..offset];
        if prefix.is_empty() {
            return Vec::new();
        }
        let scope = scope_at(module, offset);
        let candidates = if start > 0 && module.text[..start].ends_with('.') {
            let receiver_end = start - 1;
            let receiver_start = identifier_start(&module.text, receiver_end);
            self.receiver_class(module, scope, &module.text[receiver_start..receiver_end])
                .and_then(|class| self.symbols[class].body_scope.map(|body| (class, body)))
                .map(|(class, body)| {
                    let owner_module = &self.modules[&self.symbols[class].location.path];
                    owner_module
                        .symbols
                        .iter()
                        .copied()
                        .filter(|id| self.symbols[*id].scope == body)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.visible_symbols(module, scope)
        };
        let mut results = BTreeMap::new();
        for id in candidates {
            let symbol = &self.symbols[id];
            if symbol.location.name.starts_with(prefix) && symbol.location.name != prefix {
                results
                    .entry(symbol.location.name.clone())
                    .or_insert(Completion {
                        name: symbol.location.name.clone(),
                        detail: match symbol.kind {
                            Kind::Function => format!(
                                "fonction ({}){}",
                                symbol.parameters.join(", "),
                                symbol
                                    .annotation
                                    .as_ref()
                                    .map(|value| format!(" → {value}"))
                                    .unwrap_or_default()
                            ),
                            Kind::Class => "classe".to_owned(),
                            Kind::Parameter => format!(
                                "paramètre{}",
                                symbol
                                    .annotation
                                    .as_ref()
                                    .map(|value| format!(": {value}"))
                                    .unwrap_or_default()
                            ),
                            Kind::Variable => format!(
                                "variable{}",
                                symbol
                                    .annotation
                                    .as_ref()
                                    .map(|value| format!(": {value}"))
                                    .unwrap_or_default()
                            ),
                            Kind::Import => "import".to_owned(),
                        },
                        replace: start..end,
                    });
            }
        }
        results.into_values().take(64).collect()
    }

    /// Highlight references to the same lexical binding, excluding homonyms in
    /// sibling scopes, comments, strings and dynamically resolved attributes.
    pub fn occurrences(&self, path: &Path, offset: usize) -> Vec<Range<usize>> {
        let Some(module) = self.modules.get(path) else {
            return Vec::new();
        };
        let Some(node) = identifier_at(module, offset) else {
            return Vec::new();
        };
        let Some(target) = self.resolve_identifier(module, node) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        walk(module.tree.root_node(), &mut |node| {
            if node.kind() == "identifier" && self.resolve_identifier(module, node) == Some(target)
            {
                result.push(node.byte_range());
            }
        });
        result.sort_by_key(|range| range.start);
        result.dedup();
        result
    }

    /// Every edge requires an explicitly typed caller and callee. Method
    /// receivers additionally need a resolvable annotation (or lexical self).
    pub fn call_hierarchy(&self, path: &Path, offset: usize) -> Option<CallHierarchy> {
        let module = self.modules.get(path)?;
        let node = identifier_at(module, offset)?;
        let target = self.follow_import(self.resolve_identifier(module, node)?)?;
        let symbol = &self.symbols[target];
        if symbol.kind != Kind::Function || !symbol.typed {
            return None;
        }
        let mut hierarchy = CallHierarchy {
            target: symbol.location.clone(),
            callers: Vec::new(),
            callees: Vec::new(),
        };
        for (source_path, source) in &self.modules {
            walk(source.tree.root_node(), &mut |node| {
                if node.kind() != "call" || node.has_error() {
                    return;
                }
                let Some(function) = node.child_by_field_name("function") else {
                    return;
                };
                let Some(callee) = self.resolve_callable(source, function) else {
                    return;
                };
                if !self.symbols[callee].typed {
                    return;
                }
                let scope = scope_at(source, node.start_byte());
                let Some(caller) = enclosing_function(source, scope) else {
                    return;
                };
                if !self.symbols[caller].typed || (target != callee && target != caller) {
                    return;
                }
                let (line, column) = position(&source.text, node.start_byte());
                let site = CallSite {
                    caller: self.symbols[caller].location.clone(),
                    callee: self.symbols[callee].location.clone(),
                    path: source_path.clone(),
                    line,
                    column,
                };
                if target == callee {
                    hierarchy.callers.push(site.clone());
                }
                if target == caller {
                    hierarchy.callees.push(site);
                }
            });
        }
        Some(hierarchy)
    }

    pub fn quick_fixes(&self, path: &Path, offset: usize) -> Vec<QuickFix> {
        let Some(module) = self.modules.get(path) else {
            return Vec::new();
        };
        let Some(node) = identifier_at(module, offset) else {
            return Vec::new();
        };
        let name = text(node, &module.text);
        if self.resolve_identifier(module, node).is_some()
            || is_builtin(name)
            || !is_value_identifier(node)
        {
            return Vec::new();
        }
        let scope = scope_at(module, offset);
        if module.scopes[scope].unsafe_bindings {
            return Vec::new();
        }
        let mut fixes = Vec::new();
        let ending = line_ending(&module.text);
        let insertion = import_insertion(module);
        let mut imports = BTreeMap::new();
        for (id, symbol) in self.symbols.iter().enumerate() {
            if symbol.scope == 0
                && symbol.kind != Kind::Import
                && symbol.location.path != path
                && symbol.location.name == name
            {
                let from = &self.modules[&symbol.location.path].name;
                if !from.is_empty() && self.module_member(module, from, name) == Some(id) {
                    imports.insert(
                        format!("from {from} import {name}"),
                        format!("Importer {name} depuis {from}"),
                    );
                }
            }
            if symbol.location.name == name
                && symbol.location.path != path
                && let Some(import) = &symbol.import
                && self.modules[&symbol.location.path].source_root == module.source_root
                && self.import_available(module, import)
            {
                imports.insert(
                    import.statement.clone(),
                    format!("Importer {name} · {}", import.statement),
                );
            }
        }
        if let Some(from) = standard_import(name) {
            imports
                .entry(format!("from {from} import {name}"))
                .or_insert_with(|| format!("Importer {name} depuis {from}"));
        }
        let separator = if insertion > 0 && !module.text[..insertion].ends_with('\n') {
            ending
        } else {
            ""
        };
        for (statement, label) in imports {
            fixes.push(QuickFix {
                label,
                edits: vec![TextEdit {
                    range: insertion..insertion,
                    replacement: format!("{separator}{statement}{ending}"),
                }],
            });
        }
        let mut similar: Vec<_> = self
            .visible_symbols(module, scope)
            .into_iter()
            .filter_map(|id| {
                let candidate = &self.symbols[id].location.name;
                let distance = edit_distance(name, candidate);
                (distance <= 2 && distance < name.chars().count().max(2))
                    .then_some((distance, candidate.clone()))
            })
            .collect();
        similar.sort();
        similar.dedup();
        for (_, candidate) in similar.into_iter().take(5) {
            fixes.push(QuickFix {
                label: format!("Remplacer par {candidate}"),
                edits: vec![TextEdit {
                    range: node.byte_range(),
                    replacement: candidate,
                }],
            });
        }
        if node.parent().is_some_and(|parent| {
            parent.kind() == "call" && parent.child_by_field_name("function") == Some(node)
        }) {
            let mut insertion = line_start(&module.text, node.start_byte());
            let mut outer = scope;
            while let Some(parent) = module.scopes[outer].parent {
                if let Some(owner) = module.scopes[outer].owner {
                    insertion = declaration_start(module, &self.symbols[owner]);
                }
                outer = parent;
            }
            let indent = inferred_indent(&module.text);
            fixes.push(QuickFix {
                label: format!("Créer la fonction {name} (squelette)"),
                edits: vec![TextEdit { range: insertion..insertion, replacement: format!("def {name}(*args: object, **kwargs: object) -> object:{ending}{indent}raise NotImplementedError{ending}{ending}") }],
            });
        } else {
            let start = line_start(&module.text, node.start_byte());
            let indent: String = module.text[start..]
                .chars()
                .take_while(|ch| *ch == ' ' || *ch == '\t')
                .collect();
            fixes.push(QuickFix {
                label: format!("Créer la variable {name} (valeur à compléter)"),
                edits: vec![TextEdit {
                    range: start..start,
                    replacement: format!("{indent}{name} = None{ending}"),
                }],
            });
        }
        fixes
    }

    /// Fixtures declared in indexed ancestor conftest files, this module and
    /// this test's class. Plugin/runtime fixtures remain explicitly unresolved.
    pub fn fixtures(&self, path: &Path, offset: usize) -> Vec<FixtureMatch> {
        let Some(module) = self.modules.get(path) else {
            return Vec::new();
        };
        let scope = scope_at(module, offset);
        let function = identifier_at(module, offset)
            .and_then(|node| self.resolve_identifier(module, node))
            .filter(|id| self.symbols[*id].kind == Kind::Function)
            .or_else(|| enclosing_function(module, scope));
        let Some(function) = function else {
            return Vec::new();
        };
        let target = &self.symbols[function];
        if !target.location.name.starts_with("test")
            && !target.decorators.iter().any(|decorator| {
                self.is_pytest_name(module, target.scope, &decorator.name, "fixture")
            })
        {
            return Vec::new();
        }
        let target_class = class_owner(module, target.scope);
        let mut definitions: BTreeMap<String, (usize, usize, bool)> = BTreeMap::new();
        for (id, symbol) in self.symbols.iter().enumerate() {
            if symbol.kind != Kind::Function {
                continue;
            }
            let owner = &self.modules[&symbol.location.path];
            let Some(decorator) = symbol.decorators.iter().find(|decorator| {
                self.is_pytest_name(owner, symbol.scope, &decorator.name, "fixture")
            }) else {
                continue;
            };
            let rank = if symbol.location.path == path {
                if symbol.scope == 0 {
                    10000
                } else if class_owner(owner, symbol.scope) == target_class && target_class.is_some()
                {
                    10001
                } else {
                    continue;
                }
            } else if symbol
                .location
                .path
                .file_name()
                .is_some_and(|name| name == "conftest.py")
                && symbol.scope == 0
            {
                let parent = symbol.location.path.parent().unwrap_or(Path::new(""));
                if !path.parent().unwrap_or(Path::new("")).starts_with(parent) {
                    continue;
                }
                parent.components().count()
            } else {
                continue;
            };
            let fixture_name = decorator
                .keywords
                .get("name")
                .and_then(|value| unquote(value))
                .unwrap_or_else(|| symbol.location.name.clone());
            let autouse = decorator
                .keywords
                .get("autouse")
                .is_some_and(|value| value == "True");
            if definitions
                .get(&fixture_name)
                .is_none_or(|(_, prior, _)| rank > *prior)
            {
                definitions.insert(fixture_name, (id, rank, autouse));
            }
        }
        let mut requested = BTreeMap::new();
        let mut parametrized = self.parametrized_names(module, target);
        if let Some(class) = target_class {
            parametrized.extend(self.parametrized_names(module, &self.symbols[class]));
        }
        for parameter in &target.parameters {
            if !matches!(parameter.as_str(), "self" | "cls") && !parametrized.contains(parameter) {
                requested.insert(parameter.clone(), "paramètre de la fonction".to_owned());
            }
        }
        self.add_usefixtures(module, target, &mut requested);
        if let Some(class) = target_class {
            self.add_usefixtures(module, &self.symbols[class], &mut requested);
        }
        // Module-level pytestmark = pytest.mark.usefixtures("...").
        let mut cursor = module.tree.root_node().walk();
        for statement in module.tree.root_node().named_children(&mut cursor) {
            if statement.kind() == "expression_statement" {
                walk(statement, &mut |node| {
                    if node.kind() == "assignment"
                        && node
                            .child_by_field_name("left")
                            .is_some_and(|left| text(left, &module.text) == "pytestmark")
                    {
                        walk(node, &mut |call| {
                            if call.kind() == "call" {
                                let decorator = parse_decorator(call, &module.text);
                                if self.is_pytest_name(
                                    module,
                                    0,
                                    &decorator.name,
                                    "mark.usefixtures",
                                ) {
                                    for name in decorator.strings {
                                        requested.insert(name, "pytestmark du module".to_owned());
                                    }
                                }
                            }
                        });
                    }
                });
            }
        }
        for (name, (_, _, autouse)) in &definitions {
            if *autouse {
                requested
                    .entry(name.clone())
                    .or_insert_with(|| "autouse".to_owned());
            }
        }
        let mut pending: Vec<_> = requested.keys().cloned().collect();
        let mut seen = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some((id, _, _)) = definitions.get(&name) {
                for parameter in &self.symbols[*id].parameters {
                    if matches!(parameter.as_str(), "self" | "cls") {
                        continue;
                    }
                    requested
                        .entry(parameter.clone())
                        .or_insert_with(|| format!("dépendance de {name}"));
                    pending.push(parameter.clone());
                }
            }
        }
        requested
            .into_iter()
            .map(|(name, reason)| FixtureMatch {
                location: definitions
                    .get(&name)
                    .map(|(id, _, _)| self.symbols[*id].location.clone()),
                name,
                reason,
            })
            .collect()
    }

    /// Extract a complete RHS/return expression. Partial expressions that would
    /// move evaluation across another expression/control flow are rejected.
    pub fn extract_variable(
        &self,
        path: &Path,
        selection: Range<usize>,
        name: Option<&str>,
    ) -> Result<Extraction> {
        let module = self
            .modules
            .get(path)
            .context("Python source is absent from the snapshot")?;
        let selected = module
            .text
            .get(selection.clone())
            .context("Selection is not on UTF-8 boundaries")?;
        if selected.trim().is_empty() {
            bail!("Select an expression first")
        }
        let leading = selected.len() - selected.trim_start().len();
        let trimmed =
            selection.start + leading..selection.end - (selected.len() - selected.trim_end().len());
        let node = module
            .tree
            .root_node()
            .descendant_for_byte_range(trimmed.start, trimmed.end)
            .context("No Python expression selected")?;
        if node.byte_range() != trimmed || node.has_error() {
            bail!("Select a complete Python expression")
        }
        let parent = node
            .parent()
            .context("Select an expression inside a statement")?;
        let valid = match parent.kind() {
            "assignment" => parent.child_by_field_name("right") == Some(node),
            "return_statement" | "expression_statement" => true,
            _ => false,
        };
        if !valid
            || matches!(
                node.kind(),
                "assignment" | "yield" | "lambda" | "named_expression"
            )
        {
            bail!(
                "Extraction supports a complete assignment RHS, return value or expression statement; this selection would change evaluation order"
            )
        }
        let start = line_start(&module.text, trimmed.start);
        if module.text[start..trimmed.start].contains(';') {
            bail!("Split semicolon-separated statements before extraction")
        }
        let scope = scope_at(module, trimmed.start);
        let occupied: BTreeSet<_> = self
            .visible_symbols(module, scope)
            .into_iter()
            .map(|id| self.symbols[id].location.name.clone())
            .collect();
        let mut candidates = Vec::new();
        if node.kind() == "call" {
            if let Some(function) = node.child_by_field_name("function") {
                if let Some(id) = self.resolve_callable(module, function)
                    && let Some(annotation) = &self.symbols[id].annotation
                    && valid_identifier(annotation)
                {
                    candidates.push(snake_case(annotation));
                }
                let callable = text(function, &module.text)
                    .rsplit('.')
                    .next()
                    .unwrap_or("value");
                let mut value = callable;
                for prefix in [
                    "get_", "fetch_", "load_", "create_", "build_", "make_", "compute_", "parse_",
                    "find_",
                ] {
                    if let Some(rest) = callable.strip_prefix(prefix) {
                        value = rest;
                        break;
                    }
                }
                candidates.push(snake_case(value));
            }
        } else if node.kind() == "attribute"
            && let Some(attribute) = node.child_by_field_name("attribute")
        {
            candidates.push(text(attribute, &module.text).to_owned());
        }
        candidates.extend(["value".to_owned(), "result".to_owned()]);
        let mut suggested = Vec::new();
        for candidate in candidates {
            if !valid_identifier(&candidate) || is_keyword(&candidate) {
                continue;
            }
            let mut unique = candidate.clone();
            let mut suffix = 2;
            while occupied.contains(&unique) {
                unique = format!("{candidate}_{suffix}");
                suffix += 1;
            }
            if !suggested.contains(&unique) {
                suggested.push(unique);
            }
        }
        let chosen = name.unwrap_or(&suggested[0]).to_owned();
        if !valid_identifier(&chosen) || is_keyword(&chosen) {
            bail!("Variable name is not a valid Python identifier")
        }
        if occupied.contains(&chosen) {
            bail!("A binding named {chosen} already exists in this scope")
        }
        let indentation: String = module.text[start..]
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .collect();
        let assignment = format!(
            "{indentation}{chosen} = {}{}",
            &module.text[trimmed.clone()],
            line_ending(&module.text)
        );
        Ok(Extraction {
            suggested_names: suggested,
            edits: vec![
                TextEdit {
                    range: trimmed,
                    replacement: chosen,
                },
                TextEdit {
                    range: start..start,
                    replacement: assignment,
                },
            ],
        })
    }

    fn add_usefixtures(
        &self,
        module: &Module,
        symbol: &Symbol,
        requested: &mut BTreeMap<String, String>,
    ) {
        for decorator in &symbol.decorators {
            if self.is_pytest_name(module, symbol.scope, &decorator.name, "mark.usefixtures") {
                for name in &decorator.strings {
                    requested.insert(name.clone(), "usefixtures".to_owned());
                }
            }
        }
    }

    fn parametrized_names(&self, module: &Module, symbol: &Symbol) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for decorator in &symbol.decorators {
            if self.is_pytest_name(module, symbol.scope, &decorator.name, "mark.parametrize") {
                if decorator
                    .keywords
                    .get("indirect")
                    .is_some_and(|value| value == "True")
                {
                    continue;
                }
                let indirect = decorator
                    .keywords
                    .get("indirect")
                    .cloned()
                    .unwrap_or_default();
                if let Some(first) = decorator.strings.first() {
                    for name in first
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                    {
                        if !indirect.contains(&format!("'{name}'"))
                            && !indirect.contains(&format!("\"{name}\""))
                        {
                            names.insert(name.to_owned());
                        }
                    }
                }
            }
        }
        names
    }

    fn is_pytest_name(&self, module: &Module, scope: usize, name: &str, member: &str) -> bool {
        let mut parts = name.splitn(2, '.');
        let root = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        let Some(id) = self.lexical_lookup(module, scope, root) else {
            return false;
        };
        let Some(import) = &self.symbols[id].import else {
            return false;
        };
        if import.module != "pytest" {
            return false;
        }
        let resolved = match &import.member {
            Some(value) if tail.is_empty() => value.clone(),
            Some(value) => format!("{value}.{tail}"),
            None => tail.to_owned(),
        };
        resolved == member
    }

    fn collect(&mut self, path: &Path, module: &mut Module, node: Node<'_>, scope: usize) {
        let mut pending = vec![(node, scope)];
        while let Some((node, scope)) = pending.pop() {
            match node.kind() {
                "function_definition" | "class_definition" => {
                    let Some(name) = node.child_by_field_name("name") else {
                        continue;
                    };
                    let kind = if node.kind() == "function_definition" {
                        Kind::Function
                    } else {
                        Kind::Class
                    };
                    let decorators = node
                        .parent()
                        .filter(|parent| parent.kind() == "decorated_definition")
                        .map(|parent| {
                            let mut cursor = parent.walk();
                            parent
                                .named_children(&mut cursor)
                                .filter(|child| child.kind() == "decorator")
                                .filter_map(|child| child.named_child(0))
                                .map(|child| parse_decorator(child, &module.text))
                                .collect()
                        })
                        .unwrap_or_default();
                    let id = self.add_symbol(path, module, name, scope, kind, None);
                    self.symbols[id].decorators = decorators;
                    self.symbols[id].annotation = node
                        .child_by_field_name("return_type")
                        .map(|value| text(value, &module.text).to_owned());
                    let body_scope = module.scopes.len();
                    module.scopes.push(Scope {
                        range: node.byte_range(),
                        parent: Some(scope),
                        owner: Some(id),
                        kind,
                        unsafe_bindings: false,
                    });
                    self.symbols[id].body_scope = Some(body_scope);
                    let mut all_typed = self.symbols[id].annotation.is_some();
                    if let Some(parameters) = node.child_by_field_name("parameters") {
                        let mut cursor = parameters.walk();
                        let mut parameter_index = 0;
                        for parameter in parameters.named_children(&mut cursor) {
                            if matches!(
                                parameter.kind(),
                                "keyword_separator" | "positional_separator"
                            ) {
                                continue;
                            }
                            let Some(parameter_name) = parameter_identifier(parameter) else {
                                all_typed = false;
                                continue;
                            };
                            let annotation = parameter
                                .child_by_field_name("type")
                                .map(|value| text(value, &module.text).to_owned());
                            let parameter_text = text(parameter_name, &module.text).to_owned();
                            let implicit_receiver = parameter_index == 0
                                && matches!(parameter_text.as_str(), "self" | "cls")
                                && module.scopes[scope].kind == Kind::Class
                                && !self.symbols[id]
                                    .decorators
                                    .iter()
                                    .any(|decorator| decorator.name == "staticmethod");
                            all_typed &= annotation.is_some() || implicit_receiver;
                            self.add_symbol(
                                path,
                                module,
                                parameter_name,
                                body_scope,
                                Kind::Parameter,
                                annotation,
                            );
                            self.symbols[id].parameters.push(parameter_text);
                            parameter_index += 1;
                        }
                    }
                    self.symbols[id].typed =
                        kind == Kind::Function && all_typed && !node.has_error();
                    if let Some(body) = node.child_by_field_name("body") {
                        pending.push((body, body_scope));
                    }
                    continue;
                }
                "assignment" | "augmented_assignment" => {
                    if let Some(left) = node.child_by_field_name("left") {
                        let annotation = node
                            .child_by_field_name("type")
                            .map(|value| text(value, &module.text).to_owned());
                        self.collect_binding(path, module, left, scope, annotation);
                    }
                }
                "for_statement" | "for_in_clause" => {
                    if let Some(left) = node.child_by_field_name("left") {
                        self.collect_binding(path, module, left, scope, None);
                    }
                }
                "named_expression" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        self.collect_binding(path, module, name, scope, None);
                    }
                }
                "as_pattern" | "except_clause" => {
                    if let Some(alias) =
                        node.child_by_field_name("alias").and_then(first_identifier)
                    {
                        self.collect_binding(path, module, alias, scope, None);
                    }
                }
                "import_statement" | "import_from_statement" => {
                    self.collect_imports(path, module, node, scope);
                    continue;
                }
                "global_statement" | "nonlocal_statement" | "match_statement"
                | "delete_statement" => {
                    module.scopes[scope].unsafe_bindings = true;
                }
                "lambda"
                | "list_comprehension"
                | "set_comprehension"
                | "dictionary_comprehension"
                | "generator_expression" => {
                    // Give comprehensions/lambdas an isolated conservative scope so
                    // their targets never leak into containing-function suggestions.
                    let inner = module.scopes.len();
                    module.scopes.push(Scope {
                        range: node.byte_range(),
                        parent: Some(scope),
                        owner: None,
                        kind: Kind::Function,
                        unsafe_bindings: true,
                    });
                    let mut cursor = node.walk();
                    let children: Vec<_> = node.named_children(&mut cursor).collect();
                    pending.extend(children.into_iter().rev().map(|child| (child, inner)));
                    continue;
                }
                _ => {}
            }
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            pending.extend(children.into_iter().rev().map(|child| (child, scope)));
        }
    }

    fn add_symbol(
        &mut self,
        path: &Path,
        module: &mut Module,
        node: Node<'_>,
        scope: usize,
        kind: Kind,
        annotation: Option<String>,
    ) -> usize {
        let id = self.symbols.len();
        let (line, column) = position(&module.text, node.start_byte());
        self.symbols.push(Symbol {
            location: SymbolLocation {
                name: text(node, &module.text).to_owned(),
                path: path.to_owned(),
                line,
                column,
                range: node.byte_range(),
            },
            scope,
            kind,
            annotation,
            import: None,
            body_scope: None,
            typed: false,
            parameters: Vec::new(),
            decorators: Vec::new(),
        });
        module.symbols.push(id);
        id
    }

    fn collect_binding(
        &mut self,
        path: &Path,
        module: &mut Module,
        node: Node<'_>,
        scope: usize,
        annotation: Option<String>,
    ) {
        if node.kind() == "identifier" {
            let name = text(node, &module.text);
            if let Some(id) = module.symbols.iter().copied().find(|id| {
                self.symbols[*id].scope == scope
                    && self.symbols[*id].location.name == name
                    && matches!(self.symbols[*id].kind, Kind::Variable | Kind::Parameter)
            }) {
                if let Some(annotation) = annotation {
                    if self.symbols[id]
                        .annotation
                        .as_ref()
                        .is_some_and(|prior| prior != &annotation)
                    {
                        self.symbols[id].annotation = None;
                    } else {
                        self.symbols[id].annotation = Some(annotation);
                    }
                }
            } else {
                self.add_symbol(path, module, node, scope, Kind::Variable, annotation);
            }
        } else if matches!(
            node.kind(),
            "pattern_list" | "tuple_pattern" | "list_pattern" | "list_splat_pattern"
        ) {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                self.collect_binding(path, module, child, scope, None);
            }
        }
    }

    fn collect_imports(&mut self, path: &Path, module: &mut Module, node: Node<'_>, scope: usize) {
        if node.has_error() {
            module.scopes[scope].unsafe_bindings = true;
            return;
        }
        let from = node.child_by_field_name("module_name");
        let from_name =
            from.map(|value| absolute_import(&module.name, path, text(value, &module.text)));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if Some(child) == from {
                continue;
            }
            if child.kind() == "wildcard_import" {
                module.scopes[scope].unsafe_bindings = true;
                continue;
            }
            let (imported, local) = if child.kind() == "aliased_import" {
                (
                    child.child_by_field_name("name"),
                    child.child_by_field_name("alias"),
                )
            } else {
                (Some(child), None)
            };
            let Some(imported) = imported else { continue };
            let imported_text = text(imported, &module.text).to_owned();
            let local = local.or_else(|| first_identifier(imported));
            let Some(local) = local else { continue };
            let alias = if child.kind() == "aliased_import" {
                format!(" as {}", text(local, &module.text))
            } else {
                String::new()
            };
            let id = self.add_symbol(path, module, local, scope, Kind::Import, None);
            self.symbols[id].import = Some(match &from_name {
                Some(from) => Import {
                    module: from.clone(),
                    statement: format!("from {from} import {imported_text}{alias}"),
                    member: Some(imported_text),
                },
                None => Import {
                    statement: format!("import {imported_text}{alias}"),
                    module: if child.kind() == "aliased_import" {
                        imported_text
                    } else {
                        imported_text.split('.').next().unwrap_or("").to_owned()
                    },
                    member: None,
                },
            });
        }
    }

    fn visible_symbols(&self, module: &Module, mut scope: usize) -> Vec<usize> {
        let mut visible = BTreeMap::new();
        loop {
            if module.scopes[scope].unsafe_bindings {
                break;
            }
            for id in &module.symbols {
                let symbol = &self.symbols[*id];
                if symbol.scope == scope {
                    visible.entry(symbol.location.name.clone()).or_insert(*id);
                }
            }
            let Some(mut parent) = module.scopes[scope].parent else {
                break;
            };
            if module.scopes[scope].kind == Kind::Function
                && module.scopes[parent].kind == Kind::Class
            {
                let Some(outer) = module.scopes[parent].parent else {
                    break;
                };
                parent = outer;
            }
            scope = parent;
        }
        visible.into_values().collect()
    }

    fn lexical_lookup(&self, module: &Module, mut scope: usize, name: &str) -> Option<usize> {
        loop {
            if module.scopes[scope].unsafe_bindings {
                return None;
            }
            let candidates: Vec<_> = module
                .symbols
                .iter()
                .copied()
                .filter(|id| {
                    self.symbols[*id].scope == scope && self.symbols[*id].location.name == name
                })
                .collect();
            if candidates.len() == 1 {
                return candidates.first().copied();
            }
            if !candidates.is_empty() {
                return None;
            }
            let mut parent = module.scopes[scope].parent?;
            if module.scopes[scope].kind == Kind::Function
                && module.scopes[parent].kind == Kind::Class
            {
                parent = module.scopes[parent].parent?;
            }
            scope = parent;
        }
    }

    fn resolve_identifier(&self, module: &Module, node: Node<'_>) -> Option<usize> {
        if let Some(id) = module
            .symbols
            .iter()
            .copied()
            .find(|id| self.symbols[*id].location.range == node.byte_range())
        {
            return Some(id);
        }
        if let Some(parent) = node.parent() {
            if parent.kind() == "attribute" && parent.child_by_field_name("attribute") == Some(node)
            {
                return self.resolve_attribute(module, parent);
            }
            if parent.kind() == "assignment" && parent.child_by_field_name("left") == Some(node) {
                return self.lexical_lookup(
                    module,
                    scope_at(module, node.start_byte()),
                    text(node, &module.text),
                );
            }
        }
        if !is_value_identifier(node) {
            return None;
        }
        self.lexical_lookup(
            module,
            scope_at(module, node.start_byte()),
            text(node, &module.text),
        )
    }

    fn follow_import(&self, id: usize) -> Option<usize> {
        let symbol = &self.symbols[id];
        let Some(import) = &symbol.import else {
            return Some(id);
        };
        let member = import.member.as_ref()?;
        self.module_member(&self.modules[&symbol.location.path], &import.module, member)
    }

    fn module_member(&self, caller: &Module, module_name: &str, name: &str) -> Option<usize> {
        let mut modules: Vec<_> = self
            .modules
            .values()
            .filter(|module| module.name == module_name)
            .collect();
        if modules
            .iter()
            .any(|module| module.source_root == caller.source_root)
        {
            modules.retain(|module| module.source_root == caller.source_root);
        } else {
            // A project's tests may sit beside its `src` root. Prefer related
            // roots before considering modules from unrelated nested projects.
            let related_root = |module: &&Module| {
                !module.source_root.as_os_str().is_empty()
                    && !caller.source_root.as_os_str().is_empty()
                    && (module.source_root.starts_with(&caller.source_root)
                        || caller.source_root.starts_with(&module.source_root))
            };
            if modules.iter().any(related_root) {
                modules.retain(related_root);
            }
        }
        // Choose the module before looking for its member: a missing member in
        // this project must not accidentally resolve to another project's module.
        if modules.len() != 1 {
            return None;
        }
        let candidates: Vec<_> = modules[0]
            .symbols
            .iter()
            .copied()
            .filter(|id| {
                self.symbols[*id].scope == 0
                    && self.symbols[*id].location.name == name
                    && self.symbols[*id].kind != Kind::Import
            })
            .collect();
        (candidates.len() == 1).then(|| candidates[0])
    }

    fn import_available(&self, caller: &Module, import: &Import) -> bool {
        let Some(member) = &import.member else {
            return true;
        };
        !self
            .modules
            .values()
            .any(|module| module.name == import.module)
            || self.module_member(caller, &import.module, member).is_some()
    }

    fn resolve_callable(&self, module: &Module, node: Node<'_>) -> Option<usize> {
        let id = match node.kind() {
            "identifier" => self.follow_import(self.lexical_lookup(
                module,
                scope_at(module, node.start_byte()),
                text(node, &module.text),
            )?)?,
            "attribute" => self.resolve_attribute(module, node)?,
            _ => return None,
        };
        (self.symbols[id].kind == Kind::Function).then_some(id)
    }

    fn resolve_attribute(&self, module: &Module, node: Node<'_>) -> Option<usize> {
        let receiver = node.child_by_field_name("object")?;
        let attribute = node.child_by_field_name("attribute")?;
        let scope = scope_at(module, node.start_byte());
        if receiver.kind() != "identifier" {
            return None;
        }
        let receiver_text = text(receiver, &module.text);
        if let Some(id) = self.lexical_lookup(module, scope, receiver_text)
            && let Some(import) = &self.symbols[id].import
            && import.member.is_none()
        {
            return self.module_member(module, &import.module, text(attribute, &module.text));
        }
        let class = self.receiver_class(module, scope, receiver_text)?;
        let class_scope = self.symbols[class].body_scope?;
        let owner = &self.modules[&self.symbols[class].location.path];
        let candidates: Vec<_> = owner
            .symbols
            .iter()
            .copied()
            .filter(|id| {
                self.symbols[*id].scope == class_scope
                    && self.symbols[*id].location.name == text(attribute, &module.text)
            })
            .collect();
        (candidates.len() == 1).then(|| candidates[0])
    }

    fn receiver_class(&self, module: &Module, scope: usize, name: &str) -> Option<usize> {
        let id = self.lexical_lookup(module, scope, name)?;
        let symbol = &self.symbols[id];
        if matches!(name, "self" | "cls")
            && symbol.kind == Kind::Parameter
            && let Some(class) = class_owner(module, symbol.scope)
        {
            return Some(class);
        }
        if symbol.kind == Kind::Class {
            return Some(id);
        }
        let annotation = symbol.annotation.as_deref()?.trim_matches(['\'', '"']);
        let class = if valid_identifier(annotation) {
            self.follow_import(self.lexical_lookup(module, symbol.scope, annotation)?)?
        } else if let Some((root, member)) = annotation.split_once('.') {
            let import_id = self.lexical_lookup(module, symbol.scope, root)?;
            let import = self.symbols[import_id].import.as_ref()?;
            if import.member.is_some() {
                return None;
            }
            self.module_member(module, &import.module, member)?
        } else {
            return None;
        };
        (self.symbols[class].kind == Kind::Class).then_some(class)
    }
}

/// Validate and apply a batch without ever interpreting ranges as character
/// offsets. Callers must first check that their buffer matches `source()`.
pub fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Result<String> {
    let mut edits: Vec<_> = edits.iter().collect();
    edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
    let mut previous_end = 0;
    for edit in &edits {
        if edit.range.start < previous_end || source.get(edit.range.clone()).is_none() {
            bail!("Overlapping or invalid text edit")
        }
        previous_end = edit.range.end;
    }
    let mut result = source.to_owned();
    for edit in edits.into_iter().rev() {
        result.replace_range(edit.range.clone(), &edit.replacement);
    }
    Ok(result)
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn walk<'tree>(node: Node<'tree>, visitor: &mut impl FnMut(Node<'tree>)) {
    let mut cursor = node.walk();
    loop {
        if cursor.node().is_named() {
            visitor(cursor.node());
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn position(source: &str, offset: usize) -> (usize, usize) {
    let prefix = &source[..offset];
    (
        prefix.bytes().filter(|byte| *byte == b'\n').count() + 1,
        prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
    )
}

fn line_start(source: &str, offset: usize) -> usize {
    source[..offset].rfind('\n').map_or(0, |index| index + 1)
}

fn line_ending(source: &str) -> &'static str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn inferred_indent(source: &str) -> String {
    let widths: Vec<_> = source
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            let whitespace: String = line
                .chars()
                .take_while(|ch| *ch == ' ' || *ch == '\t')
                .collect();
            (!whitespace.is_empty()).then_some(whitespace)
        })
        .collect();
    if widths.iter().any(|value| value.starts_with('\t')) {
        return "\t".to_owned();
    }
    " ".repeat(
        widths
            .iter()
            .map(String::len)
            .min()
            .unwrap_or(4)
            .clamp(1, 8),
    )
}

fn identifier_start(source: &str, offset: usize) -> usize {
    source[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| *ch == '_' || ch.is_alphanumeric())
        .last()
        .map_or(offset, |(index, _)| index)
}

fn identifier_end(source: &str, offset: usize) -> usize {
    offset
        + source[offset..]
            .chars()
            .take_while(|ch| *ch == '_' || ch.is_alphanumeric())
            .map(char::len_utf8)
            .sum::<usize>()
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_alphanumeric())
}

fn identifier_at(module: &Module, offset: usize) -> Option<Node<'_>> {
    if offset > module.text.len() || !module.text.is_char_boundary(offset) {
        return None;
    }
    let mut found = None;
    walk(module.tree.root_node(), &mut |node| {
        if node.kind() == "identifier"
            && node.start_byte() <= offset
            && offset <= node.end_byte()
            && found.is_none_or(|prior: Node<'_>| node.start_byte() >= prior.start_byte())
        {
            found = Some(node);
        }
    });
    found
}

fn scope_at(module: &Module, offset: usize) -> usize {
    module
        .scopes
        .iter()
        .enumerate()
        .filter(|(_, scope)| scope.range.start <= offset && offset <= scope.range.end)
        .min_by_key(|(id, scope)| (scope.range.len(), std::cmp::Reverse(*id)))
        .map_or(0, |(id, _)| id)
}

fn enclosing_function(module: &Module, mut scope: usize) -> Option<usize> {
    loop {
        let current = &module.scopes[scope];
        if current.kind == Kind::Function {
            return current.owner;
        }
        scope = current.parent?;
    }
}

fn class_owner(module: &Module, mut scope: usize) -> Option<usize> {
    loop {
        if module.scopes[scope].kind == Kind::Class {
            return module.scopes[scope].owner;
        }
        scope = module.scopes[scope].parent?;
    }
}

fn first_identifier(node: Node<'_>) -> Option<Node<'_>> {
    let mut result = None;
    walk(node, &mut |child| {
        if result.is_none() && child.kind() == "identifier" {
            result = Some(child);
        }
    });
    result
}

fn parameter_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    if let Some(name) = node.child_by_field_name("name") {
        return first_identifier(name);
    }
    node.named_child(0).and_then(first_identifier)
}

fn is_value_identifier(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if matches!(
        parent.kind(),
        "import_statement"
            | "import_from_statement"
            | "dotted_name"
            | "aliased_import"
            | "global_statement"
            | "nonlocal_statement"
    ) {
        return false;
    }
    if matches!(
        parent.kind(),
        "function_definition"
            | "class_definition"
            | "keyword_argument"
            | "default_parameter"
            | "typed_default_parameter"
    ) && parent.child_by_field_name("name") == Some(node)
    {
        return false;
    }
    if parent.kind() == "attribute" && parent.child_by_field_name("attribute") == Some(node) {
        return false;
    }
    if parent.kind() == "assignment" && parent.child_by_field_name("left") == Some(node) {
        return false;
    }
    if matches!(
        parent.kind(),
        "parameters" | "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern"
    ) {
        return false;
    }
    true
}

fn module_name(path: &Path) -> String {
    let without_extension = path.with_extension("");
    let mut parts: Vec<_> = without_extension
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.first().is_some_and(|part| part == "src") {
        parts.remove(0);
    }
    if parts.last().is_some_and(|part| part == "__init__") {
        parts.pop();
    }
    parts.join(".")
}

fn absolute_import(current: &str, path: &Path, imported: &str) -> String {
    let level = imported.chars().take_while(|ch| *ch == '.').count();
    if level == 0 {
        return imported.to_owned();
    }
    let mut base: Vec<_> = current.split('.').collect();
    if path.file_stem().is_none_or(|name| name != "__init__") {
        base.pop();
    }
    for _ in 1..level {
        if base.pop().is_none() {
            return String::new();
        }
    }
    let suffix = imported.trim_start_matches('.');
    if !suffix.is_empty() {
        base.push(suffix);
    }
    base.join(".")
}

fn parse_decorator(node: Node<'_>, source: &str) -> Decorator {
    let mut decorator = Decorator::default();
    if node.kind() == "call" {
        decorator.name = node
            .child_by_field_name("function")
            .map(|value| text(value, source).to_owned())
            .unwrap_or_default();
        if let Some(arguments) = node.child_by_field_name("arguments") {
            let mut cursor = arguments.walk();
            for argument in arguments.named_children(&mut cursor) {
                if argument.kind() == "string" {
                    if let Some(value) = unquote(text(argument, source)) {
                        decorator.strings.push(value);
                    }
                } else if argument.kind() == "keyword_argument"
                    && let (Some(name), Some(value)) = (
                        argument.child_by_field_name("name"),
                        argument.child_by_field_name("value"),
                    )
                {
                    decorator.keywords.insert(
                        text(name, source).to_owned(),
                        text(value, source).to_owned(),
                    );
                }
            }
        }
    } else {
        decorator.name = text(node, source).to_owned();
    }
    decorator
}

fn unquote(value: &str) -> Option<String> {
    let quote = value.chars().next()?;
    if !matches!(quote, '\'' | '"') || !value.ends_with(quote) || value.len() < 2 {
        return None;
    }
    let inner = &value[1..value.len() - 1];
    (!inner.contains(['\\', '\n', '\r', '\'', '"'])).then(|| inner.to_owned())
}

fn import_insertion(module: &Module) -> usize {
    let mut offset = 0;
    let mut first_statement = true;
    let mut cursor = module.tree.root_node().walk();
    for node in module.tree.root_node().named_children(&mut cursor) {
        let docstring = first_statement
            && node.kind() == "expression_statement"
            && node
                .named_child(0)
                .is_some_and(|child| child.kind() == "string");
        if node.kind() == "comment"
            || docstring
            || matches!(
                node.kind(),
                "import_statement" | "import_from_statement" | "future_import_statement"
            )
        {
            offset = module.text[node.end_byte()..]
                .find('\n')
                .map_or(module.text.len(), |index| node.end_byte() + index + 1);
            if node.kind() != "comment" {
                first_statement = false;
            }
        } else {
            break;
        }
    }
    offset
}

fn declaration_start(module: &Module, symbol: &Symbol) -> usize {
    let Some(mut node) = module
        .tree
        .root_node()
        .descendant_for_byte_range(symbol.location.range.start, symbol.location.range.end)
    else {
        return line_start(&module.text, symbol.location.range.start);
    };
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "function_definition" | "class_definition") {
            node = parent;
            if let Some(decorated) = parent
                .parent()
                .filter(|parent| parent.kind() == "decorated_definition")
            {
                node = decorated;
            }
            return line_start(&module.text, node.start_byte());
        }
        node = parent;
    }
    line_start(&module.text, symbol.location.range.start)
}

fn standard_import(name: &str) -> Option<&'static str> {
    match name {
        "Path" | "PurePath" | "PurePosixPath" | "PureWindowsPath" => Some("pathlib"),
        "date" | "datetime" | "timedelta" | "timezone" => Some("datetime"),
        "Any" | "Optional" | "Union" | "Callable" | "TypeVar" | "Generic" | "Protocol"
        | "Literal" | "TypedDict" | "cast" => Some("typing"),
        "dataclass" | "field" | "asdict" => Some("dataclasses"),
        "Enum" | "IntEnum" => Some("enum"),
        "defaultdict" | "Counter" | "deque" => Some("collections"),
        _ => None,
    }
}

fn snake_case(value: &str) -> String {
    let chars: Vec<_> = value.chars().collect();
    let mut out = String::new();
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || chars.get(index + 1).is_some_and(|next| next.is_lowercase())
                    && chars[index - 1].is_uppercase())
        {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut previous: Vec<_> = (0..=right.len()).collect();
    for (row, ch) in left.chars().enumerate() {
        let mut current = vec![row + 1; right.len() + 1];
        for (column, other) in right.iter().enumerate() {
            current[column + 1] = (current[column] + 1)
                .min(previous[column + 1] + 1)
                .min(previous[column] + usize::from(ch != *other));
        }
        previous = current;
    }
    previous[right.len()]
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
    )
}

fn is_builtin(name: &str) -> bool {
    is_keyword(name)
        || matches!(
            name,
            "abs"
                | "aiter"
                | "all"
                | "anext"
                | "any"
                | "ascii"
                | "bin"
                | "bool"
                | "breakpoint"
                | "bytearray"
                | "bytes"
                | "callable"
                | "chr"
                | "classmethod"
                | "compile"
                | "complex"
                | "delattr"
                | "dict"
                | "dir"
                | "divmod"
                | "enumerate"
                | "eval"
                | "exec"
                | "filter"
                | "float"
                | "format"
                | "frozenset"
                | "getattr"
                | "globals"
                | "hasattr"
                | "hash"
                | "help"
                | "hex"
                | "id"
                | "input"
                | "int"
                | "isinstance"
                | "issubclass"
                | "iter"
                | "len"
                | "list"
                | "locals"
                | "map"
                | "max"
                | "memoryview"
                | "min"
                | "next"
                | "object"
                | "oct"
                | "open"
                | "ord"
                | "pow"
                | "print"
                | "property"
                | "range"
                | "repr"
                | "reversed"
                | "round"
                | "set"
                | "setattr"
                | "slice"
                | "sorted"
                | "staticmethod"
                | "str"
                | "sum"
                | "super"
                | "tuple"
                | "type"
                | "vars"
                | "zip"
                | "__import__"
                | "__name__"
                | "__file__"
                | "__package__"
                | "Exception"
                | "BaseException"
                | "ValueError"
                | "TypeError"
                | "RuntimeError"
                | "KeyError"
                | "IndexError"
                | "OSError"
                | "NotImplementedError"
                | "AssertionError"
                | "StopIteration"
                | "AttributeError"
                | "ImportError"
        )
}
