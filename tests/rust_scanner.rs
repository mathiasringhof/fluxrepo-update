mod common;

use std::path::{Path, PathBuf};

use common::{fixture_root, write_file};
use fluxrepo_update::scanner::scan_repo;

#[test]
fn scanner_links_patch_versions_back_to_base_release() {
    let repo_root = fixture_root();
    let inventory = scan_repo(&repo_root).expect("scan fixture");

    let patch_target = inventory
        .chart_targets
        .iter()
        .find(|target| {
            target.path.strip_prefix(&repo_root).expect("relative path")
                == Path::new("apps/production/paperless/release-patch.yaml")
        })
        .expect("patch target");

    assert_eq!(patch_target.chart_name.as_deref(), Some("paperless-ngx"));
    assert_eq!(patch_target.repo_name.as_deref(), Some("truecharts"));
    assert_eq!(patch_target.current_version.as_deref(), Some("11.29.10"));
    assert!(patch_target.source_is_inherited);
    assert_eq!(
        patch_target
            .source_path
            .as_ref()
            .expect("source path")
            .strip_prefix(&repo_root)
            .expect("relative source"),
        Path::new("apps/base/paperless-ngx/release.yaml")
    );
}

#[test]
fn scanner_keeps_values_only_overlays_out_of_chart_targets() {
    let repo_root = fixture_root();
    let inventory = scan_repo(&repo_root).expect("scan fixture");

    let without_chart_version_paths: Vec<_> = inventory
        .helmreleases_without_chart_version
        .iter()
        .map(|target| {
            target
                .path
                .strip_prefix(&repo_root)
                .expect("relative path")
                .to_path_buf()
        })
        .collect();

    assert!(without_chart_version_paths.contains(&PathBuf::from(
        "apps/production/audiobookshelf/release-patch.yaml"
    )));
    assert!(
        without_chart_version_paths
            .contains(&PathBuf::from("apps/production/uptimekuma-values.yaml"))
    );
}

#[test]
fn scanner_reports_image_references_and_deployment_targets() {
    let repo_root = fixture_root();
    let inventory = scan_repo(&repo_root).expect("scan fixture");

    assert!(inventory.image_references.iter().any(|image| {
        image.path.strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/base/smokeping/deployment.yaml")
            && image.image == "lscr.io/linuxserver/smokeping:latest"
    }));
    assert!(inventory.image_references.iter().any(|image| {
        image.path.strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/production/immich/release-patch.yaml")
            && image.image == "docker.io/valkey/valkey:9.0-alpine@sha256:1be494495248d53e3558b198a1c704e6b559d5e99fe4c926e14a8ad24d76c6fa"
    }));
    assert!(inventory.deployment_targets.iter().any(|target| {
        target.path.strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/base/sonarr/deployment.yaml")
            && target.resource_id.name == "sonarr-deployment"
            && target.yaml_path == "spec.template.spec.containers[0].image"
            && target.image == "linuxserver/sonarr:version-4.0.16.2944"
    }));
    assert!(inventory.deployment_targets.iter().any(|target| {
        target.path.strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/production/openssh/deployment.yaml")
            && target.resource_id.name == "openssh-deployment"
            && target.yaml_path == "spec.template.spec.initContainers[0].image"
            && target.image == "alpine:3.22"
    }));
}

#[test]
fn scanner_reports_chart_targets_with_missing_source_metadata_as_unresolved() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("release.yaml"),
        r#"apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: demo
  namespace: default
spec:
  chart:
    spec:
      version: "1.2.3"
"#,
    );

    let inventory = scan_repo(&repo_root).expect("scan temp repo");

    assert!(inventory.chart_targets.is_empty());
    assert_eq!(
        inventory
            .unresolved_chart_targets
            .iter()
            .map(|target| target.resource_id.name.as_str())
            .collect::<Vec<_>>(),
        vec!["demo"]
    );
}
