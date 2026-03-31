from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path


@dataclass(frozen=True)
class ResourceId:
    kind: str
    name: str
    namespace: str | None


@dataclass(frozen=True)
class HelmRepository:
    name: str
    url: str
    repo_type: str
    path: Path
    document_index: int


@dataclass(frozen=True)
class ImageReference:
    path: Path
    document_index: int
    manifest_kind: str
    manifest_name: str | None
    yaml_path: str
    image: str


@dataclass(frozen=True)
class DeploymentImageTarget:
    path: Path
    document_index: int
    resource_id: ResourceId
    yaml_path: str
    image: str


@dataclass
class HelmReleaseTarget:
    path: Path
    document_index: int
    resource_id: ResourceId
    chart_name: str | None
    repo_name: str | None
    repo_kind: str | None
    current_version: str | None
    source_path: Path | None = None
    source_document_index: int | None = None
    source_is_inherited: bool = False

    @property
    def can_update(self) -> bool:
        return bool(self.current_version and self.chart_name and self.repo_name)


@dataclass
class Inventory:
    repo_root: Path
    repositories: dict[str, HelmRepository] = field(default_factory=dict)
    chart_targets: list[HelmReleaseTarget] = field(default_factory=list)
    deployment_targets: list[DeploymentImageTarget] = field(default_factory=list)
    helmreleases_without_chart_version: list[HelmReleaseTarget] = field(default_factory=list)
    unresolved_chart_targets: list[HelmReleaseTarget] = field(default_factory=list)
    image_references: list[ImageReference] = field(default_factory=list)
    skipped_paths: list[Path] = field(default_factory=list)

    def to_dict(self) -> dict[str, object]:
        return {
            "repo_root": str(self.repo_root),
            "repository_count": len(self.repositories),
            "chart_target_count": len(self.chart_targets),
            "deployment_target_count": len(self.deployment_targets),
            "helmreleases_without_chart_version_count": len(self.helmreleases_without_chart_version),
            "unresolved_chart_target_count": len(self.unresolved_chart_targets),
            "image_reference_count": len(self.image_references),
            "skipped_paths": [self._relative(path) for path in self.skipped_paths],
            "chart_targets": [
                {
                    "path": self._relative(target.path),
                    "document_index": target.document_index,
                    "name": target.resource_id.name,
                    "namespace": target.resource_id.namespace,
                    "chart_name": target.chart_name,
                    "repo_name": target.repo_name,
                    "current_version": target.current_version,
                    "source_path": self._relative(target.source_path) if target.source_path else None,
                    "source_is_inherited": target.source_is_inherited,
                }
                for target in self.chart_targets
            ],
            "deployment_targets": [
                {
                    "path": self._relative(target.path),
                    "document_index": target.document_index,
                    "name": target.resource_id.name,
                    "namespace": target.resource_id.namespace,
                    "yaml_path": target.yaml_path,
                    "image": target.image,
                }
                for target in self.deployment_targets
            ],
            "helmreleases_without_chart_version": [
                {
                    "path": self._relative(target.path),
                    "document_index": target.document_index,
                    "name": target.resource_id.name,
                    "namespace": target.resource_id.namespace,
                }
                for target in self.helmreleases_without_chart_version
            ],
            "unresolved_chart_targets": [
                {
                    "path": self._relative(target.path),
                    "document_index": target.document_index,
                    "name": target.resource_id.name,
                    "namespace": target.resource_id.namespace,
                    "current_version": target.current_version,
                }
                for target in self.unresolved_chart_targets
            ],
            "image_references": [
                {
                    "path": self._relative(image.path),
                    "document_index": image.document_index,
                    "manifest_kind": image.manifest_kind,
                    "manifest_name": image.manifest_name,
                    "yaml_path": image.yaml_path,
                    "image": image.image,
                }
                for image in self.image_references
            ],
        }

    def _relative(self, path: Path) -> str:
        try:
            return str(path.relative_to(self.repo_root))
        except ValueError:
            return str(path)
