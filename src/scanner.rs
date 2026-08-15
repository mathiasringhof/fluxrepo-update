use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::models::{
    HelmReleaseTarget, HelmRepository, ImageBinding, ImageBindingValueKind, ImageReference,
    Inventory, RepoType, ResourceId,
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
                    if target.can_update() {
                        inventory.chart_targets.push(target);
                    } else if target.current_version.is_some() {
                        inventory.unresolved_chart_targets.push(target);
                    } else {
                        inventory.helmreleases_without_chart_version.push(target);
                    }
                }
                inventory.image_bindings.extend(parse_helm_value_targets(
                    &path,
                    document_index,
                    mapping,
                    document,
                ));
            } else if is_podspec_kind(&kind) {
                inventory.image_bindings.extend(parse_workload_targets(
                    &path,
                    document_index,
                    &kind,
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

    inventory
        .chart_targets
        .sort_by_key(|item| (item.path.clone(), item.document_index));
    inventory.image_bindings.sort_by_key(|item| {
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
    entries.sort_by_key(std::fs::DirEntry::path);

    for entry in entries {
        let child_path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if IGNORED_DIR_NAMES.contains(&dir_name.as_str()) || dir_name.starts_with('.') {
                continue;
            }
            collect_yaml_files(&child_path, results)?;
        } else if file_type.is_file()
            && child_path
                .extension()
                .is_some_and(|extension| extension == "yaml" || extension == "yml")
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
    let repo_type = string_field(spec, "type").map_or(RepoType::Default, RepoType::from);

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
    let source_kind = nested_string(document, &["spec", "chart", "spec", "sourceRef", "kind"]);
    let repo_name = if source_kind.as_deref() == Some("HelmRepository") {
        nested_string(document, &["spec", "chart", "spec", "sourceRef", "name"])
    } else {
        None
    };

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
        repo_name,
        source_path: Some(path.to_path_buf()),
        source_is_inherited: false,
    })
}

fn parse_workload_targets(
    path: &Path,
    document_index: usize,
    kind: &str,
    document: &Mapping,
    value: &Value,
) -> Vec<ImageBinding> {
    let Some(metadata) = mapping_field(document, "metadata") else {
        return Vec::new();
    };
    let Some(name) = string_field(metadata, "name") else {
        return Vec::new();
    };
    let namespace = string_field(metadata, "namespace");
    let resource_id = ResourceId {
        kind: kind.to_string(),
        name,
        namespace,
    };

    iter_scalar_images(value)
        .into_iter()
        .filter(|(yaml_path, _)| is_workload_image_path(kind, yaml_path))
        .map(|(yaml_path, image)| ImageBinding {
            path: path.to_path_buf(),
            document_index,
            resource_id: resource_id.clone(),
            yaml_path,
            image,
            value_kind: ImageBindingValueKind::ImageReference,
        })
        .collect()
}

fn parse_helm_value_targets(
    path: &Path,
    document_index: usize,
    document: &Mapping,
    value: &Value,
) -> Vec<ImageBinding> {
    let Some(metadata) = mapping_field(document, "metadata") else {
        return Vec::new();
    };
    let Some(name) = string_field(metadata, "name") else {
        return Vec::new();
    };
    let resource_id = ResourceId {
        kind: "HelmRelease".to_string(),
        name,
        namespace: string_field(metadata, "namespace"),
    };
    let Some(values) = value
        .as_mapping()
        .and_then(|root| root.get(Value::String("spec".to_string())))
        .and_then(Value::as_mapping)
        .and_then(|spec| spec.get(Value::String("values".to_string())))
    else {
        return Vec::new();
    };

    let mut bindings = Vec::new();
    collect_helm_value_bindings(values, "spec.values", &mut bindings);
    bindings
        .into_iter()
        .map(|(yaml_path, image, value_kind)| ImageBinding {
            path: path.to_path_buf(),
            document_index,
            resource_id: resource_id.clone(),
            yaml_path,
            image,
            value_kind,
        })
        .collect()
}

fn collect_helm_value_bindings(
    value: &Value,
    path: &str,
    results: &mut Vec<(String, String, ImageBindingValueKind)>,
) {
    match value {
        Value::Mapping(mapping) => {
            if let Some(image) = concrete_image_mapping(mapping) {
                results.push((format!("{path}.tag"), image, ImageBindingValueKind::Tag));
            } else if let Some(image) = recognizable_unsupported_image_mapping(mapping) {
                results.push((
                    path.to_string(),
                    image,
                    ImageBindingValueKind::UnsupportedSchema,
                ));
            }
            for (key, child) in mapping {
                let Some(key) = key.as_str() else {
                    continue;
                };
                let child_path = format!("{path}.{key}");
                if key == "image"
                    && let Some(image) = child.as_str()
                {
                    results.push((
                        child_path.clone(),
                        image.to_string(),
                        ImageBindingValueKind::ImageReference,
                    ));
                }
                collect_helm_value_bindings(child, &child_path, results);
            }
        }
        Value::Sequence(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_helm_value_bindings(child, &format!("{path}[{index}]"), results);
            }
        }
        _ => {}
    }
}

fn recognizable_unsupported_image_mapping(mapping: &Mapping) -> Option<String> {
    let repository = string_field(mapping, "repository")?;
    if mapping.contains_key(Value::String("tag".to_string())) {
        return None;
    }
    let (field, version) = ["version", "imageTag"]
        .into_iter()
        .find_map(|field| string_field(mapping, field).map(|value| (field, value)))?;
    Some(format!("{repository} ({field}: {version})"))
}

fn concrete_image_mapping(mapping: &Mapping) -> Option<String> {
    let repository = string_field(mapping, "repository")?;
    let tag = string_field(mapping, "tag").filter(|tag| !tag.trim().is_empty())?;
    let registry = string_field(mapping, "registry").filter(|registry| !registry.trim().is_empty());
    let repository = match registry {
        Some(registry) if !repository.starts_with(&format!("{registry}/")) => {
            format!("{registry}/{repository}")
        }
        _ => repository,
    };
    let digest = ["digest", "sha", "sha256"]
        .into_iter()
        .find_map(|field| string_field(mapping, field))
        .filter(|digest| !digest.trim().is_empty());
    Some(digest.map_or_else(
        || format!("{repository}:{tag}"),
        |digest| format!("{repository}:{tag}@{digest}"),
    ))
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

fn iter_scalar_images(value: &Value) -> Vec<(String, String)> {
    let mut results = Vec::new();
    collect_scalar_images(value, "", &mut results);
    results
}

fn collect_scalar_images(value: &Value, path: &str, results: &mut Vec<(String, String)>) {
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
                    }
                } else {
                    collect_scalar_images(child, &child_path, results);
                }
            }
        }
        Value::Sequence(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_scalar_images(child, &format!("{path}[{index}]"), results);
            }
        }
        _ => {}
    }
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

fn is_podspec_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Deployment" | "StatefulSet" | "DaemonSet" | "Job" | "CronJob" | "Pod"
    )
}

fn is_workload_image_path(kind: &str, yaml_path: &str) -> bool {
    let podspec_path = match kind {
        "Pod" => "spec",
        "CronJob" => "spec.jobTemplate.spec.template.spec",
        _ => "spec.template.spec",
    };
    ["containers", "initContainers"].into_iter().any(|field| {
        let prefix = format!("{podspec_path}.{field}[");
        let Some(remainder) = yaml_path.strip_prefix(&prefix) else {
            return false;
        };
        let Some((index, suffix)) = remainder.split_once(']') else {
            return false;
        };
        !index.is_empty()
            && index.chars().all(|character| character.is_ascii_digit())
            && suffix == ".image"
    })
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
