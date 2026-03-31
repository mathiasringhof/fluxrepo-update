from __future__ import annotations

import shutil
import threading
import time
from pathlib import Path

import pytest

import fluxrepo_update.updater as updater_module
from fluxrepo_update.cli import build_progress_callback
from fluxrepo_update.models import (
    DeploymentImageTarget,
    HelmReleaseTarget,
    HelmRepository,
    Inventory,
    ResourceId,
)
from fluxrepo_update.resolvers import StaticImageVersionResolver, StaticVersionResolver
from fluxrepo_update.scanner import scan_repo
from fluxrepo_update.updater import (
    UpdateReport,
    apply_chart_updates,
    apply_updates,
    ensure_chart_spec,
    plan_chart_updates,
    plan_updates,
    set_yaml_path_value,
)


def test_updater_changes_base_and_patch_chart_versions(tmp_path, fixture_repo_root) -> None:
    repo_root = tmp_path / "kubeflux"
    shutil.copytree(
        fixture_repo_root,
        repo_root,
        ignore=shutil.ignore_patterns(".uv-cache", ".venv", ".pytest_cache", ".ruff_cache", "__pycache__"),
    )

    inventory = scan_repo(repo_root)
    resolver = StaticVersionResolver(
        {
            ("truecharts", "paperless-ngx"): "12.1.0",
            ("truecharts", "audiobookshelf"): "13.0.1",
        }
    )

    report = plan_chart_updates(inventory, resolver)

    planned_paths = {item.path.relative_to(repo_root).as_posix() for item in report.planned}
    assert "apps/base/paperless-ngx/release.yaml" in planned_paths
    assert "apps/production/paperless/release-patch.yaml" in planned_paths
    assert "apps/production/audiobookshelf/release-patch.yaml" not in planned_paths

    changed_files = apply_chart_updates(report)

    assert changed_files >= 2
    assert 'version: "12.1.0"' in (repo_root / "apps/base/paperless-ngx/release.yaml").read_text(encoding="utf-8")
    assert 'version: "12.1.0"' in (repo_root / "apps/production/paperless/release-patch.yaml").read_text(encoding="utf-8")
    assert 'version: "13.0.1"' in (repo_root / "apps/base/audiobookshelf/release.yaml").read_text(encoding="utf-8")


def test_plan_chart_updates_reports_progress_for_each_chart_target(fixture_repo_root) -> None:
    class ConstantResolver:
        def resolve(
            self,
            repository: HelmRepository,
            chart_name: str,
            current_version: str | None = None,
        ) -> str:
            return "999.0.0"

    inventory = scan_repo(fixture_repo_root)
    seen: list[tuple[int, int, str]] = []

    plan_chart_updates(
        inventory,
        ConstantResolver(),
        progress_callback=lambda current, total, target: seen.append(
            (current, total, target.path.relative_to(fixture_repo_root).as_posix())
        ),
    )

    assert len(seen) == len(inventory.chart_targets)
    assert seen[0][0] == 1
    assert seen[-1][0] == len(inventory.chart_targets)
    assert all(total == len(inventory.chart_targets) for _, total, _ in seen)


def test_progress_callback_returns_none_when_disabled() -> None:
    assert build_progress_callback(repo_root=Path("/repo"), enabled=False) is None


