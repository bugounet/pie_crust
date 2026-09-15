use pie_crust_core::python::{PythonSource, PythonWorkspace, TextEdit, apply_text_edits};
use std::path::Path;

fn workspace(files: &[(&str, &str)]) -> PythonWorkspace {
    PythonWorkspace::new(
        files
            .iter()
            .map(|(path, text)| PythonSource {
                path: (*path).into(),
                text: (*text).to_owned(),
            })
            .collect(),
    )
    .unwrap()
}

fn workspace_with_roots(files: &[(&str, &str)], roots: &[&str]) -> PythonWorkspace {
    PythonWorkspace::with_source_roots(
        files
            .iter()
            .map(|(path, text)| PythonSource {
                path: (*path).into(),
                text: (*text).to_owned(),
            })
            .collect(),
        &roots.iter().map(|root| (*root).into()).collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn nested_project_root_resolves_imports_and_keeps_navigation_paths() {
    let api = "def load() -> int:\n    return 1\n";
    let app = "from recipes.api import load\n\ndef main() -> int:\n    return load()\n";
    let api_path = "back-end/frigo-recettes/recipes/api.py";
    let app_path = "back-end/frigo-recettes/app.py";
    let ws = workspace_with_roots(
        &[(api_path, api), (app_path, app)],
        &[".", "back-end", "back-end/frigo-recettes"],
    );
    let hierarchy = ws
        .call_hierarchy(Path::new(app_path), app.rfind("load").unwrap())
        .unwrap();
    assert_eq!(hierarchy.target.path, Path::new(api_path));
    assert_eq!(hierarchy.callers.len(), 1);
    assert_eq!(hierarchy.callers[0].caller.path, Path::new(app_path));
    assert_eq!(hierarchy.callers[0].path, Path::new(app_path));
    assert_eq!(ws.source(Path::new(api_path)), Some(api));
    assert!(
        ws.sources()
            .iter()
            .any(|source| source.path == Path::new(api_path))
    );
}

#[test]
fn nested_src_layout_supports_relative_imports_and_import_fixes() {
    let api = "def load() -> int:\n    return 1\n";
    let relative = "from .api import load\n\ndef main() -> int:\n    return load()\n";
    let unresolved = "def run() -> int:\n    return load()\n";
    let api_path = "back-end/frigo-recettes/src/recipes/api.py";
    let app_path = "back-end/frigo-recettes/src/recipes/app.py";
    let test_path = "back-end/frigo-recettes/tests/test_app.py";
    for roots in [
        vec!["back-end/frigo-recettes"],
        vec!["back-end/frigo-recettes", "back-end/frigo-recettes/src"],
    ] {
        let ws = workspace_with_roots(
            &[
                (api_path, api),
                (app_path, relative),
                (test_path, unresolved),
            ],
            &roots,
        );
        let hierarchy = ws
            .call_hierarchy(Path::new(api_path), api.find("load").unwrap())
            .unwrap();
        assert_eq!(hierarchy.callers.len(), 1);
        assert_eq!(hierarchy.callers[0].path, Path::new(app_path));
        let fixes = ws.quick_fixes(Path::new(test_path), unresolved.find("load").unwrap());
        let import = fixes
            .iter()
            .find(|fix| {
                fix.edits
                    .iter()
                    .any(|edit| edit.replacement == "from recipes.api import load\n")
            })
            .unwrap();
        let updated = apply_text_edits(unresolved, &import.edits).unwrap();
        assert!(updated.starts_with("from recipes.api import load\n"));
        let ws = workspace_with_roots(&[(api_path, api), (test_path, &updated)], &roots);
        assert_eq!(
            ws.call_hierarchy(Path::new(test_path), updated.rfind("load").unwrap())
                .unwrap()
                .target
                .path,
            Path::new(api_path)
        );
    }
}

#[test]
fn project_tests_resolve_their_own_src_modules_in_a_monorepo() {
    let api = "def load() -> int:\n    return 1\n";
    let test = "from recipes.api import load\ndef test_load() -> int:\n    return load()\n";
    let ws = workspace_with_roots(
        &[
            ("projects/first/src/recipes/api.py", api),
            ("projects/first/tests/test_api.py", test),
            ("projects/second/src/recipes/api.py", api),
            ("projects/second/tests/test_api.py", test),
        ],
        &[
            "projects/first",
            "projects/first/src",
            "projects/second",
            "projects/second/src",
        ],
    );
    for root in ["projects/first", "projects/second"] {
        let hierarchy = ws
            .call_hierarchy(
                &Path::new(root).join("tests/test_api.py"),
                test.rfind("load").unwrap(),
            )
            .unwrap();
        assert_eq!(
            hierarchy.target.path,
            Path::new(root).join("src/recipes/api.py")
        );
        assert_eq!(hierarchy.callers.len(), 1);
        assert_eq!(
            hierarchy.callers[0].path,
            Path::new(root).join("tests/test_api.py")
        );
    }
}

#[test]
fn duplicate_modules_prefer_callers_source_root_without_cross_project_members() {
    let api = "def load() -> int:\n    return 1\n";
    let foreign_api =
        "def load() -> int:\n    return 2\ndef foreign_only() -> int:\n    return 3\n";
    let app =
        "import api\nfrom api import load\n\ndef main() -> int:\n    return load() + api.load()\n";
    let missing =
        "from api import foreign_only\n\ndef missing() -> int:\n    return foreign_only()\n";
    let unresolved = "def unresolved() -> int:\n    return foreign_only()\n";
    let ws = workspace_with_roots(
        &[
            ("back-end/frigo-recettes/api.py", api),
            ("back-end/frigo-recettes/app.py", app),
            ("back-end/frigo-recettes/missing.py", missing),
            ("back-end/frigo-recettes/unresolved.py", unresolved),
            ("another-project/api.py", foreign_api),
            ("another-project/app.py", app),
            ("outside.py", app),
        ],
        &["back-end/frigo-recettes", "another-project"],
    );
    for root in ["back-end/frigo-recettes", "another-project"] {
        let path = Path::new(root).join("api.py");
        let hierarchy = ws.call_hierarchy(&path, api.find("load").unwrap()).unwrap();
        assert_eq!(hierarchy.callers.len(), 2, "{hierarchy:?}");
        assert!(
            hierarchy
                .callers
                .iter()
                .all(|call| call.path == Path::new(root).join("app.py"))
        );
    }
    assert!(
        ws.call_hierarchy(
            Path::new("back-end/frigo-recettes/missing.py"),
            missing.find("missing").unwrap(),
        )
        .unwrap()
        .callees
        .is_empty()
    );
    assert!(
        ws.call_hierarchy(Path::new("outside.py"), app.find("main").unwrap())
            .unwrap()
            .callees
            .is_empty()
    );
    assert!(
        ws.quick_fixes(
            Path::new("back-end/frigo-recettes/unresolved.py"),
            unresolved.find("foreign_only").unwrap(),
        )
        .iter()
        .all(|fix| !fix.label.starts_with("Importer"))
    );
}

#[test]
fn completion_obeys_lexical_scopes_and_survives_incomplete_source() {
    let text = "global_name = 1\ndef first(customer: str) -> str:\n    customer_total = 2\n    cust\ndef other() -> None:\n    customer_private = 3\n";
    let ws = workspace(&[("app.py", text)]);
    let at = text.find("    cust\n").unwrap() + "    cust".len();
    let names: Vec<_> = ws
        .completions(Path::new("app.py"), at)
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["customer", "customer_total"]);
    let incomplete = "def first(customer: str) -> str:\n    return cust(";
    let ws = workspace(&[("app.py", incomplete)]);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .unwrap();
    let ast = parser.parse(incomplete, None).unwrap();
    assert!(
        ws.completions(Path::new("app.py"), incomplete.len() - 1)
            .iter()
            .any(|entry| entry.name == "customer"),
        "{}",
        ast.root_node().to_sexp()
    );
}

#[test]
fn occurrences_resolve_shadowing_and_ignore_comments_and_strings() {
    let text = "value = 1\ndef one(value: int) -> int:\n    # value\n    label = 'value'\n    return value\ndef two(value: int) -> int:\n    return value\n";
    let ws = workspace(&[("app.py", text)]);
    let at = text.find("one(value").unwrap() + "one(".len();
    let references = ws.occurrences(Path::new("app.py"), at);
    assert_eq!(references.len(), 2, "{references:?}");
    assert!(
        references
            .iter()
            .all(|range| &text[range.clone()] == "value")
    );
    assert!(
        references
            .iter()
            .all(|range| range.start < text.find("def two").unwrap())
    );
}

#[test]
fn typed_hierarchy_resolves_imported_receivers_without_homonyms_or_untyped_calls() {
    let service =
        "class Service:\n    def charge(self, amount: int) -> int:\n        return amount\n";
    let other = "class Other:\n    def charge(self, amount: int) -> int:\n        return amount\n";
    let app = "from service import Service\nfrom other import Other\n\ndef checkout(service: Service, amount: int) -> int:\n    return service.charge(amount)\n\ndef unrelated(service: Other, amount: int) -> int:\n    return service.charge(amount)\n\ndef legacy(service, amount):\n    return service.charge(amount)\n\ndef dynamic(service: object, amount: int) -> int:\n    return service.charge(amount)\n";
    let ws = workspace(&[
        ("service.py", service),
        ("other.py", other),
        ("app.py", app),
    ]);
    let hierarchy = ws
        .call_hierarchy(Path::new("service.py"), service.find("charge").unwrap())
        .unwrap();
    assert_eq!(hierarchy.callers.len(), 1, "{hierarchy:?}");
    assert_eq!(hierarchy.callers[0].caller.name, "checkout");
    assert_eq!(hierarchy.callers[0].path, Path::new("app.py"));
    let caller = ws
        .call_hierarchy(Path::new("app.py"), app.find("checkout").unwrap())
        .unwrap();
    assert_eq!(caller.callees.len(), 1);
    assert_eq!(caller.callees[0].callee.name, "charge");
    assert!(
        ws.call_hierarchy(Path::new("app.py"), app.find("legacy").unwrap())
            .is_none()
    );
}

#[test]
fn typed_direct_calls_require_qualified_resolution_and_all_hints() {
    let api = "def load(value: int) -> int:\n    return value\n";
    let app = "import api as lib\nfrom api import load as fetch\n\ndef main(value: int) -> int:\n    return fetch(lib.load(value))\n\ndef ignored(value) -> int:\n    return fetch(value)\n\ndef shadow(fetch: object, value: int) -> int:\n    return fetch(value)\n";
    let ws = workspace(&[("api.py", api), ("app.py", app)]);
    let calls = ws
        .call_hierarchy(Path::new("api.py"), api.find("load").unwrap())
        .unwrap()
        .callers;
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|call| call.caller.name == "main"));
}

