use pie_crust_core::Workbench;
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::process::Command;

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn independent_search_and_index_exclusions_cover_all_four_modes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(
        root,
        ".pie_crust/config.toml",
        r#"
        [search]
        respect_gitignore = false
        exclude = ["index_only", "neither/**"]
        [index]
        respect_gitignore = false
        exclude = ["search_only/**", "neither"]
    "#,
    );
    for directory in [
        "both",
        "index_only",
        "search_only",
        "neither",
        ".pie_crust/private",
    ] {
        write(root, &format!("{directory}/source.py"), "needle()\n");
    }
    let mut workbench = Workbench::open(root).unwrap();
    let id = workbench.active_worktree_id().to_owned();
    let request = workbench.index_request(&id).unwrap();
    let stats = request.rebuild().unwrap();
    assert_eq!(stats.files, 2);
    let database = Connection::open(request.index_path()).unwrap();
    let mut statement = database
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap();
    let stored: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(stored, ["both/source.py", "index_only/source.py"]);
    let hits = workbench.search(&id, "needle()", 100).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].path, Path::new("both/source.py"));
    assert_eq!(hits[1].path, Path::new("search_only/source.py"));
    let files = workbench.files(&id).unwrap();
    assert_eq!(files.len(), 3);
    assert!(
        workbench
            .read_document(&id, Path::new("neither/source.py"))
            .is_ok()
    );
    assert!(
        workbench
            .read_document(&id, Path::new(".pie_crust/private/source.py"))
            .is_err()
    );
}

#[test]
fn persistent_index_reconciles_changes_and_removals() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "a.py", "before()\n");
    write(root, "b.py", "delete_me()\n");
    let workbench = Workbench::open(root).unwrap();
    let id = workbench.active_worktree_id().to_owned();
    let request = workbench.index_request(&id).unwrap();
    assert_eq!(request.rebuild().unwrap().updated, 2);
    assert_eq!(request.rebuild().unwrap().updated, 0);
    let timestamp = fs::metadata(root.join("a.py")).unwrap().modified().unwrap();
    write(root, "a.py", "after_()\n");
    fs::File::options()
        .write(true)
        .open(root.join("a.py"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(timestamp))
        .unwrap();
    fs::remove_file(root.join("b.py")).unwrap();
    write(root, "c.py", "created()\n");
    let stats = request.rebuild().unwrap();
    assert_eq!((stats.files, stats.updated, stats.removed), (2, 2, 1));
    drop(workbench);
    let reopened = Workbench::open(root).unwrap();
    assert_eq!(reopened.active_worktree_id(), id);
    assert!(reopened.search(&id, "before", 10).unwrap().is_empty());
    assert_eq!(reopened.search(&id, "after_()", 10).unwrap().len(), 1);
}

#[test]
fn refresh_reuses_unchanged_persistent_entries_while_rebuild_revalidates_them() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "source.py", "disk_value()\n");
    let workbench = Workbench::open(root).unwrap();
    let id = workbench.active_worktree_id().to_owned();
    let request = workbench.index_request(&id).unwrap();
    request.rebuild().unwrap();

    let database = Connection::open(request.index_path()).unwrap();
    database
        .execute(
            "UPDATE files SET hash = 'cache-sentinel', content = 'cached_value()' WHERE path = 'source.py'",
            [],
        )
        .unwrap();

    let stats = request.refresh().unwrap();
    assert_eq!((stats.files, stats.updated, stats.removed), (1, 0, 0));
    let cached: String = database
        .query_row(
            "SELECT content FROM files WHERE path = 'source.py'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cached, "cached_value()");

    assert_eq!(request.rebuild().unwrap().updated, 1);
    let revalidated: String = database
        .query_row(
            "SELECT content FROM files WHERE path = 'source.py'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revalidated, "disk_value()\n");
}

#[test]
fn dirty_buffers_override_disk_and_stale_edits_and_saves_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "source.py", "old_name()\r\n");
    let mut workbench = Workbench::open(root).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    workbench.index_request(&wt).unwrap().rebuild().unwrap();
    let document = workbench
        .read_document(&wt, Path::new("source.py"))
        .unwrap();
    let version = workbench
        .edit_document(&document.id, document.version, "new_name()\r\n".into())
        .unwrap();
    assert_eq!(version, 2);
    assert!(
        workbench
            .edit_document(&document.id, document.version, "stale".into())
            .is_err()
    );
    assert_eq!(workbench.search(&wt, "new_name", 10).unwrap().len(), 1);
    assert!(workbench.search(&wt, "old_name", 10).unwrap().is_empty());
    write(root, "source.py", "external_change()\r\n");
    assert!(workbench.save_document(&document.id).is_err());
    assert_eq!(
        fs::read_to_string(root.join("source.py")).unwrap(),
        "external_change()\r\n"
    );
    assert_eq!(
        workbench.document(&document.id).unwrap().text,
        "new_name()\r\n"
    );
    assert!(workbench.document(&document.id).unwrap().dirty);
}

