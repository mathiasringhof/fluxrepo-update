from __future__ import annotations

import re
import threading
import weakref
from collections import defaultdict
from collections.abc import Callable
from concurrent.futures import Future, ThreadPoolExecutor, as_completed
from concurrent.futures.thread import _threads_queues, _worker
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ruamel.yaml import YAML

from fluxrepo_update.models import DeploymentImageTarget, HelmReleaseTarget, Inventory
from fluxrepo_update.resolvers import (
    ChartVersionResolver,
    ImageVersionResolver,
    is_newer_version,
    parse_image_reference,
)

_ROUNDTRIP_YAML = YAML()
_ROUNDTRIP_YAML.preserve_quotes = True
_ROUNDTRIP_YAML.width = 4096


@dataclass(frozen=True)
class PlannedChartUpdate:
    path: Path
    document_index: int
    target_name: str
    chart_name: str
    repo_name: str
    current_version: str
    latest_version: str
    inherited_source: bool
    target_kind: str = "HelmRelease"

    def to_dict(self, repo_root: Path) -> dict[str, object]:
        return {
            "path": str(self.path.relative_to(repo_root)),
            "document_index": self.document_index,
            "target_kind": self.target_kind,
            "target_name": self.target_name,
            "yaml_path": "spec.chart.spec.version",
            "chart_name": self.chart_name,
            "repo_name": self.repo_name,
            "current_version": self.current_version,
            "latest_version": self.latest_version,
            "inherited_source": self.inherited_source,
        }


@dataclass(frozen=True)
class PlannedDeploymentUpdate:
    path: Path
    document_index: int
    target_name: str
    yaml_path: str
    current_image: str
    latest_image: str
    current_version: str
    latest_version: str
    target_kind: str = "Deployment"

    def to_dict(self, repo_root: Path) -> dict[str, object]:
        return {
            "path": str(self.path.relative_to(repo_root)),
            "document_index": self.document_index,
            "target_kind": self.target_kind,
            "target_name": self.target_name,
            "yaml_path": self.yaml_path,
            "current_image": self.current_image,
            "latest_image": self.latest_image,
            "current_version": self.current_version,
            "latest_version": self.latest_version,
            "inherited_source": False,
        }


@dataclass(frozen=True)
class UpdateReport:
    planned: list[PlannedChartUpdate | PlannedDeploymentUpdate]
    skipped: list[str]

    def to_dict(
        self,
        repo_root: Path,
        *,
        mode: str,
        strict: bool,
        non_interactive: bool,
        applied_count: int,
        changed_file_count: int,
    ) -> dict[str, object]:
        return {
            "mode": mode,
            "strict": strict,
            "non_interactive": non_interactive,
            "summary": {
                "planned_count": len(self.planned),
                "applied_count": applied_count,
                "skipped_count": len(self.skipped),
                "changed_file_count": changed_file_count,
            },
            "planned": [item.to_dict(repo_root) for item in self.planned],
            "skipped": [self._serialize_skipped_item(item, repo_root) for item in self.skipped],
        }

    def _serialize_skipped_item(self, item: str, repo_root: Path) -> dict[str, str]:
        path_text, separator, reason = item.partition(": ")
        if separator:
            path = Path(path_text)
            try:
                relative_path = str(path.relative_to(repo_root))
            except ValueError:
                relative_path = path_text
            return {"path": relative_path, "reason": reason}
        return {"path": "", "reason": item}


ProgressCallback = Callable[[int, int, HelmReleaseTarget | DeploymentImageTarget], None]


class _InterruptibleThreadPoolExecutor(ThreadPoolExecutor):
    def _adjust_thread_count(self) -> None:
        if self._idle_semaphore.acquire(timeout=0):
            return

        def weakref_cb(_, queue=self._work_queue):
            queue.put(None)

        num_threads = len(self._threads)
        if num_threads < self._max_workers:
            thread_name = f"{self._thread_name_prefix}_{num_threads}"
            worker_thread = threading.Thread(
                name=thread_name,
                target=_worker,
                args=(weakref.ref(self, weakref_cb), self._create_worker_context(), self._work_queue),
                daemon=True,
            )
            worker_thread.start()
            self._threads.add(worker_thread)
            _threads_queues[worker_thread] = self._work_queue


