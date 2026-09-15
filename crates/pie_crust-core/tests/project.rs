use pie_crust_core::{PythonProjectLayout, discover_python_environment};
use std::fs;
use std::path::{Path, PathBuf};

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn paths(items: &[&str]) -> Vec<PathBuf> {
    items.iter().map(PathBuf::from).collect()
}

fn interpreter(environment: &str) -> PathBuf {
    Path::new(environment).join(if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    })
}

fn write_interpreter(root: &Path, environment: &str) {
    write(root, interpreter(environment).to_str().unwrap(), "");
}

#[test]
fn discovers_nested_project_independently_of_search_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, ".gitignore", "back-end/\n*.toml\n");
    write(
        root,
        ".pie_crust/config.toml",
        "[index]\ninclude = ['*.py']\n",
    );
    write(
        root,
        "back-end/frigo-recettes/pyproject.toml",
        "[project]\nname = 'frigo-recettes'\n",
    );
    write(root, "back-end/frigo-recettes/manage.py", "");
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert_eq!(
        layout.manifests,
        paths(&["back-end/frigo-recettes/pyproject.toml"])
    );
    assert_eq!(
        layout.primary_manifest(),
        Some(Path::new("back-end/frigo-recettes/pyproject.toml"))
    );
    assert_eq!(
        layout.primary_source_root,
        Path::new("back-end/frigo-recettes")
    );
    assert_eq!(layout.source_roots, paths(&["back-end/frigo-recettes"]));
}

#[test]
fn ignores_dependency_generated_and_internal_manifests() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for excluded in [
        ".git",
        ".pie_crust",
        ".codex",
        ".venv",
        "venv",
        "node_modules",
        "target",
        "build",
        "dist",
        "__pycache__",
        "vendor",
        ".tox",
        "site-packages",
    ] {
        write(root, &format!("{excluded}/library/pyproject.toml"), "");
        write(root, &format!("nested/{excluded}/pyproject.toml"), "");
    }
    write(root, "back-end/app/pyproject.toml", "");
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert_eq!(layout.manifests, paths(&["back-end/app/pyproject.toml"]));
}

#[test]
fn prefers_root_manifest_and_keeps_all_nested_projects_in_deterministic_order() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for manifest in [
        "z/pyproject.toml",
        "a/deeper/pyproject.toml",
        "a/pyproject.toml",
        "pyproject.toml",
    ] {
        write(root, manifest, "");
    }
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert_eq!(
        layout.manifests,
        paths(&[
            "pyproject.toml",
            "a/pyproject.toml",
            "z/pyproject.toml",
            "a/deeper/pyproject.toml"
        ])
    );
    assert_eq!(layout.primary_source_root, Path::new("."));
    assert_eq!(layout.source_roots, paths(&[".", "a", "z", "a/deeper"]));
    assert_eq!(PythonProjectLayout::discover(root).unwrap(), layout);
}

#[test]
fn prefers_shallower_then_lexical_nested_manifest() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for manifest in [
        "a/deep/pyproject.toml",
        "z/pyproject.toml",
        "b/pyproject.toml",
    ] {
        write(root, manifest, "");
    }
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert_eq!(layout.primary_source_root, Path::new("b"));
    assert_eq!(
        layout.primary_manifest(),
        Some(Path::new("b/pyproject.toml"))
    );
}

#[test]
fn includes_src_import_root_without_changing_execution_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "back-end/app/pyproject.toml", "");
    write(root, "back-end/app/src/app/__init__.py", "");
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert_eq!(layout.primary_source_root, Path::new("back-end/app"));
    assert_eq!(
        layout.source_roots,
        paths(&["back-end/app", "back-end/app/src"])
    );
}

#[test]
fn falls_back_to_django_marker_then_worktree_and_conventional_src() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "src/app/__init__.py", "");
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert!(layout.manifests.is_empty());
    assert_eq!(layout.primary_manifest(), None);
    assert_eq!(layout.primary_source_root, Path::new("."));
    assert_eq!(layout.source_roots, paths(&[".", "src"]));
    write(root, "back-end/app/manage.py", "");
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert_eq!(layout.primary_source_root, Path::new("back-end/app"));
    assert_eq!(layout.source_roots, paths(&["back-end/app"]));
}

