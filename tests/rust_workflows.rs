#![allow(clippy::needless_raw_string_hashes)]

mod common;

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;

use common::{ResponseSpec, StaticResolverFactory, TestHttpServer, copy_fixture, write_file};
use fluxrepo_update::cli::{DefaultResolverFactory, run_with_args};
use fluxrepo_update::updater::PlanOptions;
use serde_json::Value;

#[test]
fn update_helm_workflow_plans_and_applies_from_local_remotes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    let chart_server = TestHttpServer::new(vec![
        ResponseSpec::new(
            200,
            r#"
entries:
  demo:
    - version: "1.2.0"
    - version: "1.3.0"
"#,
        ),
        ResponseSpec::new(
            200,
            r#"
entries:
  demo:
    - version: "1.2.0"
    - version: "1.3.0"
"#,
        ),
    ]);
    let registry_server = TestHttpServer::new(vec![
        ResponseSpec::new(200, r#"{"tags":["2.4.0","2.4.1","2.5.0","latest"]}"#)
            .header("Content-Type", "application/json"),
        ResponseSpec::new(200, r#"{"tags":["2.4.0","2.4.1","2.5.0","latest"]}"#)
            .header("Content-Type", "application/json"),
    ]);
    let registry_host = registry_server
        .base_url
        .strip_prefix("http://")
        .expect("registry base url");

    write_file(
        &repo_root.join("source.yaml"),
        &format!(
            r#"apiVersion: source.toolkit.fluxcd.io/v1
kind: HelmRepository
metadata:
  name: demo-repo
  namespace: flux-system
spec:
  url: {}
"#,
            chart_server.base_url
        ),
    );
    write_file(
        &repo_root.join("release.yaml"),
        r#"apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: demo
  namespace: flux-system
spec:
  chart:
    spec:
      chart: demo
      version: "1.2.0"
      sourceRef:
        kind: HelmRepository
        name: demo-repo
"#,
    );
    write_file(
        &repo_root.join("deployment.yaml"),
        &format!(
            r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: demo
spec:
  template:
    spec:
      containers:
        - name: demo
          image: "{registry_host}/demo/app:2.4.0"
"#
        ),
    );

    let (plan_code, plan_stdout, plan_stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
            "--json",
            "--non-interactive",
        ],
        &DefaultResolverFactory,
    );

    assert_eq!(plan_code, 10);
    assert_eq!(plan_stderr, "");
    let plan: Value = serde_json::from_str(&plan_stdout).expect("plan json");
    assert_eq!(plan["mode"], "plan");
    assert_eq!(plan["summary"]["planned_count"], 2);
    assert!(plan["planned"].as_array().unwrap().iter().any(|item| {
        item["path"] == "release.yaml"
            && item["target_kind"] == "HelmRelease"
            && item["current_version"] == "1.2.0"
            && item["latest_version"] == "1.3.0"
    }));
    assert!(plan["planned"].as_array().unwrap().iter().any(|item| {
        item["path"] == "deployment.yaml"
            && item["target_kind"] == "Deployment"
            && item["current_version"] == "2.4.0"
            && item["latest_version"] == "2.5.0"
    }));

    let (write_code, write_stdout, write_stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
            "--json",
            "--write",
            "--non-interactive",
        ],
        &DefaultResolverFactory,
    );

    assert_eq!(write_code, 20);
    assert_eq!(write_stderr, "");
    let written: Value = serde_json::from_str(&write_stdout).expect("write json");
    assert_eq!(written["mode"], "apply");
    assert_eq!(written["summary"]["applied_count"], 2);
    assert!(
        fs::read_to_string(repo_root.join("release.yaml"))
            .expect("read release")
            .contains(r#"version: "1.3.0""#)
    );
    assert!(
        fs::read_to_string(repo_root.join("deployment.yaml"))
            .expect("read deployment")
            .contains(&format!(r#"image: "{registry_host}/demo/app:2.5.0""#))
    );

    assert_eq!(chart_server.finish().len(), 2);
    assert_eq!(registry_server.finish().len(), 2);
}

#[test]
fn update_helm_write_workflow_keeps_unsupported_and_generated_files_untouched() {
    let (_temp, repo_root) = copy_fixture();
    let generated_path = repo_root.join("clusters/production/flux-system/gotk-sync.yaml");
    let values_only_path = repo_root.join("apps/production/audiobookshelf/release-patch.yaml");
    let latest_image_path = repo_root.join("apps/base/smokeping/deployment.yaml");
    let values_image_path = repo_root.join("apps/production/immich/release-patch.yaml");
    let generated_before = fs::read_to_string(&generated_path).expect("read generated");
    let values_only_before = fs::read_to_string(&values_only_path).expect("read values-only");
    let latest_image_before = fs::read_to_string(&latest_image_path).expect("read latest image");
    let values_image_before = fs::read_to_string(&values_image_path).expect("read values image");
    let factory = StaticResolverFactory::new(
        HashMap::from([(
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        )]),
        HashMap::from([(
            "linuxserver/sonarr:version-4.0.16.2944".to_string(),
            "linuxserver/sonarr:version-4.0.17.3000".to_string(),
        )]),
    );

    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
            "--json",
            "--write",
            "--non-interactive",
        ],
        &factory,
    );

    assert_eq!(code, 20);
    assert_eq!(stderr, "");
    let payload: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(payload["mode"], "apply");
    assert!(
        fs::read_to_string(repo_root.join("apps/base/paperless-ngx/release.yaml"))
            .expect("read base release")
            .contains("12.1.0")
    );
    assert!(
        fs::read_to_string(repo_root.join("apps/base/sonarr/deployment.yaml"))
            .expect("read sonarr")
            .contains("linuxserver/sonarr:version-4.0.17.3000")
    );
    assert_eq!(
        fs::read_to_string(&generated_path).expect("read generated after"),
        generated_before
    );
    assert_eq!(
        fs::read_to_string(&values_only_path).expect("read values-only after"),
        values_only_before
    );
    assert_eq!(
        fs::read_to_string(&latest_image_path).expect("read latest image after"),
        latest_image_before
    );
    assert_eq!(
        fs::read_to_string(&values_image_path).expect("read values image after"),
        values_image_before
    );
}

fn run_cli(
    args: &[&str],
    factory: &impl fluxrepo_update::cli::ResolverFactory,
) -> (u8, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_with_args(
        args,
        Cursor::new([].as_slice()),
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
