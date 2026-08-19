#![allow(clippy::needless_raw_string_hashes)]

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use common::{ResponseSpec, TestHttpServer, copy_fixture, write_file};
use fluxrepo_update::models::{
    HelmRepository, ImageBinding, ImageBindingValueKind, Inventory, RepoType, ResourceId,
};
use fluxrepo_update::resolvers::{
    ImageVersionResolver, RepositoryChartResolver, StaticImageVersionResolver,
    StaticVersionResolver,
};
use fluxrepo_update::scanner::scan_repo;
use fluxrepo_update::updater::{
    PlanOptions, PlannedUpdate, SkippedUpdate, UpdateReport, apply_updates, plan_updates,
    plan_updates_with_options, plan_updates_with_progress,
};

#[test]
fn plans_and_applies_chart_and_deployment_updates() {
    let (_temp, repo_root) = copy_fixture();
    let inventory = scan_repo(&repo_root).expect("scan fixture");
    let chart_resolver = StaticVersionResolver::new(HashMap::from([(
        ("truecharts".to_string(), "paperless-ngx".to_string()),
        "12.1.0".to_string(),
    )]));
    let image_resolver = StaticImageVersionResolver::new(HashMap::from([
        (
            "linuxserver/sonarr:version-4.0.16.2944".to_string(),
            "linuxserver/sonarr:version-4.0.17.3000".to_string(),
        ),
        ("alpine:3.22".to_string(), "alpine:3.22.1".to_string()),
    ]));

    let report = plan_updates(&inventory, &chart_resolver, &image_resolver);

    assert!(report.planned.iter().any(|item| {
        item.path().strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/base/paperless-ngx/release.yaml")
    }));
    assert!(report.planned.iter().any(|item| {
        item.path().strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/base/sonarr/deployment.yaml")
            && item.target_kind() == "ImageBinding"
            && item.current_version() == "version-4.0.16.2944"
            && item.latest_version() == "version-4.0.17.3000"
    }));
    assert!(report.planned.iter().any(|item| {
        item.path().strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/production/openssh/deployment.yaml")
            && item.target_kind() == "ImageBinding"
            && item.current_version() == "3.22"
            && item.latest_version() == "3.22.1"
    }));

    let changed_files = apply_updates(&report).expect("apply updates");

    assert!(changed_files >= 3);
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
    assert!(
        fs::read_to_string(repo_root.join("apps/production/openssh/deployment.yaml"))
            .expect("read openssh")
            .contains("alpine:3.22.1")
    );
}

