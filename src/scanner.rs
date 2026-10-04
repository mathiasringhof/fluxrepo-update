use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use yaml_edit::{Document as EditDocument, YamlFile, YamlNode};

use crate::models::{
    HelmReleaseTarget, HelmRepository, ImageBinding, ImageBindingValueKind, ImageReference,
    Inventory, RemoteResourceTarget, RepoType, ResourceId, UncheckedVersionDeclaration,
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
            inventory
                .manifest_documents
                .insert((path.clone(), document_index), document.clone());
            let Some(mapping) = document.as_mapping() else {
                continue;
            };

            let kind = string_field(mapping, "kind").unwrap_or_default();
            if kind == "HelmRepository" {
                if let Some(repository) = parse_repository(&path, document_index, mapping) {
                    inventory.add_repository(repository);
                }
                continue;
            }

            if kind == "HelmRelease" {
                if let Some(values) = value_at_path(document, "spec.values") {
                    let mut declarations = Vec::new();
                    collect_tag_only_overrides(values, "spec.values", &mut declarations);
                    inventory.unchecked_version_declarations.extend(declarations.into_iter().map(|(yaml_path, current_value)| UncheckedVersionDeclaration {
                        path: path.clone(), document_index, yaml_path, current_value,
                        reason: "image tag has no manifest-local repository; chart defaults are not evaluated".into(),
                    }));
                }
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

            if kind == "Cluster"
                && string_field(mapping, "apiVersion")
                    .is_some_and(|version| version.starts_with("postgresql.cnpg.io/"))
                && let Some(image) =
                    value_at_path(document, "spec.imageName").and_then(Value::as_str)
            {
                inventory
                    .unchecked_version_declarations
                    .push(UncheckedVersionDeclaration {
                        path: path.clone(),
                        document_index,
                        yaml_path: "spec.imageName".into(),
                        current_value: image.into(),
                        reason: "CloudNativePG imageName checking is not supported".into(),
                    });
            }

            inventory.image_references.extend(parse_image_references(
                &path,
                document_index,
                &kind,
                mapping,
                document,
            ));
            if (kind == "Kustomization"
                || (kind.is_empty()
                    && matches!(
                        path.file_name().and_then(|name| name.to_str()),
                        Some("kustomization.yaml" | "kustomization.yml")
                    )))
                && let Some(resources) = mapping
                    .get(Value::String("resources".into()))
                    .and_then(Value::as_sequence)
            {
                for (index, resource) in resources.iter().enumerate() {
                    if let Some(url) = resource.as_str()
                        && (url.split_once("://").is_some_and(|(scheme, _)| {
                            scheme.eq_ignore_ascii_case("https")
                                || scheme.eq_ignore_ascii_case("http")
                        }) || url.to_ascii_lowercase().starts_with("github.com/"))
                    {
                        if let Some(reference) = crate::github::parse_github_resource(url) {
                            inventory
                                .remote_resource_targets
                                .push(RemoteResourceTarget {
                                    path: path.clone(),
                                    document_index,
                                    resource_id: parse_resource_id("Kustomization", mapping)
                                        .unwrap_or_else(|| ResourceId {
                                            kind: "Kustomization".into(),
                                            name: path
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy()
                                                .into_owned(),
                                            namespace: None,
                                        }),
                                    yaml_path: format!("resources[{index}]"),
                                    reference,
                                });
                            continue;
                        }
                        inventory.unchecked_version_declarations.push(
                            UncheckedVersionDeclaration {
                                path: path.clone(),
                                document_index,
                                yaml_path: format!("resources[{index}]"),
                                current_value: url.into(),
                                reason:
                                    "remote Kustomize resource is not a supported public GitHub release version pin"
                                        .into(),
                            },
                        );
                    }
                }
            }
        }
    }

    inventory.combine_equivalent_sources();
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
    inventory.remote_resource_targets.sort_by_key(|item| {
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
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "flux-system")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("gotk-"))
}

fn load_yaml_documents(path: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read YAML file {}", path.display()))?;
    parse_yaml_documents(&text)
        .with_context(|| format!("failed to parse YAML file {}", path.display()))
}

