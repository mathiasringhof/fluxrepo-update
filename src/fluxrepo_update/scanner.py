from __future__ import annotations

from collections.abc import Iterator
from os import walk
from pathlib import Path
from typing import Any

from ruamel.yaml import YAML

from fluxrepo_update.models import (
    DeploymentImageTarget,
    HelmReleaseTarget,
    HelmRepository,
    ImageReference,
    Inventory,
    ResourceId,
)

_SAFE_YAML = YAML(typ="safe")
_EXCLUDED_PATH_PARTS = ("flux-system/gotk-",)
_IGNORED_DIR_NAMES = {".git", ".pytest_cache", ".ruff_cache", ".uv-cache", ".venv", "__pycache__"}


def scan_repo(repo_root: Path) -> Inventory:
    repo_root = repo_root.resolve()
    inventory = Inventory(repo_root=repo_root)

    release_candidates: list[HelmReleaseTarget] = []
    full_source_by_id: dict[ResourceId, HelmReleaseTarget] = {}

    for path in iter_yaml_files(repo_root):
        if is_skipped_path(path):
            inventory.skipped_paths.append(path)
            continue

        for document_index, document in enumerate(load_yaml_documents(path)):
            if not isinstance(document, dict):
                continue

            kind = str(document.get("kind") or "")
            if kind == "HelmRepository":
                repository = parse_repository(path, document_index, document)
                if repository:
                    inventory.repositories[repository.name] = repository
                continue

            if kind == "HelmRelease":
                target = parse_helmrelease(path, document_index, document)
                if target is None:
                    continue
                release_candidates.append(target)
                if target.chart_name and target.repo_name:
                    existing = full_source_by_id.get(target.resource_id)
                    if existing is None or prefer_source(target.path, existing.path):
                        full_source_by_id[target.resource_id] = target
            elif kind == "Deployment":
                inventory.deployment_targets.extend(parse_deployment_targets(path, document_index, document))

            inventory.image_references.extend(parse_image_references(path, document_index, document))

    for target in release_candidates:
        if target.chart_name is None or target.repo_name is None:
            source = full_source_by_id.get(target.resource_id)
            if source is not None:
                target.chart_name = source.chart_name
                target.repo_name = source.repo_name
                target.repo_kind = source.repo_kind
                target.source_path = source.path
                target.source_document_index = source.document_index
                target.source_is_inherited = source.path != target.path or source.document_index != target.document_index

        if target.can_update:
            inventory.chart_targets.append(target)
        elif target.current_version:
            inventory.unresolved_chart_targets.append(target)
        else:
            inventory.helmreleases_without_chart_version.append(target)

    inventory.chart_targets.sort(key=lambda item: (str(item.path), item.document_index))
    inventory.deployment_targets.sort(key=lambda item: (str(item.path), item.document_index, item.yaml_path))
    inventory.helmreleases_without_chart_version.sort(key=lambda item: (str(item.path), item.document_index))
    inventory.unresolved_chart_targets.sort(key=lambda item: (str(item.path), item.document_index))
    inventory.image_references.sort(key=lambda item: (str(item.path), item.document_index, item.yaml_path))

    return inventory


def iter_yaml_files(repo_root: Path) -> Iterator[Path]:
    for root, dirs, files in walk(repo_root):
        dirs[:] = sorted(
            dir_name for dir_name in dirs if dir_name not in _IGNORED_DIR_NAMES and not dir_name.startswith(".")
        )
        for file_name in sorted(files):
            if file_name.endswith(".yaml"):
                yield Path(root, file_name)


def is_skipped_path(path: Path) -> bool:
    path_text = path.as_posix()
    return any(part in path_text for part in _EXCLUDED_PATH_PARTS)


def load_yaml_documents(path: Path) -> list[Any]:
    with path.open("r", encoding="utf-8") as handle:
        return list(_SAFE_YAML.load_all(handle))


def parse_repository(path: Path, document_index: int, document: dict[str, Any]) -> HelmRepository | None:
    metadata = document.get("metadata") or {}
    spec = document.get("spec") or {}
    name = metadata.get("name")
    url = spec.get("url")
    if not isinstance(name, str) or not isinstance(url, str):
        return None
    repo_type = spec.get("type")
    return HelmRepository(
        name=name,
        url=url,
        repo_type=str(repo_type) if repo_type else "default",
        path=path,
        document_index=document_index,
    )