#[test]
fn changes_only_manifest_local_chart_versions() {
    let (_temp, repo_root) = copy_fixture();
    let inventory = scan_repo(&repo_root).expect("scan fixture");
    let chart_resolver = StaticVersionResolver::new(HashMap::from([
        (
            ("truecharts".to_string(), "paperless-ngx".to_string()),
            "12.1.0".to_string(),
        ),
        (
            ("truecharts".to_string(), "audiobookshelf".to_string()),
            "13.0.1".to_string(),
        ),
    ]));

    let report = plan_updates(
        &inventory,
        &chart_resolver,
        &StaticImageVersionResolver::new(HashMap::new()),
    );

    let planned_paths = report
        .planned
        .iter()
        .map(|item| {
            item.path()
                .strip_prefix(&repo_root)
                .expect("relative path")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert!(planned_paths.contains(&PathBuf::from("apps/base/paperless-ngx/release.yaml")));
    assert!(!planned_paths.contains(&PathBuf::from(
        "apps/production/paperless/release-patch.yaml"
    )));
    assert!(!planned_paths.contains(&PathBuf::from(
        "apps/production/audiobookshelf/release-patch.yaml"
    )));

    let changed_files = apply_updates(&report).expect("apply updates");

    assert!(changed_files >= 2);
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
    assert!(
        fs::read_to_string(repo_root.join("apps/base/audiobookshelf/release.yaml"))
            .expect("read audiobookshelf")
            .contains("13.0.1")
    );
}

#[test]
fn progress_reports_each_resolved_target() {
    let (_temp, repo_root) = copy_fixture();
    let inventory = scan_repo(&repo_root).expect("scan fixture");
    let chart_resolver = StaticVersionResolver::new(HashMap::from([(
        ("truecharts".to_string(), "paperless-ngx".to_string()),
        "12.1.0".to_string(),
    )]));
    let image_resolver = StaticImageVersionResolver::new(HashMap::new());
    let seen = std::sync::Mutex::new(Vec::new());

    let _report = plan_updates_with_progress(
        &inventory,
        &chart_resolver,
        &image_resolver,
        PlanOptions { max_workers: 1 },
        &|current, total, target_path| {
            seen.lock().expect("progress lock").push((
                current,
                total,
                target_path
                    .strip_prefix(&repo_root)
                    .expect("relative path")
                    .to_path_buf(),
            ));
        },
    );

    let seen = seen.into_inner().expect("progress lock");
    let total_targets = inventory.chart_targets.len()
        + inventory.unresolved_chart_targets.len()
        + inventory.image_bindings.len();
    assert_eq!(seen.len(), total_targets);
    assert_eq!(seen.first().map(|event| event.0), Some(1));
    assert_eq!(seen.last().map(|event| event.0), Some(total_targets));
    assert!(seen.iter().all(|(_, total, _)| *total == total_targets));
    assert!(
        seen.iter()
            .any(|(_, _, path)| { path == &PathBuf::from("apps/base/paperless-ngx/release.yaml") })
    );
}

#[test]
fn skips_missing_chart_repository_and_same_or_older_images() {
    let mut inventory = Inventory::new(PathBuf::from("/repo"));
    inventory
        .chart_targets
        .push(fluxrepo_update::models::HelmReleaseTarget {
            path: PathBuf::from("/repo/release.yaml"),
            document_index: 0,
            resource_id: ResourceId {
                kind: "HelmRelease".to_string(),
                name: "demo".to_string(),
                namespace: Some("default".to_string()),
            },
            chart_name: Some("demo".to_string()),
            repo_name: Some("missing-repo".to_string()),
            current_version: Some("1.0.0".to_string()),
            source_path: Some(PathBuf::from("/repo/release.yaml")),
            source_is_inherited: false,
        });
    inventory.repositories.insert(
        "unused".to_string(),
        HelmRepository {
            name: "unused".to_string(),
            url: "https://charts.example.test".to_string(),
            repo_type: RepoType::Default,
            path: PathBuf::from("/repo/repository.yaml"),
            document_index: 0,
        },
    );
    inventory.image_bindings.extend([
        ImageBinding {
            path: PathBuf::from("/repo/service-same.yaml"),
            document_index: 0,
            resource_id: ResourceId {
                kind: "Deployment".to_string(),
                name: "service-same".to_string(),
                namespace: Some("default".to_string()),
            },
            yaml_path: "spec.template.spec.containers[0].image".to_string(),
            image: "example/service:2.0.0".to_string(),
            value_kind: ImageBindingValueKind::ImageReference,
        },
        ImageBinding {
            path: PathBuf::from("/repo/service-older.yaml"),
            document_index: 0,
            resource_id: ResourceId {
                kind: "Deployment".to_string(),
                name: "service-older".to_string(),
                namespace: Some("default".to_string()),
            },
            yaml_path: "spec.template.spec.containers[0].image".to_string(),
            image: "example/service:1.0.0".to_string(),
            value_kind: ImageBindingValueKind::ImageReference,
        },
    ]);

    let report = plan_updates(
        &inventory,
        &StaticVersionResolver::new(HashMap::new()),
        &StaticImageVersionResolver::new(HashMap::from([
            (
                "example/service:1.0.0".to_string(),
                "example/service:1.0.0".to_string(),
            ),
            (
                "example/service:2.0.0".to_string(),
                "example/service:1.0.0".to_string(),
            ),
        ])),
    );

    assert!(report.planned.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(
        report.skipped[0].path,
        Some(PathBuf::from("/repo/release.yaml"))
    );

    let payload = report.to_json_value(Path::new("/repo"), "plan", true, 0, 0);
    assert_eq!(
        payload["skipped"][0]["reason"],
        "missing HelmRepository missing-repo"
    );
    assert_eq!(
        payload["skipped"][0]["reason_code"],
        "missing_helm_repository"
    );
    assert_eq!(payload["skipped"][0]["retryable"], false);
    assert_eq!(payload["skipped"][0]["source_url"], serde_json::Value::Null);
}

#[test]
fn planning_reports_manifest_local_chart_versions_without_identity_as_unresolved() {
    let mut inventory = Inventory::new(PathBuf::from("/repo"));
    inventory
        .unresolved_chart_targets
        .push(fluxrepo_update::models::HelmReleaseTarget {
            path: PathBuf::from("/repo/overlay.yaml"),
            document_index: 0,
            resource_id: ResourceId {
                kind: "HelmRelease".to_string(),
                name: "demo".to_string(),
                namespace: None,
            },
            chart_name: None,
            repo_name: None,
            current_version: Some("1.0.0".to_string()),
            source_path: Some(PathBuf::from("/repo/overlay.yaml")),
            source_is_inherited: false,
        });

    let report = plan_updates(
        &inventory,
        &StaticVersionResolver::new(HashMap::new()),
        &StaticImageVersionResolver::new(HashMap::new()),
    );
    let payload = report.to_json_value(Path::new("/repo"), "plan", true, 0, 0);

    assert_eq!(payload["summary"]["skipped_count"], 1);
    assert_eq!(
        payload["skipped"][0]["reason_code"],
        "missing_chart_identity"
    );
    assert_eq!(
        payload["skipped"][0]["yaml_path"],
        "spec.chart.spec.version"
    );
    assert!(
        payload["skipped"][0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("v1:"))
    );
}

#[test]
fn update_report_serializes_retryable_chart_request_skip_with_source_url() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(500, "temporary failure")]);
    let mut inventory = Inventory::new(PathBuf::from("/repo"));
    inventory.repositories.insert(
        "demo-repo".to_string(),
        HelmRepository {
            name: "demo-repo".to_string(),
            url: server.base_url.clone(),
            repo_type: RepoType::Default,
            path: PathBuf::from("/repo/repository.yaml"),
            document_index: 0,
        },
    );
    inventory
        .chart_targets
        .push(fluxrepo_update::models::HelmReleaseTarget {
            path: PathBuf::from("/repo/release.yaml"),
            document_index: 0,
            resource_id: ResourceId {
                kind: "HelmRelease".to_string(),
                name: "demo".to_string(),
                namespace: Some("default".to_string()),
            },
            chart_name: Some("demo".to_string()),
            repo_name: Some("demo-repo".to_string()),
            current_version: Some("1.0.0".to_string()),
            source_path: Some(PathBuf::from("/repo/release.yaml")),
            source_is_inherited: false,
        });

    let report = plan_updates(
        &inventory,
        &RepositoryChartResolver::default(),
        &StaticImageVersionResolver::new(HashMap::new()),
    );
    let payload = report.to_json_value(Path::new("/repo"), "plan", true, 0, 0);

    assert_eq!(payload["skipped"][0]["path"], "release.yaml");
    assert_eq!(payload["skipped"][0]["reason_code"], "chart_request_failed");
    assert_eq!(payload["skipped"][0]["retryable"], true);
    assert_eq!(
        payload["skipped"][0]["source_url"],
        format!("{}/index.yaml", server.base_url)
    );
    let reason = payload["skipped"][0]["reason"].as_str().expect("reason");
    assert!(reason.contains("500"));
    assert!(reason.contains(&format!("{}/index.yaml", server.base_url)));
    server.finish();
}