#[test]
fn fixtures_include_nearest_conftest_autouse_dependencies_and_marks() {
    let root = "import pytest\n@pytest.fixture\ndef database():\n    return 'root'\n@pytest.fixture(autouse=True)\ndef clean(database):\n    pass\n";
    let nested = "from pytest import fixture\n@fixture(name='database')\ndef local_database():\n    return 'local'\n";
    let unrelated = "import pytest\n@pytest.fixture(autouse=True)\ndef unwanted():\n    pass\n";
    let test = "import pytest\n@pytest.fixture\ndef user(database):\n    return database\n@pytest.mark.usefixtures('external_plugin')\n@pytest.mark.parametrize('count', [1, 2])\ndef test_checkout(user, count):\n    pass\n";
    let ws = workspace(&[
        ("conftest.py", root),
        ("tests/conftest.py", nested),
        ("elsewhere/conftest.py", unrelated),
        ("tests/test_app.py", test),
    ]);
    let fixtures = ws.fixtures(
        Path::new("tests/test_app.py"),
        test.find("test_checkout").unwrap(),
    );
    let names: Vec<_> = fixtures
        .iter()
        .map(|fixture| fixture.name.as_str())
        .collect();
    assert_eq!(names, ["clean", "database", "external_plugin", "user"]);
    assert_eq!(
        fixtures
            .iter()
            .find(|fixture| fixture.name == "database")
            .unwrap()
            .location
            .as_ref()
            .unwrap()
            .path,
        Path::new("tests/conftest.py")
    );
    assert!(
        fixtures
            .iter()
            .find(|fixture| fixture.name == "external_plugin")
            .unwrap()
            .location
            .is_none()
    );
}