#[test]
fn save_preserves_bom_crlf_and_refresh_advances_versions() {
    let temporary = tempfile::tempdir().unwrap();
    write(temporary.path(), "source.py", "\u{feff}one = 1\r\n");
    let mut workbench = Workbench::open(temporary.path()).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    let document = workbench
        .read_document(&wt, Path::new("source.py"))
        .unwrap();
    workbench
        .edit_document(&document.id, 1, "\u{feff}one = 2\r\n".into())
        .unwrap();
    workbench.save_document(&document.id).unwrap();
    assert_eq!(
        fs::read(temporary.path().join("source.py")).unwrap(),
        "\u{feff}one = 2\r\n".as_bytes()
    );
    assert!(!workbench.document(&document.id).unwrap().dirty);
    write(temporary.path(), "source.py", "external\n");
    let refreshed = workbench
        .read_document(&wt, Path::new("source.py"))
        .unwrap();
    assert_eq!(refreshed.version, 3);
    assert_eq!(refreshed.text, "external\n");
}

#[test]
fn paths_stay_inside_worktree_and_focus_uses_unicode_positions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    fs::create_dir(&root).unwrap();
    write(temporary.path(), "outside.py", "secret\n");
    write(&root, "source.py", "é😀 needle()\n");
    let mut workbench = Workbench::open(&root).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    assert!(
        workbench
            .read_document(&wt, Path::new("../outside.py"))
            .is_err()
    );
    assert!(
        workbench
            .read_document(&wt, &temporary.path().join("outside.py"))
            .is_err()
    );
    assert!(
        workbench
            .read_document("unknown", Path::new("source.py"))
            .is_err()
    );
    let hits = workbench.search(&wt, "needle", 10).unwrap();
    assert_eq!((hits[0].line, hits[0].column), (1, 4));
    let focus = workbench
        .focus_document(&wt, Path::new("source.py"), 1, 4)
        .unwrap();
    assert_eq!(focus.serial, 1);
    assert!(
        workbench
            .focus_document(&wt, Path::new("source.py"), 0, 1)
            .is_err()
    );
    assert!(
        workbench
            .focus_document(&wt, Path::new("source.py"), 1, 99)
            .is_err()
    );
}

