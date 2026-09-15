//! Python project layout discovery, independent of file indexing and search filters.

use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};

/// The Python projects and import roots found inside one worktree.
///
/// All paths are worktree-relative; `.` denotes the worktree root. The primary
/// source root is the project directory used for commands, even for a `src`
/// layout. Discovery only inspects directory entries and never executes Python.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonProjectLayout {
    /// Manifests ordered by depth, then path, with the worktree manifest first.
    pub manifests: Vec<PathBuf>,
    /// Project directories and their conventional `src` import directories.
    pub source_roots: Vec<PathBuf>,
    pub primary_source_root: PathBuf,
}

impl Default for PythonProjectLayout {
    fn default() -> Self {
        Self {
            manifests: Vec::new(),
            source_roots: vec![PathBuf::from(".")],
            primary_source_root: PathBuf::from("."),
        }
    }
}

impl PythonProjectLayout {
    pub fn discover(worktree_root: &Path) -> Result<Self> {
        let root = worktree_root
            .canonicalize()
            .with_context(|| format!("Cannot resolve worktree {}", worktree_root.display()))?;
        let mut pending = VecDeque::from([(PathBuf::new(), 0usize)]);
        let mut manifests = Vec::new();
        let mut django_roots = Vec::new();
        let mut visited_entries = 0usize;

        // These limits guard pathological trees without depending on the much
        // smaller interactive search/index limits. Never silently return a
        // partial layout when a discovery limit is reached.
        const MAX_DEPTH: usize = 128;
        const MAX_ENTRIES: usize = 1_000_000;

        while let Some((relative, depth)) = pending.pop_front() {
            let directory = root.join(&relative);
            let entries = fs::read_dir(&directory).with_context(|| {
                format!("Cannot discover Python projects in {}", directory.display())
            })?;
            let mut directories = Vec::new();
            for entry in entries {
                let entry = entry.with_context(|| {
                    format!("Cannot read project directory {}", directory.display())
                })?;
                visited_entries += 1;
                if visited_entries > MAX_ENTRIES {
                    bail!("Python project discovery exceeded {MAX_ENTRIES} directory entries");
                }
                let file_type = entry.file_type().with_context(|| {
                    format!("Cannot inspect project entry {}", entry.path().display())
                })?;
                if file_type.is_symlink() {
                    continue;
                }
                let name = entry.file_name();
                if file_type.is_dir() {
                    if excluded_directory(&name.to_string_lossy()) {
                        continue;
                    }
                    // DirEntry retains directory metadata on Windows. Avoid a
                    // separate stat of every asset/source file in large trees,
                    // while still excluding junctions and other reparse points.
                    let metadata = entry.metadata().with_context(|| {
                        format!(
                            "Cannot inspect project directory {}",
                            entry.path().display()
                        )
                    })?;
                    if is_link(&metadata) || has_environment_marker(&entry.path()) {
                        continue;
                    }
                    if depth >= MAX_DEPTH {
                        bail!("Python project discovery exceeded {MAX_DEPTH} directory levels");
                    }
                    directories.push(relative.join(&name));
                } else if file_type.is_file() && (name == "pyproject.toml" || name == "manage.py") {
                    let metadata = entry.metadata().with_context(|| {
                        format!(
                            "Cannot inspect Python project marker {}",
                            entry.path().display()
                        )
                    })?;
                    if is_link(&metadata) {
                        continue;
                    }
                    if name == "pyproject.toml" {
                        manifests.push(relative.join(&name));
                    } else {
                        django_roots.push(normalize_root(&relative));
                    }
                }
            }
            directories.sort();
            pending.extend(directories.into_iter().map(|path| (path, depth + 1)));
        }

        sort_by_depth(&mut manifests);
        sort_by_depth(&mut django_roots);
        let project_roots = if manifests.is_empty() {
            if django_roots.is_empty() {
                vec![PathBuf::from(".")]
            } else {
                django_roots
            }
        } else {
            manifests
                .iter()
                .map(|manifest| normalize_root(manifest.parent().unwrap_or(Path::new(""))))
                .collect()
        };
        let primary_source_root = project_roots[0].clone();
        let mut source_roots = Vec::new();
        for project_root in project_roots {
            let src = if project_root == Path::new(".") {
                PathBuf::from("src")
            } else {
                project_root.join("src")
            };
            if !source_roots.contains(&project_root) {
                source_roots.push(project_root);
            }
            if fs::symlink_metadata(root.join(&src))
                .is_ok_and(|metadata| metadata.is_dir() && !is_link(&metadata))
                && !source_roots.contains(&src)
            {
                source_roots.push(src);
            }
        }
        Ok(Self {
            manifests,
            source_roots,
            primary_source_root,
        })
    }

    pub fn primary_manifest(&self) -> Option<&Path> {
        self.manifests.first().map(PathBuf::as_path)
    }
}

/// A discovered virtual environment, with paths relative to the worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonEnvironment {
    pub interpreter: PathBuf,
    pub root: PathBuf,
}