#[test]
fn unrelated_fixture_decorator_is_not_treated_as_pytest_and_indirect_is_fixture() {
    let text = "import pytest as pt\nimport custom\n@custom.fixture(autouse=True)\ndef unwanted():\n    pass\n@pt.fixture\ndef user():\n    pass\n@pt.mark.parametrize('user', [1], indirect=True)\ndef test_app(user):\n    pass\n";
    let ws = workspace(&[("test_app.py", text)]);
    let fixtures = ws.fixtures(Path::new("test_app.py"), text.find("test_app").unwrap());
    assert_eq!(fixtures.len(), 1, "{fixtures:?}");
    assert_eq!(fixtures[0].name, "user");
    assert!(fixtures[0].location.is_some());
}

#[test]
fn quick_fixes_propose_real_imports_similar_bindings_and_create_stub() {
    let api = "def calculate(value: int) -> int:\n    return value\n";
    let app = "#!/usr/bin/env python\r\n\"\"\"Module docs.\"\"\"\r\nfrom __future__ import annotations\r\n\r\ndef main(value: int) -> int:\r\n  return calculate(value)\r\n";
    let ws = workspace(&[("api.py", api), ("app.py", app)]);
    let fixes = ws.quick_fixes(Path::new("app.py"), app.find("calculate").unwrap());
    let import = fixes
        .iter()
        .find(|fix| fix.label.starts_with("Importer"))
        .unwrap();
    let updated = apply_text_edits(app, &import.edits).unwrap();
    assert!(
        updated.contains("from __future__ import annotations\r\nfrom api import calculate\r\n"),
        "{updated:?}"
    );
    let create = fixes
        .iter()
        .find(|fix| fix.label.starts_with("Créer la fonction"))
        .unwrap();
    assert!(
        apply_text_edits(app, &create.edits)
            .unwrap()
            .contains("\r\n  raise NotImplementedError\r\n")
    );
    let typo = "def run(customer: str) -> str:\n    return custmer\n";
    let ws = workspace(&[("app.py", typo)]);
    let fixes = ws.quick_fixes(Path::new("app.py"), typo.find("custmer").unwrap());
    assert!(
        fixes
            .iter()
            .any(|fix| fix.label == "Remplacer par customer")
    );
    assert!(
        ws.quick_fixes(Path::new("app.py"), typo.find("customer").unwrap())
            .is_empty()
    );
}

