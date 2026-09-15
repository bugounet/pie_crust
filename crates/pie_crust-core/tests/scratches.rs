use pie_crust_core::{DocumentKind, Workbench};
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn scratches_persist_edit_save_and_reopen_with_a_separate_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("scratch.py"), "source = True\n").unwrap();
    let mut workbench = Workbench::open(root).unwrap();
    assert!(workbench.list_scratches().unwrap().is_empty());
    let wt = workbench.active_worktree_id().to_owned();
    let source = workbench
        .read_document(&wt, Path::new("scratch.py"))
        .unwrap();
    let scratch = workbench
        .create_scratch("scratch.py", "\u{feff}é = 1\r\n")
        .unwrap();
    assert_eq!(scratch.kind, DocumentKind::Scratch);
    assert_eq!(source.kind, DocumentKind::Source);
    assert_ne!(scratch.id, source.id);
    assert!(scratch.id.starts_with("scratch-"));
    assert_eq!(scratch.path, Path::new(".pie_crust/scratches/scratch.py"));
    assert_eq!(scratch.worktree_id, wt);
    assert_eq!(
        fs::read_to_string(root.join(&scratch.path)).unwrap(),
        scratch.text
    );
    let version = workbench
        .edit_document(&scratch.id, scratch.version, "\u{feff}é = 2\r\n".into())
        .unwrap();
    assert!(workbench.read_scratch("scratch.py").unwrap().dirty);
    let focus = workbench.focus_scratch("scratch.py", 1, 3).unwrap();
    assert_eq!(focus.document_id, scratch.id);
    assert_eq!(focus.column, 3);
    assert!(workbench.focus_scratch("scratch.py", 0, 1).is_err());
    assert!(workbench.focus_scratch("scratch.py", 1, 99).is_err());
    workbench.save_document(&scratch.id).unwrap();
    let saved = workbench.read_scratch("scratch.py").unwrap();
    assert_eq!(saved.version, version);
    assert!(!saved.dirty);
    assert_eq!(
        fs::read(root.join(&scratch.path)).unwrap(),
        "\u{feff}é = 2\r\n".as_bytes()
    );
    drop(workbench);
    let mut reopened = Workbench::open(root).unwrap();
    let listed = reopened.list_scratches().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "scratch.py");
    assert_eq!(listed[0].path, scratch.path);
    let document = reopened.read_scratch(&listed[0].name).unwrap();
    assert_eq!(document.id, scratch.id);
    assert_eq!(document.text, "\u{feff}é = 2\r\n");
}

#[test]
fn scratches_never_enter_source_navigation_search_or_indexes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("source.py"), "source_literal()\n").unwrap();
    let mut workbench = Workbench::open(root).unwrap();
    let wt = workbench.active_worktree_id().to_owned();
    let scratch = workbench
        .create_scratch("notes.py", "scratch_literal()\n")
        .unwrap();
    let request = workbench.index_request(&wt).unwrap();
    assert_eq!(request.rebuild().unwrap().files, 1);
    assert_eq!(workbench.files(&wt).unwrap().len(), 1);
    assert!(
        workbench
            .search(&wt, "scratch_literal", 10)
            .unwrap()
            .is_empty()
    );
    workbench
        .edit_document(
            &scratch.id,
            scratch.version,
            "dirty_scratch_literal()\n".into(),
        )
        .unwrap();
    assert!(
        workbench
            .search(&wt, "dirty_scratch_literal", 10)
            .unwrap()
            .is_empty()
    );
    assert!(workbench.search_snapshot(&wt).unwrap().1.is_empty());
    let dirty = workbench.read_scratch("notes.py").unwrap();
    assert!(
        request
            .search("dirty_scratch_literal", 10, std::iter::once(&dirty))
            .unwrap()
            .is_empty()
    );
    for path in [scratch.path.clone(), root.join(&scratch.path)] {
        assert!(workbench.read_document(&wt, &path).is_err());
        assert!(workbench.focus_document(&wt, &path, 1, 1).is_err());
    }
    let count: u64 = Connection::open(request.index_path())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM files WHERE content LIKE '%scratch_literal%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn scratch_names_payloads_and_collisions_are_validated_without_overwriting() {
    let temporary = tempfile::tempdir().unwrap();
    let mut workbench = Workbench::open(temporary.path()).unwrap();
    for name in [
        "",
        ".",
        "..",
        "../escape.py",
        "sub/escape.py",
        "sub\\escape.py",
        "C:\\escape.py",
        "stream:secret",
        "trailing.",
        "trailing ",
        "nul.py",
        "COM1.py",
        "lpt¹.py",
        "bad\n.py",
    ] {
        assert!(
            workbench.create_scratch(name, "content").is_err(),
            "accepted {name:?}"
        );
        assert!(workbench.read_scratch(name).is_err(), "read {name:?}");
    }
    assert!(workbench.create_scratch("binary.py", "a\0b").is_err());
    assert!(
        workbench
            .create_scratch("oversized.py", &"x".repeat(2 * 1024 * 1024 + 1))
            .is_err()
    );
    assert!(workbench.list_scratches().unwrap().is_empty());
    let scratch = workbench.create_scratch("essai é.py", "original").unwrap();
    assert!(
        workbench
            .create_scratch("essai é.py", "replacement")
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(temporary.path().join(scratch.path)).unwrap(),
        "original"
    );
    assert_eq!(workbench.list_scratches().unwrap().len(), 1);
}