pub(crate) fn parse_yaml_documents(text: &str) -> Result<Vec<Value>> {
    let mut values = Deserializer::from_str(text)
        .map(Value::deserialize)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let edit: YamlFile = text.parse()?;
    for (value, document) in values.iter_mut().zip(edit.documents()) {
        if let Some(mapping) = document.as_mapping() {
            preserve_numeric_text(value, &YamlNode::Mapping(mapping));
        }
    }
    Ok(values)
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
        namespace: string_field(metadata, "namespace"),
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
    let resource_id = parse_resource_id("HelmRelease", document)?;
    let source_kind = nested_string(document, &["spec", "chart", "spec", "sourceRef", "kind"]);
    let repo_name = if source_kind.as_deref() == Some("HelmRepository") {
        nested_string(document, &["spec", "chart", "spec", "sourceRef", "name"])
    } else {
        None
    };

    Some(HelmReleaseTarget {
        path: path.to_path_buf(),
        document_index,
        resource_id,
        chart_name: nested_string(document, &["spec", "chart", "spec", "chart"]),
        current_version: nested_string(document, &["spec", "chart", "spec", "version"]),
        repo_name,
        source_path: Some(path.to_path_buf()),
        source_is_inherited: false,
    })
}

fn parse_resource_id(kind: &str, document: &Mapping) -> Option<ResourceId> {
    let metadata = mapping_field(document, "metadata")?;
    Some(ResourceId {
        kind: kind.to_string(),
        name: string_field(metadata, "name")?,
        namespace: string_field(metadata, "namespace"),
    })
}

fn parse_workload_targets(
    path: &Path,
    document_index: usize,
    kind: &str,
    document: &Mapping,
    value: &Value,
) -> Vec<ImageBinding> {
    let Some(resource_id) = parse_resource_id(kind, document) else {
        return Vec::new();
    };
    let podspec_path = match kind {
        "Pod" => "spec",
        "CronJob" => "spec.jobTemplate.spec.template.spec",
        _ => "spec.template.spec",
    };
    ["containers", "initContainers"]
        .into_iter()
        .flat_map(|field| {
            let containers_path = format!("{podspec_path}.{field}");
            value_at_path(value, &containers_path)
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(move |(index, container)| {
                    Some((
                        format!("{containers_path}[{index}].image"),
                        container.get("image")?.as_str()?.to_string(),
                    ))
                })
        })
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
    let Some(resource_id) = parse_resource_id("HelmRelease", document) else {
        return Vec::new();
    };
    let Some(values) = value_at_path(value, "spec.values") else {
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
    walk_yaml(value, path, None, &mut |value, path, key| {
        if let Some(mapping) = value.as_mapping() {
            if let Some(image) = concrete_image_mapping(mapping) {
                results.push((format!("{path}.tag"), image, ImageBindingValueKind::Tag));
            } else if let Some(image) = recognizable_unsupported_image_mapping(mapping) {
                results.push((
                    path.to_string(),
                    image,
                    ImageBindingValueKind::UnsupportedSchema,
                ));
            }
        }
        if key == Some("image")
            && let Some(image) = value.as_str()
        {
            results.push((
                path.to_string(),
                image.to_string(),
                ImageBindingValueKind::ImageReference,
            ));
        }
    });
}

fn collect_tag_only_overrides(value: &Value, path: &str, results: &mut Vec<(String, String)>) {
    walk_yaml(value, path, None, &mut |value, path, key| {
        if key == Some("image")
            && let Some(image) = value.as_mapping()
            && string_field(image, "repository")
                .is_none_or(|repository| repository.trim().is_empty())
            && let Some(tag) = string_field(image, "tag")
        {
            results.push((format!("{path}.tag"), tag));
        }
    });
}

fn walk_yaml(
    value: &Value,
    path: &str,
    key: Option<&str>,
    visit: &mut impl FnMut(&Value, &str, Option<&str>),
) {
    visit(value, path, key);
    match value {
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                let Some(key) = key.as_str() else { continue };
                walk_yaml(child, &append_yaml_key(path, key), Some(key), visit);
            }
        }
        Value::Sequence(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_yaml(child, &format!("{path}[{index}]"), None, visit);
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
    let repository =
        string_field(mapping, "repository").filter(|value| !value.trim().is_empty())?;
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
    walk_yaml(value, "", None, &mut |value, path, key| {
        if let Some(mapping) = value.as_mapping() {
            if let Some(image) = concrete_image_mapping(mapping) {
                results.push((path.to_string(), image));
            } else if key == Some("image")
                && let Some(repository) = string_field(mapping, "repository")
            {
                let rendered = match string_field(mapping, "tag") {
                    Some(tag) if !tag.is_empty() => format!("{repository}:{tag}"),
                    _ => repository,
                };
                results.push((path.to_string(), rendered));
            }
        }
        if key == Some("image")
            && let Some(image) = value.as_str()
        {
            results.push((path.to_string(), image.to_string()));
        }
    });
    results
}

fn is_podspec_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Deployment" | "StatefulSet" | "DaemonSet" | "Job" | "CronJob" | "Pod"
    )
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

