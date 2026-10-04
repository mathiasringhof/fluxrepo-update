#![allow(clippy::needless_raw_string_hashes)]

mod common;

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Cursor;

use common::{
    ResponseSpec, StaticResolverFactory, TestHttpServer, TestResolvers, copy_fixture, fixture_root,
    write_file,
};
use fluxrepo_update::cli::run_with_args;
use fluxrepo_update::resolvers::{
    ChartVersionResolver, ImageVersionResolver, RepositoryChartResolver, StaticImageVersionResolver,
};
use serde_json::Value;

#[test]
fn version_flag_prints_package_version() {
    let (code, stdout, stderr) = run_cli(
        &["fluxrepo-update", "--version"],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        format!("fluxrepo-update {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(stderr, "");
}

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
    assert!(payload["repositories"].as_array().is_some());
    assert!(payload["chart_targets"].as_array().is_some());
    assert!(payload["image_bindings"].as_array().is_some());
    assert!(
        payload["helmreleases_without_chart_version"]
            .as_array()
            .is_some()
    );
    assert!(payload["unresolved_chart_targets"].as_array().is_some());
    assert!(payload["image_references"].as_array().is_some());
    assert!(payload["chart_target_count"].as_u64().expect("chart count") >= 2);
    assert!(
        payload["image_binding_count"]
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
    assert!(
        payload["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["path"] == "infrastructure/base/sources/truecharts.yaml"
                    && item["document_index"] == 0
                    && item["name"] == "truecharts"
                    && item["repo_type"] == "oci"
                    && item["url"] == "oci://oci.trueforge.org/truecharts"
            })
    );
    assert!(
        payload["chart_targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["path"] == "apps/base/paperless-ngx/release.yaml"
                    && item["document_index"] == 0
                    && item["name"] == "paperless-ngx"
                    && item["namespace"] == "flux-system"
                    && item["chart_name"] == "paperless-ngx"
                    && item["repo_name"] == "truecharts"
                    && item["current_version"] == "12.0.0"
                    && item["source_path"] == "apps/base/paperless-ngx/release.yaml"
                    && item["source_is_inherited"] == false
            })
    );
    assert!(
        payload["unresolved_chart_targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["path"] == "apps/production/paperless/release-patch.yaml"
                    && item["name"] == "paperless-ngx"
                    && item["current_version"] == "11.29.10"
            })
    );
    assert!(
        payload["image_bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["path"] == "apps/base/sonarr/deployment.yaml"
                    && item["document_index"] == 0
                    && item["name"] == "sonarr-deployment"
                    && item["namespace"] == "default"
                    && item["yaml_path"] == "spec.template.spec.containers[0].image"
                    && item["image"] == "linuxserver/sonarr:version-4.0.16.2944"
            })
    );
    assert!(
        payload["helmreleases_without_chart_version"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["path"] == "apps/production/uptimekuma-values.yaml"
                    && item["document_index"] == 0
                    && item["name"] == "uptime-kuma"
                    && item["namespace"].is_null()
            })
    );
    assert!(
        payload["image_references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["path"] == "apps/production/immich/release-patch.yaml"
                    && item["manifest_kind"] == "HelmRelease"
                    && item["manifest_name"] == "immich"
                    && item["yaml_path"] == "spec.values.valkey.controllers.main.containers.main.image"
                    && item["image"] == "docker.io/valkey/valkey:9.0-alpine@sha256:1be494495248d53e3558b198a1c704e6b559d5e99fe4c926e14a8ad24d76c6fa"
            })
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
    assert!(stdout.contains("repository manifests"));
    assert!(stdout.contains("Version declarations:"));
    assert!(stdout.contains("Repositories:"));
    assert!(stdout.contains("Chart targets:"));
    assert!(stdout.contains("Image bindings:"));
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
    assert!(payload.get("strict").is_none());
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
        item["path"] == "apps/base/paperless-ngx/release.yaml"
            && item["inherited_source"] == false
            && item["yaml_path"] == "spec.chart.spec.version"
            && item["latest_version"] == "12.1.0"
    }));
    assert!(payload["planned"].as_array().unwrap().iter().any(|item| {
        item["path"] == "apps/base/sonarr/deployment.yaml"
            && item["target_kind"] == "ImageBinding"
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
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read release")
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
            item["id"].as_str().is_some_and(|id| id.starts_with("v2:")),
            "planned item should include a stable v2 apply id: {item}"
        );
    }
}

#[test]
fn update_helm_json_plan_resolves_generic_oci_with_repository_resolver() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(200, r#"{"tags":["14.1.3","14.3.0"]}"#)
            .header("Content-Type", "application/json"),
    ]);
    write_file(
        &repo_root.join("source.yaml"),
        &format!(
            r#"apiVersion: source.toolkit.fluxcd.io/v1
kind: HelmRepository
metadata:
  name: public-oci
  namespace: flux-system
spec:
  type: oci
  url: oci://{}/charts
"#,
            server.base_url.trim_start_matches("http://")
        ),
    );
    write_file(
        &repo_root.join("release.yaml"),
        r#"apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: jellyseerr
  namespace: flux-system
spec:
  chart:
    spec:
      chart: jellyseerr
      version: "14.1.3"
      sourceRef:
        kind: HelmRepository
        name: public-oci
        namespace: flux-system
"#,
    );
    let factory = RepositoryResolverFactory;

    let (code, stdout, stderr) = run_cli_with_any_factory(
        [
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
    assert_eq!(stderr, "");
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["summary"]["planned_count"], 1);
    assert_eq!(payload["summary"]["skipped_count"], 0);
    assert!(payload["planned"].as_array().unwrap().iter().any(|item| {
        item["path"] == "release.yaml"
            && item["target_kind"] == "HelmRelease"
            && item["current_version"] == "14.1.3"
            && item["latest_version"] == "14.3.0"
    }));
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v2/charts/jellyseerr/tags/list?n=1000");
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
        "n\n",
        &factory,
    );

    assert_eq!(code, 0);
    assert!(stderr.contains("Update apps/base/paperless-ngx/release.yaml"));
    for detail in [
        "HelmRelease paperless-ngx",
        "truecharts/paperless-ngx",
        "spec.chart.spec.version",
    ] {
        assert!(stderr.contains(detail), "missing prompt detail: {detail}");
    }
    assert!(stderr.contains("12.0.0 -> 12.1.0"));
    assert!(stderr.contains("[y/N] n\n"));
}