def test_plan_updates_and_apply_updates_include_deployment_images(tmp_path, fixture_repo_root) -> None:
    repo_root = tmp_path / "kubeflux"
    shutil.copytree(
        fixture_repo_root,
        repo_root,
        ignore=shutil.ignore_patterns(".uv-cache", ".venv", ".pytest_cache", ".ruff_cache", "__pycache__"),
    )

    inventory = scan_repo(repo_root)
    chart_resolver = StaticVersionResolver({("truecharts", "paperless-ngx"): "12.1.0"})
    image_resolver = StaticImageVersionResolver(
        {
            "linuxserver/sonarr:version-4.0.16.2944": "linuxserver/sonarr:version-4.0.17.3000",
            "alpine:3.22": "alpine:3.22.1",
        }
    )

    report = plan_updates(inventory, chart_resolver, image_resolver)

    assert any(
        item.path.relative_to(repo_root).as_posix() == "apps/production/paperless/release-patch.yaml"
        for item in report.planned
    )
    assert any(
        item.path.relative_to(repo_root).as_posix() == "apps/base/sonarr/deployment.yaml"
        and item.target_kind == "Deployment"
        and item.current_version == "version-4.0.16.2944"
        and item.latest_version == "version-4.0.17.3000"
        for item in report.planned
    )
    assert any(
        item.path.relative_to(repo_root).as_posix() == "apps/production/openssh/deployment.yaml"
        and item.target_kind == "Deployment"
        and item.current_version == "3.22"
        and item.latest_version == "3.22.1"
        for item in report.planned
    )

    changed_files = apply_updates(report)

    assert changed_files >= 3
    assert 'image: linuxserver/sonarr:version-4.0.17.3000' in (
        repo_root / "apps/base/sonarr/deployment.yaml"
    ).read_text(encoding="utf-8")
    assert 'image: alpine:3.22.1' in (repo_root / "apps/production/openssh/deployment.yaml").read_text(
        encoding="utf-8"
    )


def test_plan_updates_resolves_multiple_targets_concurrently() -> None:
    active_calls = 0
    max_active_calls = 0
    lock = threading.Lock()

    class ConcurrentImageResolver:
        def resolve(self, image: str) -> str:
            nonlocal active_calls, max_active_calls
            with lock:
                active_calls += 1
                max_active_calls = max(max_active_calls, active_calls)

            try:
                time.sleep(0.05)
                return image.replace(":1.0.0", ":1.0.1")
            finally:
                with lock:
                    active_calls -= 1

    inventory = Inventory(
        repo_root=Path("/repo"),
        deployment_targets=[
            DeploymentImageTarget(
                path=Path(f"/repo/service-{index}.yaml"),
                document_index=0,
                resource_id=ResourceId(kind="Deployment", name=f"service-{index}", namespace="default"),
                yaml_path="spec.template.spec.containers[0].image",
                image=f"example/service-{index}:1.0.0",
            )
            for index in range(4)
        ],
    )

    report = plan_updates(inventory, StaticVersionResolver({}), ConcurrentImageResolver())

    assert len(report.planned) == 4
    assert max_active_calls > 1


def test_plan_updates_preserves_input_order_for_skipped_targets() -> None:
    delays = {
        "example/service-one:1.0.0": 0.03,
        "example/service-two:1.0.0": 0.01,
        "example/service-three:1.0.0": 0.02,
    }

    class DelayedImageResolver:
        def resolve(self, image: str) -> str:
            time.sleep(delays[image])
            if image != "example/service-three:1.0.0":
                service_name = image.split("/")[1].split(":")[0]
                raise ValueError(f"failed for {service_name}")
            return "example/service-three:1.0.1"

    inventory = Inventory(
        repo_root=Path("/repo"),
        deployment_targets=[
            DeploymentImageTarget(
                path=Path("/repo/service-one.yaml"),
                document_index=0,
                resource_id=ResourceId(kind="Deployment", name="service-one", namespace="default"),
                yaml_path="spec.template.spec.containers[0].image",
                image="example/service-one:1.0.0",
            ),
            DeploymentImageTarget(
                path=Path("/repo/service-two.yaml"),
                document_index=0,
                resource_id=ResourceId(kind="Deployment", name="service-two", namespace="default"),
                yaml_path="spec.template.spec.containers[0].image",
                image="example/service-two:1.0.0",
            ),
            DeploymentImageTarget(
                path=Path("/repo/service-three.yaml"),
                document_index=0,
                resource_id=ResourceId(kind="Deployment", name="service-three", namespace="default"),
                yaml_path="spec.template.spec.containers[0].image",
                image="example/service-three:1.0.0",
            ),
        ],
    )

    report = plan_updates(inventory, StaticVersionResolver({}), DelayedImageResolver())

    assert [item.path.name for item in report.planned] == ["service-three.yaml"]
    assert report.skipped == [
        "/repo/service-one.yaml: failed for service-one",
        "/repo/service-two.yaml: failed for service-two",
    ]