#[test]
fn git_worktrees_have_stable_ids_and_independent_databases_and_buffers() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("main checkout");
    let linked = temporary.path().join("feature checkout");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.email", "tests@example.invalid"]);
    git(&root, &["config", "user.name", "pie_crust tests"]);
    write(&root, "source.py", "main_content()\n");
    git(&root, &["add", "source.py"]);
    git(&root, &["commit", "-m", "initial"]);
    git(
        &root,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    write(&linked, "source.py", "feature_content()\n");
    let mut workbench = Workbench::open(&linked).unwrap();
    assert_eq!(workbench.project_root(), root.canonicalize().unwrap());
    assert_eq!(workbench.worktrees().len(), 2);
    let main = workbench
        .worktrees()
        .iter()
        .find(|wt| wt.branch.as_deref() == Some("main"))
        .unwrap()
        .id
        .clone();
    let feature = workbench.active_worktree_id().to_owned();
    assert_ne!(main, feature);
    let main_index = workbench.index_request(&main).unwrap();
    let feature_index = workbench.index_request(&feature).unwrap();
    main_index.rebuild().unwrap();
    feature_index.rebuild().unwrap();
    assert_ne!(main_index.index_path(), feature_index.index_path());
    assert!(
        workbench
            .search(&main, "feature_content", 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        workbench
            .search(&feature, "main_content", 10)
            .unwrap()
            .is_empty()
    );
    let document = workbench
        .read_document(&feature, Path::new("source.py"))
        .unwrap();
    workbench
        .edit_document(&document.id, 1, "unsaved()\n".into())
        .unwrap();
    workbench.focus_worktree(&main).unwrap();
    assert!(workbench.document(&document.id).unwrap().dirty);
    assert_eq!(workbench.search(&feature, "unsaved", 10).unwrap().len(), 1);
    git(&linked, &["checkout", "-b", "renamed-branch"]);
    let reopened = Workbench::open(&linked).unwrap();
    assert_eq!(reopened.active_worktree_id(), feature);
    assert!(root.join(".pie_crust/worktrees.json").is_file());
    assert!(!linked.join(".pie_crust").exists());
    let moved = temporary.path().join("moved feature checkout");
    git(
        &root,
        &[
            "worktree",
            "move",
            linked.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let moved_workbench = Workbench::open(&moved).unwrap();
    assert_eq!(moved_workbench.active_worktree_id(), feature);
    assert_eq!(
        moved_workbench
            .index_request(&feature)
            .unwrap()
            .index_path(),
        feature_index.index_path()
    );
}

#[test]
fn gitignore_respect_is_independent_between_search_and_index() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, ".gitignore", "ignored/\n");
    write(root, "ignored/source.py", "needle\n");
    write(
        root,
        ".pie_crust/config.toml",
        "[search]\nrespect_gitignore = false\nexclude = []\n[index]\nrespect_gitignore = true\nexclude = []\n",
    );
    let workbench = Workbench::open(root).unwrap();
    let wt = workbench.active_worktree_id();
    let request = workbench.index_request(wt).unwrap();
    request.rebuild().unwrap();
    let database = Connection::open(request.index_path()).unwrap();
    let count: usize = database
        .query_row(
            "SELECT count(*) FROM files WHERE path='ignored/source.py'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(workbench.search(wt, "needle", 10).unwrap().len(), 1);
}

#[test]
fn trigram_search_is_literal_case_sensitive_and_supports_unicode_and_multiline() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(
        root,
        "source.py",
        "é😀 target(\"x\")\r\nnext_line()\r\nUPPER_lower\nabc OR def\n",
    );
    write(root, "other.py", "xyz OR uvw\n");
    let mut workbench = Workbench::open(root).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    let request = workbench.index_request(&wt).unwrap();
    request.rebuild().unwrap();
    for query in [
        "target(\"x\")",
        "é😀 ",
        "x\")\r\nnext_",
        "abc OR def",
        "UPPER",
        "()",
        "é",
    ] {
        assert!(
            !workbench.search(&wt, query, 20).unwrap().is_empty(),
            "missing literal {query:?}"
        );
    }
    assert!(workbench.search(&wt, "upper", 20).unwrap().is_empty());
    assert!(workbench.search(&wt, "abc OR uvw", 20).unwrap().is_empty());
    let hits = workbench.search(&wt, "target", 20).unwrap();
    assert_eq!((hits[0].line, hits[0].column), (1, 4));
    let database = Connection::open(request.index_path()).unwrap();
    let row_count: usize = database
        .query_row(
            "SELECT count(*) FROM code_search WHERE code_search MATCH '\"target\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(row_count, 1);
    let document = workbench
        .read_document(&wt, Path::new("source.py"))
        .unwrap();
    workbench
        .edit_document(&document.id, document.version, "snapshot_buffer()\n".into())
        .unwrap();
    let (snapshot_request, buffers) = workbench.search_snapshot(&wt).unwrap();
    workbench
        .edit_document(
            &document.id,
            document.version + 1,
            "later_buffer()\n".into(),
        )
        .unwrap();
    assert_eq!(
        snapshot_request
            .search("snapshot_buffer", 10, buffers.iter())
            .unwrap()
            .len(),
        1
    );
    assert!(
        snapshot_request
            .search("later_buffer", 10, buffers.iter())
            .unwrap()
            .is_empty()
    );
    assert_eq!(workbench.search(&wt, "later_buffer", 10).unwrap().len(), 1);
    write(root, "source.py", "replacement()\n");
    fs::remove_file(root.join("other.py")).unwrap();
    request.rebuild().unwrap();
    let rows: usize = database
        .query_row(
            "SELECT count(*) FROM code_search WHERE code_search MATCH '\"target\" OR \"uvw\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn an_inaccessible_worktree_does_not_clear_its_index() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    fs::create_dir(&root).unwrap();
    write(&root, "source.py", "retained()\n");
    let workbench = Workbench::open(&root).unwrap();
    let request = workbench
        .index_request(workbench.active_worktree_id())
        .unwrap();
    request.rebuild().unwrap();
    let relocated = temporary.path().join("relocated");
    fs::rename(&root, &relocated).unwrap();
    assert!(request.rebuild().is_err());
    let database = Connection::open(
        relocated
            .join(".pie_crust/indexes")
            .join(request.index_path().file_name().unwrap()),
    )
    .unwrap();
    let count: usize = database
        .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn duplicate_or_path_shaped_registry_ids_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    Workbench::open(root).unwrap();
    let path = root.join(".pie_crust/worktrees.json");
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut duplicate = original.clone();
    let mut entry = duplicate["worktrees"][0].clone();
    entry["identity"] = "different-identity".into();
    duplicate["worktrees"].as_array_mut().unwrap().push(entry);
    fs::write(&path, serde_json::to_vec(&duplicate).unwrap()).unwrap();
    assert!(Workbench::open(root).is_err());
    let mut invalid = original;
    invalid["worktrees"][0]["id"] = "wt-../../escape".into();
    fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert!(Workbench::open(root).is_err());
}

#[test]
fn concurrent_registry_and_initial_index_creation_remain_consistent() {
    use std::sync::{Arc, Barrier};
    let temporary = tempfile::tempdir().unwrap();
    write(temporary.path(), "source.py", "concurrent_literal()\n");
    let barrier = Arc::new(Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let root = temporary.path().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let workbench = Workbench::open(&root).unwrap();
                let wt = workbench.active_worktree_id().to_owned();
                barrier.wait();
                let stats = workbench.index_request(&wt).unwrap().rebuild().unwrap();
                assert_eq!(stats.files, 1);
                assert_eq!(
                    workbench
                        .search(&wt, "concurrent_literal", 10)
                        .unwrap()
                        .len(),
                    1
                );
                wt
            })
        })
        .collect();
    let ids: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
}

