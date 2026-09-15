use pie_crust_core::Workbench;
use rusqlite::Connection;
use std::{fs, path::Path, process::Command};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn unrestricted_config(root: &Path) {
    write(
        root,
        ".pie_crust/config.toml",
        "[index]\nrespect_gitignore = false\nexclude = []\n[search]\nrespect_gitignore = false\nexclude = []\n",
    );
}

#[test]
fn environments_are_pruned_with_empty_exclusions_and_without_gitignore() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    unrestricted_config(root);
    for env in [
        ".venv",
        ".venv.windows",
        "backend/.venv-linux",
        "backend/venv",
        "backend/custom-runtime",
    ] {
        write(
            root,
            &format!("{env}/Lib/site-packages/library.py"),
            "venv_only_needle\n",
        );
        write(
            root,
            &format!("{env}/bin/library.pyi"),
            "venv_only_needle\n",
        );
        // A custom name is recognized from metadata, without needing a working
        // host-platform interpreter (e.g. a Linux environment copied to Windows).
        if env.ends_with("custom-runtime") {
            write(root, &format!("{env}/pyvenv.cfg"), "home = /usr/bin\n");
        }
    }
    for source in ["app.py", "env/config.py", "venv_tools/helpers.py"] {
        write(root, source, "project_needle\n");
    }
    let mut workbench = Workbench::open(root).unwrap();
    let id = workbench.active_worktree_id().to_owned();
    let request = workbench.index_request(&id).unwrap();
    assert_eq!(request.rebuild().unwrap().files, 3);
    assert_eq!(workbench.files(&id).unwrap().len(), 3);
    assert!(
        workbench
            .search(&id, "venv_only_needle", 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        workbench.search(&id, "project_needle", 100).unwrap().len(),
        3
    );
    // Explicit opening still works; it must not put the library back in search.
    let path = Path::new("backend/custom-runtime/Lib/site-packages/library.py");
    let document = workbench.read_document(&id, path).unwrap();
    workbench
        .edit_document(&document.id, document.version, "dirty_venv_needle".into())
        .unwrap();
    assert!(
        workbench
            .search(&id, "dirty_venv_needle", 100)
            .unwrap()
            .is_empty()
    );
    fs::remove_file(root.join(path)).unwrap();
    assert!(
        workbench
            .search(&id, "dirty_venv_needle", 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn recognizing_an_environment_removes_its_old_index_and_fts_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    unrestricted_config(root);
    write(root, "app.py", "project_code\n");
    write(
        root,
        "runtime/Lib/site-packages/library.py",
        "obsolete_library_needle\n",
    );
    let workbench = Workbench::open(root).unwrap();
    let id = workbench.active_worktree_id();
    let request = workbench.index_request(id).unwrap();
    assert_eq!(request.rebuild().unwrap().files, 2);
    write(root, "runtime/pyvenv.cfg", "home = /usr/bin\n");
    // Even before rebuilding, stale rows must never surface in search.
    assert!(
        workbench
            .search(id, "obsolete_library_needle", 100)
            .unwrap()
            .is_empty()
    );
    let stats = request.rebuild().unwrap();
    assert_eq!((stats.files, stats.updated, stats.removed), (1, 0, 1));
    let db = Connection::open(request.index_path()).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM files", [], |row| row.get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM code_search WHERE code_search MATCH 'obsolete_library_needle'",
            [],
            |row| row.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn explicit_library_search_bypasses_ignores_without_polluting_project_scope() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, ".gitignore", ".venv/\nruntime/\nignored/\n");
    write(root, "app.py", "project_needle\n");
    write(root, "ignored/app.py", "library_needle\n");
    write(root, ".venv/Lib/pkg.py", "library_needle\n");
    write(root, "runtime/pyvenv.cfg", "home = /usr/bin\n");
    write(root, "runtime/lib/pkg.py", "library_needle\n");
    let mut workbench = Workbench::open(root).unwrap();
    let id = workbench.active_worktree_id().to_owned();
    let request = workbench.index_request(&id).unwrap();
    let before = request.rebuild().unwrap().files;
    let files = request.environment_files().unwrap();
    assert_eq!(files.len(), 3);
    assert!(files.iter().all(|file| !file.path.starts_with("ignored")));
    assert!(
        request
            .search("library_needle", 100, std::iter::empty())
            .unwrap()
            .is_empty()
    );
    let hits = request
        .search_filtered_with_environments(
            "library_needle",
            100,
            std::iter::empty(),
            |_| true,
            true,
        )
        .unwrap();
    assert_eq!(hits.len(), 2);
    let hits = request
        .search_filtered_with_environments(
            "library_needle",
            1,
            std::iter::empty(),
            |path| path.starts_with("runtime"),
            true,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].path.starts_with("runtime"));
    let doc = workbench
        .read_document(&id, Path::new("runtime/lib/pkg.py"))
        .unwrap();
    workbench
        .edit_document(&doc.id, doc.version, "dirty_library_needle".into())
        .unwrap();
    fs::remove_file(root.join("runtime/lib/pkg.py")).unwrap();
    let (_, documents) = workbench.search_snapshot(&id).unwrap();
    assert_eq!(
        request
            .search_filtered_with_environments(
                "dirty_library_needle",
                100,
                documents.iter(),
                |_| true,
                true
            )
            .unwrap()
            .len(),
        1
    );
    assert!(
        request
            .search("dirty_library_needle", 100, documents.iter())
            .unwrap()
            .is_empty()
    );
    assert!(request.files().unwrap().iter().all(|file| {
        !pie_crust_core::is_python_environment_path(&root.canonicalize().unwrap(), &file.path)
    }));
    let db = Connection::open(request.index_path()).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM files", [], |row| row
            .get::<_, usize>(0))
            .unwrap(),
        before
    );
    assert_eq!(request.rebuild().unwrap().files, before);
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn copied_environments_are_excluded_in_every_git_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    let branch = temp.path().join("feature");
    fs::create_dir(&main).unwrap();
    git(&main, &["init", "-b", "main"]);
    git(&main, &["config", "user.email", "test@example.invalid"]);
    git(&main, &["config", "user.name", "Test"]);
    write(&main, "app.py", "source_only\n");
    git(&main, &["add", "app.py"]);
    git(&main, &["commit", "-m", "fixture"]);
    git(
        &main,
        &["worktree", "add", "-b", "feature", branch.to_str().unwrap()],
    );
    unrestricted_config(&main);
    for root in [&main, &branch] {
        write(
            root,
            "backend/copied-runtime/pyvenv.cfg",
            "home = /usr/bin\n",
        );
        write(
            root,
            "backend/copied-runtime/lib/python3.13/site-packages/library.py",
            "copied_library_needle\n",
        );
    }
    let workbench = Workbench::open(&main).unwrap();
    assert_eq!(workbench.worktrees().len(), 2);
    for tree in workbench.worktrees() {
        assert_eq!(
            workbench
                .index_request(&tree.id)
                .unwrap()
                .rebuild()
                .unwrap()
                .files,
            1
        );
        assert_eq!(workbench.files(&tree.id).unwrap().len(), 1);
        assert!(
            workbench
                .search(&tree.id, "copied_library_needle", 100)
                .unwrap()
                .is_empty()
        );
    }
}
