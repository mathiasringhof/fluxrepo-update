mod common;

use std::path::{Path, PathBuf};

use common::{fixture_root, write_file};
use fluxrepo_update::scanner::scan_repo;

fn namespace_manifest(root: &Path) {
    write_file(
        &root.join("base/source.yaml"),
        "kind: HelmRepository\nmetadata: {name: demo, namespace: raw}\nspec: {url: https://charts.example.org}\n",
    );
}

#[test]
fn namespace_transform_follows_local_directories_and_outermost_namespace_wins() {
    let temp = tempfile::tempdir().unwrap();
    namespace_manifest(temp.path());
    for (path, text) in [
        (
            "base/kustomization.yml",
            "namespace: inner\nresources: [source.yaml]\n",
        ),
        (
            "middle/kustomization.yaml",
            "kind: Kustomization\nresources: [../base]\n",
        ),
        (
            "kustomization.yaml",
            "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nnamespace: outer\nresources: [middle]\n",
        ),
    ] {
        write_file(&temp.path().join(path), text);
    }
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    let source = &inventory["repositories"][0];
    assert_eq!(source["namespace"], "raw");
    assert_eq!(source["effective_namespace"], "outer");
    assert_eq!(
        source["namespace_kustomizations"],
        serde_json::json!(["kustomization.yaml"])
    );
    assert_eq!(source["namespace_ambiguous"], false);
}

#[test]
fn namespace_transform_reports_all_agreeing_chains_and_conflicting_chains() {
    for second in [Some("app"), Some("other"), None] {
        let temp = tempfile::tempdir().unwrap();
        namespace_manifest(temp.path());
        for (directory, namespace) in [("a", Some("app")), ("b", second)] {
            let namespace =
                namespace.map_or_else(String::new, |value| format!("namespace: {value}\n"));
            write_file(
                &temp.path().join(directory).join("kustomization.yaml"),
                &format!("kind: Kustomization\n{namespace}resources: [../base/source.yaml]\n"),
            );
        }
        let inventory = scan_repo(temp.path()).unwrap().to_json_value();
        let source = &inventory["repositories"][0];
        assert_eq!(source["namespace_ambiguous"], second != Some("app"));
        assert_eq!(
            source["effective_namespace"],
            if second == Some("app") {
                serde_json::json!("app")
            } else {
                serde_json::Value::Null
            }
        );
        assert_eq!(
            source["namespace_kustomizations"][0],
            "a/kustomization.yaml"
        );
        if second.is_some() {
            assert_eq!(
                source["namespace_kustomizations"][1],
                "b/kustomization.yaml"
            );
        }
    }
}