#[test]
fn reusing_a_moved_worktree_path_cannot_steal_its_identity_or_index() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("main");
    let original = temporary.path().join("a-original");
    let moved = temporary.path().join("z-moved");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.email", "tests@example.invalid"]);
    git(&root, &["config", "user.name", "pie_crust tests"]);
    write(&root, "source.py", "main_content()\n");
    git(&root, &["add", "source.py"]);
    git(&root, &["commit", "-m", "initial"]);
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            "original",
            original.to_str().unwrap(),
        ],
    );
    let before = Workbench::open(&original).unwrap();
    let original_id = before.active_worktree_id().to_owned();
    before
        .index_request(&original_id)
        .unwrap()
        .rebuild()
        .unwrap();
    git(
        &root,
        &[
            "worktree",
            "move",
            original.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            "replacement",
            original.to_str().unwrap(),
        ],
    );
    write(&original, "source.py", "replacement_content()\n");
    write(&moved, "source.py", "moved_content()\n");
    let after = Workbench::open(&moved).unwrap();
    let replacement = after
        .worktrees()
        .iter()
        .find(|wt| wt.root == original.canonicalize().unwrap())
        .unwrap();
    assert_eq!(after.active_worktree_id(), original_id);
    assert_ne!(replacement.id, original_id);
    let moved_index = after.index_request(&original_id).unwrap();
    let replacement_index = after.index_request(&replacement.id).unwrap();
    assert_ne!(moved_index.index_path(), replacement_index.index_path());
    moved_index.rebuild().unwrap();
    replacement_index.rebuild().unwrap();
    assert_eq!(
        after
            .search(&original_id, "moved_content", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        after
            .search(&original_id, "replacement_content", 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        after
            .search(&replacement.id, "replacement_content", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        after
            .search(&replacement.id, "moved_content", 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn deleted_dirty_documents_remain_readable_focusable_and_searchable_with_exclusions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "src/source.py", "initial()\n");
    let mut workbench = Workbench::open(root).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    workbench.index_request(&wt).unwrap().rebuild().unwrap();
    let document = workbench
        .read_document(&wt, Path::new("src/source.py"))
        .unwrap();
    workbench
        .edit_document(&document.id, document.version, "unsaved_literal()\n".into())
        .unwrap();
    fs::remove_file(root.join("src/source.py")).unwrap();
    fs::remove_dir(root.join("src")).unwrap();
    let retained = workbench
        .read_document(&wt, Path::new("src/source.py"))
        .unwrap();
    assert_eq!(retained.text, "unsaved_literal()\n");
    assert!(retained.dirty);
    assert_eq!(
        workbench
            .read_document(&wt, &root.join("src/source.py"))
            .unwrap()
            .id,
        document.id
    );
    assert_eq!(
        workbench
            .focus_document(&wt, Path::new("src/source.py"), 1, 3)
            .unwrap()
            .document_id,
        document.id
    );
    assert_eq!(
        workbench.search(&wt, "unsaved_literal", 10).unwrap().len(),
        1
    );
    workbench.index_request(&wt).unwrap().rebuild().unwrap();
    assert_eq!(
        workbench.search(&wt, "unsaved_literal", 10).unwrap().len(),
        1
    );
    assert!(workbench.save_document(&document.id).is_err());
    assert!(!root.join("src/source.py").exists());
    assert!(workbench.document(&document.id).unwrap().dirty);
    assert!(
        workbench
            .read_document(&wt, Path::new("../outside_missing.py"))
            .is_err()
    );
    write(
        root,
        ".pie_crust/config.toml",
        "[search]\nrespect_gitignore = false\nexclude = ['src']\n",
    );
    assert!(
        workbench
            .search(&wt, "unsaved_literal", 10)
            .unwrap()
            .is_empty()
    );
    write(
        root,
        ".pie_crust/config.toml",
        "[search]\nrespect_gitignore = true\nexclude = []\n",
    );
    write(root, ".gitignore", "src/\n");
    assert!(
        workbench
            .search(&wt, "unsaved_literal", 10)
            .unwrap()
            .is_empty()
    );
    write(root, ".gitignore", "\n");
    assert_eq!(
        workbench.search(&wt, "unsaved_literal", 10).unwrap().len(),
        1
    );
    write(root, ".ignore", "src/source.py\n");
    assert!(
        workbench
            .search(&wt, "unsaved_literal", 10)
            .unwrap()
            .is_empty()
    );
}

#[cfg(windows)]
#[test]
fn windows_junction_escape_is_not_traversed_or_opened() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    fs::create_dir(&root).unwrap();
    write(temporary.path(), "outside/source.py", "needle\n");
    let result = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(root.join("linked"))
        .arg(temporary.path().join("outside"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Cannot create test junction: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut workbench = Workbench::open(&root).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    assert_eq!(
        workbench
            .index_request(&wt)
            .unwrap()
            .rebuild()
            .unwrap()
            .files,
        0
    );
    assert!(workbench.search(&wt, "needle", 10).unwrap().is_empty());
    assert!(
        workbench
            .read_document(&wt, Path::new("linked/source.py"))
            .is_err()
    );
    assert!(temporary.path().join("outside/source.py").is_file());
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_not_traversed_or_opened() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    fs::create_dir(&root).unwrap();
    write(temporary.path(), "outside/source.py", "needle\n");
    std::os::unix::fs::symlink(temporary.path().join("outside"), root.join("linked")).unwrap();
    let mut workbench = Workbench::open(&root).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    assert_eq!(
        workbench
            .index_request(&wt)
            .unwrap()
            .rebuild()
            .unwrap()
            .files,
        0
    );
    assert!(workbench.search(&wt, "needle", 10).unwrap().is_empty());
    assert!(
        workbench
            .read_document(&wt, Path::new("linked/source.py"))
            .is_err()
    );
}
