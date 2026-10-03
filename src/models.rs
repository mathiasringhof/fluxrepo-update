use std::collections::{BTreeMap, HashMap};
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
    ImageBinding,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HelmRelease => "HelmRelease",
            Self::ImageBinding => "ImageBinding",
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
    pub namespace: Option<String>,
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
pub struct UncheckedVersionDeclaration {
    pub path: PathBuf,
    pub document_index: usize,
    pub yaml_path: String,
    pub current_value: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ImageBinding {
    pub path: PathBuf,
    pub document_index: usize,
    pub resource_id: ResourceId,
    pub yaml_path: String,
    pub image: String,
    pub value_kind: ImageBindingValueKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageBindingValueKind {
    ImageReference,
    Tag,
    UnsupportedSchema,
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
    pub manifest_documents: HashMap<(PathBuf, usize), yaml_serde::Value>,
    pub repositories: HashMap<String, HelmRepository>,
    pub ambiguous_repositories: BTreeMap<String, Vec<HelmRepository>>,
    pub equivalent_repositories: BTreeMap<String, Vec<HelmRepository>>,
    pub chart_targets: Vec<HelmReleaseTarget>,
    pub image_bindings: Vec<ImageBinding>,
    pub helmreleases_without_chart_version: Vec<HelmReleaseTarget>,
    pub unresolved_chart_targets: Vec<HelmReleaseTarget>,
    pub image_references: Vec<ImageReference>,
    pub unchecked_version_declarations: Vec<UncheckedVersionDeclaration>,
    pub skipped_paths: Vec<PathBuf>,
}

impl Inventory {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            manifest_documents: HashMap::new(),
            repositories: HashMap::new(),
            ambiguous_repositories: BTreeMap::new(),
            equivalent_repositories: BTreeMap::new(),
            chart_targets: Vec::new(),
            image_bindings: Vec::new(),
            helmreleases_without_chart_version: Vec::new(),
            unresolved_chart_targets: Vec::new(),
            image_references: Vec::new(),
            unchecked_version_declarations: Vec::new(),
            skipped_paths: Vec::new(),
        }
    }

    pub fn repository_count(&self) -> usize {
        self.repositories.len()
            + self
                .equivalent_repositories
                .values()
                .map(|sources| sources.len() - 1)
                .sum::<usize>()
            + self
                .ambiguous_repositories
                .values()
                .map(Vec::len)
                .sum::<usize>()
    }

    pub fn declaration_count(&self) -> usize {
        self.chart_targets.len()
            + self.unresolved_chart_targets.len()
            + self.image_bindings.len()
            + self.unchecked_version_declarations.len()
    }

    pub(crate) fn add_repository(&mut self, repository: HelmRepository) {
        if let Some(sources) = self.ambiguous_repositories.get_mut(&repository.name) {
            sources.push(repository);
        } else if let Some(previous) = self.repositories.remove(&repository.name) {
            self.ambiguous_repositories
                .insert(repository.name.clone(), vec![previous, repository]);
        } else {
            self.repositories
                .insert(repository.name.clone(), repository);
        }
    }

    pub(crate) fn combine_equivalent_sources(&mut self) {
        for (name, sources) in std::mem::take(&mut self.ambiguous_repositories) {
            let first = &sources[0];
            let specification = self.source_specification(first);
            if specification.is_some()
                && sources.iter().all(|source| {
                    source.namespace == first.namespace
                        && self.source_specification(source) == specification
                })
            {
                self.repositories.insert(name.clone(), first.clone());
                self.equivalent_repositories.insert(name, sources);
            } else {
                self.ambiguous_repositories.insert(name, sources);
            }
        }
    }

    fn source_specification(&self, source: &HelmRepository) -> Option<&yaml_serde::Value> {
        self.manifest_documents
            .get(&(source.path.clone(), source.document_index))?
            .get("spec")
    }

    pub(crate) fn repository_sources(&self, name: &str) -> Vec<&HelmRepository> {
        self.equivalent_repositories.get(name).map_or_else(
            || self.repositories.get(name).into_iter().collect(),
            |sources| sources.iter().collect(),
        )
    }

    pub fn to_json_value(&self) -> Value {
        let mut repositories = self.repositories.values().collect::<Vec<_>>();
        repositories.sort_by_key(|repository| {
            (
                repository.path.clone(),
                repository.document_index,
                repository.name.clone(),
            )
        });

        json!({
            "repo_root": self.repo_root,
            "scope": "repository_manifests",
            "discovered_count": self.declaration_count(),
            "repository_count": self.repository_count(),
            "chart_target_count": self.chart_targets.len(),
            "image_binding_count": self.image_bindings.len(),
            "helmreleases_without_chart_version_count": self.helmreleases_without_chart_version.len(),
            "unresolved_chart_target_count": self.unresolved_chart_targets.len(),
            "image_reference_count": self.image_references.len(),
            "unchecked_version_declarations": self.unchecked_version_declarations.iter().map(|declaration| json!({
                "path": self.relative(&declaration.path),
                "document_index": declaration.document_index,
                "yaml_path": declaration.yaml_path,
                "current_value": declaration.current_value,
                "reason": declaration.reason,
            })).collect::<Vec<_>>(),
            "skipped_paths": self.skipped_paths.iter().map(|path| self.relative(path)).collect::<Vec<_>>(),
            "repositories": repositories.iter().map(|repository| self.repository_json(repository)).collect::<Vec<_>>(),
            "ambiguous_repositories": self.ambiguous_repositories.iter().map(|(name, sources)| json!({
                "name": name,
                "sources": sources.iter().map(|source| self.repository_json(source)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "equivalent_repositories": self.equivalent_repositories.iter().map(|(name, sources)| json!({
                "name": name,
                "sources": sources.iter().map(|source| self.repository_json(source)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
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
            "image_bindings": self.image_bindings.iter().map(|target| json!({
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

    fn repository_json(&self, repository: &HelmRepository) -> Value {
        json!({
            "path": self.relative(&repository.path),
            "document_index": repository.document_index,
            "name": repository.name,
            "namespace": repository.namespace,
            "url": repository.url,
            "repo_type": repo_type_json_value(&repository.repo_type),
        })
    }

    pub fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }
}

fn repo_type_json_value(repo_type: &RepoType) -> String {
    match repo_type {
        RepoType::Default => "default".to_string(),
        RepoType::Oci => "oci".to_string(),
        RepoType::Other(value) => value.clone(),
    }
}