#[test]
fn scratch_external_changes_preserve_dirty_buffers_and_reject_stale_saves() {
    let temporary = tempfile::tempdir().unwrap();
    let mut workbench = Workbench::open(temporary.path()).unwrap();
    let scratch = workbench.create_scratch("notes.py", "before").unwrap();
    workbench
        .edit_document(&scratch.id, scratch.version, "unsaved".into())
        .unwrap();
    let absolute = temporary.path().join(&scratch.path);
    fs::write(&absolute, "external").unwrap();
    assert!(workbench.save_document(&scratch.id).is_err());
    assert_eq!(fs::read_to_string(&absolute).unwrap(), "external");
    assert_eq!(workbench.read_scratch("notes.py").unwrap().text, "unsaved");
    fs::remove_file(&absolute).unwrap();
    assert_eq!(workbench.read_scratch("notes.py").unwrap().text, "unsaved");
    assert!(workbench.focus_scratch("notes.py", 1, 2).is_ok());
    assert!(workbench.save_document(&scratch.id).is_err());
    assert!(!absolute.exists());
    assert!(workbench.create_scratch("notes.py", "replacement").is_err());
    assert!(!absolute.exists());
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
fn scratches_are_shared_across_worktrees_and_focus_keeps_the_active_context() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("main");
    let linked = temporary.path().join("feature");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.email", "tests@example.invalid"]);
    git(&root, &["config", "user.name", "pie_crust tests"]);
    fs::write(root.join("source.py"), "source()\n").unwrap();
    git(&root, &["add", "source.py"]);
    git(&root, &["commit", "-m", "initial"]);
    git(
        &root,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    let mut workbench = Workbench::open(&linked).unwrap();
    let feature_id = workbench.active_worktree_id().to_owned();
    let main_id = workbench
        .worktrees()
        .iter()
        .find(|wt| wt.branch.as_deref() == Some("main"))
        .unwrap()
        .id
        .clone();
    let scratch = workbench
        .create_scratch("shared.py", "shared_literal()\n")
        .unwrap();
    assert!(root.join(&scratch.path).is_file());
    assert!(!linked.join(".pie_crust").exists());
    workbench.focus_worktree(&main_id).unwrap();
    let focus = workbench.focus_scratch("shared.py", 1, 1).unwrap();
    assert_eq!(focus.worktree_id, main_id);
    assert_eq!(workbench.active_worktree_id(), main_id);
    assert_eq!(
        workbench.read_scratch("shared.py").unwrap().worktree_id,
        feature_id
    );
    assert_eq!(workbench.list_scratches().unwrap().len(), 1);
    for wt in [&main_id, &feature_id] {
        assert!(
            workbench
                .search(wt, "shared_literal", 10)
                .unwrap()
                .is_empty()
        );
    }
    let mut second = Workbench::open(&root).unwrap();
    assert_eq!(second.read_scratch("shared.py").unwrap().id, scratch.id);
}

#[cfg(any(unix, windows))]
fn link_directory(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        // cmd's mklink treats forward slashes as options, unlike Rust's file APIs.
        let native_link: std::path::PathBuf = link.components().collect();
        let native_target: std::path::PathBuf = target.components().collect();
        let output = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(native_link)
            .arg(native_target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Cannot create junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(any(unix, windows))]
#[test]
fn scratch_storage_rejects_redirected_directories_before_writing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.py"), "external_secret").unwrap();
    let mut workbench = Workbench::open(&root).unwrap();
    link_directory(&outside, &root.join(".pie_crust/scratches"));
    assert!(workbench.list_scratches().is_err());
    assert!(workbench.read_scratch("secret.py").is_err());
    assert!(workbench.create_scratch("escape.py", "content").is_err());
    assert!(!outside.join("escape.py").exists());
    assert_eq!(
        fs::read_to_string(outside.join("secret.py")).unwrap(),
        "external_secret"
    );
    let second_root = temporary.path().join("second-project");
    fs::create_dir(&second_root).unwrap();
    let mut second = Workbench::open(&second_root).unwrap();
    fs::rename(
        second_root.join(".pie_crust"),
        second_root.join("original-pie_crust"),
    )
    .unwrap();
    link_directory(&outside, &second_root.join(".pie_crust"));
    assert!(second.list_scratches().is_err());
    assert!(second.create_scratch("escape.py", "content").is_err());
    assert!(!outside.join("scratches").exists());
}

#[cfg(unix)]
#[test]
fn scratch_file_symlinks_are_not_opened_or_overwritten() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    fs::create_dir(&root).unwrap();
    let outside = temporary.path().join("secret.py");
    fs::write(&outside, "secret").unwrap();
    let mut workbench = Workbench::open(&root).unwrap();
    workbench.create_scratch("valid.py", "valid").unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".pie_crust/scratches/linked.py")).unwrap();
    assert!(workbench.read_scratch("linked.py").is_err());
    assert!(workbench.create_scratch("linked.py", "replace").is_err());
    assert_eq!(workbench.list_scratches().unwrap().len(), 1);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "secret");
}