def parse_helmrelease(path: Path, document_index: int, document: dict[str, Any]) -> HelmReleaseTarget | None:
    metadata = document.get("metadata") or {}
    name = metadata.get("name")
    if not isinstance(name, str):
        return None

    namespace = metadata.get("namespace")
    if namespace is not None:
        namespace = str(namespace)

    resource_id = ResourceId(kind="HelmRelease", name=name, namespace=namespace)
    chart_name = get_nested(document, "spec", "chart", "spec", "chart")
    current_version = get_nested(document, "spec", "chart", "spec", "version")
    repo_name = get_nested(document, "spec", "chart", "spec", "sourceRef", "name")
    repo_kind = get_nested(document, "spec", "chart", "spec", "sourceRef", "kind")

    return HelmReleaseTarget(
        path=path,
        document_index=document_index,
        resource_id=resource_id,
        chart_name=str(chart_name) if chart_name is not None else None,
        repo_name=str(repo_name) if repo_name is not None else None,
        repo_kind=str(repo_kind) if repo_kind is not None else None,
        current_version=str(current_version) if current_version is not None else None,
        source_path=path,
        source_document_index=document_index,
        source_is_inherited=False,
    )


def parse_deployment_targets(path: Path, document_index: int, document: dict[str, Any]) -> list[DeploymentImageTarget]:
    metadata = document.get("metadata") or {}
    name = metadata.get("name")
    if not isinstance(name, str):
        return []

    namespace = metadata.get("namespace")
    if namespace is not None:
        namespace = str(namespace)

    resource_id = ResourceId(kind="Deployment", name=name, namespace=namespace)
    results: list[DeploymentImageTarget] = []

    for yaml_path, image in iter_images(document):
        if not is_deployment_image_path(yaml_path):
            continue
        results.append(
            DeploymentImageTarget(
                path=path,
                document_index=document_index,
                resource_id=resource_id,
                yaml_path=yaml_path,
                image=image,
            )
        )

    return results


def get_nested(document: dict[str, Any], *path: str) -> Any:
    value: Any = document
    for key in path:
        if not isinstance(value, dict) or key not in value:
            return None
        value = value[key]
    return value


def prefer_source(candidate: Path, existing: Path) -> bool:
    candidate_text = candidate.as_posix()
    existing_text = existing.as_posix()
    if "/base/" in candidate_text and "/base/" not in existing_text:
        return True
    if "/base/" not in candidate_text and "/base/" in existing_text:
        return False
    return candidate_text < existing_text


def parse_image_references(path: Path, document_index: int, document: dict[str, Any]) -> list[ImageReference]:
    metadata = document.get("metadata") or {}
    manifest_name = metadata.get("name")
    kind = str(document.get("kind") or "Unknown")
    results: list[ImageReference] = []

    for yaml_path, image in iter_images(document):
        results.append(
            ImageReference(
                path=path,
                document_index=document_index,
                manifest_kind=kind,
                manifest_name=str(manifest_name) if manifest_name is not None else None,
                yaml_path=yaml_path,
                image=image,
            )
        )

    return results


def is_deployment_image_path(yaml_path: str) -> bool:
    return yaml_path.startswith("spec.template.spec.containers[") or yaml_path.startswith(
        "spec.template.spec.initContainers["
    )


def iter_images(value: Any, path: str = "") -> Iterator[tuple[str, str]]:
    if isinstance(value, dict):
        for key, child in value.items():
            child_path = f"{path}.{key}" if path else str(key)
            if key == "image" and isinstance(child, str):
                yield child_path, child
            elif key == "image" and isinstance(child, dict):
                repository = child.get("repository")
                tag = child.get("tag")
                if isinstance(repository, str):
                    rendered = f"{repository}:{tag}" if isinstance(tag, str) and tag else repository
                    yield child_path, rendered
            else:
                yield from iter_images(child, child_path)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            child_path = f"{path}[{index}]"
            yield from iter_images(child, child_path)
