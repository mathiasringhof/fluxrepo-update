use std::fs;

#[test]
fn pre_commit_hook_runs_ci_quality_gates() {
    let hook = fs::read_to_string(".githooks/pre-commit").expect("pre-commit hook exists");
    let fmt = hook.find("cargo fmt --all --check").expect("fmt check");
    let clippy = hook
        .find("cargo clippy --all-targets --locked -- -D warnings")
        .expect("clippy check");
    let test = hook.find("cargo test --locked").expect("test check");

    assert!(fmt < clippy);
    assert!(clippy < test);
}