#[test]
fn update_report_serializes_permanent_image_skip_codes() {
    let mut inventory = Inventory::new(PathBuf::from("/repo"));
    inventory.image_bindings.extend([
        image_binding("/repo/latest.yaml", "latest", "example/app:latest"),
        image_binding("/repo/missing-tag.yaml", "missing-tag", "example/app"),
        image_binding("/repo/digest.yaml", "digest", "example/app@sha256:deadbeef"),
    ]);

    let report = plan_updates(
        &inventory,
        &StaticVersionResolver::new(HashMap::new()),
        &fluxrepo_update::resolvers::RegistryImageResolver::default(),
    );
    let payload = report.to_json_value(Path::new("/repo"), "plan", true, 0, 0);
    let skipped = payload["skipped"].as_array().expect("skipped array");

    let compact = skipped
        .iter()
        .map(|item| {
            (
                item["path"].as_str().expect("path"),
                item["reason_code"].as_str().expect("reason code"),
                item["retryable"].as_bool().expect("retryable"),
                item["source_url"].is_null(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        compact,
        vec![
            ("latest.yaml", "mutable_image_tag", false, true),
            (
                "missing-tag.yaml",
                "image_reference_missing_tag",
                false,
                true,
            ),
            (
                "digest.yaml",
                "image_reference_pinned_by_digest",
                false,
                true,
            ),
        ]
    );
}

#[test]
fn resolves_multiple_image_bindings_concurrently() {
    struct ConcurrentImageResolver {
        active_calls: AtomicUsize,
        max_active_calls: AtomicUsize,
    }

    impl ImageVersionResolver for ConcurrentImageResolver {
        fn resolve(&self, image: &str) -> anyhow::Result<String> {
            let active = self.active_calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_calls.fetch_max(active, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(50));
            self.active_calls.fetch_sub(1, Ordering::SeqCst);
            Ok(image.replace(":1.0.0", ":1.0.1"))
        }
    }

    let inventory = Inventory {
        repo_root: PathBuf::from("/repo"),
        image_bindings: (0..4)
            .map(|index| ImageBinding {
                path: PathBuf::from(format!("/repo/service-{index}.yaml")),
                document_index: 0,
                resource_id: ResourceId {
                    kind: "Deployment".to_string(),
                    name: format!("service-{index}"),
                    namespace: Some("default".to_string()),
                },
                yaml_path: "spec.template.spec.containers[0].image".to_string(),
                image: format!("example/service-{index}:1.0.0"),
                value_kind: ImageBindingValueKind::ImageReference,
            })
            .collect(),
        ..Inventory::new(PathBuf::from("/repo"))
    };
    let resolver = ConcurrentImageResolver {
        active_calls: AtomicUsize::new(0),
        max_active_calls: AtomicUsize::new(0),
    };

    let report = plan_updates(
        &inventory,
        &StaticVersionResolver::new(HashMap::new()),
        &resolver,
    );

    assert_eq!(report.planned.len(), 4);
    assert!(resolver.max_active_calls.load(Ordering::SeqCst) > 1);
}

#[test]
fn planning_options_can_force_sequential_resolution() {
    struct SequentialImageResolver {
        active_calls: AtomicUsize,
        max_active_calls: AtomicUsize,
    }

    impl ImageVersionResolver for SequentialImageResolver {
        fn resolve(&self, image: &str) -> anyhow::Result<String> {
            let active = self.active_calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_calls.fetch_max(active, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(10));
            self.active_calls.fetch_sub(1, Ordering::SeqCst);
            Ok(image.replace(":1.0.0", ":1.0.1"))
        }
    }

    let inventory = Inventory {
        repo_root: PathBuf::from("/repo"),
        image_bindings: (0..3)
            .map(|index| ImageBinding {
                path: PathBuf::from(format!("/repo/service-{index}.yaml")),
                document_index: 0,
                resource_id: ResourceId {
                    kind: "Deployment".to_string(),
                    name: format!("service-{index}"),
                    namespace: Some("default".to_string()),
                },
                yaml_path: "spec.template.spec.containers[0].image".to_string(),
                image: format!("example/service-{index}:1.0.0"),
                value_kind: ImageBindingValueKind::ImageReference,
            })
            .collect(),
        ..Inventory::new(PathBuf::from("/repo"))
    };
    let resolver = SequentialImageResolver {
        active_calls: AtomicUsize::new(0),
        max_active_calls: AtomicUsize::new(0),
    };

    let report = plan_updates_with_options(
        &inventory,
        &StaticVersionResolver::new(HashMap::new()),
        &resolver,
        PlanOptions { max_workers: 1 },
    );

    assert_eq!(report.planned.len(), 3);
    assert_eq!(resolver.max_active_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn sequential_and_concurrent_plans_have_identical_order_ids_and_reasons() {
    let mut inventory = Inventory::new(PathBuf::from("/repo"));
    inventory.image_bindings.extend([
        image_binding("/repo/c.yaml", "c", "example/c:1.0.0"),
        image_binding("/repo/a.yaml", "a", "example/a:1.0.0"),
        image_binding("/repo/b.yaml", "b", "example/b:latest"),
    ]);
    let resolver = StaticImageVersionResolver::new(HashMap::from([
        ("example/a:1.0.0".to_string(), "example/a:2.0.0".to_string()),
        ("example/c:1.0.0".to_string(), "example/c:3.0.0".to_string()),
    ]));
    let chart_resolver = StaticVersionResolver::new(HashMap::new());

    let sequential = plan_updates_with_options(
        &inventory,
        &chart_resolver,
        &resolver,
        PlanOptions { max_workers: 1 },
    );
    let concurrent = plan_updates_with_options(
        &inventory,
        &chart_resolver,
        &resolver,
        PlanOptions { max_workers: 4 },
    );

    assert_eq!(
        sequential.to_json_value(Path::new("/repo"), "plan", true, 0, 0),
        concurrent.to_json_value(Path::new("/repo"), "plan", true, 0, 0)
    );
}

#[test]
fn preserves_input_order_for_skipped_image_bindings() {
    struct DelayedImageResolver;

    impl ImageVersionResolver for DelayedImageResolver {
        fn resolve(&self, image: &str) -> anyhow::Result<String> {
            let delay = match image {
                "example/service-one:1.0.0" => 30,
                "example/service-two:1.0.0" => 10,
                "example/service-three:1.0.0" => 20,
                _ => 0,
            };
            thread::sleep(Duration::from_millis(delay));
            if image != "example/service-three:1.0.0" {
                let service_name = image
                    .split('/')
                    .nth(1)
                    .and_then(|segment| segment.split(':').next())
                    .unwrap_or("service");
                anyhow::bail!("failed for {service_name}");
            }
            Ok("example/service-three:1.0.1".to_string())
        }
    }

    let mut inventory = Inventory::new(PathBuf::from("/repo"));
    inventory.image_bindings.extend([
        image_binding(
            "/repo/service-one.yaml",
            "service-one",
            "example/service-one:1.0.0",
        ),
        image_binding(
            "/repo/service-two.yaml",
            "service-two",
            "example/service-two:1.0.0",
        ),
        image_binding(
            "/repo/service-three.yaml",
            "service-three",
            "example/service-three:1.0.0",
        ),
    ]);

    let report = plan_updates(
        &inventory,
        &StaticVersionResolver::new(HashMap::new()),
        &DelayedImageResolver,
    );

    assert_eq!(
        report
            .planned
            .iter()
            .map(|item| item
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string())
            .collect::<Vec<_>>(),
        vec!["service-three.yaml"]
    );
    assert_eq!(
        report
            .skipped
            .iter()
            .map(|skip| (skip.path.as_deref(), skip.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                Some(Path::new("/repo/service-one.yaml")),
                "failed for service-one"
            ),
            (
                Some(Path::new("/repo/service-two.yaml")),
                "failed for service-two"
            ),
        ]
    );
}

#[test]
fn update_report_serializes_non_repo_path_skip_reason() {
    let report = UpdateReport {
        planned: Vec::new(),
        skipped: vec![SkippedUpdate::new(None, "network timeout")],
    };

    let payload = report.to_json_value(Path::new("/repo"), "plan", true, 0, 0);

    assert_eq!(
        payload["skipped"],
        serde_json::json!([{
            "id": "v1::unresolved:unclassified",
            "path": "",
            "yaml_path": null,
            "reason": "network timeout",
            "reason_code": "unclassified",
            "retryable": false,
            "source_url": null
        }])
    );
}

#[test]
fn apply_updates_rejects_non_mapping_helmrelease_documents() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("release.yaml");
    write_file(&path, "- not\n- a\n- mapping\n");
    let report = UpdateReport {
        planned: vec![PlannedUpdate::chart(
            path,
            0,
            "demo".to_string(),
            "demo".to_string(),
            "demo".to_string(),
            "1.0.0".to_string(),
            "1.0.1".to_string(),
            false,
        )],
        skipped: Vec::new(),
    };

    let error = apply_updates(&report).expect_err("reject non-mapping document");

    assert!(
        error
            .to_string()
            .contains("HelmRelease document must be a YAML mapping")
    );
}

#[test]
fn apply_updates_rejects_a_chart_scalar_changed_after_planning() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("release.yaml");
    let changed = "kind: HelmRelease\nspec: {chart: {spec: {version: 1.5.0}}}\n";
    write_file(&path, changed);
    let report = UpdateReport {
        planned: vec![PlannedUpdate::chart(
            path.clone(),
            0,
            "demo".to_string(),
            "demo".to_string(),
            "demo".to_string(),
            "1.0.0".to_string(),
            "2.0.0".to_string(),
            false,
        )],
        skipped: Vec::new(),
    };

    let error = apply_updates(&report).expect_err("reject stale chart plan");

    assert!(error.to_string().contains("changed after planning"));
    assert_eq!(fs::read_to_string(path).expect("read release"), changed);
}

#[test]
fn apply_updates_rejects_an_image_scalar_changed_after_planning() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("pod.yaml");
    let changed = "kind: Pod\nspec: {containers: [{image: example/demo:1.5.0}]}\n";
    write_file(&path, changed);
    let report = UpdateReport {
        planned: vec![PlannedUpdate::image(
            path.clone(),
            0,
            "demo".to_string(),
            "spec.containers[0].image".to_string(),
            "example/demo:1.0.0".to_string(),
            "example/demo:2.0.0".to_string(),
            "1.0.0".to_string(),
            "2.0.0".to_string(),
            ImageBindingValueKind::ImageReference,
        )],
        skipped: Vec::new(),
    };

    let error = apply_updates(&report).expect_err("reject stale image plan");

    assert!(error.to_string().contains("changed after planning"));
    assert_eq!(fs::read_to_string(path).expect("read pod"), changed);
}

#[test]
fn apply_updates_prepares_every_file_before_writing_any_file() {
    let temp = tempfile::tempdir().expect("temp dir");
    let first_path = temp.path().join("a.yaml");
    let second_path = temp.path().join("b.yaml");
    let manifest = "kind: Pod\nmetadata:\n  name: demo\nspec:\n  containers:\n    - image: example/demo:1.0.0\n";
    write_file(&first_path, manifest);
    write_file(&second_path, manifest);
    let update = |path: PathBuf, yaml_path: &str| {
        PlannedUpdate::image(
            path,
            0,
            "demo".to_string(),
            yaml_path.to_string(),
            "example/demo:1.0.0".to_string(),
            "example/demo:2.0.0".to_string(),
            "1.0.0".to_string(),
            "2.0.0".to_string(),
            ImageBindingValueKind::ImageReference,
        )
    };
    let report = UpdateReport {
        planned: vec![
            update(first_path.clone(), "spec.containers[0].image"),
            update(second_path, "spec.missing[0].image"),
        ],
        skipped: Vec::new(),
    };

    apply_updates(&report).expect_err("later transformation should fail");

    assert_eq!(
        fs::read_to_string(first_path).expect("read first file"),
        manifest
    );
}

#[cfg(unix)]
#[test]
fn apply_updates_reports_partial_application_after_a_later_write_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temp dir");
    let first_path = temp.path().join("a.yaml");
    let second_path = temp.path().join("b.yaml");
    let manifest =
        "kind: Pod\nmetadata: {name: demo}\nspec: {containers: [{image: example/demo:1.0.0}]}\n";
    write_file(&first_path, manifest);
    write_file(&second_path, manifest);
    fs::set_permissions(&second_path, fs::Permissions::from_mode(0o444))
        .expect("make second file read-only");
    let update = |path: PathBuf| {
        PlannedUpdate::image(
            path,
            0,
            "demo".to_string(),
            "spec.containers[0].image".to_string(),
            "example/demo:1.0.0".to_string(),
            "example/demo:2.0.0".to_string(),
            "1.0.0".to_string(),
            "2.0.0".to_string(),
            ImageBindingValueKind::ImageReference,
        )
    };
    let report = UpdateReport {
        planned: vec![update(first_path.clone()), update(second_path.clone())],
        skipped: Vec::new(),
    };

    let error = apply_updates(&report).expect_err("second write should fail");

    assert!(error.to_string().contains("after 1 file(s) were applied"));
    assert!(
        error
            .to_string()
            .contains(&second_path.display().to_string())
    );
    assert!(
        fs::read_to_string(first_path)
            .expect("read first file")
            .contains("example/demo:2.0.0")
    );
}

#[test]
fn apply_updates_supports_terminal_list_indexes_in_yaml_paths() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("deployment.yaml");
    write_file(
        &path,
        r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: demo
spec:
  template:
    spec:
      containers:
        - old
"#,
    );
    let report = UpdateReport {
        planned: vec![PlannedUpdate::image(
            path.clone(),
            0,
            "demo".to_string(),
            "spec.template.spec.containers[0]".to_string(),
            "old".to_string(),
            "new".to_string(),
            "old".to_string(),
            "new".to_string(),
            ImageBindingValueKind::ImageReference,
        )],
        skipped: Vec::new(),
    };

    apply_updates(&report).expect("apply terminal list index");

    assert!(
        fs::read_to_string(path)
            .expect("read updated deployment")
            .contains("- new")
    );
}

