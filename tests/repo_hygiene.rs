use std::fs;
use std::path::Path;

#[test]
fn legacy_python_project_files_are_removed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let forbidden_paths = [
        "pyproject.toml",
        "uv.lock",
        "src/fluxrepo_update",
        "tests/conftest.py",
        "tests/test_cli.py",
        "tests/test_resolvers.py",
        "tests/test_scanner.py",
        "tests/test_updater.py",
    ];

    let present_paths = forbidden_paths
        .into_iter()
        .filter(|path| root.join(path).exists())
        .collect::<Vec<_>>();

    assert!(
        present_paths.is_empty(),
        "legacy Python files remain: {}",
        present_paths.join(", ")
    );
}

#[test]
fn readme_no_longer_points_to_legacy_python_parity_tests() {
    let readme =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();

    for stale_text in [
        "Python `>=3.12`",
        "legacy parity tests",
        "original Python implementation",
        "uv run pytest",
    ] {
        assert!(
            !readme.contains(stale_text),
            "README still references removed Python support: {stale_text}"
        );
    }
}

#[test]
fn rust_tooling_is_pinned_and_lints_are_enforced() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let toolchain = fs::read_to_string(root.join("rust-toolchain.toml")).unwrap();
    let cargo_toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();

    assert!(toolchain.contains(r#"channel = "1.95""#));
    assert!(toolchain.contains(r#"components = ["rustfmt", "clippy"]"#));
    assert!(cargo_toml.contains("[lints.rust]"));
    assert!(cargo_toml.contains(r#"unsafe_code = "forbid""#));
    assert!(cargo_toml.contains("[lints.clippy]"));
    assert!(cargo_toml.contains(r#"all = { level = "warn", priority = -1 }"#));
    assert!(cargo_toml.contains(r#"dbg_macro = "deny""#));
    assert!(cargo_toml.contains(r#"todo = "deny""#));
    assert!(cargo_toml.contains(r#"unimplemented = "deny""#));
}

#[test]
fn ci_runs_the_required_rust_quality_gates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap();

    for expected in [
        "cargo fmt --all --check",
        "cargo clippy --all-targets --locked -- -D warnings",
        "cargo test --locked",
    ] {
        assert!(
            workflow.contains(expected),
            "CI workflow is missing required command: {expected}"
        );
    }
}

#[test]
fn readme_uses_relative_docs_links() {
    let readme =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();

    assert!(!readme.contains("/Users/"));
    for expected in [
        "[Usage](docs/usage.md)",
        "[Output](docs/output.md)",
        "[Coverage](docs/coverage.md)",
        "[Docs Index](docs/README.md)",
    ] {
        assert!(
            readme.contains(expected),
            "README is missing relative docs link: {expected}"
        );
    }
}

#[test]
fn deprecated_serde_yaml_dependency_is_removed() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();

    assert!(
        !manifest.contains("serde_yaml"),
        "Cargo.toml should use a maintained YAML serde crate"
    );
}

#[test]
fn unused_cli_test_harness_dependencies_are_not_declared() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();

    for dependency in ["assert_cmd", "predicates"] {
        assert!(
            !manifest.contains(dependency),
            "Cargo.toml should not declare unused CLI test dependency: {dependency}"
        );
    }
}

#[test]
fn yaml_dependency_is_imported_directly_without_local_wrapper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let wrapper_path = root.join("src/yaml.rs");
    let source_files = [
        "src/lib.rs",
        "src/scanner.rs",
        "src/resolvers.rs",
        "src/updater.rs",
    ];
    let wrapper_imports = source_files
        .into_iter()
        .filter(|path| {
            fs::read_to_string(root.join(path))
                .expect("read source file")
                .contains("crate::yaml")
        })
        .collect::<Vec<_>>();

    assert!(
        !wrapper_path.exists(),
        "YAML serde dependency should be imported directly"
    );
    assert!(
        wrapper_imports.is_empty(),
        "source files still import the local YAML wrapper: {}",
        wrapper_imports.join(", ")
    );
}

#[test]
fn parallel_update_planning_uses_rayon_instead_of_hand_rolled_scheduler() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let updater = fs::read_to_string(root.join("src/updater.rs")).unwrap();

    assert!(
        cargo_toml.contains("\nrayon = "),
        "Cargo.toml should declare rayon for parallel update planning"
    );
    assert!(
        updater.contains("rayon::ThreadPoolBuilder"),
        "update planning should build a bounded Rayon thread pool"
    );
    for stale_scheduler_detail in ["AtomicUsize", "thread::scope"] {
        assert!(
            !updater.contains(stale_scheduler_detail),
            "update planning should not keep hand-rolled scheduler detail: {stale_scheduler_detail}"
        );
    }
}
