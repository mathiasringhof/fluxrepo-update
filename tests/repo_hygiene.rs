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
