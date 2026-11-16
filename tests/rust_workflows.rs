#![allow(clippy::needless_raw_string_hashes)]

mod common;

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;

use common::{
    DefaultResolverFactory, ResponseSpec, StaticResolverFactory, TestHttpServer, TestResolvers,
    copy_fixture, write_file,
};
use fluxrepo_update::cli::run_with_args;
use serde_json::Value;

fn write_namespace_chart(root: &std::path::Path, url: &str, explicit_source_namespace: bool) {
    write_file(
        &root.join("source.yaml"),
        &format!(
            "kind: HelmRepository\nmetadata: {{name: shared, namespace: flux-system}}\nspec: {{url: '{url}'}}\n"
        ),
    );
    let namespace = if explicit_source_namespace {
        ", namespace: jellyfin"
    } else {
        ""
    };
    write_file(
        &root.join("release.yaml"),
        &format!(
            "kind: HelmRelease\nmetadata: {{name: demo, namespace: raw-release}}\nspec:\n  chart:\n    spec:\n      chart: demo\n      version: 1.0.0\n      sourceRef: {{kind: HelmRepository, name: shared{namespace}}}\n"
        ),
    );
}

#[test]
fn kustomize_namespace_plans_registry_backed_chart_updates() {
    for explicit_source_namespace in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let server = TestHttpServer::new(vec![ResponseSpec::new(
            200,
            "entries:\n  demo:\n  - version: 2.0.0\n  - version: 1.0.0\n",
        )]);
        for environment in ["production", "testing"] {
            let root = temp.path().join(environment);
            write_namespace_chart(
                &root.join("app"),
                &server.base_url,
                explicit_source_namespace,
            );
            write_file(
                &root.join("app/kustomization.yml"),
                "namespace: inner\nresources: [source.yaml, release.yaml]\n",
            );
            write_file(
                &root.join("kustomization.yaml"),
                "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nnamespace: jellyfin\nresources: [app]\n",
            );
        }
        // Raw namespaces can differ when both effective namespaces and full specs agree.
        let source = temp.path().join("testing/app/source.yaml");
        fs::write(
            &source,
            fs::read_to_string(&source)
                .unwrap()
                .replace("flux-system", "other-raw"),
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(
            &[
                "fluxrepo-update",
                "update-helm",
                temp.path().to_str().unwrap(),
                "--json",
                "--non-interactive",
            ],
            &DefaultResolverFactory,
        );
        assert_eq!(code, 10, "{stderr}: {stdout}");
        let report: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(report["planned"].as_array().unwrap().len(), 2);
        assert_eq!(report["skipped"], serde_json::json!([]));
        for (update, environment) in report["planned"]
            .as_array()
            .unwrap()
            .iter()
            .zip(["production", "testing"])
        {
            assert_eq!(update["current_version"], "1.0.0");
            assert_eq!(update["latest_version"], "2.0.0");
            assert_eq!(update["namespace"], "raw-release");
            assert_eq!(update["effective_namespace"], "jellyfin");
            assert_eq!(
                update["namespace_kustomizations"],
                serde_json::json!([format!("{environment}/kustomization.yaml")])
            );
            for (source, directory) in update["sources"]
                .as_array()
                .unwrap()
                .iter()
                .zip(["production", "testing"])
            {
                assert_eq!(source["effective_namespace"], "jellyfin");
                assert_eq!(
                    source["namespace_kustomizations"],
                    serde_json::json!([format!("{directory}/kustomization.yaml")])
                );
            }
        }
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/index.yaml");
    }
}

