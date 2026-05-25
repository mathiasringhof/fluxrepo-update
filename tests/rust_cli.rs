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
fn inventory_missing_repo_root_prints_help() {
    let (code, _, stderr) = run_cli(
        &["fluxrepo-update", "inventory"],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
    assert!(stderr.contains("Usage:"));
    assert!(stderr.contains("inventory"));
    assert!(stderr.contains("REPO_ROOT"));
}

#[test]
fn inventory_help_lists_stable_contract() {
    let (code, stdout, stderr) = run_cli(
        &["fluxrepo-update", "inventory", "--help"],
        "",
        &StaticResolverFactory::default(),
    );
    let help = format!("{stdout}{stderr}");

    assert_eq!(code, 0);
    assert!(help.contains("inventory"));
    assert!(help.contains("REPO_ROOT"));
    assert!(help.contains("--json"));
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
    assert!(payload["skipped"][0].get("path").is_some());
    assert!(payload["skipped"][0].get("reason").is_some());
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
fn update_helm_missing_repo_root_prints_help() {
    let (code, _, stderr) = run_cli(
        &["fluxrepo-update", "update-helm"],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
    assert!(stderr.contains("Usage:"));
    assert!(stderr.contains("update-helm"));
    assert!(stderr.contains("REPO_ROOT"));
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
    assert!(help.contains("--best-effort"));
    assert!(help.contains("--non-interactive"));
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
