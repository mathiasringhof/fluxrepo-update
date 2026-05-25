mod common;

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;

use common::{StaticResolverFactory, copy_fixture, fixture_root};
use fluxrepo_update::cli::run_with_args;
use fluxrepo_update::updater::PlanOptions;
use serde_json::Value;

#[test]
fn inventory_json_matches_fixture_contract() {
    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "inventory",
            fixture_root().to_str().expect("fixture path"),
            "--json",
        ],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 0);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["repository_count"], 1);
    assert!(payload["chart_target_count"].as_u64().expect("chart count") >= 2);
    assert!(
        payload["deployment_target_count"]
            .as_u64()
            .expect("deployment count")
            >= 2
    );
    assert!(
        payload["image_reference_count"]
            .as_u64()
            .expect("image count")
            >= 4
    );
    assert_eq!(
        payload["skipped_paths"][0],
        "clusters/production/flux-system/gotk-sync.yaml"
    );
}

#[test]
fn inventory_human_output_prints_summary_counts() {
    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "inventory",
            fixture_root().to_str().expect("fixture path"),
        ],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 0);
    assert!(stdout.contains("Repositories:"));
    assert!(stdout.contains("Chart targets:"));
    assert!(stdout.contains("Deployment targets:"));
    assert!(stdout.contains("Unresolved chart targets:"));
}

#[test]
fn inventory_missing_repo_root_returns_parse_error() {
    let (code, _, _) = run_cli(
        &["fluxrepo-update", "inventory"],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
}

#[test]
fn inventory_json_runtime_error_is_structured() {
    let (code, stdout, stderr) = run_cli(
        &["fluxrepo-update", "inventory", "/missing/path", "--json"],
        "",
        &StaticResolverFactory::default(),
    );
    let output = json_error_output(&stdout, &stderr);
    let payload: Value = serde_json::from_str(output).expect("json error output");

    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(payload["error"], "runtime_error");
    assert_eq!(payload["exit_code"], 2);
    assert!(
        payload["message"]
            .as_str()
            .expect("message")
            .contains("/missing/path")
    );
}

#[test]
fn update_helm_json_dry_run_returns_agent_friendly_payload() {
    let factory = paperless_update_factory();

    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--json",
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 10);
    assert_eq!(stderr, "");
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["mode"], "plan");
    assert_eq!(payload["strict"], false);
    assert_eq!(payload["non_interactive"], true);
    assert_eq!(payload["summary"]["applied_count"], 0);
    assert!(payload["summary"]["planned_count"].as_u64().unwrap() >= 1);
    assert!(payload["summary"]["skipped_count"].as_u64().unwrap() >= 1);
    for skipped in payload["skipped"].as_array().expect("skipped array") {
        assert!(skipped.get("path").is_some());
        assert!(skipped.get("reason").is_some());
        assert!(skipped.get("reason_code").is_some());
        assert!(skipped.get("retryable").and_then(Value::as_bool).is_some());
        assert!(skipped.get("source_url").is_some());
    }
    assert!(payload["planned"].as_array().unwrap().iter().any(|item| {
        item["path"] == "apps/production/paperless/release-patch.yaml"
            && item["inherited_source"] == true
            && item["yaml_path"] == "spec.chart.spec.version"
            && item["latest_version"] == "12.1.0"
    }));
    assert!(payload["planned"].as_array().unwrap().iter().any(|item| {
        item["path"] == "apps/base/sonarr/deployment.yaml"
            && item["target_kind"] == "Deployment"
            && item["yaml_path"] == "spec.template.spec.containers[0].image"
            && item["latest_image"] == "linuxserver/sonarr:version-4.0.17.3000"
    }));
}