#[derive(Debug, Clone)]
pub(crate) enum YamlPathPart {
    Key(String),
    Index(usize),
}

fn append_yaml_key(path: &str, key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-'))
    {
        if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        }
    } else {
        format!(
            "{path}[{}]",
            serde_json::to_string(key).expect("string serializes")
        )
    }
}

pub(crate) fn yaml_path_parts(path: &str) -> Option<Vec<YamlPathPart>> {
    let mut result = Vec::new();
    let mut remaining = path;
    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix('.') {
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix('[') {
            if rest.starts_with('"') {
                let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<String>();
                let key = stream.next()?.ok()?;
                remaining = rest.get(stream.byte_offset()..)?.strip_prefix(']')?;
                result.push(YamlPathPart::Key(key));
            } else {
                let (index, rest) = rest.split_once(']')?;
                result.push(YamlPathPart::Index(index.parse().ok()?));
                remaining = rest;
            }
        } else {
            let end = remaining.find(['.', '[']).unwrap_or(remaining.len());
            result.push(YamlPathPart::Key(remaining[..end].to_string()));
            remaining = &remaining[end..];
        }
    }
    Some(result)
}

pub(crate) fn edit_node_at_path(document: &EditDocument, path: &str) -> Option<YamlNode> {
    let mut current = YamlNode::Mapping(document.as_mapping()?);
    for part in yaml_path_parts(path)? {
        current = match part {
            YamlPathPart::Key(key) => current.as_mapping()?.get(key.as_str())?,
            YamlPathPart::Index(index) => current.as_sequence()?.get(index)?,
        };
    }
    Some(current)
}

pub(crate) fn value_at_path<'a>(document: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = document;
    for part in yaml_path_parts(path)? {
        current = match part {
            YamlPathPart::Key(key) => current.as_mapping()?.get(Value::String(key))?,
            YamlPathPart::Index(index) => current.as_sequence()?.get(index)?,
        };
    }
    Some(current)
}

pub(crate) fn value_at_path_mut<'a>(document: &'a mut Value, path: &str) -> Option<&'a mut Value> {
    let mut current = document;
    for part in yaml_path_parts(path)? {
        current = match part {
            YamlPathPart::Key(key) => current.as_mapping_mut()?.get_mut(Value::String(key))?,
            YamlPathPart::Index(index) => current.as_sequence_mut()?.get_mut(index)?,
        };
    }
    Some(current)
}

fn preserve_numeric_text(value: &mut Value, node: &YamlNode) {
    match value {
        Value::Number(_) => {
            if let Some(scalar) = node.as_scalar() {
                *value = Value::String(scalar.as_string());
            }
        }
        Value::Mapping(mapping) => {
            if let Some(edit_mapping) = node.as_mapping() {
                for (key, child) in mapping {
                    if let Some(key) = key.as_str()
                        && let Some(edit_child) = edit_mapping.get(key)
                    {
                        preserve_numeric_text(child, &edit_child);
                    }
                }
            }
        }
        Value::Sequence(items) => {
            if let Some(sequence) = node.as_sequence() {
                for (index, child) in items.iter_mut().enumerate() {
                    if let Some(edit_child) = sequence.get(index) {
                        preserve_numeric_text(child, &edit_child);
                    }
                }
            }
        }
        _ => {}
    }
}
