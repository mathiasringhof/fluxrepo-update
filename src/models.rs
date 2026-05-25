use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RepoType {
    Default,
    Oci,
    Other(String),
}

impl From<&str> for RepoType {
    fn from(value: &str) -> Self {
        match value {
            "default" => Self::Default,
            "oci" => Self::Oci,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<String> for RepoType {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKind {
    HelmRelease,
    Deployment,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HelmRelease => "HelmRelease",
            Self::Deployment => "Deployment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceId {
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HelmRepository {
    pub name: String,
    pub url: String,
    pub repo_type: RepoType,
    pub path: PathBuf,
    pub document_index: usize,
}

#[derive(Debug, Clone)]
pub struct ImageReference {
    pub path: PathBuf,
    pub document_index: usize,
    pub manifest_kind: String,
    pub manifest_name: Option<String>,
    pub yaml_path: String,
    pub image: String,
}

#[derive(Debug, Clone)]
pub struct DeploymentImageTarget {
    pub path: PathBuf,
    pub document_index: usize,
    pub resource_id: ResourceId,
    pub yaml_path: String,
    pub image: String,
}

#[derive(Debug, Clone)]
pub struct HelmReleaseTarget {
    pub path: PathBuf,
    pub document_index: usize,
    pub resource_id: ResourceId,
    pub chart_name: Option<String>,
    pub repo_name: Option<String>,
    pub current_version: Option<String>,
    pub source_path: Option<PathBuf>,
    pub source_is_inherited: bool,
}

impl HelmReleaseTarget {
    pub fn can_update(&self) -> bool {
        self.current_version.is_some() && self.chart_name.is_some() && self.repo_name.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct Inventory {
    pub repo_root: PathBuf,
    pub repositories: HashMap<String, HelmRepository>,
    pub chart_targets: Vec<HelmReleaseTarget>,
    pub deployment_targets: Vec<DeploymentImageTarget>,
    pub helmreleases_without_chart_version: Vec<HelmReleaseTarget>,
    pub unresolved_chart_targets: Vec<HelmReleaseTarget>,
    pub image_references: Vec<ImageReference>,
    pub skipped_paths: Vec<PathBuf>,
}

impl Inventory {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            repositories: HashMap::new(),
            chart_targets: Vec::new(),
            deployment_targets: Vec::new(),
            helmreleases_without_chart_version: Vec::new(),
            unresolved_chart_targets: Vec::new(),
            image_references: Vec::new(),
            skipped_paths: Vec::new(),
        }
    }

    pub fn to_json_value(&self) -> Value {
        json!({
            "repo_root": self.repo_root,
            "repository_count": self.repositories.len(),
            "chart_target_count": self.chart_targets.len(),
            "deployment_target_count": self.deployment_targets.len(),
            "helmreleases_without_chart_version_count": self.helmreleases_without_chart_version.len(),
            "unresolved_chart_target_count": self.unresolved_chart_targets.len(),
            "image_reference_count": self.image_references.len(),
            "skipped_paths": self.skipped_paths.iter().map(|path| self.relative(path)).collect::<Vec<_>>(),
            "chart_targets": self.chart_targets.iter().map(|target| json!({
                "path": self.relative(&target.path),
                "document_index": target.document_index,
                "name": target.resource_id.name,
                "namespace": target.resource_id.namespace,
                "chart_name": target.chart_name,
                "repo_name": target.repo_name,
                "current_version": target.current_version,
                "source_path": target.source_path.as_ref().map(|path| self.relative(path)),
                "source_is_inherited": target.source_is_inherited,
            })).collect::<Vec<_>>(),
            "deployment_targets": self.deployment_targets.iter().map(|target| json!({
                "path": self.relative(&target.path),
                "document_index": target.document_index,
                "name": target.resource_id.name,
                "namespace": target.resource_id.namespace,
                "yaml_path": target.yaml_path,
                "image": target.image,
            })).collect::<Vec<_>>(),
            "helmreleases_without_chart_version": self.helmreleases_without_chart_version.iter().map(|target| json!({
                "path": self.relative(&target.path),
                "document_index": target.document_index,
                "name": target.resource_id.name,
                "namespace": target.resource_id.namespace,
            })).collect::<Vec<_>>(),
            "unresolved_chart_targets": self.unresolved_chart_targets.iter().map(|target| json!({
                "path": self.relative(&target.path),
                "document_index": target.document_index,
                "name": target.resource_id.name,
                "namespace": target.resource_id.namespace,
                "current_version": target.current_version,
            })).collect::<Vec<_>>(),
            "image_references": self.image_references.iter().map(|image| json!({
                "path": self.relative(&image.path),
                "document_index": image.document_index,
                "manifest_kind": image.manifest_kind,
                "manifest_name": image.manifest_name,
                "yaml_path": image.yaml_path,
                "image": image.image,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }
}
