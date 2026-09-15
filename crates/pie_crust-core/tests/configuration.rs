use pie_crust_core::{Workbench, validate_config_text};
use std::fs;

#[test]
fn config_validation_checks_typed_fields_and_both_glob_scopes() {
    let valid = "[search]\nrespect_gitignore = false\nexclude = ['vendor/**']\n[index]\nexclude = ['**/.venv/**']\n";
    assert!(validate_config_text("").is_ok());
    assert!(validate_config_text(valid).is_ok());
    for invalid in [
        "[search]\nexclude = 42\n",
        "[index]\nrespect_gitignore = 'yes'\n",
        "[search]\nexclude = ['[']\n",
        "[index]\nexclude = ['[']\n",
    ] {
        assert!(
            validate_config_text(invalid).is_err(),
            "Accepted {invalid:?}"
        );
    }
}

#[test]
fn existing_configurations_use_the_same_validation_without_rewriting_them() {
    let temporary = tempfile::tempdir().unwrap();
    let workbench = Workbench::open(temporary.path()).unwrap();
    let path = temporary.path().join(".pie_crust/config.toml");
    let wt = workbench.active_worktree_id();
    for invalid in ["[search]\nexclude = 42\n", "[index]\nexclude = ['[']\n"] {
        fs::write(&path, invalid).unwrap();
        assert!(workbench.index_request(wt).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
    }
    fs::write(&path, "[search]\nexclude = ['vendor']\n").unwrap();
    assert!(workbench.index_request(wt).is_ok());
}