#[test]
fn namespace_transform_ignores_flux_objects_and_unsupported_inclusion_fields() {
    for (filename, text) in [
        (
            "kustomization.yaml",
            "apiVersion: kustomize.toolkit.fluxcd.io/v1\nkind: Kustomization\nnamespace: ignored\nspec: {targetNamespace: ignored}\nresources: [base/source.yaml]\n",
        ),
        (
            "other.yaml",
            "kind: Kustomization\nnamespace: ignored\nresources: [base/source.yaml]\n",
        ),
        (
            "kustomization.yaml",
            "kind: Kustomization\nnamespace: ignored\nbases: [base]\ncomponents: [base]\npatches: [{path: base/source.yaml}]\nresources: [https://example.org/base/source.yaml, github.com/example/base?ref=v1]\n",
        ),
        (
            "kustomization.yaml",
            "kind: Kustomization\nnamespace: ignored\nresources: [base]\n",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        namespace_manifest(temp.path());
        write_file(&temp.path().join(filename), text);
        let inventory = scan_repo(temp.path()).unwrap().to_json_value();
        let source = &inventory["repositories"][0];
        assert_eq!(source["effective_namespace"], "raw", "{text}");
        assert_eq!(source["namespace_kustomizations"], serde_json::json!([]));
        assert_eq!(source["namespace_ambiguous"], false);
    }
}

#[test]
fn namespace_transform_cycles_are_unresolved() {
    let temp = tempfile::tempdir().unwrap();
    namespace_manifest(temp.path());
    write_file(
        &temp.path().join("base/kustomization.yaml"),
        "kind: Kustomization\nnamespace: inner\nresources: [source.yaml, ..]\n",
    );
    write_file(
        &temp.path().join("kustomization.yaml"),
        "kind: Kustomization\nnamespace: outer\nresources: [base]\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["repositories"][0]["namespace_ambiguous"], true);
    assert!(inventory["repositories"][0]["effective_namespace"].is_null());
}

#[test]
fn namespace_transform_recurses_only_through_directory_references() {
    let temp = tempfile::tempdir().unwrap();
    namespace_manifest(temp.path());
    write_file(
        &temp.path().join("base/kustomization.yaml"),
        "kind: Kustomization\nnamespace: inner\nresources: [source.yaml]\n",
    );
    write_file(
        &temp.path().join("kustomization.yaml"),
        "kind: Kustomization\nnamespace: outer\nresources: [base/kustomization.yaml]\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["repositories"][0]["effective_namespace"], "inner");
    assert_eq!(
        inventory["repositories"][0]["namespace_kustomizations"],
        serde_json::json!(["base/kustomization.yaml"])
    );
}

#[test]
fn namespace_transform_compares_raw_fallback_per_document() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        &temp.path().join("sources.yaml"),
        "kind: HelmRepository\nmetadata: {name: agrees, namespace: app}\nspec: {url: https://charts.example.org}\n---\nkind: HelmRepository\nmetadata: {name: disagrees, namespace: raw}\nspec: {url: https://charts.example.org}\n",
    );
    write_file(
        &temp.path().join("a/kustomization.yaml"),
        "namespace: app\nresources: [../sources.yaml]\n",
    );
    write_file(
        &temp.path().join("b/kustomization.yaml"),
        "resources: [../sources.yaml]\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["repositories"][0]["namespace_ambiguous"], false);
    assert_eq!(inventory["repositories"][0]["effective_namespace"], "app");
    assert_eq!(inventory["repositories"][1]["namespace_ambiguous"], true);
    assert!(inventory["repositories"][1]["effective_namespace"].is_null());
}

#[test]
fn namespace_transform_ignores_paths_through_missing_directories() {
    let temp = tempfile::tempdir().unwrap();
    namespace_manifest(temp.path());
    write_file(
        &temp.path().join("kustomization.yaml"),
        "namespace: ignored\nresources: [missing/../base/source.yaml]\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["repositories"][0]["effective_namespace"], "raw");
}

#[cfg(unix)]
#[test]
fn namespace_transform_ignores_paths_through_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    namespace_manifest(temp.path());
    std::os::unix::fs::symlink(temp.path().join("base"), temp.path().join("link")).unwrap();
    write_file(
        &temp.path().join("kustomization.yaml"),
        "namespace: ignored\nresources: [link/../base/source.yaml]\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["repositories"][0]["effective_namespace"], "raw");
}

#[test]
fn scanner_excludes_only_bootstrap_files_directly_inside_flux_system() {
    let temp = tempfile::tempdir().expect("temp dir");
    let manifests = [
        "clusters/demo/flux-system/gotk-components.yaml",
        "clusters/demo/my-flux-system/gotk-app.yaml",
        "clusters/demo/flux-system/gotk-apps/deployment.yaml",
        "clusters/demo/flux-system/application.yaml",
    ];
    for path in manifests {
        write_file(
            &temp.path().join(path),
            "kind: Pod\nmetadata: {name: app}\nspec:\n  containers:\n  - image: example/app:1.0.0\n",
        );
    }

    let inventory = scan_repo(temp.path()).expect("scan");

    assert_eq!(inventory.image_bindings.len(), 3);
    assert_eq!(inventory.skipped_paths.len(), 1);
    assert_eq!(
        inventory.relative(&inventory.skipped_paths[0]),
        manifests[0]
    );
}

#[test]
fn inventory_reports_unchecked_declarations_and_includes_inactive_manifests() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("apps/base/immich/release.yaml"),
        "kind: HelmRelease\nmetadata: {name: immich}\nspec:\n  values:\n    image: {tag: v3.1.0}\n",
    );
    write_file(
        &temp
            .path()
            .join("apps/production/immich/cloudnative-pg.yaml"),
        "apiVersion: postgresql.cnpg.io/v1\nkind: Cluster\nmetadata: {name: immich}\nspec: {imageName: 'ghcr.io/tensorchord/cloudnative-vectorchord:16.9-0.4.3'}\n",
    );
    write_file(
        &temp.path().join("kustomization.yaml"),
        "kind: Kustomization\nresources:\n- https://example.org/releases/v1.8.0/operator.yaml\n# - apps/base/uptimekuma\n",
    );
    write_file(
        &temp.path().join("apps/base/uptimekuma/release.yaml"),
        "kind: HelmRelease\nmetadata: {name: uptime-kuma}\nspec:\n  chart:\n    spec:\n      chart: uptime-kuma\n      version: 4.1.0\n      sourceRef: {kind: HelmRepository, name: public}\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["scope"], "repository_manifests");
    assert_eq!(inventory["discovered_count"], 4);
    let unchecked = inventory["unchecked_version_declarations"]
        .as_array()
        .unwrap();
    assert_eq!(unchecked.len(), 3);
    assert!(
        unchecked
            .iter()
            .any(|item| item["yaml_path"] == "spec.values.image.tag"
                && item["current_value"] == "v3.1.0")
    );
    assert!(unchecked.iter().all(|item| item["document_index"] == 0
        && !item["path"].as_str().unwrap().is_empty()
        && !item["reason"].as_str().unwrap().is_empty()));
    assert_eq!(inventory["chart_targets"][0]["name"], "uptime-kuma");
}

#[test]
fn an_image_tag_with_an_empty_repository_is_counted_once_as_unchecked() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("release.yaml"),
        "kind: HelmRelease\nmetadata: {name: demo}\nspec:\n  values:\n    image: {repository: '', tag: v3.1.0}\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["discovered_count"], 1);
    assert_eq!(inventory["image_binding_count"], 0);
    assert_eq!(
        inventory["unchecked_version_declarations"][0]["current_value"],
        "v3.1.0"
    );
}

#[test]
fn kustomization_files_without_kind_still_expose_remote_resources() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("kustomization.yml"),
        "resources:\n- https://github.com/intel/intel-device-plugins-for-kubernetes/deployments/nfd?ref=v0.34.1\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["discovered_count"], 1);
    assert_eq!(
        inventory["remote_resource_targets"][0]["yaml_path"],
        "resources[0]"
    );
}

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