#[test]
fn extraction_suggests_return_type_and_callable_names_and_preserves_bytes() {
    let text = "class UserProfile:\r\n\tpass\r\ndef fetch_account() -> UserProfile:\r\n\treturn UserProfile()\r\ndef render() -> None:\r\n\tlabel = 'é'\r\n\tprofile = fetch_account()\r\n";
    let ws = workspace(&[("app.py", text)]);
    let at = text.rfind("fetch_account()").unwrap();
    let extraction = ws
        .extract_variable(Path::new("app.py"), at..at + "fetch_account()".len(), None)
        .unwrap();
    assert_eq!(
        &extraction.suggested_names[..2],
        ["user_profile", "account"]
    );
    let updated = apply_text_edits(text, &extraction.edits).unwrap();
    assert!(updated.contains("\tuser_profile = fetch_account()\r\n\tprofile = user_profile\r\n"));
    assert!(updated.contains("label = 'é'\r\n"));
    assert_eq!(
        updated.matches('\n').count(),
        updated.matches("\r\n").count()
    );
    assert!(
        ws.extract_variable(
            Path::new("app.py"),
            at..at + "fetch_account()".len(),
            Some("profile")
        )
        .is_err()
    );
}

#[test]
fn extraction_rejects_evaluation_order_changes_and_invalid_edit_boundaries() {
    let text = "def run() -> int:\n    return first() + second()\n";
    let ws = workspace(&[("app.py", text)]);
    let at = text.find("second()").unwrap();
    assert!(
        ws.extract_variable(Path::new("app.py"), at..at + "second()".len(), None)
            .is_err()
    );
    assert!(
        apply_text_edits(
            "é",
            &[TextEdit {
                range: 1..2,
                replacement: "x".to_owned()
            }]
        )
        .is_err()
    );
    assert!(
        apply_text_edits(
            "abcdef",
            &[
                TextEdit {
                    range: 0..4,
                    replacement: String::new()
                },
                TextEdit {
                    range: 3..5,
                    replacement: String::new()
                }
            ]
        )
        .is_err()
    );
}

#[test]
fn member_completion_uses_receiver_annotation_and_not_constructor_guessing() {
    let api = "class Service:\n    def charge(self, amount: int) -> int:\n        return amount\n";
    let app = "from api import Service\ndef run(service: Service) -> None:\n    service.cha\n";
    let ws = workspace(&[("api.py", api), ("app.py", app)]);
    let names = ws.completions(Path::new("app.py"), app.rfind("cha").unwrap() + 3);
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(names[0].name, "charge");
    let app = "from api import Service\ndef run(service) -> None:\n    service.cha\n";
    let ws = workspace(&[("api.py", api), ("app.py", app)]);
    assert!(
        ws.completions(Path::new("app.py"), app.rfind("cha").unwrap() + 3)
            .is_empty()
    );
}

