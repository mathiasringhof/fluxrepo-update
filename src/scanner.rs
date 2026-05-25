use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::models::{
    DeploymentImageTarget, HelmReleaseTarget, HelmRepository, ImageReference, Inventory, RepoType,
    ResourceId,
};
use yaml_serde::{Deserializer, Mapping, Value};

const IGNORED_DIR_NAMES: &[&str] = &[
    ".git",
    ".pytest_cache",
    ".ruff_cache",
    ".uv-cache",
    ".venv",
    "__pycache__",
    "target",
];

pub fn scan_repo(repo_root: &Path) -> Result<Inventory> {
    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", repo_root.display()))?;
    let mut inventory = Inventory::new(repo_root.clone());
    let mut release_candidates = Vec::new();
    let mut full_source_by_id: HashMap<ResourceId, HelmReleaseTarget> = HashMap::new();

    for path in iter_yaml_files(&repo_root)? {
        if is_skipped_path(&path) {
            inventory.skipped_paths.push(path);
            continue;
        }

        let documents = load_yaml_documents(&path)?;
        for (document_index, document) in documents.iter().enumerate() {
            let Some(mapping) = document.as_mapping() else {
                continue;
            };

            let kind = string_field(mapping, "kind").unwrap_or_default();
            if kind == "HelmRepository" {
                if let Some(repository) = parse_repository(&path, document_index, mapping) {
                    inventory
                        .repositories
                        .insert(repository.name.clone(), repository);
                }
                continue;
            }

            if kind == "HelmRelease" {
                if let Some(target) = parse_helmrelease(&path, document_index, mapping) {
                    if target.chart_name.is_some() && target.repo_name.is_some() {
                        let replace_existing = full_source_by_id
                            .get(&target.resource_id)
                            .is_none_or(|existing| prefer_source(&target.path, &existing.path));
                        if replace_existing {
                            full_source_by_id.insert(target.resource_id.clone(), target.clone());
                        }
                    }
                    release_candidates.push(target);
                }
            } else if kind == "Deployment" {
                inventory
                    .deployment_targets
                    .extend(parse_deployment_targets(
                        &path,
                        document_index,
                        mapping,
                        document,
                    ));
            }

            inventory.image_references.extend(parse_image_references(
                &path,
                document_index,
                &kind,
                mapping,
                document,
            ));
        }
    }

    for mut target in release_candidates {
        if (target.chart_name.is_none() || target.repo_name.is_none())
            && let Some(source) = full_source_by_id.get(&target.resource_id)
        {
            target.chart_name.clone_from(&source.chart_name);
            target.repo_name.clone_from(&source.repo_name);
            target.repo_kind.clone_from(&source.repo_kind);
            target.source_path = Some(source.path.clone());
            target.source_document_index = Some(source.document_index);
            target.source_is_inherited =
                source.path != target.path || source.document_index != target.document_index;
        }

        if target.can_update() {
            inventory.chart_targets.push(target);
        } else if target.current_version.is_some() {
            inventory.unresolved_chart_targets.push(target);
        } else {
            inventory.helmreleases_without_chart_version.push(target);
        }
    }

    inventory
        .chart_targets
        .sort_by_key(|item| (item.path.clone(), item.document_index));
    inventory.deployment_targets.sort_by_key(|item| {
        (
            item.path.clone(),
            item.document_index,
            item.yaml_path.clone(),
        )
    });
    inventory
        .helmreleases_without_chart_version
        .sort_by_key(|item| (item.path.clone(), item.document_index));
    inventory
        .unresolved_chart_targets
        .sort_by_key(|item| (item.path.clone(), item.document_index));
    inventory.image_references.sort_by_key(|item| {
        (
            item.path.clone(),
            item.document_index,
            item.yaml_path.clone(),
        )
    });

    Ok(inventory)
}

fn iter_yaml_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();
    collect_yaml_files(repo_root, &mut results)?;
    results.sort();
    Ok(results)
}

fn collect_yaml_files(path: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read directory {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let child_path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if IGNORED_DIR_NAMES.contains(&dir_name.as_str()) || dir_name.starts_with('.') {
                continue;
            }
            collect_yaml_files(&child_path, results)?;
        } else if child_path
            .extension()
            .is_some_and(|extension| extension == "yaml")
        {
            results.push(child_path);
        }
    }
    Ok(())
}

fn is_skipped_path(path: &Path) -> bool {
    path.to_string_lossy().contains("flux-system/gotk-")
}

fn load_yaml_documents(path: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read YAML file {}", path.display()))?;
    Deserializer::from_str(&text)
        .map(Value::deserialize)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse YAML file {}", path.display()))
}

fn parse_repository(
    path: &Path,
    document_index: usize,
    document: &Mapping,
) -> Option<HelmRepository> {
    let metadata = mapping_field(document, "metadata")?;
    let spec = mapping_field(document, "spec")?;
    let name = string_field(metadata, "name")?;
    let url = string_field(spec, "url")?;
    let repo_type = string_field(spec, "type")
        .map(RepoType::from)
        .unwrap_or(RepoType::Default);

    Some(HelmRepository {
        name,
        url,
        repo_type,
        path: path.to_path_buf(),
        document_index,
    })
}

