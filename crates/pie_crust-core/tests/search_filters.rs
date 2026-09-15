use pie_crust_core::Workbench;
#[test]
fn path_filters_are_applied_before_the_result_limit_and_include_dirty_buffers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "needle\n".repeat(1100)).unwrap();
    std::fs::create_dir(root.path().join("app")).unwrap();
    let path = std::path::Path::new("app/z.py");
    std::fs::write(root.path().join(path), "value = 'original'\n").unwrap();
    let mut workbench = Workbench::open(root.path()).unwrap();
    let id = workbench.active_worktree_id().to_owned();
    let document = workbench.read_document(&id, path).unwrap();
    workbench
        .edit_document(&document.id, document.version, "value = 'needle'\n".into())
        .unwrap();
    let (request, documents) = workbench.search_snapshot(&id).unwrap();
    request.rebuild().unwrap();
    let hits = request
        .search_filtered("needle", 1, documents.iter(), |candidate| {
            candidate.starts_with("app") && candidate.extension().is_some_and(|ext| ext == "py")
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, path);
}

#[test]
fn streaming_search_publishes_matches_file_by_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.py"), "needle\nneedle\n").unwrap();
    std::fs::write(root.path().join("b.py"), "needle\n").unwrap();
    let workbench = Workbench::open(root.path()).unwrap();
    let id = workbench.active_worktree_id().to_owned();
    let (request, documents) = workbench.search_snapshot(&id).unwrap();
    request.rebuild().unwrap();
    let mut batches = Vec::new();

    let hits = request
        .search_filtered_with_environments_streaming(
            "needle",
            10,
            documents.iter(),
            |_| true,
            false,
            |batch| batches.push(batch.to_vec()),
        )
        .unwrap();

    assert_eq!(hits.len(), 3);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 2);
    assert!(
        batches[0]
            .iter()
            .all(|hit| hit.path == std::path::Path::new("a.py"))
    );
    assert_eq!(batches[1][0].path, std::path::Path::new("b.py"));
}