#[test]
fn update_helm_json_write_returns_applied_exit_code() {
    let (_temp, repo_root) = copy_fixture();
    let factory = paperless_update_factory();

    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
            "--json",
            "--write",
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 20);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["mode"], "apply");
    assert!(payload["summary"]["applied_count"].as_u64().unwrap() >= 1);
    assert!(payload["summary"]["changed_file_count"].as_u64().unwrap() >= 1);
    assert!(
        fs::read_to_string(repo_root.join("apps/production/paperless/release-patch.yaml"))
            .expect("read patch")
            .contains("12.1.0")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/sonarr/deployment.yaml"))
            .expect("read sonarr")
            .contains("linuxserver/sonarr:version-4.0.17.3000")
    );
}

#[test]
fn update_helm_json_plan_includes_stable_apply_ids() {
    let factory = paperless_update_factory();

    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--json",
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 10);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    let planned = payload["planned"].as_array().expect("planned array");
    assert!(!planned.is_empty());
    for item in planned {
        assert!(
            item["id"].as_str().is_some_and(|id| id.starts_with("v1:")),
            "planned item should include a stable v1 apply id: {item}"
        );
    }
}

#[test]
fn update_helm_json_plan_apply_ids_are_unique() {
    let factory = paperless_update_factory();

    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--json",
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 10);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    let mut ids = std::collections::BTreeSet::new();
    for item in payload["planned"].as_array().expect("planned array") {
        let id = item["id"].as_str().expect("planned item id");
        assert!(ids.insert(id.to_string()), "duplicate apply id: {id}");
    }
}

#[test]
fn update_helm_non_interactive_write_applies_selected_apply_id_only() {
    let (_temp, repo_root) = copy_fixture();
    let factory = paperless_update_factory();
    let plan = json_plan(&repo_root, &factory);
    let sonarr_id = planned_id_for_path(&plan, "apps/base/sonarr/deployment.yaml");
    let args = vec![
        "fluxrepo-update".to_string(),
        "update-helm".to_string(),
        repo_root.to_str().expect("repo path").to_string(),
        "--json".to_string(),
        "--write".to_string(),
        "--non-interactive".to_string(),
        "--apply-id".to_string(),
        sonarr_id,
    ];

    let (code, stdout, _) = run_cli_owned(args, "", &factory);

    assert_eq!(code, 20);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["mode"], "apply");
    assert_eq!(payload["summary"]["applied_count"], 1);
    assert_eq!(
        payload["planned"].as_array().expect("planned array").len(),
        1
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/sonarr/deployment.yaml"))
            .expect("read sonarr")
            .contains("linuxserver/sonarr:version-4.0.17.3000")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read base")
            .contains("12.0.0")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/production/paperless/release-patch.yaml"))
            .expect("read patch")
            .contains("11.29.10")
    );
}

#[test]
fn update_helm_non_interactive_write_applies_multiple_apply_ids() {
    let (_temp, repo_root) = copy_fixture();
    let factory = paperless_update_factory();
    let plan = json_plan(&repo_root, &factory);
    let sonarr_id = planned_id_for_path(&plan, "apps/base/sonarr/deployment.yaml");
    let paperless_id = planned_id_for_path(&plan, "apps/base/paperless-ngx/release.yaml");
    let args = vec![
        "fluxrepo-update".to_string(),
        "update-helm".to_string(),
        repo_root.to_str().expect("repo path").to_string(),
        "--json".to_string(),
        "--write".to_string(),
        "--non-interactive".to_string(),
        "--apply-id".to_string(),
        sonarr_id,
        "--apply-id".to_string(),
        paperless_id,
    ];

    let (code, stdout, _) = run_cli_owned(args, "", &factory);

    assert_eq!(code, 20);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["mode"], "apply");
    assert_eq!(payload["summary"]["applied_count"], 2);
    assert_eq!(
        payload["planned"].as_array().expect("planned array").len(),
        2
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/sonarr/deployment.yaml"))
            .expect("read sonarr")
            .contains("linuxserver/sonarr:version-4.0.17.3000")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read base")
            .contains("12.1.0")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/production/paperless/release-patch.yaml"))
            .expect("read patch")
            .contains("11.29.10")
    );
}