#[test]
fn chart_namespace_without_transform_still_mismatches() {
    for kustomization in [
        None,
        Some("kind: Kustomization\nresources: [source.yaml, release.yaml]\n"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        write_namespace_chart(temp.path(), "https://unused.example", true);
        if let Some(text) = kustomization {
            write_file(&temp.path().join("kustomization.yaml"), text);
        }
        let (code, stdout, stderr) = run_cli(
            &[
                "fluxrepo-update",
                "update-helm",
                temp.path().to_str().unwrap(),
                "--json",
                "--non-interactive",
            ],
            &StaticResolverFactory::default(),
        );
        assert_eq!(code, 0, "{stderr}");
        let report: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(report["planned"], serde_json::json!([]));
        assert_eq!(
            report["skipped"][0]["reason_code"],
            "missing_helm_repository"
        );
        assert!(
            report["skipped"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("namespace mismatch")
        );
        assert_eq!(report["skipped"][0]["effective_namespace"], "raw-release");
        assert_eq!(
            report["skipped"][0]["sources"][0]["effective_namespace"],
            "flux-system"
        );
    }
}

#[test]
fn conflicting_kustomize_namespace_chains_are_unresolved() {
    for resources in [
        "source.yaml",
        "release.yaml",
        "source.yaml, ../release.yaml",
    ] {
        let temp = tempfile::tempdir().unwrap();
        write_namespace_chart(temp.path(), "https://unused.example", true);
        for (directory, namespace) in [("a", "jellyfin"), ("b", "other")] {
            write_file(
                &temp.path().join(directory).join("kustomization.yaml"),
                &format!(
                    "kind: Kustomization\nnamespace: {namespace}\nresources: [../{resources}]\n"
                ),
            );
        }
        let (code, stdout, stderr) = run_cli(
            &[
                "fluxrepo-update",
                "update-helm",
                temp.path().to_str().unwrap(),
                "--json",
                "--non-interactive",
            ],
            &StaticResolverFactory::default(),
        );
        assert_eq!(code, 0, "{stderr}");
        let report: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(report["planned"], serde_json::json!([]));
        assert_eq!(
            report["skipped"][0]["reason_code"],
            "ambiguous_kustomize_namespace"
        );
        assert_eq!(report["skipped"][0]["retryable"], false);
    }
}

#[test]
fn transformed_conflicting_sources_remain_ambiguous() {
    let temp = tempfile::tempdir().unwrap();
    write_namespace_chart(temp.path(), "https://first.example", true);
    write_file(
        &temp.path().join("other-source.yaml"),
        "kind: HelmRepository\nmetadata: {name: shared, namespace: different-raw}\nspec: {url: https://second.example}\n",
    );
    write_file(
        &temp.path().join("kustomization.yaml"),
        "kind: Kustomization\nnamespace: jellyfin\nresources: [source.yaml, other-source.yaml, release.yaml]\n",
    );
    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().unwrap(),
            "--json",
            "--non-interactive",
        ],
        &StaticResolverFactory::default(),
    );
    assert_eq!(code, 0, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["planned"], serde_json::json!([]));
    let skipped = &report["skipped"][0];
    assert_eq!(skipped["reason_code"], "ambiguous_helm_repository");
    assert_eq!(skipped["sources"].as_array().unwrap().len(), 2);
    assert!(
        skipped["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["effective_namespace"] == "jellyfin")
    );
}

#[test]
fn tag_only_image_overrides_are_visible_and_never_written() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("release.yaml");
    let original = "kind: HelmRelease\nmetadata: {name: immich}\nspec:\n  values:\n    controllers:\n      main:\n        containers:\n          main:\n            image: {tag: v3.1.0}\n    zfsPlugin:\n      image: {tag: 2.10.1}\n";
    write_file(&path, original);
    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().unwrap(),
            "--json",
            "--write",
            "--non-interactive",
        ],
        &DefaultResolverFactory,
    );
    assert_eq!(code, 0, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    let skipped = report["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 2);
    assert!(skipped.iter().any(|item| item["yaml_path"]
        == "spec.values.controllers.main.containers.main.image.tag"
        && item["current_value"] == "v3.1.0"));
    assert!(skipped.iter().any(
        |item| item["yaml_path"] == "spec.values.zfsPlugin.image.tag"
            && item["current_value"] == "2.10.1"
    ));
    assert!(skipped.iter().all(|item| item["path"] == "release.yaml"
        && item["reason"].as_str().unwrap().contains("repository")));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn cloudnativepg_images_are_visible_without_enabling_new_writes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("cloudnative-pg.yaml");
    let original = "apiVersion: postgresql.cnpg.io/v1\nkind: Cluster\nmetadata: {name: immich}\nspec:\n  imageName: ghcr.io/tensorchord/cloudnative-vectorchord:16.9-0.4.3\n";
    write_file(&path, original);
    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().unwrap(),
            "--json",
            "--write",
            "--non-interactive",
        ],
        &DefaultResolverFactory,
    );
    assert_eq!(code, 0, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["skipped"].as_array().unwrap().len(), 1);
    assert_eq!(report["skipped"][0]["yaml_path"], "spec.imageName");
    assert_eq!(
        report["skipped"][0]["current_value"],
        "ghcr.io/tensorchord/cloudnative-vectorchord:16.9-0.4.3"
    );
    assert!(!report["skipped"][0]["reason"].as_str().unwrap().is_empty());
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn unsupported_remote_kustomize_resources_are_reported_without_fetching_or_editing_them() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("kustomization.yaml");
    let original = "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n- https://downloads.example.org/kubevirt/kubevirt/releases/download/v1.8.0/kubevirt-operator.yaml\n- https://downloads.example.org/kubevirt/containerized-data-importer/releases/download/v1.65.0/cdi-operator.yaml\n- https://github.com/intel/intel-device-plugins-for-kubernetes/deployments/nfd?ref=main\n- local.yaml\n# - https://example.org/commented/v1.0.0.yaml\n";
    write_file(&path, original);
    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().unwrap(),
            "--json",
            "--write",
            "--non-interactive",
        ],
        &DefaultResolverFactory,
    );
    assert_eq!(code, 0, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    let skipped = report["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 3);
    assert_eq!(skipped[0]["yaml_path"], "resources[0]");
    assert!(
        skipped[0]["current_value"]
            .as_str()
            .unwrap()
            .contains("v1.8.0")
    );
    assert_eq!(skipped[2]["yaml_path"], "resources[2]");
    assert!(
        skipped[2]["current_value"]
            .as_str()
            .unwrap()
            .ends_with("?ref=main")
    );
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn coverage_counts_include_current_and_unchecked_declarations_in_plan_and_apply() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("pod.yaml"),
        "kind: Pod\nmetadata: {name: demo}\nspec:\n  containers:\n  - {name: current, image: 'example/current:1.0.0'}\n  - {name: update, image: 'example/update:1.0.0'}\n",
    );
    write_file(
        &temp.path().join("release.yaml"),
        "kind: HelmRelease\nmetadata: {name: override}\nspec:\n  values:\n    image: {tag: v3.1.0}\n",
    );
    let factory = StaticResolverFactory::new(
        HashMap::new(),
        HashMap::from([
            (
                "example/current:1.0.0".into(),
                "example/current:1.0.0".into(),
            ),
            ("example/update:1.0.0".into(), "example/update:2.0.0".into()),
        ]),
    );
    for write in [false, true] {
        let mut args = vec![
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().unwrap(),
            "--json",
            "--non-interactive",
        ];
        if write {
            args.push("--write");
        }
        let (code, stdout, stderr) = run_cli(&args, &factory);
        assert_eq!(code, if write { 20 } else { 10 }, "{stderr}");
        let report: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(report["scope"], "repository_manifests");
        assert_eq!(report["summary"]["discovered_count"], 3);
        assert_eq!(report["summary"]["checked_count"], 2);
        assert_eq!(report["summary"]["unchecked_count"], 1);
        assert_eq!(report["summary"]["planned_count"], 1);
    }
}

