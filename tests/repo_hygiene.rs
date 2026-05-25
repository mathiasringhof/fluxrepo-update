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
fn deprecated_serde_yaml_dependency_is_removed() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();

    assert!(
        !manifest.contains("serde_yaml"),
        "Cargo.toml should use a maintained YAML serde crate"
    );
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