#[test]
fn update_helm_non_interactive_write_rejects_unknown_apply_id_without_writing() {
    let (_temp, repo_root) = copy_fixture();
    let factory = paperless_update_factory();
    let args = vec![
        "fluxrepo-update".to_string(),
        "update-helm".to_string(),
        repo_root.to_str().expect("repo path").to_string(),
        "--json".to_string(),
        "--write".to_string(),
        "--non-interactive".to_string(),
        "--apply-id".to_string(),
        "v1:missing".to_string(),
    ];

    let (code, stdout, stderr) = run_cli_owned(args, "", &factory);
    let output = json_error_output(&stdout, &stderr);
    let payload: Value = serde_json::from_str(output).expect("json error output");

    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(payload["error"], "invalid_arguments");
    assert_eq!(payload["exit_code"], 2);
    assert!(
        payload["message"]
            .as_str()
            .expect("message")
            .contains("Unknown apply id: v1:missing")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/sonarr/deployment.yaml"))
            .expect("read sonarr")
            .contains("linuxserver/sonarr:version-4.0.16.2944")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/production/paperless/release-patch.yaml"))
            .expect("read patch")
            .contains("11.29.10")
    );
}

#[test]
fn update_helm_non_interactive_write_rejects_mixed_apply_ids_without_writing() {
    let (_temp, repo_root) = copy_fixture();
    let factory = paperless_update_factory();
    let plan = json_plan(&repo_root, &factory);
    let sonarr_id = planned_id_for_path(&plan, "apps/base/sonarr/deployment.yaml");
    let args = vec![
        "fluxrepo-update".to_string(),
        "update-helm".to_string(),
        repo_root.to_str().expect("repo path").to_string(),
        "--json".to_string(),
        "--write".to_string(),
        "--non-interactive".to_string(),
        "--apply-id".to_string(),
        sonarr_id,
        "--apply-id".to_string(),
        "v1:missing".to_string(),
    ];

    let (code, stdout, stderr) = run_cli_owned(args, "", &factory);
    let output = json_error_output(&stdout, &stderr);
    let payload: Value = serde_json::from_str(output).expect("json error output");

    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(payload["error"], "invalid_arguments");
    assert_eq!(payload["exit_code"], 2);
    assert!(
        payload["message"]
            .as_str()
            .expect("message")
            .contains("Unknown apply id: v1:missing")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/sonarr/deployment.yaml"))
            .expect("read sonarr")
            .contains("linuxserver/sonarr:version-4.0.16.2944")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read base")
            .contains("12.0.0")
    );
}

#[test]
fn update_helm_non_interactive_write_rejects_stale_apply_id_when_no_updates_are_planned() {
    let (_temp, repo_root) = copy_fixture();
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "11.29.10".to_string(),
        )]),
        HashMap::new(),
    );
    let args = vec![
        "fluxrepo-update".to_string(),
        "update-helm".to_string(),
        repo_root.to_str().expect("repo path").to_string(),
        "--json".to_string(),
        "--write".to_string(),
        "--non-interactive".to_string(),
        "--apply-id".to_string(),
        "v1:stale".to_string(),
    ];

    let (code, stdout, stderr) = run_cli_owned(args, "", &factory);
    let output = json_error_output(&stdout, &stderr);
    let payload: Value = serde_json::from_str(output).expect("json error output");

    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(payload["error"], "invalid_arguments");
    assert_eq!(payload["exit_code"], 2);
    assert!(
        payload["message"]
            .as_str()
            .expect("message")
            .contains("Unknown apply id: v1:stale")
    );
}

#[test]
fn update_helm_best_effort_flag_is_not_supported() {
    let (code, _, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--best-effort",
            "--non-interactive",
        ],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
}