@dataclass(frozen=True)
class _ResolutionOutcome:
    planned: PlannedChartUpdate | PlannedDeploymentUpdate | None = None
    skipped: str | None = None


def plan_chart_updates(
    inventory: Inventory,
    resolver: ChartVersionResolver,
    *,
    progress_callback: ProgressCallback | None = None,
) -> UpdateReport:
    outcomes = _resolve_targets_concurrently(
        inventory.chart_targets,
        lambda target: _resolve_chart_target(inventory, target, resolver),
        progress_callback=progress_callback,
    )
    planned = [
        outcome.planned for outcome in outcomes if isinstance(outcome.planned, PlannedChartUpdate)
    ]
    skipped = [outcome.skipped for outcome in outcomes if outcome.skipped is not None]

    planned.sort(key=lambda item: (str(item.path), item.document_index))
    return UpdateReport(planned=planned, skipped=skipped)


def plan_updates(
    inventory: Inventory,
    chart_resolver: ChartVersionResolver,
    image_resolver: ImageVersionResolver,
    *,
    progress_callback: ProgressCallback | None = None,
) -> UpdateReport:
    chart_outcomes = _resolve_targets_concurrently(
        inventory.chart_targets,
        lambda target: _resolve_chart_target(inventory, target, chart_resolver),
        progress_callback=progress_callback,
        progress_offset=0,
        progress_total=len(inventory.chart_targets) + len(inventory.deployment_targets),
    )
    deployment_outcomes = _resolve_targets_concurrently(
        inventory.deployment_targets,
        lambda target: _resolve_deployment_target(target, image_resolver),
        progress_callback=progress_callback,
        progress_offset=len(inventory.chart_targets),
        progress_total=len(inventory.chart_targets) + len(inventory.deployment_targets),
    )

    outcomes = [*chart_outcomes, *deployment_outcomes]
    planned = [outcome.planned for outcome in outcomes if outcome.planned is not None]
    skipped = [outcome.skipped for outcome in outcomes if outcome.skipped is not None]

    planned.sort(key=lambda item: (str(item.path), item.document_index, getattr(item, "yaml_path", "")))
    return UpdateReport(planned=planned, skipped=skipped)


def _resolve_targets_concurrently(
    targets: list[HelmReleaseTarget] | list[DeploymentImageTarget],
    resolve_target: Callable[[HelmReleaseTarget | DeploymentImageTarget], _ResolutionOutcome],
    *,
    progress_callback: ProgressCallback | None,
    progress_offset: int = 0,
    progress_total: int | None = None,
) -> list[_ResolutionOutcome]:
    total_targets = progress_total if progress_total is not None else len(targets)
    if not targets:
        return []

    outcomes: list[_ResolutionOutcome | None] = [None] * len(targets)
    completed = 0
    max_workers = min(32, len(targets))
    executor = _InterruptibleThreadPoolExecutor(max_workers=max_workers)
    try:
        future_map: dict[Future[_ResolutionOutcome], tuple[int, HelmReleaseTarget | DeploymentImageTarget]] = {
            executor.submit(resolve_target, target): (index, target) for index, target in enumerate(targets)
        }

        for future in as_completed(future_map):
            index, target = future_map[future]
            outcomes[index] = future.result()
            completed += 1
            if progress_callback is not None:
                progress_callback(progress_offset + completed, total_targets, target)
    except BaseException:
        executor.shutdown(wait=False, cancel_futures=True)
        raise
    else:
        executor.shutdown(wait=True)

    return [outcome for outcome in outcomes if outcome is not None]


def _resolve_chart_target(
    inventory: Inventory,
    target: HelmReleaseTarget,
    resolver: ChartVersionResolver,
) -> _ResolutionOutcome:
    repository = inventory.repositories.get(target.repo_name or "")
    if repository is None:
        return _ResolutionOutcome(skipped=f"{target.path}: missing HelmRepository {target.repo_name}")

    try:
        latest_version = resolver.resolve(
            repository,
            target.chart_name or "",
            target.current_version,
        )
    except Exception as exc:
        return _ResolutionOutcome(skipped=f"{target.path}: {exc}")

    current_version = target.current_version or ""
    if not is_newer_version(current_version, latest_version):
        return _ResolutionOutcome()

    return _ResolutionOutcome(
        planned=PlannedChartUpdate(
            path=target.path,
            document_index=target.document_index,
            target_name=target.resource_id.name,
            chart_name=target.chart_name or "",
            repo_name=target.repo_name or "",
            current_version=current_version,
            latest_version=latest_version,
            inherited_source=target.source_is_inherited,
        )
    )