#[test]
fn human_report_explains_unchecked_fields_and_repository_coverage() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("release.yaml"),
        "kind: HelmRelease\nmetadata: {name: demo}\nspec:\n  values:\n    image: {tag: v3.1.0}\n",
    );
    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().unwrap(),
            "--non-interactive",
        ],
        &DefaultResolverFactory,
    );
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    for fact in [
        "repository manifests",
        "1 discovered",
        "0 checked",
        "1 unchecked",
        "release.yaml",
        "spec.values.image.tag",
        "v3.1.0",
    ] {
        assert!(stderr.contains(fact), "missing {fact}: {stderr}");
    }
    assert!(!stderr.contains("No updates required"));
}

#[test]
fn chart_plans_identify_every_equivalent_source_copy() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_ambiguous_source_fixture(temp.path(), "apps");
    fs::copy(
        temp.path().join("a-source.yaml"),
        temp.path().join("z-source.yaml"),
    )
    .unwrap();
    let factory = StaticResolverFactory::new(
        HashMap::from([(("shared".into(), "demo".into()), "2.0.0".into())]),
        HashMap::new(),
    );
    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().unwrap(),
            "--json",
            "--non-interactive",
        ],
        &factory,
    );
    assert_eq!(code, 10, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        report["planned"][0]["sources"],
        serde_json::json!([
            {"path": "a-source.yaml", "document_index": 0, "namespace": "apps", "effective_namespace": "apps", "namespace_kustomizations": [], "namespace_ambiguous": false},
            {"path": "z-source.yaml", "document_index": 0, "namespace": "apps", "effective_namespace": "apps", "namespace_kustomizations": [], "namespace_ambiguous": false},
        ])
    );
}

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
            && item["target_kind"] == "ImageBinding"
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
fn update_helm_workflow_updates_recursive_mapping_and_scalar_helm_values() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("release.yml"),
        r#"kind: HelmRelease