#[test]
fn relative_import_and_ambiguous_duplicate_definitions_are_conservative() {
    let api = "def load() -> int:\n    return 1\n";
    let app = "from .api import load\ndef main() -> int:\n    return load()\n";
    let ws = workspace(&[("pkg/api.py", api), ("pkg/app.py", app)]);
    assert_eq!(
        ws.call_hierarchy(Path::new("pkg/api.py"), api.find("load").unwrap())
            .unwrap()
            .callers
            .len(),
        1
    );
    let ambiguous = "def load() -> int:\n    return 1\ndef load() -> int:\n    return 2\n";
    let ws = workspace(&[("pkg/api.py", ambiguous), ("pkg/app.py", app)]);
    assert!(
        ws.call_hierarchy(Path::new("pkg/api.py"), ambiguous.find("load").unwrap())
            .unwrap()
            .callers
            .is_empty()
    );
}

#[test]
fn local_binding_forms_never_resolve_to_an_unrelated_global_function() {
    let text = "def load() -> int:\n    return 1\n\ndef context(manager: object) -> int:\n    with manager as load:\n        return load()\n\ndef excepted() -> int:\n    try:\n        pass\n    except Exception as load:\n        return load()\n\ndef assigned(other: object) -> int:\n    if load := other:\n        return load()\n\ndef augmented() -> int:\n    load += 1\n    return load()\n\ndef matched(value: object) -> int:\n    match value:\n        case load:\n            return load()\n";
    let ws = workspace(&[("app.py", text)]);
    assert!(
        ws.call_hierarchy(Path::new("app.py"), text.find("load").unwrap())
            .unwrap()
            .callers
            .is_empty()
    );
}

#[test]
fn reassignments_share_occurrences_and_staticmethod_self_is_not_implicitly_typed() {
    let text = "def counter(value: int) -> int:\n    value = value + 1\n    return value\n\nclass Service:\n    @staticmethod\n    def invalid(self) -> int:\n        return 1\n";
    let ws = workspace(&[("app.py", text)]);
    assert_eq!(
        ws.occurrences(Path::new("app.py"), text.find("value").unwrap())
            .len(),
        4
    );
    assert!(
        ws.call_hierarchy(Path::new("app.py"), text.find("invalid").unwrap())
            .is_none()
    );
}

#[test]
fn imports_observed_in_other_sources_include_external_libraries_and_aliases() {
    let observed =
        "from django.db import models\nfrom decimal import Decimal as Money\nimport numpy as np\n";
    for (missing, statement) in [
        ("models", "from django.db import models"),
        ("Money", "from decimal import Decimal as Money"),
        ("np", "import numpy as np"),
        ("Path", "from pathlib import Path"),
    ] {
        let source = format!("value = {missing}\n");
        let ws = workspace(&[("other.py", observed), ("app.py", &source)]);
        let fixes = ws.quick_fixes(Path::new("app.py"), source.find(missing).unwrap());
        let fix = fixes
            .iter()
            .find(|fix| {
                fix.edits
                    .iter()
                    .any(|edit| edit.replacement.contains(statement))
            })
            .unwrap_or_else(|| panic!("Missing {statement}: {fixes:?}"));
        assert!(
            apply_text_edits(&source, &fix.edits)
                .unwrap()
                .starts_with(statement)
        );
    }
}

#[test]
fn created_function_does_not_steal_an_existing_decorator() {
    let text = "@decorator\ndef run() -> None:\n    missing()\n";
    let ws = workspace(&[("app.py", text)]);
    let fixes = ws.quick_fixes(Path::new("app.py"), text.find("missing").unwrap());
    let fix = fixes
        .iter()
        .find(|fix| fix.label.starts_with("Créer la fonction"))
        .unwrap();
    let updated = apply_text_edits(text, &fix.edits).unwrap();
    assert!(updated.starts_with("def missing("));
    assert!(updated.contains("@decorator\ndef run() -> None:"));
}

#[test]
fn ordinary_helper_parameters_are_not_presented_as_fixtures() {
    let text = "def helper(value: int) -> int:\n    return value\n";
    let ws = workspace(&[("app.py", text)]);
    assert!(
        ws.fixtures(Path::new("app.py"), text.find("helper").unwrap())
            .is_empty()
    );
}

#[test]
fn completion_replaces_the_whole_existing_identifier_when_caret_is_in_its_middle() {
    let text = "def run(customer: str) -> str:\n    return custmer\n";
    let ws = workspace(&[("app.py", text)]);
    let at = text.find("custmer").unwrap() + 4;
    let choice = ws
        .completions(Path::new("app.py"), at)
        .into_iter()
        .find(|entry| entry.name == "customer")
        .unwrap();
    assert_eq!(&text[choice.replace], "custmer");
}