#[test]
fn update_helm_apply_id_requires_write() {
    let (_temp, repo_root) = copy_fixture();
    let factory = paperless_update_factory();
    let plan = json_plan(&repo_root, &factory);
    let sonarr_id = planned_id_for_path(&plan, "apps/base/sonarr/deployment.yaml");
    let args = vec![
        "fluxrepo-update".to_string(),
        "update-helm".to_string(),
        repo_root.to_str().expect("repo path").to_string(),
        "--json".to_string(),
        "--non-interactive".to_string(),
        "--apply-id".to_string(),
        sonarr_id,
    ];

    let (code, stdout, stderr) = run_cli_owned(args, "", &factory);
    let output = json_error_output(&stdout, &stderr);
    let payload: Value = serde_json::from_str(output).expect("json error output");

    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(payload["error"], "invalid_arguments");
    assert_eq!(payload["exit_code"], 2);
    assert!(
        payload["message"]
            .as_str()
            .expect("message")
            .contains("--apply-id requires --write")
    );
}

#[test]
fn update_helm_interactive_mode_applies_selected_updates_only() {
    let (_temp, repo_root) = copy_fixture();
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::new(),
    );

    let (code, _, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
        ],
        "y\nn\n",
        &factory,
    );

    assert_eq!(code, 20);
    assert!(
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read base")
            .contains("12.1.0")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/production/paperless/release-patch.yaml"))
            .expect("read patch")
            .contains("11.29.10")
    );
}

#[test]
fn update_helm_interactive_prompt_includes_update_details() {
    let (_temp, repo_root) = copy_fixture();
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::new(),
    );

    let (code, _, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
        ],
        "n\nn\n",
        &factory,
    );

    assert_eq!(code, 0);
    assert!(
        stderr.contains(
            "Update apps/base/paperless-ngx/release.yaml (chart 12.0.0 -> 12.1.0)? [y/N]"
        )
    );
}

#[test]
fn update_helm_interactive_mode_defaults_empty_answer_to_no() {
    let (_temp, repo_root) = copy_fixture();
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::new(),
    );

    let (code, _, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
        ],
        "\n\n",
        &factory,
    );

    assert_eq!(code, 0);
    assert!(stderr.contains("No updates were approved."));
    assert!(
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read base")
            .contains("12.0.0")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/production/paperless/release-patch.yaml"))
            .expect("read patch")
            .contains("11.29.10")
    );
}

#[test]
fn update_helm_non_interactive_plans_without_writing() {
    let (_temp, repo_root) = copy_fixture();
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::new(),
    );

    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
            "--json",
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 10);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["mode"], "plan");
    assert_eq!(payload["summary"]["applied_count"], 0);
    assert!(
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read base")
            .contains("12.0.0")
    );
}

#[test]
fn update_helm_write_requires_non_interactive() {
    let (code, _, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--write",
        ],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
    assert!(stderr.contains("--non-interactive"));
}

#[test]
fn update_helm_json_write_requires_non_interactive_error_is_structured() {
    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--write",
            "--json",
        ],
        "",
        &StaticResolverFactory::default(),
    );
    let output = json_error_output(&stdout, &stderr);
    let payload: Value = serde_json::from_str(output).expect("json error output");

    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(payload["error"], "invalid_arguments");
    assert_eq!(payload["exit_code"], 2);
    assert!(
        payload["message"]
            .as_str()
            .expect("message")
            .contains("--non-interactive")
    );
}

#[test]
fn update_helm_missing_repo_root_returns_parse_error() {
    let (code, _, _) = run_cli(
        &["fluxrepo-update", "update-helm"],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
}

#[test]
fn update_helm_help_lists_stable_contract() {
    let (code, stdout, stderr) = run_cli(
        &["fluxrepo-update", "update-helm", "--help"],
        "",
        &StaticResolverFactory::default(),
    );
    let help = format!("{stdout}{stderr}");

    assert_eq!(code, 0);
    assert!(help.contains("update-helm"));
    assert!(help.contains("REPO_ROOT"));
    assert!(help.contains("--json"));
    assert!(help.contains("--write"));
    assert!(help.contains("--strict"));
    assert!(help.contains("--non-interactive"));
    assert!(help.contains("--apply-id"));
}

#[test]
fn update_helm_strict_fails_on_skipped_resolution() {
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::new(),
    );

    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--json",
            "--strict",
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 2);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["strict"], true);
    assert!(payload["summary"]["skipped_count"].as_u64().unwrap() >= 1);
    assert!(payload["summary"]["planned_count"].as_u64().unwrap() >= 1);
}