metadata:
  name: dashboard
spec:
  values:
    sidecar:
      image:
        registry: registry.example
        repository: helpers/sidecar
        tag: "3.0.0" # retain this comment
    helper:
      image: registry.example/helpers/helper:1.0.0
    nested:
      metrics:
        repository: registry.example/metrics
        tag: "2.0.0"
"#,
    );
    let factory = StaticResolverFactory::new(
        HashMap::new(),
        HashMap::from([
            (
                "registry.example/helpers/sidecar:3.0.0".to_string(),
                "registry.example/helpers/sidecar:4.0.0".to_string(),
            ),
            (
                "registry.example/helpers/helper:1.0.0".to_string(),
                "registry.example/helpers/helper:1.1.0".to_string(),
            ),
            (
                "registry.example/metrics:2.0.0".to_string(),
                "registry.example/metrics:2.1.0".to_string(),
            ),
        ]),
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

    assert_eq!(code, 20, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).expect("report json");
    assert_eq!(report["summary"]["applied_count"], 3);
    let paths = report["planned"]
        .as_array()
        .expect("planned updates")
        .iter()
        .map(|item| item["yaml_path"].as_str().expect("yaml path"))
        .collect::<Vec<_>>();
    assert!(paths.contains(&"spec.values.sidecar.image.tag"));
    assert!(paths.contains(&"spec.values.helper.image"));
    assert!(paths.contains(&"spec.values.nested.metrics.tag"));
    assert_eq!(
        fs::read_to_string(repo_root.join("release.yml")).expect("read release"),
        r#"kind: HelmRelease
metadata:
  name: dashboard
spec:
  values:
    sidecar:
      image:
        registry: registry.example
        repository: helpers/sidecar
        tag: "4.0.0" # retain this comment
    helper:
      image: registry.example/helpers/helper:1.1.0
    nested:
      metrics:
        repository: registry.example/metrics
        tag: "2.1.0"
"#
    );
}

#[test]
fn inventory_workflow_covers_yml_exclusions_and_incomplete_documents() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("pod.yml"),
        "kind: Pod\nmetadata: {name: visible}\nspec: {containers: [{image: example/app:1.0.0}]}\n",
    );
    write_file(
        &repo_root.join(".hidden/pod.yaml"),
        "kind: Pod\nmetadata: {name: hidden}\nspec: {containers: [{image: example/hidden:1.0.0}]}\n",
    );
    write_file(
        &repo_root.join(".cache/pod.yml"),
        "kind: Pod\nmetadata: {name: cached}\nspec: {containers: [{image: example/cached:1.0.0}]}\n",
    );
    write_file(
        &repo_root.join("clusters/prod/flux-system/gotk-components.yaml"),
        "kind: Pod\nmetadata: {name: generated}\nspec: {containers: [{image: example/generated:1.0.0}]}\n",
    );
    write_file(
        &repo_root.join("incomplete.yaml"),
        "metadata: {name: incomplete}\nspec: {containers: [{image: example/incomplete:1.0.0}]}\n",
    );

    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "inventory",
            repo_root.to_str().expect("repo path"),
            "--json",
        ],
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 0, "{stderr}");
    let inventory: Value = serde_json::from_str(&stdout).expect("inventory json");
    assert_eq!(inventory["image_binding_count"], 1);
    assert_eq!(inventory["image_bindings"][0]["path"], "pod.yml");
    assert_eq!(
        inventory["skipped_paths"],
        serde_json::json!(["clusters/prod/flux-system/gotk-components.yaml"])
    );
}

