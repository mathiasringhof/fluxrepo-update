//! Only local `resources` inclusion and namespace precedence; no rendering or writes.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use yaml_serde::Value;

use crate::models::{Inventory, NamespaceContext};

#[derive(Default)]
struct InclusionGraph {
    namespaces: BTreeMap<PathBuf, Option<String>>,
    parents: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    manifests: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
}

#[derive(Default)]
struct NamespaceChains {
    // None represents a complete inclusion chain with no namespace transformer.
    outcomes: BTreeSet<Option<(String, PathBuf)>>,
    bindings: BTreeSet<PathBuf>,
    cyclic: bool,
}

pub(super) fn derive_namespaces(inventory: &mut Inventory) {
    let mut files = BTreeMap::<_, Vec<_>>::new();
    for ((path, index), document) in &inventory.manifest_documents {
        files.entry(path).or_default().push((*index, document));
    }
    let kustomizations = inventory
        .manifest_documents
        .iter()
        .filter(|((path, index), document)| {
            *index == 0
                && !inventory
                    .manifest_documents
                    .contains_key(&(path.clone(), 1))
                && is_kustomization(path, document)
        })
        .collect::<Vec<_>>();
    let mut graph = InclusionGraph::default();
    for ((path, _), document) in &kustomizations {
        graph.namespaces.insert(
            path.clone(),
            document
                .get("namespace")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }
    for ((path, _), document) in kustomizations {
        let Some(resources) = document.get("resources").and_then(Value::as_sequence) else {
            continue;
        };
        for resource in resources.iter().filter_map(Value::as_str) {
            let Some(resource) = local_resource(path, resource, &inventory.repo_root) else {
                continue;
            };
            if files.contains_key(&resource) {
                graph
                    .manifests
                    .entry(resource)
                    .or_default()
                    .insert(path.clone());
            } else {
                for filename in ["kustomization.yaml", "kustomization.yml"] {
                    let child = resource.join(filename);
                    if graph.namespaces.contains_key(&child) {
                        graph.parents.entry(child).or_default().insert(path.clone());
                    }
                }
            }
        }
    }
    for (path, documents) in files {
        let Some(parents) = graph.manifests.get(path) else {
            continue;
        };
        let mut chains = NamespaceChains::default();
        for parent in parents {
            graph.walk(parent, None, &mut BTreeSet::new(), &mut chains);
        }
        for (index, document) in documents {
            let raw_namespace = document
                .as_mapping()
                .and_then(|mapping| super::nested_string(mapping, &["metadata", "namespace"]));
            let mut context = NamespaceContext::raw(raw_namespace.as_deref());
            let namespaces = chains
                .outcomes
                .iter()
                .map(|outcome| {
                    outcome
                        .as_ref()
                        .map(|(namespace, _)| namespace.as_str())
                        .or(raw_namespace.as_deref())
                })
                .collect::<BTreeSet<_>>();
            context.namespace_ambiguous = chains.cyclic || namespaces.len() != 1;
            context.effective_namespace = if context.namespace_ambiguous {
                None
            } else {
                namespaces.first().copied().flatten().map(str::to_owned)
            };
            context.namespace_kustomizations = chains
                .outcomes
                .iter()
                .flatten()
                .map(|(_, path)| path.clone())
                .collect();
            context.bindings.clone_from(&chains.bindings);
            inventory
                .namespace_contexts
                .insert((path.clone(), index), context);
        }
    }
}

impl InclusionGraph {
    fn walk(
        &self,
        path: &Path,
        inner: Option<(String, PathBuf)>,
        visiting: &mut BTreeSet<PathBuf>,
        chains: &mut NamespaceChains,
    ) {
        if !visiting.insert(path.to_path_buf()) {
            chains.cyclic = true;
            return;
        }
        chains.bindings.insert(path.to_path_buf());
        let namespace = self
            .namespaces
            .get(path)
            .and_then(Option::as_ref)
            .map(|namespace| (namespace.clone(), path.to_path_buf()))
            .or(inner);
        if let Some(parents) = self.parents.get(path) {
            for parent in parents {
                self.walk(parent, namespace.clone(), visiting, chains);
            }
        } else {
            chains.outcomes.insert(namespace);
        }
        visiting.remove(path);
    }
}

fn is_kustomization(path: &Path, document: &Value) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("kustomization.yaml" | "kustomization.yml")
    ) && document
        .get("kind")
        .is_none_or(|kind| kind.as_str() == Some("Kustomization"))
        && document.get("apiVersion").is_none_or(|version| {
            version
                .as_str()
                .is_some_and(|version| version.starts_with("kustomize.config.k8s.io/"))
        })
}

fn local_resource(kustomization: &Path, resource: &str, repo_root: &Path) -> Option<PathBuf> {
    if resource.contains([':', '?', '#'])
        || resource.starts_with("github.com/")
        || Path::new(resource).is_absolute()
    {
        return None;
    }
    let mut path = kustomization.parent()?.to_path_buf();
    for component in Path::new(resource).components() {
        match component {
            Component::Normal(name) => path.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                path.pop();
            }
            _ => return None,
        }
        if !path.starts_with(repo_root) {
            return None;
        }
        // Do not normalize through missing paths or symlinks excluded by the scan.
        if path.symlink_metadata().ok()?.file_type().is_symlink() {
            return None;
        }
    }
    (path.canonicalize().ok()? == path).then_some(path)
}