#[test]
fn update_helm_returns_zero_when_no_updates_needed() {
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "11.29.10".to_string(),
        )]),
        HashMap::new(),
    );

    let (code, stdout, _) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--json",
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 0);
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["summary"]["planned_count"], 0);
    assert_eq!(payload["summary"]["applied_count"], 0);
}

#[test]
fn update_helm_non_json_plan_output_includes_skip_reasons_and_plan_hint() {
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::new(),
    );

    let (code, _, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 10);
    assert!(stderr.contains("skip: "));
    assert!(stderr.contains("No static version configured for truecharts/audiobookshelf"));
    assert!(stderr.contains("Plan only. Re-run without --non-interactive"));
}

#[test]
fn update_helm_non_json_plan_output_includes_target_context() {
    let factory = paperless_update_factory();

    let (code, _, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--non-interactive",
        ],
        "",
        &factory,
    );

    assert_eq!(code, 10);
    assert!(stderr.contains(
        "apps/base/sonarr/deployment.yaml: Deployment sonarr-deployment spec.template.spec.containers[0].image version-4.0.16.2944 -> version-4.0.17.3000"
    ));
    assert!(stderr.contains(
        "apps/production/paperless/release-patch.yaml: HelmRelease paperless-ngx paperless-ngx 11.29.10 -> 12.1.0 inherited-source"
    ));
}

fn paperless_update_factory() -> StaticResolverFactory {
    StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::from([(
            "linuxserver/sonarr:version-4.0.16.2944".to_string(),
            "linuxserver/sonarr:version-4.0.17.3000".to_string(),
        )]),
    )
}

fn run_cli(args: &[&str], input: &str, factory: &StaticResolverFactory) -> (u8, String, String) {
    run_cli_with_options(args, input, factory, PlanOptions { max_workers: 1 })
}

fn run_cli_owned(
    args: Vec<String>,
    input: &str,
    factory: &StaticResolverFactory,
) -> (u8, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_with_args(
        args,
        Cursor::new(input.as_bytes()),
        &mut stdout,
        &mut stderr,
        factory,
        PlanOptions { max_workers: 1 },
    )
    .expect("run cli");
    (
        code,
        String::from_utf8(stdout).expect("utf8 stdout"),
        String::from_utf8(stderr).expect("utf8 stderr"),
    )
}

fn run_cli_with_options(
    args: &[&str],
    input: &str,
    factory: &StaticResolverFactory,
    plan_options: PlanOptions,
) -> (u8, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_with_args(
        args,
        Cursor::new(input.as_bytes()),
        &mut stdout,
        &mut stderr,
        factory,
        plan_options,
    )
    .expect("run cli");
    (
        code,
        String::from_utf8(stdout).expect("utf8 stdout"),
        String::from_utf8(stderr).expect("utf8 stderr"),
    )
}

fn json_error_output<'a>(stdout: &'a str, stderr: &'a str) -> &'a str {
    if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    }
}

fn json_plan(repo_root: &std::path::Path, factory: &StaticResolverFactory) -> Value {
    let args = vec![
        "fluxrepo-update".to_string(),
        "update-helm".to_string(),
        repo_root.to_str().expect("repo path").to_string(),
        "--json".to_string(),
        "--non-interactive".to_string(),
    ];
    let (code, stdout, _) = run_cli_owned(args, "", factory);
    assert_eq!(code, 10);
    serde_json::from_str(&stdout).expect("json output")
}

fn planned_id_for_path(payload: &Value, path: &str) -> String {
    payload["planned"]
        .as_array()
        .expect("planned array")
        .iter()
        .find(|item| item["path"] == path)
        .and_then(|item| item["id"].as_str())
        .expect("planned item id")
        .to_string()
}