#[test]
fn inventory_workflow_reports_malformed_yaml_as_json_error() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(&repo_root.join("broken.yml"), "kind: [\n");

    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "inventory",
            repo_root.to_str().expect("repo path"),
            "--json",
        ],
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    let error: Value = serde_json::from_str(&stderr).expect("error json");
    assert_eq!(error["error"], "runtime_error");
    assert!(error["message"].as_str().unwrap().contains("broken.yml"));
}

#[test]
fn inventory_workflow_reports_ambiguous_repository_sources_without_choosing_one() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_ambiguous_source_fixture(temp.path(), "other");

    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "inventory",
            temp.path().to_str().expect("repo path"),
            "--json",
        ],
        &StaticResolverFactory::default(),
    );

    assert_eq!(code, 0, "{stderr}");
    let inventory: Value = serde_json::from_str(&stdout).expect("inventory JSON");
    assert!(
        inventory["ambiguous_repositories"].is_array(),
        "repository collisions must be visible instead of choosing the last source: {inventory}"
    );
    let collisions = inventory["ambiguous_repositories"].as_array().unwrap();
    assert_eq!(collisions.len(), 1);
    assert_eq!(collisions[0]["name"], "shared");
    let sources = collisions[0]["sources"]
        .as_array()
        .expect("colliding sources");
    assert_eq!(sources.len(), 2);
    for (source, (path, namespace, url)) in sources.iter().zip([
        ("a-source.yaml", "apps", "https://expected.example/charts"),
        ("z-source.yaml", "other", "https://wrong.example/charts"),
    ]) {
        assert_eq!(source["path"], path);
        assert_eq!(source["namespace"], namespace);
        assert_eq!(source["url"], url);
        assert_eq!(source["document_index"], 0);
    }
    assert!(
        inventory["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["name"] != "shared")
    );
}

#[test]
fn update_workflow_skips_ambiguous_chart_sources_and_applies_independent_images() {
    for duplicate_namespace in ["other", "apps"] {
        let temp = tempfile::tempdir().expect("temp dir");
        write_ambiguous_source_fixture(temp.path(), duplicate_namespace);
        let release_path = temp.path().join("release.yaml");
        let original_release = fs::read_to_string(&release_path).expect("read release");
        let factory = StaticResolverFactory::new(
            HashMap::from([(
                ("shared".to_string(), "demo".to_string()),
                "9.9.9".to_string(),
            )]),
            HashMap::from([(
                "example/safe:1.0.0".to_string(),
                "example/safe:2.0.0".to_string(),
            )]),
        );

        let (code, stdout, stderr) = run_cli(
            &[
                "fluxrepo-update",
                "update-helm",
                temp.path().to_str().expect("repo path"),
                "--json",
                "--write",
                "--non-interactive",
            ],
            &factory,
        );

        assert_eq!(code, 20, "{stderr}");
        let report: Value = serde_json::from_str(&stdout).expect("update JSON");
        assert_eq!(
            report["summary"]["planned_count"], 1,
            "ambiguous chart source must not yield a planned update: {report}"
        );
        assert_eq!(report["summary"]["applied_count"], 1);
        assert_eq!(report["summary"]["skipped_count"], 1);
        assert_eq!(report["skipped"][0]["path"], "release.yaml");
        assert_eq!(
            report["skipped"][0]["reason_code"],
            "ambiguous_helm_repository"
        );
        assert_eq!(report["skipped"][0]["retryable"], false);
        assert!(!report["skipped"][0]["id"].as_str().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(&release_path).expect("read unchanged release"),
            original_release
        );
        assert!(
            fs::read_to_string(temp.path().join("pod.yaml"))
                .expect("read updated pod")
                .contains("example/safe:2.0.0")
        );
    }
}

