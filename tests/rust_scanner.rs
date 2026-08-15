mod common;

use std::path::{Path, PathBuf};

use common::{fixture_root, write_file};
use fluxrepo_update::scanner::scan_repo;

#[test]
fn scanner_does_not_inherit_chart_identity_for_patch_versions() {
    let repo_root = fixture_root();
    let inventory = scan_repo(&repo_root).expect("scan fixture");

    let patch_target = inventory
        .unresolved_chart_targets
        .iter()
        .find(|target| {
            target.path.strip_prefix(&repo_root).expect("relative path")
                == Path::new("apps/production/paperless/release-patch.yaml")
        })
        .expect("patch target");

    assert_eq!(patch_target.chart_name, None);
    assert_eq!(patch_target.repo_name, None);
    assert_eq!(patch_target.current_version.as_deref(), Some("11.29.10"));
    assert!(!patch_target.source_is_inherited);
    assert_eq!(
        patch_target
            .source_path
            .as_ref()
            .expect("source path")
            .strip_prefix(&repo_root)
            .expect("relative source"),
        Path::new("apps/production/paperless/release-patch.yaml")
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
fn scanner_reports_image_references_and_image_bindings() {
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
    assert!(inventory.image_bindings.iter().any(|target| {
        target.path.strip_prefix(&repo_root).expect("relative path")
            == Path::new("apps/base/sonarr/deployment.yaml")
            && target.resource_id.name == "sonarr-deployment"
            && target.yaml_path == "spec.template.spec.containers[0].image"
            && target.image == "linuxserver/sonarr:version-4.0.16.2944"
    }));
    assert!(inventory.image_bindings.iter().any(|target| {
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

#[test]
fn scanner_requires_a_manifest_local_helm_repository_source_kind() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("release.yaml"),
        r"kind: HelmRelease
metadata: {name: demo}
spec:
  chart:
    spec:
      chart: demo
      version: 1.0.0
      sourceRef:
        kind: GitRepository
        name: same-name
",
    );

    let inventory = scan_repo(&repo_root).expect("scan temp repo");

    assert!(inventory.chart_targets.is_empty());
    assert_eq!(inventory.unresolved_chart_targets.len(), 1);
    assert_eq!(inventory.unresolved_chart_targets[0].repo_name, None);
}

#[test]
fn scanner_discovers_yml_and_keeps_helmreleases_manifest_local() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("base.yml"),
        r"kind: HelmRelease
metadata:
  name: demo
spec:
  chart:
    spec:
      chart: demo
      version: 1.0.0
      sourceRef:
        kind: HelmRepository
        name: public
",
    );
    write_file(
        &repo_root.join("overlay.yaml"),
        r"kind: HelmRelease
metadata:
  name: demo
spec:
  chart:
    spec:
      version: 1.1.0
",
    );

    let inventory = scan_repo(&repo_root).expect("scan temp repo");

    assert_eq!(inventory.chart_targets.len(), 1);
    assert_eq!(inventory.unresolved_chart_targets.len(), 1);
    assert!(!inventory.chart_targets[0].source_is_inherited);
    assert_eq!(
        inventory.unresolved_chart_targets[0]
            .path
            .file_name()
            .and_then(|name| name.to_str()),
        Some("overlay.yaml")
    );
}

#[test]
fn scanner_discovers_standard_podspec_images_and_recursive_helm_value_bindings() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("images.yaml"),
        r"kind: StatefulSet
metadata:
  name: database
spec:
  template:
    spec:
      initContainers:
        - image: registry.example/init:1.0.0
      containers:
        - image: registry.example/database:2.0.0
---
kind: HelmRelease
metadata:
  name: dashboard
spec:
  values:
    sidecar:
      image:
        registry: registry.example
        repository: helpers/sidecar
        tag: 3.0.0
    nested:
      image:
        repository: registry.example/metrics
        tag: 4.0.0
",
    );

    let inventory = scan_repo(&repo_root).expect("scan temp repo");
    let bindings = inventory
        .image_bindings
        .iter()
        .map(|target| {
            (
                target.resource_id.kind.as_str(),
                target.yaml_path.as_str(),
                target.image.as_str(),
            )
        })
        .collect::<Vec<_>>();

    assert!(bindings.contains(&(
        "StatefulSet",
        "spec.template.spec.initContainers[0].image",
        "registry.example/init:1.0.0"
    )));
    assert!(bindings.contains(&(
        "StatefulSet",
        "spec.template.spec.containers[0].image",
        "registry.example/database:2.0.0"
    )));
    assert!(bindings.contains(&(
        "HelmRelease",
        "spec.values.sidecar.image.tag",
        "registry.example/helpers/sidecar:3.0.0"
    )));
    assert!(bindings.contains(&(
        "HelmRelease",
        "spec.values.nested.image.tag",
        "registry.example/metrics:4.0.0"
    )));
}

#[test]
fn scanner_discovers_repository_tag_mappings_at_any_helm_values_depth() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("release.yaml"),
        r"kind: HelmRelease
metadata:
  name: dashboard
spec:
  values:
    metrics:
      repository: registry.example/metrics
      tag: 1.2.3
",
    );

    let inventory = scan_repo(&repo_root).expect("scan temp repo");

    assert!(inventory.image_bindings.iter().any(|binding| {
        binding.yaml_path == "spec.values.metrics.tag"
            && binding.image == "registry.example/metrics:1.2.3"
    }));
}

#[test]
fn scanner_represents_one_shared_helm_value_tag_as_one_image_binding() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    write_file(
        &repo_root.join("release.yaml"),
        r"kind: HelmRelease
metadata: {name: dashboard}
spec:
  values:
    sharedImage:
      repository: registry.example/shared
      tag: 1.2.3
",
    );

    let inventory = scan_repo(&repo_root).expect("scan temp repo");

    assert_eq!(inventory.image_bindings.len(), 1);
    assert_eq!(
        inventory.image_bindings[0].yaml_path,
        "spec.values.sharedImage.tag"
    );
}

#[cfg(unix)]
#[test]
fn scanner_excludes_yaml_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path().join("repo");
    let outside = temp.path().join("outside.yaml");
    write_file(
        &outside,
        "kind: Pod\nmetadata:\n  name: linked\nspec:\n  containers:\n    - image: example/linked:1.0.0\n",
    );
    std::fs::create_dir_all(&repo_root).expect("create repo");
    symlink(&outside, repo_root.join("linked.yaml")).expect("create symlink");

    let inventory = scan_repo(&repo_root).expect("scan temp repo");

    assert!(inventory.image_bindings.is_empty());
}