#[test]
fn scanner_preserves_image_locations_and_categories_in_nested_helm_values() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("release.yaml"),
        r#"kind: HelmRelease
metadata: {name: app}
spec:
  values:
    "nested.list":
      - image: example/scalar:1.0.0
      - image: {repository: example/mapped, tag: 2.0.0}
      - image: {tag: 3.0.0}
      - image: {repository: example/custom, version: 4.0.0}
      - image:
          children:
            - image: {tag: 5.0.0}
            - image: example/deep:6.0.0
      - image: {tag: 7.0.0}
"#,
    );
    let inventory = scan_repo(temp.path()).expect("scan");
    let bindings = inventory
        .image_bindings
        .iter()
        .map(|binding| (binding.yaml_path.as_str(), binding.image.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        bindings,
        [
            (
                r#"spec.values["nested.list"][0].image"#,
                "example/scalar:1.0.0"
            ),
            (
                r#"spec.values["nested.list"][1].image.tag"#,
                "example/mapped:2.0.0"
            ),
            (
                r#"spec.values["nested.list"][3].image"#,
                "example/custom (version: 4.0.0)"
            ),
            (
                r#"spec.values["nested.list"][4].image.children[1].image"#,
                "example/deep:6.0.0"
            ),
        ]
    );
    assert_eq!(
        inventory
            .unchecked_version_declarations
            .iter()
            .map(|declaration| (
                declaration.yaml_path.as_str(),
                declaration.current_value.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (r#"spec.values["nested.list"][2].image.tag"#, "3.0.0"),
            (
                r#"spec.values["nested.list"][4].image.children[0].image.tag"#,
                "5.0.0"
            ),
            (r#"spec.values["nested.list"][5].image.tag"#, "7.0.0"),
        ]
    );
    assert_eq!(inventory.image_references.len(), 4);
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

#[test]
fn scanner_uses_full_identity_for_recursive_image_references() {
    let temp = tempfile::tempdir().expect("temp dir");
    write_file(
        &temp.path().join("release.yaml"),
        "kind: HelmRelease\nmetadata: {name: demo}\nspec:\n  values:\n    sharedImage:\n      registry: registry.example\n      repository: example/demo\n      tag: 3.20\n      digest: sha256:abc\n    nested:\n      image: {registry: registry.example, repository: example/sidecar, tag: 1.0.0}\n",
    );
    let inventory = scan_repo(temp.path()).expect("scan");
    assert_eq!(inventory.image_bindings.len(), 2);
    for binding in &inventory.image_bindings {
        assert!(
            inventory
                .image_references
                .iter()
                .any(|reference| reference.image == binding.image),
            "missing full image reference {}",
            binding.image
        );
    }
    assert!(
        inventory
            .image_references
            .iter()
            .any(|reference| reference.image == "registry.example/example/demo:3.20@sha256:abc")
    );
}
