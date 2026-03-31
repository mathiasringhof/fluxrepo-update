from __future__ import annotations

from pathlib import Path

from fluxrepo_update.scanner import scan_repo


def test_scanner_links_patch_versions_back_to_base_release(fixture_repo_root) -> None:
    inventory = scan_repo(fixture_repo_root)

    patch_target = next(
        target
        for target in inventory.chart_targets
        if target.path.relative_to(fixture_repo_root).as_posix() == "apps/production/paperless/release-patch.yaml"
    )

    assert patch_target.chart_name == "paperless-ngx"
    assert patch_target.repo_name == "truecharts"
    assert patch_target.current_version == "11.29.10"
    assert patch_target.source_is_inherited is True
    assert patch_target.source_path is not None
    assert patch_target.source_path.relative_to(fixture_repo_root).as_posix() == "apps/base/paperless-ngx/release.yaml"


def test_scanner_keeps_values_only_overlays_out_of_chart_targets(fixture_repo_root) -> None:
    inventory = scan_repo(fixture_repo_root)

    without_chart_version_paths = {
        target.path.relative_to(fixture_repo_root).as_posix()
        for target in inventory.helmreleases_without_chart_version
    }

    assert "apps/production/audiobookshelf/release-patch.yaml" in without_chart_version_paths
    assert "apps/production/uptimekuma-values.yaml" in without_chart_version_paths


def test_scanner_reports_image_references_as_gap_inventory(fixture_repo_root) -> None:
    inventory = scan_repo(fixture_repo_root)

    image_refs = {
        (image.path.relative_to(fixture_repo_root).as_posix(), image.image)
        for image in inventory.image_references
    }

    assert ("apps/base/smokeping/deployment.yaml", "lscr.io/linuxserver/smokeping:latest") in image_refs
    assert (
        "apps/production/immich/release-patch.yaml",
        "docker.io/valkey/valkey:9.0-alpine@sha256:1be494495248d53e3558b198a1c704e6b559d5e99fe4c926e14a8ad24d76c6fa",
    ) in image_refs


def test_scanner_reports_deployment_targets_with_direct_image_fields(fixture_repo_root) -> None:
    inventory = scan_repo(fixture_repo_root)

    deployment_targets = {
        (
            target.path.relative_to(fixture_repo_root).as_posix(),
            target.resource_id.name,
            target.yaml_path,
            target.image,
        )
        for target in inventory.deployment_targets
    }

    assert (
        "apps/base/sonarr/deployment.yaml",
        "sonarr-deployment",
        "spec.template.spec.containers[0].image",
        "linuxserver/sonarr:version-4.0.16.2944",
    ) in deployment_targets
    assert (
        "apps/production/openssh/deployment.yaml",
        "openssh-deployment",
        "spec.template.spec.initContainers[0].image",
        "alpine:3.22",
    ) in deployment_targets


def test_scanner_reports_chart_targets_with_missing_source_metadata_as_unresolved(tmp_path: Path) -> None:
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    (repo_root / "release.yaml").write_text(
        """
apiVersion: helm.toolkit.fluxcd.io/v2
kind: HelmRelease
metadata:
  name: demo
  namespace: default
spec:
  chart:
    spec:
      version: "1.2.3"
""".strip()
        + "\n",
        encoding="utf-8",
    )

    inventory = scan_repo(repo_root)

    assert inventory.chart_targets == []
    assert [target.resource_id.name for target in inventory.unresolved_chart_targets] == ["demo"]