#[test]
fn update_helm_prompts_identify_each_image_before_approval() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("pods.yaml");
    let original = "kind: Pod\nmetadata: {name: demo}\nspec:\n  containers:\n    - image: example/one:1.0.0\n    - image: example/two:1.0.0\n---\nkind: Pod\nmetadata: {name: demo}\nspec:\n  containers:\n    - image: example/three:1.0.0\n";
    write_file(&path, original);
    let factory = StaticResolverFactory::new(
        HashMap::new(),
        ["one", "two", "three"]
            .into_iter()
            .map(|name| {
                (
                    format!("example/{name}:1.0.0"),
                    format!("example/{name}:2.0.0"),
                )
            })
            .collect(),
    );

    let (code, _, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().expect("repo path"),
        ],
        "nyn",
        &factory,
    );

    assert_eq!(code, 20);
    let prompts = stderr
        .lines()
        .filter(|line| line.contains("[y/N]"))
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 3);
    for (prompt, (document, index, image)) in
        prompts
            .iter()
            .zip([(1, 0, "one"), (1, 1, "two"), (2, 0, "three")])
    {
        let before_approval = prompt.split("[y/N]").next().expect("prompt details");
        for detail in [
            "Pod demo".to_string(),
            format!("document {document}"),
            format!("spec.containers[{index}].image"),
            format!("example/{image}:1.0.0"),
            format!("example/{image}:2.0.0"),
        ] {
            assert!(
                before_approval.contains(&detail),
                "missing prompt detail {detail}: {before_approval}"
            );
        }
    }
    assert_eq!(
        fs::read_to_string(path).expect("read pods"),
        original.replace("example/two:1.0.0", "example/two:2.0.0")
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
        "\n",
        &factory,
    );

    assert_eq!(code, 0);
    assert!(stderr.contains("[y/N] n\n"));
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
fn invalid_update_modes_are_reported_before_accessing_the_repository() {
    let temp = tempfile::tempdir().expect("temp dir");
    let missing = temp.path().join("missing");
    for (flags, required) in [
        (vec!["--write"], "--non-interactive"),
        (vec!["--apply-id", "selected"], "--write"),
    ] {
        let mut args = vec![
            "fluxrepo-update",
            "update-helm",
            missing.to_str().expect("repo path"),
            "--json",
        ];
        args.extend(flags);
        let (code, stdout, stderr) = run_cli(&args, "", &StaticResolverFactory::default());
        let payload: Value = serde_json::from_str(&stderr).expect("JSON error");

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(payload["error"], "invalid_arguments");
        assert_eq!(payload["exit_code"], 2);
        assert!(payload["message"].as_str().unwrap().contains(required));
        assert!(!missing.exists());
    }
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
fn update_helm_rejects_the_removed_strict_option() {
    let (code, _, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            fixture_root().to_str().expect("fixture path"),
            "--json",
            "--strict",
            "--non-interactive",
        ],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
    assert!(!stderr.is_empty());
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
fn update_helm_does_not_claim_skipped_targets_are_current() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("pod.yaml"),
        "kind: Pod\nmetadata: {name: demo}\nspec: {containers: [{image: example/app:latest}]}\n",
    );

    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().expect("repo path"),
            "--non-interactive",
        ],
        "",
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(!stderr.contains("No updates required"));
    for detail in ["No updates planned", "1 target", "skipped"] {
        assert!(stderr.contains(detail), "missing summary detail: {detail}");
    }
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
    for expected in [
        "apps/base/sonarr/deployment.yaml",
        "ImageBinding",
        "sonarr-deployment",
        "spec.template.spec.containers[0].image",
        "version-4.0.16.2944 -> version-4.0.17.3000",
        "apps/base/paperless-ngx/release.yaml",
        "HelmRelease",
        "paperless-ngx",
        "12.0.0 -> 12.1.0",
    ] {
        assert!(
            stderr.contains(expected),
            "missing output detail: {expected}"
        );
    }
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

struct RepositoryResolverFactory;

impl TestResolvers for RepositoryResolverFactory {
    fn chart_resolver(&self) -> Box<dyn ChartVersionResolver + Sync> {
        Box::new(RepositoryChartResolver::default())
    }

    fn image_resolver(&self) -> Box<dyn ImageVersionResolver + Sync> {
        Box::new(StaticImageVersionResolver::new(HashMap::new()))
    }
}

fn run_cli(args: &[&str], input: &str, factory: &StaticResolverFactory) -> (u8, String, String) {
    run_cli_with_any_factory(args, input, factory)
}

fn run_cli_owned(
    args: Vec<String>,
    input: &str,
    factory: &StaticResolverFactory,
) -> (u8, String, String) {
    run_cli_with_any_factory(args, input, factory)
}

fn run_cli_with_any_factory<I, T>(
    args: I,
    input: &str,
    factory: &impl TestResolvers,
) -> (u8, String, String)
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_with_args(
        args,
        Cursor::new(input.as_bytes()),
        &mut stdout,
        &mut stderr,
        factory.chart_resolver().as_ref(),
        factory.image_resolver().as_ref(),
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