#[test]
fn apply_updates_preserves_yaml_formatting_around_chart_scalar_changes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("release.yaml");
    let original = r#"apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: demo
spec:
  chart:
    spec:
      chart: demo
      version: "1.0.0" # keep the quote style and comment
  values:
    env:
      - name: DEMO
        value: "unchanged"
"#;
    write_file(&path, original);
    let report = UpdateReport {
        planned: vec![PlannedUpdate::chart(
            path.clone(),
            0,
            "demo".to_string(),
            "demo".to_string(),
            "demo".to_string(),
            "1.0.0".to_string(),
            "1.0.1".to_string(),
            false,
        )],
        skipped: Vec::new(),
    };

    apply_updates(&report).expect("apply chart update");

    assert_eq!(
        fs::read_to_string(path).expect("read updated release"),
        original.replace(r#"version: "1.0.0""#, r#"version: "1.0.1""#)
    );
}

#[test]
fn apply_updates_preserves_yaml_formatting_around_deployment_image_changes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("deployment.yaml");
    let original = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: demo
spec:
  template:
    spec:
      containers:
      - name: demo
        image: "example/demo:1.0.0" # keep this comment
      - name: sidecar
        image: "example/sidecar:1.0.0"
---
apiVersion: v1
kind: Service
metadata:
  name: demo