/// Finds an existing virtual environment without running any interpreter.
///
/// Environments alongside the source root and its ancestors take precedence;
/// other nested environments are considered in depth/path order. Environment
/// directories must be real directories inside the worktree, but the Python
/// executable may be a symlink, as is usual for Unix virtual environments. Only
/// the host platform's interpreter layout is considered.
pub fn discover_python_environment(
    worktree_root: &Path,
    source_root: &Path,
) -> Option<PythonEnvironment> {
    let root = worktree_root.canonicalize().ok()?;
    if source_root.is_absolute()
        || source_root
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    let source = root.join(source_root).canonicalize().ok()?;
    if !source.starts_with(&root) || !source.is_dir() {
        return None;
    }
    let mut ancestor = source.as_path();
    loop {
        if let Some(environment) = environment_at(&root, ancestor) {
            return Some(environment);
        }
        for name in [".venv", "venv", "env", ".env"] {
            if let Some(environment) = environment_at(&root, &ancestor.join(name)) {
                return Some(environment);
            }
        }
        for directory in child_directories(ancestor) {
            if let Some(environment) = environment_at(&root, &directory) {
                return Some(environment);
            }
        }
        if ancestor == root {
            break;
        }
        ancestor = ancestor.parent()?;
    }

    let mut pending = VecDeque::from([(root.clone(), 0usize)]);
    let mut visited_directories = 0usize;
    while let Some((directory, depth)) = pending.pop_front() {
        visited_directories += 1;
        if visited_directories > 100_000 {
            return None;
        }
        if let Some(environment) = environment_at(&root, &directory) {
            return Some(environment);
        }
        let name = directory.file_name().unwrap_or_default().to_string_lossy();
        if depth >= 128
            || (directory != root && excluded_directory(&name))
            || has_environment_marker(&directory)
        {
            continue;
        }
        pending.extend(
            child_directories(&directory)
                .into_iter()
                .map(|child| (child, depth + 1)),
        );
    }
    None
}

fn environment_at(worktree_root: &Path, directory: &Path) -> Option<PythonEnvironment> {
    let relative = directory.strip_prefix(worktree_root).ok()?;
    // Check every directory component, including parents, so an otherwise real
    // environment below an alias/junction cannot escape the worktree.
    let mut current = worktree_root.to_owned();
    for component in relative.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).ok()?;
        if !metadata.is_dir() || is_link(&metadata) {
            return None;
        }
    }
    let common_name = directory
        .file_name()
        .is_some_and(|name| common_environment_name(&name.to_string_lossy()));
    if !common_name && !has_environment_marker(directory) {
        return None;
    }
    #[cfg(windows)]
    let executables = ["Scripts/python.exe"];
    #[cfg(not(windows))]
    let executables = ["bin/python", "bin/python3"];
    for executable in executables {
        let path = directory.join(executable);
        let executable_directory = path.parent()?;
        let metadata = fs::symlink_metadata(executable_directory).ok();
        if metadata.is_some_and(|metadata| metadata.is_dir() && !is_link(&metadata))
            && fs::metadata(&path).is_ok_and(|metadata| metadata.is_file())
        {
            return Some(PythonEnvironment {
                interpreter: path.strip_prefix(worktree_root).ok()?.to_owned(),
                root: normalize_root(relative),
            });
        }
    }
    None
}

fn child_directories(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut directories: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                return None;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if excluded_directory(&name) && !common_environment_name(&name) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            (!is_link(&metadata)).then(|| entry.path())
        })
        .collect();
    directories.sort();
    directories
}

fn common_environment_name(name: &str) -> bool {
    matches!(name, ".venv" | "venv" | "env" | ".env")
}

fn has_environment_marker(directory: &Path) -> bool {
    fs::symlink_metadata(directory.join("pyvenv.cfg"))
        .is_ok_and(|metadata| metadata.is_file() && !is_link(&metadata))
}

/// Cheap traversal boundary, independent of the host OS and user exclusions.
/// Recognizes copied/incomplete conventional venvs as well as custom names.
pub(crate) fn is_environment_directory(directory: &Path) -> bool {
    let reserved_name = directory.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        matches!(
            name.as_str(),
            ".venv" | "venv" | ".virtualenv" | "virtualenv"
        ) || name
            .strip_prefix(".venv")
            .is_some_and(|suffix| suffix.starts_with(['.', '-', '_']))
    });
    reserved_name || has_environment_marker(directory)
}

/// Tests a worktree-relative file path without traversing any environment contents.
/// Used for open buffers so they cannot re-enter project-wide analysis/search.
pub fn is_python_environment_path(root: &Path, relative: &Path) -> bool {
    if relative.components().any(|part| {
        !matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return true;
    }
    let mut directory = root.to_owned();
    if is_environment_directory(&directory) {
        return true;
    }
    let Some(parent) = relative.parent() else {
        return false;
    };
    for part in parent.components() {
        directory.push(part);
        if is_environment_directory(&directory) {
            return true;
        }
    }
    false
}

fn normalize_root(path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_owned()
    }
}

fn sort_by_depth(paths: &mut [PathBuf]) {
    paths.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
}

fn excluded_directory(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git"
            | ".hg"
            | ".svn"
            | ".pie_crust"
            | ".agents"
            | ".codex"
            | ".claude"
            | ".idea"
            | ".vscode"
            | ".venv"
            | "venv"
            | "env"
            | ".env"
            | "virtualenv"
            | ".virtualenv"
            | "node_modules"
            | "bower_components"
            | "vendor"
            | "site-packages"
            | "target"
            | "build"
            | "dist"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".tox"
            | ".nox"
            | ".cache"
            | ".next"
            | ".nuxt"
            | "htmlcov"
            | "coverage"
    ) || name.ends_with(".egg-info")
        || name.ends_with(".dist-info")
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes junctions and other reparse points, not only symlinks.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}
