mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::{ResponseSpec, TestHttpServer, write_file};
use serde_json::{Value, json};

#[test]
fn fully_current_charts_and_images_report_successful_checks() {
    let repo = tempfile::tempdir().expect("repo");
    let chart = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        "entries:\n  demo:\n    - version: '1.0.0'\n",
    )]);
    let registry = TestHttpServer::new(vec![ResponseSpec::new(200, r#"{"tags":["1.0.0"]}"#)]);
    write_chart(repo.path(), "demo", &chart.base_url);
    write_image(repo.path(), "demo", &registry.base_url, "1.0.0");

    let output = run_check(repo.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
    assert_eq!(report["check_completeness"], "complete");
    assert_eq!(
        report["summary"],
        json!({
            "planned_count": 0, "applied_count": 0, "skipped_count": 0,
            "changed_file_count": 0, "checked_unchanged_count": 2,
            "update_available_count": 0, "unsupported_count": 0, "failed_count": 0,
            "discovered_count": 2, "checked_count": 2, "unchecked_count": 0,
        })
    );
    chart.finish();
    registry.finish();
}

#[test]
fn fully_checked_updates_report_complete_in_plan_and_apply() {
    let repo = tempfile::tempdir().expect("repo");
    let registry = TestHttpServer::new(
        (0..2)
            .map(|_| ResponseSpec::new(200, r#"{"tags":["1.0.0","1.1.0"]}"#))
            .collect(),
    );
    write_image(repo.path(), "update", &registry.base_url, "1.0.0");

    for (args, exit_code, applied_count) in [(vec![], 10, 0), (vec!["--write"], 20, 1)] {
        let output = run_check(repo.path(), &args);
        assert_eq!(output.status.code(), Some(exit_code));
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
        assert_eq!(report["check_completeness"], "complete");
        assert_eq!(report["summary"]["planned_count"], 1);
        assert_eq!(report["summary"]["applied_count"], applied_count);
        assert_eq!(report["summary"]["changed_file_count"], applied_count);
        assert_check_counts(&report, 0, 1, 0, 0);
    }
    assert!(
        std::fs::read_to_string(repo.path().join("update-pod.yaml"))
            .expect("updated pod")
            .contains("/update/app:1.1.0")
    );
    assert_eq!(registry.finish().len(), 2);
}

#[test]
fn all_failed_requests_return_failure_with_the_full_report() {
    for args in [
        vec!["--non-interactive"],
        vec!["--non-interactive", "--write"],
        vec![],
    ] {
        let repo = tempfile::tempdir().expect("repo");
        let chart = TestHttpServer::new(vec![ResponseSpec::new(503, "unavailable")]);
        let registry = TestHttpServer::new(vec![ResponseSpec::new(403, "forbidden")]);
        write_chart(repo.path(), "demo", &chart.base_url);
        write_image(repo.path(), "demo", &registry.base_url, "1.0.0");
        write_image(repo.path(), "mutable", &registry.base_url, "latest");

        let output = Command::new(env!("CARGO_BIN_EXE_fluxrepo-update"))
            .arg("update-helm")
            .arg(repo.path())
            .arg("--json")
            .args(args)
            .output()
            .expect("run CLI");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
        assert_eq!(report["check_completeness"], "failed");
        assert_check_counts(&report, 0, 0, 1, 2);
        assert_eq!(report["summary"]["changed_file_count"], 0);
        assert_eq!(report["planned"], json!([]));
        let skips = report["skipped"].as_array().expect("skips");
        assert_eq!(skips.len(), 3);
        assert!(skips.iter().any(|skip| {
            skip["reason_code"] == "chart_request_failed" && skip["retryable"] == true
        }));
        assert!(skips.iter().any(|skip| {
            skip["reason_code"] == "registry_request_failed" && skip["retryable"] == false
        }));
        assert_eq!(chart.finish().len(), 1);
        assert_eq!(registry.finish().len(), 1);
    }
}

#[test]
fn unsupported_only_reports_nothing_checked_without_failing() {
    let repo = tempfile::tempdir().expect("repo");
    write_image(repo.path(), "mutable", "http://127.0.0.1:1", "latest");
    write_file(
        &repo.path().join("missing-source.yaml"),
        "kind: HelmRelease\nmetadata: {name: missing-source}\nspec:\n  chart:\n    spec:\n      chart: demo\n      version: '1.0.0'\n      sourceRef: {kind: HelmRepository, name: absent}\n",
    );
    write_file(
        &repo.path().join("missing-identity.yaml"),
        "kind: HelmRelease\nmetadata: {name: missing-identity}\nspec:\n  chart:\n    spec:\n      version: '1.0.0'\n",
    );
    write_file(
        &repo.path().join("unsupported.yaml"),
        "kind: HelmRelease\nmetadata: {name: override}\nspec: {values: {image: {tag: v1.0.0}}}\n---\napiVersion: postgresql.cnpg.io/v1\nkind: Cluster\nmetadata: {name: database}\nspec: {imageName: 'example/database:1.0.0'}\n---\nkind: Kustomization\nresources:\n- https://example.org/operator/v1.0.0/install.yaml\n- github.com/example/operator/deploy?ref=main\n",
    );

    let output = run_check(repo.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
    assert_eq!(report["check_completeness"], "not_checked");
    assert_check_counts(&report, 0, 0, 7, 0);
    assert_eq!(
        report["skipped"]
            .as_array()
            .expect("skips")
            .iter()
            .filter(|skip| skip["reason_code"] == "unsupported_version_declaration")
            .count(),
        4
    );

    let human = Command::new(env!("CARGO_BIN_EXE_fluxrepo-update"))
        .arg("update-helm")
        .arg(repo.path())
        .arg("--non-interactive")
        .output()
        .expect("run CLI");
    assert_eq!(human.status.code(), Some(0));
    assert!(human.stdout.is_empty());
    let stderr = String::from_utf8(human.stderr).expect("human output");
    assert!(stderr.contains("Nothing checked"), "{stderr}");
    assert!(stderr.contains("not_checked"), "{stderr}");
    assert!(stderr.contains("7 unsupported"), "{stderr}");
    assert!(!stderr.contains("No updates required"));
}

#[test]
fn mixed_checks_retain_partial_plans_and_full_counts_after_selected_apply() {
    let repo = tempfile::tempdir().expect("repo");
    let chart = TestHttpServer::new(
        (0..3)
            .map(|_| {
                ResponseSpec::new(
                    200,
                    "entries:\n  demo:\n    - version: '1.0.0'\n    - version: '1.1.0'\n",
                )
            })
            .collect(),
    );
    let current_chart = TestHttpServer::new(
        (0..3)
            .map(|_| ResponseSpec::new(200, "entries:\n  demo:\n    - version: '1.0.0'\n"))
            .collect(),
    );
    let registry = TestHttpServer::new(
        (0..3)
            .map(|_| ResponseSpec::new(200, r#"{"tags":["1.0.0","1.1.0"]}"#))
            .collect(),
    );
    let unavailable = TestHttpServer::new(
        (0..3)
            .map(|_| ResponseSpec::new(503, "unavailable"))
            .collect(),
    );
    write_chart(repo.path(), "update", &chart.base_url);
    write_chart(repo.path(), "current", &current_chart.base_url);
    write_image(repo.path(), "update", &registry.base_url, "1.0.0");
    write_image(repo.path(), "failed", &unavailable.base_url, "1.0.0");
    write_image(repo.path(), "mutable", &registry.base_url, "latest");
    let unselected_path = repo.path().join("update-pod.yaml");
    let unselected_original = std::fs::read_to_string(&unselected_path).expect("original pod");

    let output = run_check(repo.path(), &[]);
    assert_eq!(output.status.code(), Some(10));
    let plan: Value = serde_json::from_slice(&output.stdout).expect("plan report");
    assert_eq!(plan["summary"]["planned_count"], 2);
    let chart_id = plan["planned"]
        .as_array()
        .expect("updates")
        .iter()
        .find(|update| update["target_kind"] == "HelmRelease")
        .expect("chart update")["id"]
        .as_str()
        .expect("update ID");

    let output = Command::new(env!("CARGO_BIN_EXE_fluxrepo-update"))
        .arg("update-helm")
        .arg(repo.path())
        .arg("--json")
        .output()
        .expect("run CLI with no approvals");
    assert_eq!(output.status.code(), Some(0));
    let declined: Value = serde_json::from_slice(&output.stdout).expect("declined report");
    assert_eq!(declined["summary"]["planned_count"], 0);
    assert_eq!(declined["summary"]["applied_count"], 0);
    assert_eq!(declined["summary"]["changed_file_count"], 0);
    assert!(
        std::fs::read_to_string(repo.path().join("update-chart.yaml"))
            .expect("declined chart")
            .contains("version: '1.0.0'")
    );

    let output = run_check(repo.path(), &["--write", "--apply-id", chart_id]);
    assert_eq!(output.status.code(), Some(20));
    let applied: Value = serde_json::from_slice(&output.stdout).expect("apply report");
    assert_eq!(applied["summary"]["planned_count"], 1);
    assert_eq!(applied["summary"]["applied_count"], 1);
    assert_eq!(applied["summary"]["changed_file_count"], 1);
    assert_eq!(applied["planned"][0]["id"], chart_id);
    for report in [&plan, &declined, &applied] {
        assert_eq!(report["check_completeness"], "incomplete");
        assert_check_counts(report, 1, 2, 1, 1);
        assert_eq!(report["skipped"].as_array().expect("skips").len(), 2);
    }
    assert_eq!(
        std::fs::read_to_string(unselected_path).expect("unselected pod"),
        unselected_original
    );
    chart.finish();
    current_chart.finish();
    registry.finish();
    unavailable.finish();
}

#[test]
fn successful_unchanged_checks_keep_mixed_failures_best_effort() {
    let repo = tempfile::tempdir().expect("repo");
    let chart = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        "entries:\n  demo:\n    - version: '1.0.0'\n",
    )]);
    let registry = TestHttpServer::new(vec![ResponseSpec::new(503, "unavailable")]);
    write_chart(repo.path(), "current", &chart.base_url);
    write_image(repo.path(), "failed", &registry.base_url, "1.0.0");

    let output = run_check(repo.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
    assert_eq!(report["check_completeness"], "incomplete");
    assert_check_counts(&report, 1, 0, 0, 1);
    chart.finish();
    registry.finish();
}

#[test]
fn unusable_source_results_fail_but_unsupported_versions_remain_unchecked() {
    for (body, current, reason, code, completeness, failed, unsupported) in [
        ("entries: [", "1.0.0", "unclassified", 2, "failed", 1, 0),
        ("{}", "1.0.0", "unclassified", 2, "failed", 1, 0),
        ("entries: {}", "1.0.0", "chart_not_found", 2, "failed", 1, 0),
        (
            "entries:\n  demo:\n    - version: '1.1.0'\n",
            "1.0.0",
            "current_version_not_found",
            2,
            "failed",
            1,
            0,
        ),
        (
            "entries:\n  demo:\n    - version: '0.9.0'\n",
            "1.0.0",
            "current_version_newer_than_source",
            2,
            "failed",
            1,
            0,
        ),
        (
            "entries:\n  demo:\n    - version: 'rolling'\n",
            "rolling",
            "incompatible_version_scheme",
            0,
            "not_checked",
            0,
            1,
        ),
    ] {
        let repo = tempfile::tempdir().expect("repo");
        let chart = TestHttpServer::new(vec![ResponseSpec::new(200, body)]);
        write_chart(repo.path(), "demo", &chart.base_url);
        let path = repo.path().join("demo-chart.yaml");
        let manifest = std::fs::read_to_string(&path).expect("chart manifest");
        write_file(
            &path,
            &manifest.replace("version: '1.0.0'", &format!("version: '{current}'")),
        );

        let output = run_check(repo.path(), &[]);
        assert_eq!(output.status.code(), Some(code), "{reason}");
        let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
        assert_eq!(report["check_completeness"], completeness, "{reason}");
        assert_check_counts(&report, 0, 0, unsupported, failed);
        assert_eq!(report["skipped"][0]["reason_code"], reason);
        assert_eq!(report["skipped"][0]["retryable"], false);
        chart.finish();
    }
}

#[test]
fn empty_inventory_reports_nothing_checked() {
    let repo = tempfile::tempdir().expect("repo");
    let output = run_check(repo.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
    assert_eq!(report["check_completeness"], "not_checked");
    assert_check_counts(&report, 0, 0, 0, 0);

    let human = run_human_check(repo.path());
    assert_eq!(human.status.code(), Some(0));
    let stderr = String::from_utf8(human.stderr).expect("human output");
    assert!(stderr.contains("Nothing checked"), "{stderr}");
    assert!(stderr.contains("not_checked"), "{stderr}");
    assert!(!stderr.contains("No updates required"));
}

#[test]
fn human_total_failure_reports_failed_checks() {
    let repo = tempfile::tempdir().expect("repo");
    let registry = TestHttpServer::new(vec![ResponseSpec::new(503, "unavailable")]);
    write_image(repo.path(), "failed", &registry.base_url, "1.0.0");

    let output = run_human_check(repo.path());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("human output");
    for fact in [
        "Version checks: failed",
        "0 unchanged",
        "0 updates available",
        "0 unsupported",
        "1 failed",
    ] {
        assert!(stderr.contains(fact), "missing {fact}: {stderr}");
    }
    assert!(!stderr.contains("No updates required"));
    registry.finish();
}

fn assert_check_counts(
    report: &Value,
    unchanged: usize,
    available: usize,
    unsupported: usize,
    failed: usize,
) {
    let summary = &report["summary"];
    assert_eq!(summary["checked_unchanged_count"], unchanged);
    assert_eq!(summary["update_available_count"], available);
    assert_eq!(summary["unsupported_count"], unsupported);
    assert_eq!(summary["failed_count"], failed);
    assert_eq!(summary["checked_count"], unchanged + available);
    assert_eq!(summary["unchecked_count"], unsupported + failed);
    assert_eq!(summary["skipped_count"], unsupported + failed);
    assert_eq!(
        summary["discovered_count"],
        unchanged + available + unsupported + failed
    );
}

fn run_human_check(repo: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fluxrepo-update"))
        .arg("update-helm")
        .arg(repo)
        .arg("--non-interactive")
        .output()
        .expect("run human CLI")
}

fn run_check(repo: &Path, extra_args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fluxrepo-update"))
        .arg("update-helm")
        .arg(repo)
        .args(["--json", "--non-interactive"])
        .args(extra_args)
        .output()
        .expect("run CLI")
}

fn write_chart(repo: &Path, name: &str, source_url: &str) {
    write_file(
        &repo.join(format!("{name}-chart.yaml")),
        &format!(
            "apiVersion: source.toolkit.fluxcd.io/v1\nkind: HelmRepository\nmetadata:\n  name: {name}\nspec:\n  url: {source_url}\n---\napiVersion: helm.toolkit.fluxcd.io/v2\nkind: HelmRelease\nmetadata:\n  name: {name}\nspec:\n  chart:\n    spec:\n      chart: demo\n      version: '1.0.0'\n      sourceRef:\n        kind: HelmRepository\n        name: {name}\n"
        ),
    );
}

fn write_image(repo: &Path, name: &str, registry_url: &str, tag: &str) {
    let host = registry_url.strip_prefix("http://").expect("registry URL");
    write_file(
        &repo.join(format!("{name}-pod.yaml")),
        &format!(
            "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {name}\nspec:\n  containers:\n    - name: app\n      image: '{host}/{name}/app:{tag}'\n"
        ),
    );
}