fn write_ambiguous_source_fixture(repo_root: &std::path::Path, duplicate_namespace: &str) {
    write_file(
        &repo_root.join("a-source.yaml"),
        "kind: HelmRepository\nmetadata: {name: shared, namespace: apps}\nspec: {url: https://expected.example/charts}\n",
    );
    write_file(
        &repo_root.join("z-source.yaml"),
        &format!(
            "kind: HelmRepository\nmetadata: {{name: shared, namespace: {duplicate_namespace}}}\nspec: {{url: https://wrong.example/charts}}\n"
        ),
    );
    write_file(
        &repo_root.join("release.yaml"),
        "kind: HelmRelease\nmetadata: {name: demo, namespace: apps}\nspec:\n  chart:\n    spec:\n      chart: demo\n      version: 1.0.0\n      sourceRef: {kind: HelmRepository, name: shared, namespace: apps}\n",
    );
    write_file(
        &repo_root.join("pod.yaml"),
        "kind: Pod\nmetadata: {name: independent}\nspec: {containers: [{image: example/safe:1.0.0}]}\n",
    );
}

#[test]
fn update_helm_workflow_reports_conservative_helm_value_skips() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("release.yaml"),
        r#"kind: HelmRelease
metadata: {name: conservative}
spec:
  values:
    tagless: {image: registry.example/tagless/app}
    mutable: {image: registry.example/mutable/app:latest}
    templated: {image: "{{ .Values.image }}"}
    pinned:
      image:
        repository: registry.example/pinned/app
        tag: "1.0.0"
        digest: sha256:deadbeef
    blank:
      image:
        repository: registry.example/blank/app
        tag: ""
    unknown:
      image:
        repository: registry.example/unknown/app
        version: "1.0.0"
"#,
    );

    let (code, stdout, stderr) = run_cli(
        &[
            "fluxrepo-update",
            "update-helm",
            repo_root.to_str().expect("repo path"),
            "--json",
            "--non-interactive",
        ],
        &DefaultResolverFactory,
    );

    assert_eq!(code, 0, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).expect("report json");
    assert_eq!(report["summary"]["planned_count"], 0);
    assert_eq!(report["summary"]["skipped_count"], 5);
    let reasons = report["skipped"]
        .as_array()
        .expect("skipped updates")
        .iter()
        .map(|item| item["reason_code"].as_str().expect("reason code"))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(report["skipped"].as_array().unwrap().iter().any(|item| {
        item["reason_code"] == "image_reference_pinned_by_digest"
            && item["current_value"] == "1.0.0"
            && item["document_index"] == 0
    }));
    assert_eq!(
        reasons,
        std::collections::BTreeSet::from([
            "image_reference_missing_tag",
            "image_reference_pinned_by_digest",
            "mutable_image_tag",
            "templated_image_reference",
            "unsupported_image_schema",
        ])
    );
}

#[test]
#[cfg(unix)]
fn update_helm_partial_write_failure_identifies_changes_in_human_output() {
    assert_cli_partial_write_diagnostics(false);
}

#[test]
#[cfg(unix)]
fn update_helm_partial_write_failure_identifies_changes_in_json_output() {
    assert_cli_partial_write_diagnostics(true);
}