"#;
    write_file(&path, original);
    let report = UpdateReport {
        planned: vec![PlannedUpdate::image(
            path.clone(),
            0,
            "demo".to_string(),
            "spec.template.spec.containers[0].image".to_string(),
            "example/demo:1.0.0".to_string(),
            "example/demo:1.0.1".to_string(),
            "1.0.0".to_string(),
            "1.0.1".to_string(),
            ImageBindingValueKind::ImageReference,
        )],
        skipped: Vec::new(),
    };

    apply_updates(&report).expect("apply deployment update");

    assert_eq!(
        fs::read_to_string(path).expect("read updated deployment"),
        original.replace(
            r#"image: "example/demo:1.0.0""#,
            r#"image: "example/demo:1.0.1""#
        )
    );
}

#[test]
fn apply_updates_preserves_crlf_line_endings_around_scalar_changes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("release.yaml");
    let original = "apiVersion: helm.toolkit.fluxcd.io/v2\r\nkind: HelmRelease\r\nmetadata:\r\n  name: demo\r\nspec:\r\n  chart:\r\n    spec:\r\n      chart: demo\r\n      version: \"1.0.0\"\r\n";
    write_file(&path, original);
    let report = UpdateReport {
        planned: vec![PlannedUpdate::chart(
            path.clone(),
            0,
            "demo".to_string(),
            "demo".to_string(),
            "demo".to_string(),
            "1.0.0".to_string(),
            "1.0.1".to_string(),
            false,
        )],
        skipped: Vec::new(),
    };

    apply_updates(&report).expect("apply chart update");

    assert_eq!(
        fs::read_to_string(path).expect("read updated release"),
        original.replace("1.0.0", "1.0.1")
    );
}

#[test]
fn apply_updates_preserves_missing_final_newline_around_scalar_changes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("release.yaml");
    let original = r#"apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: demo
spec:
  chart:
    spec:
      chart: demo
      version: "1.0.0""#;
    write_file(&path, original);
    let report = UpdateReport {
        planned: vec![PlannedUpdate::chart(
            path.clone(),
            0,
            "demo".to_string(),
            "demo".to_string(),
            "demo".to_string(),
            "1.0.0".to_string(),
            "1.0.1".to_string(),
            false,
        )],
        skipped: Vec::new(),
    };

    apply_updates(&report).expect("apply chart update");

    assert_eq!(
        fs::read_to_string(path).expect("read updated release"),
        original.replace("1.0.0", "1.0.1")
    );
}

fn image_binding(path: &str, name: &str, image: &str) -> ImageBinding {
    ImageBinding {
        path: PathBuf::from(path),
        document_index: 0,
        resource_id: ResourceId {
            kind: "Deployment".to_string(),
            name: name.to_string(),
            namespace: Some("default".to_string()),
        },
        yaml_path: "spec.template.spec.containers[0].image".to_string(),
        image: image.to_string(),
        value_kind: ImageBindingValueKind::ImageReference,
    }
}