#[cfg(unix)]
#[test]
fn does_not_follow_directory_or_manifest_symlinks() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(external.path(), "pyproject.toml", "");
    symlink(external.path(), root.join("linked-project")).unwrap();
    symlink(
        external.path().join("pyproject.toml"),
        root.join("pyproject.toml"),
    )
    .unwrap();
    symlink(external.path(), root.join("src")).unwrap();
    write(root, "real/pyproject.toml", "");
    symlink(root.join("real"), root.join("internal-alias")).unwrap();
    let layout = PythonProjectLayout::discover(root).unwrap();
    assert_eq!(layout.manifests, paths(&["real/pyproject.toml"]));
    assert_eq!(layout.source_roots, paths(&["real"]));
}

#[test]
fn prefers_environment_next_to_nested_source_over_worktree_environment() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write_interpreter(root, ".venv");
    write_interpreter(root, "back-end/app/.venv");
    let environment = discover_python_environment(root, Path::new("back-end/app")).unwrap();
    assert_eq!(environment.root, Path::new("back-end/app/.venv"));
    assert_eq!(environment.interpreter, interpreter("back-end/app/.venv"));
}

#[test]
fn discovers_ancestor_and_arbitrarily_named_nested_virtual_environments() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir_all(root.join("back-end/app")).unwrap();
    write_interpreter(root, "back-end/venv");
    let environment = discover_python_environment(root, Path::new("back-end/app")).unwrap();
    assert_eq!(environment.root, Path::new("back-end/venv"));
    assert_eq!(environment.interpreter, interpreter("back-end/venv"));

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "tools/python-runtime/pyvenv.cfg", "home = /usr/bin\n");
    write_interpreter(root, "tools/python-runtime");
    write(root, "tools/python-runtime/lib/library/pyproject.toml", "");
    let environment = discover_python_environment(root, Path::new(".")).unwrap();
    assert_eq!(environment.root, Path::new("tools/python-runtime"));
    assert_eq!(environment.interpreter, interpreter("tools/python-runtime"));
    assert_eq!(
        PythonProjectLayout::discover(root).unwrap(),
        PythonProjectLayout::default()
    );
}

#[test]
fn requires_existing_interpreter_and_ignores_dependency_environments() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, ".venv/pyvenv.cfg", "");
    write_interpreter(root, "node_modules/library/.venv");
    write(root, ".git/pyvenv.cfg", "");
    write_interpreter(root, ".git");
    write_interpreter(root, "vendor/env");
    assert!(discover_python_environment(root, Path::new(".")).is_none());
    assert!(discover_python_environment(root, Path::new("..")).is_none());
    assert!(discover_python_environment(root, root).is_none());
}

#[test]
fn accepts_an_opened_environment_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "pyvenv.cfg", "");
    write_interpreter(root, ".");
    let environment = discover_python_environment(root, Path::new(".")).unwrap();
    assert_eq!(environment.root, Path::new("."));
    assert_eq!(
        environment.interpreter,
        interpreter(".").strip_prefix(".").unwrap()
    );
}

#[cfg(windows)]
#[test]
fn chooses_windows_environment_when_unix_environment_also_exists() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, ".venv/pyvenv.cfg", "");
    write(root, ".venv/bin/python", "");
    write(root, ".venv.windows/pyvenv.cfg", "");
    write(root, ".venv.windows/Scripts/python.exe", "");
    let environment = discover_python_environment(root, Path::new(".")).unwrap();
    assert_eq!(environment.root, Path::new(".venv.windows"));
    assert_eq!(
        environment.interpreter,
        Path::new(".venv.windows/Scripts/python.exe")
    );
}

#[cfg(unix)]
#[test]
fn accepts_interpreter_symlinks_but_never_environment_directory_symlinks() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(external.path(), "venv/bin/python", "");
    symlink(external.path().join("venv"), root.join(".venv")).unwrap();
    assert!(discover_python_environment(root, Path::new(".")).is_none());
    fs::create_dir_all(root.join("venv/bin")).unwrap();
    symlink(
        external.path().join("venv/bin/python"),
        root.join("venv/bin/python"),
    )
    .unwrap();
    let environment = discover_python_environment(root, Path::new(".")).unwrap();
    assert_eq!(environment.root, Path::new("venv"));
    assert_eq!(environment.interpreter, Path::new("venv/bin/python"));
}