def _resolve_deployment_target(
    target: DeploymentImageTarget,
    resolver: ImageVersionResolver,
) -> _ResolutionOutcome:
    try:
        latest_image = resolver.resolve(target.image)
    except Exception as exc:
        return _ResolutionOutcome(skipped=f"{target.path}: {exc}")

    if latest_image == target.image:
        return _ResolutionOutcome()

    current_version = parse_image_reference(target.image).tag
    latest_version = parse_image_reference(latest_image).tag
    if current_version is None or latest_version is None:
        return _ResolutionOutcome(
            skipped=f"{target.path}: could not determine comparable image tags for {target.image}"
        )
    if not is_newer_version(current_version, latest_version):
        return _ResolutionOutcome()

    return _ResolutionOutcome(
        planned=PlannedDeploymentUpdate(
            path=target.path,
            document_index=target.document_index,
            target_name=target.resource_id.name,
            yaml_path=target.yaml_path,
            current_image=target.image,
            latest_image=latest_image,
            current_version=current_version,
            latest_version=latest_version,
        )
    )


def apply_chart_updates(report: UpdateReport) -> int:
    return apply_updates(report)


def apply_updates(report: UpdateReport) -> int:
    updates_by_path: dict[Path, list[PlannedChartUpdate | PlannedDeploymentUpdate]] = defaultdict(list)
    for update in report.planned:
        updates_by_path[update.path].append(update)

    changed_files = 0
    for path, updates in updates_by_path.items():
        with path.open("r", encoding="utf-8") as handle:
            documents = list(_ROUNDTRIP_YAML.load_all(handle))

        for update in updates:
            document = documents[update.document_index]
            if isinstance(update, PlannedChartUpdate):
                ensure_chart_spec(document)
                document["spec"]["chart"]["spec"]["version"] = update.latest_version
            else:
                set_yaml_path_value(document, update.yaml_path, update.latest_image)

        with path.open("w", encoding="utf-8") as handle:
            _ROUNDTRIP_YAML.dump_all(documents, handle)
        changed_files += 1

    return changed_files


def ensure_chart_spec(document: Any) -> None:
    if not isinstance(document, dict):
        raise ValueError("HelmRelease document must be a YAML mapping")

    spec = document.setdefault("spec", {})
    if not isinstance(spec, dict):
        raise ValueError("HelmRelease spec must be a YAML mapping")

    chart = spec.setdefault("chart", {})
    if not isinstance(chart, dict):
        raise ValueError("HelmRelease chart must be a YAML mapping")

    chart_spec = chart.setdefault("spec", {})
    if not isinstance(chart_spec, dict):
        raise ValueError("HelmRelease chart.spec must be a YAML mapping")


def set_yaml_path_value(document: Any, yaml_path: str, value: Any) -> None:
    parts = _parse_yaml_path(yaml_path)
    if not parts:
        raise ValueError("yaml path must not be empty")

    current = document
    for part in parts[:-1]:
        if isinstance(part, str):
            if not isinstance(current, dict):
                raise ValueError(f"Expected YAML mapping while traversing {yaml_path}")
            current = current[part]
        else:
            if not isinstance(current, list):
                raise ValueError(f"Expected YAML sequence while traversing {yaml_path}")
            current = current[part]

    last_part = parts[-1]
    if isinstance(last_part, str):
        if not isinstance(current, dict):
            raise ValueError(f"Expected YAML mapping at {yaml_path}")
        current[last_part] = value
        return

    if not isinstance(current, list):
        raise ValueError(f"Expected YAML sequence at {yaml_path}")
    current[last_part] = value


def _parse_yaml_path(yaml_path: str) -> list[str | int]:
    parts: list[str | int] = []
    for segment in yaml_path.split("."):
        if not segment:
            continue
        match = re.match(r"^([^\[]+)", segment)
        if match is not None:
            parts.append(match.group(1))
        parts.extend(int(index) for index in re.findall(r"\[(\d+)\]", segment))
    return parts