def test_plan_updates_skips_chart_target_when_repository_is_missing() -> None:
    inventory = Inventory(repo_root=Path("/repo"))
    inventory.chart_targets = [
        HelmReleaseTarget(
            path=Path("/repo/release.yaml"),
            document_index=0,
            resource_id=ResourceId(kind="HelmRelease", name="demo", namespace="default"),
            chart_name="demo",
            repo_name="missing-repo",
            repo_kind="HelmRepository",
            current_version="1.0.0",
            source_path=Path("/repo/release.yaml"),
            source_document_index=0,
            source_is_inherited=False,
        )
    ]

    report = plan_updates(inventory, StaticVersionResolver({}), StaticImageVersionResolver({}))

    assert report.planned == []
    assert report.skipped == ["/repo/release.yaml: missing HelmRepository missing-repo"]


def test_plan_updates_skips_deployment_when_resolver_returns_same_or_older_image() -> None:
    inventory = Inventory(
        repo_root=Path("/repo"),
        deployment_targets=[
            DeploymentImageTarget(
                path=Path("/repo/service-same.yaml"),
                document_index=0,
                resource_id=ResourceId(kind="Deployment", name="service-same", namespace="default"),
                yaml_path="spec.template.spec.containers[0].image",
                image="example/service:1.0.0",
            ),
            DeploymentImageTarget(
                path=Path("/repo/service-older.yaml"),
                document_index=0,
                resource_id=ResourceId(kind="Deployment", name="service-older", namespace="default"),
                yaml_path="spec.template.spec.containers[0].image",
                image="example/service:1.0.0",
            ),
        ],
    )

    class MixedImageResolver:
        def __init__(self) -> None:
            self.calls = 0

        def resolve(self, image: str) -> str:
            self.calls += 1
            if self.calls == 1:
                return image
            return "example/service:0.9.0"

    report = plan_updates(inventory, StaticVersionResolver({}), MixedImageResolver())

    assert report.planned == []
    assert report.skipped == []


def test_update_report_serializes_non_repo_path_skip_reason() -> None:
    report = UpdateReport(planned=[], skipped=["network timeout"])

    payload = report.to_dict(
        Path("/repo"),
        mode="plan",
        strict=False,
        non_interactive=True,
        applied_count=0,
        changed_file_count=0,
    )

    assert payload["skipped"] == [{"path": "", "reason": "network timeout"}]


def test_ensure_chart_spec_rejects_non_mapping_documents() -> None:
    with pytest.raises(ValueError, match="HelmRelease document must be a YAML mapping"):
        ensure_chart_spec(["not", "a", "mapping"])


def test_set_yaml_path_value_supports_terminal_list_indexes() -> None:
    document = {"spec": {"template": {"spec": {"containers": ["old"]}}}}

    set_yaml_path_value(document, "spec.template.spec.containers[0]", "new")

    assert document["spec"]["template"]["spec"]["containers"] == ["new"]


def test_resolve_targets_concurrently_cancels_executor_on_keyboard_interrupt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    shutdown_calls: list[tuple[bool, bool]] = []

    class FakeFuture:
        pass

    class FakeExecutor:
        def __init__(self, max_workers: int) -> None:
            self.max_workers = max_workers

        def submit(self, resolve_target, target):
            return FakeFuture()

        def shutdown(self, wait: bool = True, *, cancel_futures: bool = False) -> None:
            shutdown_calls.append((wait, cancel_futures))

    def fake_as_completed(futures):
        raise KeyboardInterrupt()
        yield futures

    monkeypatch.setattr(updater_module, "_InterruptibleThreadPoolExecutor", FakeExecutor)
    monkeypatch.setattr(updater_module, "as_completed", fake_as_completed)

    with pytest.raises(KeyboardInterrupt):
        updater_module._resolve_targets_concurrently(
            [
                DeploymentImageTarget(
                    path=Path("/repo/service.yaml"),
                    document_index=0,
                    resource_id=ResourceId(kind="Deployment", name="service", namespace="default"),
                    yaml_path="spec.template.spec.containers[0].image",
                    image="example/service:1.0.0",
                )
            ],
            lambda target: updater_module._ResolutionOutcome(),
            progress_callback=None,
        )

    assert shutdown_calls == [(False, True)]