fn parse_helmrelease(
    path: &Path,
    document_index: usize,
    document: &Mapping,
) -> Option<HelmReleaseTarget> {
    let metadata = mapping_field(document, "metadata")?;
    let name = string_field(metadata, "name")?;
    let namespace = string_field(metadata, "namespace");

    Some(HelmReleaseTarget {
        path: path.to_path_buf(),
        document_index,
        resource_id: ResourceId {
            kind: "HelmRelease".to_string(),
            name,
            namespace,
        },
        chart_name: nested_string(document, &["spec", "chart", "spec", "chart"]),
        current_version: nested_string(document, &["spec", "chart", "spec", "version"]),
        repo_name: nested_string(document, &["spec", "chart", "spec", "sourceRef", "name"]),
        repo_kind: nested_string(document, &["spec", "chart", "spec", "sourceRef", "kind"]),
        source_path: Some(path.to_path_buf()),
        source_document_index: Some(document_index),
        source_is_inherited: false,
    })
}

fn parse_deployment_targets(
    path: &Path,
    document_index: usize,
    document: &Mapping,
    value: &Value,
) -> Vec<DeploymentImageTarget> {
    let Some(metadata) = mapping_field(document, "metadata") else {
        return Vec::new();
    };
    let Some(name) = string_field(metadata, "name") else {
        return Vec::new();
    };
    let namespace = string_field(metadata, "namespace");
    let resource_id = ResourceId {
        kind: "Deployment".to_string(),
        name,
        namespace,
    };

    iter_images(value)
        .into_iter()
        .filter(|(yaml_path, _)| is_deployment_image_path(yaml_path))
        .map(|(yaml_path, image)| DeploymentImageTarget {
            path: path.to_path_buf(),
            document_index,
            resource_id: resource_id.clone(),
            yaml_path,
            image,
        })
        .collect()
}

fn parse_image_references(
    path: &Path,
    document_index: usize,
    kind: &str,
    document: &Mapping,
    value: &Value,
) -> Vec<ImageReference> {
    let manifest_name =
        mapping_field(document, "metadata").and_then(|metadata| string_field(metadata, "name"));

    iter_images(value)
        .into_iter()
        .map(|(yaml_path, image)| ImageReference {
            path: path.to_path_buf(),
            document_index,
            manifest_kind: if kind.is_empty() {
                "Unknown".to_string()
            } else {
                kind.to_string()
            },
            manifest_name: manifest_name.clone(),
            yaml_path,
            image,
        })
        .collect()
}

fn iter_images(value: &Value) -> Vec<(String, String)> {
    let mut results = Vec::new();
    collect_images(value, "", &mut results);
    results
}

fn collect_images(value: &Value, path: &str, results: &mut Vec<(String, String)>) {
    match value {
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                let Some(key_text) = key.as_str() else {
                    continue;
                };
                let child_path = if path.is_empty() {
                    key_text.to_string()
                } else {
                    format!("{path}.{key_text}")
                };

                if key_text == "image" {
                    if let Some(image) = child.as_str() {
                        results.push((child_path, image.to_string()));
                    } else if let Some(image_mapping) = child.as_mapping()
                        && let Some(repository) = string_field(image_mapping, "repository")
                    {
                        let rendered = match string_field(image_mapping, "tag") {
                            Some(tag) if !tag.is_empty() => format!("{repository}:{tag}"),
                            _ => repository,
                        };
                        results.push((child_path, rendered));
                    }
                } else {
                    collect_images(child, &child_path, results);
                }
            }
        }
        Value::Sequence(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_images(child, &format!("{path}[{index}]"), results);
            }
        }
        _ => {}
    }
}

fn is_deployment_image_path(yaml_path: &str) -> bool {
    yaml_path.starts_with("spec.template.spec.containers[")
        || yaml_path.starts_with("spec.template.spec.initContainers[")
}

fn prefer_source(candidate: &Path, existing: &Path) -> bool {
    let candidate_text = candidate.to_string_lossy();
    let existing_text = existing.to_string_lossy();
    if candidate_text.contains("/base/") && !existing_text.contains("/base/") {
        return true;
    }
    if !candidate_text.contains("/base/") && existing_text.contains("/base/") {
        return false;
    }
    candidate_text < existing_text
}

fn nested_string(document: &Mapping, path: &[&str]) -> Option<String> {
    let mut current = document;
    for (index, key) in path.iter().enumerate() {
        let value = current.get(Value::String((*key).to_string()))?;
        if index == path.len() - 1 {
            return value_to_string(value);
        }
        current = value.as_mapping()?;
    }
    None
}

fn mapping_field<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Mapping> {
    mapping
        .get(Value::String(key.to_string()))
        .and_then(Value::as_mapping)
}

fn string_field(mapping: &Mapping, key: &str) -> Option<String> {
    mapping
        .get(Value::String(key.to_string()))
        .and_then(value_to_string)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}