#[cfg(unix)]
fn assert_cli_partial_write_diagnostics(json_output: bool) {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let temp = tempfile::tempdir().expect("temp dir");
    let server = TestHttpServer::new(
        (0..2)
            .map(|_| ResponseSpec::new(200, r#"{"tags":["1.0.0","2.0.0"]}"#))
            .collect(),
    );
    let image = format!(
        "{}/demo/app:1.0.0",
        server.base_url.trim_start_matches("http://")
    );
    let original = format!(
        "kind: Pod\nmetadata: {{name: demo}}\nspec:\n  containers:\n  - name: app\n    image: {image}\n  - name: sidecar\n    image: {image}\n"
    );
    let paths = ["a.yaml", "b.yaml", "c.yaml"].map(|name| {
        let path = temp.path().join(name);
        write_file(&path, &original);
        path.canonicalize().unwrap()
    });
    let plan = Command::new(env!("CARGO_BIN_EXE_fluxrepo-update"))
        .arg("update-helm")
        .arg(temp.path())
        .args(["--json", "--non-interactive"])
        .output()
        .expect("run plan");
    assert_eq!(plan.status.code(), Some(10), "{:?}", plan.stderr);
    let plan: Value = serde_json::from_slice(&plan.stdout).expect("plan JSON");
    let updates = plan["planned"].as_array().unwrap();
    assert_eq!(updates.len(), 6);
    let mut command = Command::new(env!("CARGO_BIN_EXE_fluxrepo-update"));
    command
        .arg("update-helm")
        .arg(temp.path())
        .args(["--write", "--non-interactive"]);
    if json_output {
        command.arg("--json");
    }
    // Report only approved updates, including multiple selections in one file.
    let selected = [0, 1, 2, 4];
    for index in selected {
        command.args(["--apply-id", updates[index]["id"].as_str().unwrap()]);
    }
    let ids = [vec![0, 1], vec![2], vec![4]].map(|indexes| {
        indexes
            .into_iter()
            .map(|index| updates[index]["id"].as_str().unwrap().to_owned())
            .collect()
    });
    let permissions = fs::metadata(&paths[1]).unwrap().permissions();
    fs::set_permissions(&paths[1], fs::Permissions::from_mode(0o444)).unwrap();
    let output = command.output().expect("run apply");
    fs::set_permissions(&paths[1], permissions).unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let message = if json_output {
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_str(&stderr).expect("error JSON");
        assert_eq!(error["error"], "runtime_error");
        assert_eq!(error["exit_code"], 2);
        error["message"].as_str().unwrap().to_owned()
    } else {
        stderr
    };
    common::assert_partial_write_diagnostics(&message, &paths, &ids);
    for index in [3, 5] {
        assert!(!message.contains(updates[index]["id"].as_str().unwrap()));
    }
    assert_eq!(
        fs::read_to_string(&paths[0]).unwrap(),
        original.replace(":1.0.0", ":2.0.0")
    );
    assert_eq!(fs::read_to_string(&paths[1]).unwrap(), original);
    assert_eq!(fs::read_to_string(&paths[2]).unwrap(), original);
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn update_helm_apply_id_edits_only_selected_deployment_image_in_block_lists() {
    let original_example = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: demo\nspec:\n  template:\n    spec:\n      containers:\n        - name: app\n          image: example/app:1.0.0\n";
    let siblings = format!(
        "{original_example}      initContainers:\n        - name: init\n          image: example/init:1.0.0 # retain init comment\n      restartPolicy: Always\n"
    );
    let indentless = siblings
        .replace("        - name:", "      - name:")
        .replace("          image:", "        image:");
    for original in [original_example, &siblings, &indentless] {
        let paths = if original == original_example {
            vec!["spec.template.spec.containers[0].image"]
        } else {
            vec![
                "spec.template.spec.containers[0].image",
                "spec.template.spec.initContainers[0].image",
            ]
        };
        assert_selected_image_edits(original, &paths);
    }
}

#[test]
fn update_helm_apply_id_edits_only_selected_image_in_indentless_helm_values_lists() {
    let original = r#"kind: HelmRelease
metadata: {name: demo}
spec:
  values:
    initContainers:
    - name: init
      image: 'example/init:1.0.0' # retain comment
    extraContainers:
    - name: app
      image:
        repository: example/app
        tag: "1.0.0" # retain tag quoting
    description: "example/app:1.0.0 is unrelated"
"#;
    assert_selected_image_edits(
        original,
        &[
            "spec.values.initContainers[0].image",
            "spec.values.extraContainers[0].image.tag",
        ],
    );
}

fn assert_selected_image_edits(original: &str, yaml_paths: &[&str]) {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("manifest.yaml");
    let factory = StaticResolverFactory::new(
        HashMap::new(),
        HashMap::from([
            ("example/app:1.0.0".into(), "example/app:2.0.0".into()),
            ("example/init:1.0.0".into(), "example/init:2.0.0".into()),
        ]),
    );
    for yaml_path in yaml_paths {
        write_file(&path, original);
        let (code, stdout, stderr) = run_cli(
            &[
                "fluxrepo-update",
                "update-helm",
                temp.path().to_str().unwrap(),
                "--json",
                "--non-interactive",
            ],
            &factory,
        );
        assert_eq!(code, 10, "{stderr}");
        let plan: Value = serde_json::from_str(&stdout).expect("plan JSON");
        assert_eq!(plan["planned"].as_array().unwrap().len(), yaml_paths.len());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        let update = plan["planned"]
            .as_array()
            .unwrap()
            .iter()
            .find(|update| update["yaml_path"] == *yaml_path)
            .expect("planned image at exact path");
        let (code, _, stderr) = run_cli(
            &[
                "fluxrepo-update",
                "update-helm",
                temp.path().to_str().unwrap(),
                "--write",
                "--non-interactive",
                "--apply-id",
                update["id"].as_str().unwrap(),
            ],
            &factory,
        );
        assert_eq!(code, 20, "{yaml_path}: {stderr}");
        let expected = if yaml_path.rsplit('.').next() == Some("tag") {
            original.replace("tag: \"1.0.0\"", "tag: \"2.0.0\"")
        } else {
            original.replacen(
                update["current_image"].as_str().unwrap(),
                update["latest_image"].as_str().unwrap(),
                1,
            )
        };
        assert_eq!(fs::read_to_string(&path).unwrap(), expected, "{yaml_path}");
    }
}

#[test]
fn update_helm_workflow_applies_all_standard_podspec_image_bindings() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    let path = repo_root.join("workloads.yml");
    write_file(
        &path,
        r"kind: Deployment
metadata: {name: deployment}
spec: {template: {spec: {containers: [{image: example/deployment:1.0.0}], initContainers: [{image: example/deployment-init:1.0.0}]}}}
---
kind: StatefulSet
metadata: {name: statefulset}
spec: {template: {spec: {containers: [{image: example/statefulset:1.0.0}], initContainers: [{image: example/statefulset-init:1.0.0}]}}}
---
kind: DaemonSet
metadata: {name: daemonset}
spec: {template: {spec: {containers: [{image: example/daemonset:1.0.0}], initContainers: [{image: example/daemonset-init:1.0.0}]}}}
---
kind: Job
metadata: {name: job}
spec: {template: {spec: {containers: [{image: example/job:1.0.0}], initContainers: [{image: example/job-init:1.0.0}]}}}
---
kind: CronJob
metadata: {name: cronjob}
spec: {jobTemplate: {spec: {template: {spec: {containers: [{image: example/cronjob:1.0.0}], initContainers: [{image: example/cronjob-init:1.0.0}]}}}}}
---
kind: Pod
metadata: {name: pod}
spec: {containers: [{image: example/pod:1.0.0}], initContainers: [{image: example/pod-init:1.0.0}]}
",
    );
    let image_versions = [
        "deployment",
        "deployment-init",
        "statefulset",
        "statefulset-init",
        "daemonset",
        "daemonset-init",
        "job",
        "job-init",
        "cronjob",
        "cronjob-init",
        "pod",
        "pod-init",
    ]
    .into_iter()
    .map(|name| {
        (
            format!("example/{name}:1.0.0"),
            format!("example/{name}:2.0.0"),
        )
    })
    .collect();
    let factory = StaticResolverFactory::new(HashMap::new(), image_versions);

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

    assert_eq!(code, 20, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).expect("report json");
    assert_eq!(report["summary"]["applied_count"], 12);
    let written = fs::read_to_string(path).expect("read workloads");
    assert!(!written.contains(":1.0.0"));
    assert_eq!(written.matches(":2.0.0").count(), 12);
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

fn run_cli(args: &[&str], factory: &impl TestResolvers) -> (u8, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_with_args(
        args,
        Cursor::new([].as_slice()),
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
