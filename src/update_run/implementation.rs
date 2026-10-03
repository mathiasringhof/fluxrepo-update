use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use serde_json::{Value as JsonValue, json};
use yaml_edit::{Document as EditDocument, Scalar as EditScalar, ScalarValue, YamlFile};

use crate::scanner::{edit_node_at_path, parse_yaml_documents, value_at_path, value_at_path_mut};

use crate::models::{
    HelmReleaseTarget, ImageBinding, ImageBindingValueKind, Inventory, ResourceId, TargetKind,
    UncheckedVersionDeclaration,
};
use crate::resolvers::{
    ChartVersionResolver, ImageVersionResolver, ResolverError, ResolverErrorCode,
    is_newer_image_tag, is_newer_version, parse_image_reference,
};

#[derive(Debug, Clone)]
pub struct PlannedChartUpdate {
    pub manifest_identity: Option<ManifestIdentity>,
    pub path: PathBuf,
    pub document_index: usize,
    pub target_name: String,
    pub chart_name: String,
    pub repo_name: String,
    pub current_version: String,
    pub latest_version: String,
}

#[derive(Debug, Clone)]
pub struct PlannedImageUpdate {
    pub manifest_identity: Option<ManifestIdentity>,
    pub path: PathBuf,
    pub document_index: usize,
    pub target_name: String,
    pub yaml_path: String,
    pub current_image: String,
    pub latest_image: String,
    pub current_version: String,
    pub latest_version: String,
    pub value_kind: ImageBindingValueKind,
}

#[derive(Debug, Clone)]
pub struct ManifestIdentity {
    pub resource_id: ResourceId,
    fields: Vec<(String, Option<yaml_serde::Value>)>,
    sources: Vec<SourceIdentity>,
}

#[derive(Debug, Clone)]
struct SourceIdentity {
    path: PathBuf,
    document_index: usize,
    document: yaml_serde::Value,
}

impl ManifestIdentity {
    pub(super) fn source_locations(&self) -> Vec<(&Path, usize)> {
        self.sources
            .iter()
            .map(|source| (source.path.as_path(), source.document_index))
            .collect()
    }

    fn selection_context(&self, repo_root: &Path) -> JsonValue {
        let fields = self
            .fields
            .iter()
            .map(|(path, value)| {
                let value = value.as_ref().map_or_else(
                    || json!({"present": false}),
                    |value| json!({"present": true, "value": canonical_yaml_value(value)}),
                );
                (path.clone(), value)
            })
            .collect::<serde_json::Map<_, _>>();
        let sources = self
            .sources
            .iter()
            .map(|source| {
                json!({
                    "path": relative_identity_path(&source.path, repo_root),
                    "document_index": source.document_index,
                    "document": canonical_yaml_value(&source.document),
                })
            })
            .collect::<Vec<_>>();
        let source = match sources.as_slice() {
            [] => JsonValue::Null,
            [source] => source.clone(),
            _ => json!(sources),
        };
        json!({"fields": fields, "source": source})
    }

    fn capture(
        inventory: &Inventory,
        path: &Path,
        document_index: usize,
        resource_id: &ResourceId,
        paths: &[String],
    ) -> Option<Self> {
        let document = inventory
            .manifest_documents
            .get(&(path.to_path_buf(), document_index))?;
        let fields = ["kind", "metadata.name", "metadata.namespace"]
            .into_iter()
            .map(str::to_string)
            .chain(paths.iter().cloned())
            .map(|path| {
                let value = value_at_path(document, &path).cloned();
                (path, value)
            })
            .collect();
        Some(Self {
            resource_id: resource_id.clone(),
            fields,
            sources: Vec::new(),
        })
    }

    fn verify(&self, document: &yaml_serde::Value) -> Result<()> {
        for (path, expected) in &self.fields {
            if value_at_path(document, path) != expected.as_ref() {
                return Err(anyhow!("target identity at {path} changed after planning"));
            }
        }
        for source in &self.sources {
            let text = fs::read_to_string(&source.path)
                .with_context(|| format!("failed to recheck source {}", source.path.display()))?;
            let documents = parse_yaml_documents(&text)?;
            if documents.get(source.document_index) != Some(&source.document) {
                return Err(anyhow!(
                    "chart source {} changed after planning",
                    source.path.display()
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum PlannedUpdate {
    Chart(PlannedChartUpdate),
    Image(PlannedImageUpdate),
}

impl PlannedUpdate {
    pub(crate) fn has_original_identity(&self) -> bool {
        match self {
            Self::Chart(update) => update
                .manifest_identity
                .as_ref()
                .is_some_and(|identity| !identity.sources.is_empty()),
            Self::Image(update) => update.manifest_identity.is_some(),
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Chart(update) => &update.path,
            Self::Image(update) => &update.path,
        }
    }

    pub fn document_index(&self) -> usize {
        match self {
            Self::Chart(update) => update.document_index,
            Self::Image(update) => update.document_index,
        }
    }

    pub fn target_kind(&self) -> &str {
        self.kind().as_str()
    }

    pub fn kind(&self) -> TargetKind {
        match self {
            Self::Chart(_) => TargetKind::HelmRelease,
            Self::Image(_) => TargetKind::ImageBinding,
        }
    }

    pub fn target_name(&self) -> &str {
        match self {
            Self::Chart(update) => &update.target_name,
            Self::Image(update) => &update.target_name,
        }
    }

    pub fn current_version(&self) -> &str {
        match self {
            Self::Chart(update) => &update.current_version,
            Self::Image(update) => &update.current_version,
        }
    }

    pub fn latest_version(&self) -> &str {
        match self {
            Self::Chart(update) => &update.latest_version,
            Self::Image(update) => &update.latest_version,
        }
    }

    pub fn selection_id(&self, repo_root: &Path) -> String {
        let path = relative_identity_path(self.path(), repo_root);
        let mut parts = vec![
            self.target_kind().to_string(),
            path,
            self.document_index().to_string(),
            self.target_name().to_string(),
        ];
        match self {
            Self::Chart(update) => {
                parts.extend([
                    "spec.chart.spec.version".to_string(),
                    update.repo_name.clone(),
                    update.chart_name.clone(),
                    update.current_version.clone(),
                    update.latest_version.clone(),
                ]);
            }
            Self::Image(update) => {
                parts.extend([
                    update.yaml_path.clone(),
                    update.current_image.clone(),
                    update.latest_image.clone(),
                    update.current_version.clone(),
                    update.latest_version.clone(),
                ]);
            }
        }
        let identity = match self {
            Self::Chart(update) => &update.manifest_identity,
            Self::Image(update) => &update.manifest_identity,
        };
        parts.push(
            identity
                .as_ref()
                .map_or(JsonValue::Null, |identity| {
                    identity.selection_context(repo_root)
                })
                .to_string(),
        );
        format!(
            "v2:{}",
            parts
                .iter()
                .map(|part| encode_selection_id_part(part))
                .collect::<Vec<_>>()
                .join(":")
        )
    }
}

fn relative_identity_path(path: &Path, repo_root: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(repo_root) {
        return relative.to_string_lossy().into_owned();
    }
    repo_root
        .canonicalize()
        .ok()
        .and_then(|root| {
            path.strip_prefix(root)
                .ok()
                .map(|relative| relative.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn canonical_yaml_value(value: &yaml_serde::Value) -> JsonValue {
    use yaml_serde::Value;
    match value {
        Value::Null => JsonValue::Null,
        Value::Bool(value) => json!(value),
        Value::Number(value) => json!({"number": value.to_string()}),
        Value::String(value) => json!(value),
        Value::Sequence(values) => {
            JsonValue::Array(values.iter().map(canonical_yaml_value).collect())
        }
        Value::Mapping(mapping) => {
            let mut entries = mapping
                .iter()
                .map(|(key, value)| (canonical_yaml_value(key), canonical_yaml_value(value)))
                .collect::<Vec<_>>();
            entries.sort_by_cached_key(|(key, _)| key.to_string());
            json!({"mapping": entries})
        }
        Value::Tagged(value) => {
            json!({"tag": value.tag.to_string(), "value": canonical_yaml_value(&value.value)})
        }
    }
}

fn encode_selection_id_part(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push(nibble_to_hex(byte >> 4));
                encoded.push(nibble_to_hex(byte & 0x0f));
            }
        }
    }
    encoded
}

fn nibble_to_hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + value - 10) as char,
        _ => unreachable!("nibble values are always below 16"),
    }
}

#[derive(Debug, Clone)]
pub struct UpdateReport {
    pub planned: Vec<PlannedUpdate>,
    pub skipped: Vec<SkippedUpdate>,
    pub checked_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedUpdate {
    pub path: Option<PathBuf>,
    pub reason: String,
    pub reason_code: SkipReasonCode,
    pub retryable: bool,
    pub source_url: Option<String>,
    identity_suffix: Option<String>,
    yaml_path: Option<String>,
    current_value: Option<String>,
    document_index: Option<usize>,
}

impl SkippedUpdate {
    fn unchecked_declaration(declaration: &UncheckedVersionDeclaration) -> Self {
        let mut skipped = Self::with_reason(
            Some(declaration.path.clone()),
            SkipReason::new(
                &declaration.reason,
                SkipReasonCode::UnsupportedVersionDeclaration,
                false,
                None,
            ),
        )
        .with_target_identity(
            format!(
                "VersionDeclaration:{}:{}:{}",
                declaration.document_index, declaration.yaml_path, declaration.current_value
            ),
            &declaration.yaml_path,
        );
        skipped.current_value = Some(declaration.current_value.clone());
        skipped.document_index = Some(declaration.document_index);
        skipped
    }

    pub fn new(path: Option<PathBuf>, reason: impl Into<String>) -> Self {
        Self::with_reason(path, SkipReason::unclassified(reason))
    }

    pub fn missing_helm_repository(path: Option<PathBuf>, repo_name: &str) -> Self {
        Self::with_reason(
            path,
            SkipReason::new(
                format!("missing HelmRepository {repo_name}"),
                SkipReasonCode::MissingHelmRepository,
                false,
                None,
            ),
        )
    }

    pub fn from_resolution_error(path: Option<PathBuf>, error: anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<ResolverError>() {
            return Self::with_reason(path, SkipReason::from_resolver_error(error));
        }
        Self::new(path, error.to_string())
    }

    fn unresolved_chart(target: &HelmReleaseTarget) -> Self {
        Self::with_reason(
            Some(target.path.clone()),
            SkipReason::new(
                "explicit chart version lacks manifest-local chart or source identity",
                SkipReasonCode::MissingChartIdentity,
                false,
                None,
            ),
        )
        .with_target_identity(
            format!(
                "HelmRelease:{}:{}:{}",
                target.document_index,
                target.resource_id.name,
                target.current_version.as_deref().unwrap_or_default()
            ),
            "spec.chart.spec.version",
        )
    }

    fn from_image_resolution_error(target: &ImageBinding, error: anyhow::Error) -> Self {
        Self::from_resolution_error(Some(target.path.clone()), error).with_target_identity(
            format!(
                "ImageBinding:{}:{}:{}:{}",
                target.document_index, target.resource_id.name, target.yaml_path, target.image
            ),
            &target.yaml_path,
        )
    }

    fn unsupported_image_schema(target: &ImageBinding) -> Self {
        Self::with_reason(
            Some(target.path.clone()),
            SkipReason::new(
                "recognizable image mapping uses an unsupported version schema",
                SkipReasonCode::UnsupportedImageSchema,
                false,
                None,
            ),
        )
        .with_target_identity(
            format!(
                "ImageBinding:{}:{}:{}:{}",
                target.document_index, target.resource_id.name, target.yaml_path, target.image
            ),
            &target.yaml_path,
        )
    }

    fn from_chart_resolution_error(target: &HelmReleaseTarget, error: anyhow::Error) -> Self {
        Self::from_resolution_error(Some(target.path.clone()), error).with_target_identity(
            format!(
                "HelmRelease:{}:{}:{}",
                target.document_index,
                target.resource_id.name,
                target.current_version.as_deref().unwrap_or_default()
            ),
            "spec.chart.spec.version",
        )
    }

    fn with_target_identity(mut self, identity_suffix: String, yaml_path: &str) -> Self {
        self.identity_suffix = Some(identity_suffix);
        self.yaml_path = Some(yaml_path.to_string());
        self
    }

    fn with_reason(path: Option<PathBuf>, reason: SkipReason) -> Self {
        Self {
            path,
            reason: reason.message,
            reason_code: reason.code,
            retryable: reason.retryable,
            source_url: reason.source_url,
            identity_suffix: None,
            yaml_path: None,
            current_value: None,
            document_index: None,
        }
    }

    pub fn selection_id(&self, repo_root: &Path) -> String {
        let path = self
            .path
            .as_ref()
            .map(|path| {
                path.strip_prefix(repo_root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        format!(
            "v1:{}:{}:{}",
            encode_selection_id_part(&path),
            encode_selection_id_part(self.identity_suffix.as_deref().unwrap_or("unresolved")),
            self.reason_code.as_str()
        )
    }

    pub fn yaml_path(&self) -> Option<&str> {
        self.yaml_path.as_deref()
    }

    pub fn current_value(&self) -> Option<&str> {
        self.current_value.as_deref()
    }

    pub fn document_index(&self) -> Option<usize> {
        self.document_index
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkipReason {
    message: String,
    code: SkipReasonCode,
    retryable: bool,
    source_url: Option<String>,
}

impl SkipReason {
    fn new(
        message: impl Into<String>,
        code: SkipReasonCode,
        retryable: bool,
        source_url: Option<String>,
    ) -> Self {
        Self {
            message: message.into(),
            code,
            retryable,
            source_url,
        }
    }

    fn unclassified(message: impl Into<String>) -> Self {
        Self::new(message, SkipReasonCode::Unclassified, false, None)
    }

    fn from_resolver_error(error: &ResolverError) -> Self {
        Self::new(
            error.to_string(),
            SkipReasonCode::from(error.code()),
            error.retryable(),
            error.source_url().map(str::to_string),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReasonCode {
    MissingHelmRepository,
    AmbiguousHelmRepository,
    MissingChartIdentity,
    UnsupportedRepositoryType,
    ChartNotFound,
    IncompatibleVersionScheme,
    CurrentVersionNotFound,
    CurrentVersionNewerThanSource,
    ChartRequestFailed,
    RegistryRequestFailed,
    MutableImageTag,
    ImageReferenceMissingTag,
    ImageReferencePinnedByDigest,
    TemplatedImageReference,
    UnparseableImageReference,
    UnsupportedImageSchema,
    UnsupportedVersionDeclaration,
    Unclassified,
}

impl SkipReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingHelmRepository => "missing_helm_repository",
            Self::AmbiguousHelmRepository => "ambiguous_helm_repository",
            Self::MissingChartIdentity => "missing_chart_identity",
            Self::UnsupportedRepositoryType => "unsupported_repository_type",
            Self::ChartNotFound => "chart_not_found",
            Self::IncompatibleVersionScheme => "incompatible_version_scheme",
            Self::CurrentVersionNotFound => "current_version_not_found",
            Self::CurrentVersionNewerThanSource => "current_version_newer_than_source",
            Self::ChartRequestFailed => "chart_request_failed",
            Self::RegistryRequestFailed => "registry_request_failed",
            Self::MutableImageTag => "mutable_image_tag",
            Self::ImageReferenceMissingTag => "image_reference_missing_tag",
            Self::ImageReferencePinnedByDigest => "image_reference_pinned_by_digest",
            Self::TemplatedImageReference => "templated_image_reference",
            Self::UnparseableImageReference => "unparseable_image_reference",
            Self::UnsupportedImageSchema => "unsupported_image_schema",
            Self::UnsupportedVersionDeclaration => "unsupported_version_declaration",
            Self::Unclassified => "unclassified",
        }
    }
}

impl From<ResolverErrorCode> for SkipReasonCode {
    fn from(code: ResolverErrorCode) -> Self {
        match code {
            ResolverErrorCode::UnsupportedRepositoryType => Self::UnsupportedRepositoryType,
            ResolverErrorCode::ChartNotFound => Self::ChartNotFound,
            ResolverErrorCode::IncompatibleVersionScheme => Self::IncompatibleVersionScheme,
            ResolverErrorCode::CurrentVersionNotFound => Self::CurrentVersionNotFound,
            ResolverErrorCode::CurrentVersionNewerThanSource => Self::CurrentVersionNewerThanSource,
            ResolverErrorCode::ChartRequestFailed => Self::ChartRequestFailed,
            ResolverErrorCode::RegistryRequestFailed => Self::RegistryRequestFailed,
            ResolverErrorCode::MutableImageTag => Self::MutableImageTag,
            ResolverErrorCode::ImageReferenceMissingTag => Self::ImageReferenceMissingTag,
            ResolverErrorCode::ImageReferencePinnedByDigest => Self::ImageReferencePinnedByDigest,
            ResolverErrorCode::TemplatedImageReference => Self::TemplatedImageReference,
            ResolverErrorCode::UnparseableImageReference => Self::UnparseableImageReference,
            ResolverErrorCode::Unclassified => Self::Unclassified,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanOptions {
    pub max_workers: usize,
}

pub type ProgressCallback<'a> = dyn Fn(usize, usize, &Path) + Sync + 'a;

impl Default for PlanOptions {
    fn default() -> Self {
        Self { max_workers: 8 }
    }
}

pub fn plan_updates(
    inventory: &Inventory,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
) -> UpdateReport {
    plan_updates_with_options(
        inventory,
        chart_resolver,
        image_resolver,
        PlanOptions::default(),
    )
}

pub fn plan_updates_with_options(
    inventory: &Inventory,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
    options: PlanOptions,
) -> UpdateReport {
    plan_updates_with_optional_progress(inventory, chart_resolver, image_resolver, options, None)
}

pub fn plan_updates_with_progress(
    inventory: &Inventory,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
    options: PlanOptions,
    progress_callback: &ProgressCallback<'_>,
) -> UpdateReport {
    plan_updates_with_optional_progress(
        inventory,
        chart_resolver,
        image_resolver,
        options,
        Some(progress_callback),
    )
}

fn plan_updates_with_optional_progress(
    inventory: &Inventory,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
    options: PlanOptions,
    progress_callback: Option<&ProgressCallback<'_>>,
) -> UpdateReport {
    let mut indexed_outcomes = resolve_targets(
        inventory,
        chart_resolver,
        image_resolver,
        options,
        progress_callback,
    );
    indexed_outcomes.sort_by_key(|(index, _)| *index);
    let checked_count = indexed_outcomes
        .iter()
        .filter(|(_, outcome)| !matches!(outcome, ResolutionOutcome::Skipped(_)))
        .count();

    let mut planned = indexed_outcomes
        .iter()
        .filter_map(|(_, outcome)| match outcome {
            ResolutionOutcome::Planned(update) => Some(update.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    planned.sort_by_key(|item| {
        (
            item.path().to_path_buf(),
            item.document_index(),
            match item {
                PlannedUpdate::Image(update) => update.yaml_path.clone(),
                PlannedUpdate::Chart(_) => String::new(),
            },
        )
    });
    let skipped = indexed_outcomes
        .into_iter()
        .filter_map(|(_, outcome)| match outcome {
            ResolutionOutcome::Skipped(reason) => Some(reason),
            _ => None,
        })
        .collect();

    UpdateReport {
        planned,
        skipped,
        checked_count,
    }
}

enum ResolutionTask<'a> {
    Chart(usize, &'a HelmReleaseTarget),
    UnresolvedChart(usize, &'a HelmReleaseTarget),
    Image(usize, &'a ImageBinding),
    Unchecked(usize, &'a UncheckedVersionDeclaration),
}

impl ResolutionTask<'_> {
    fn path(&self) -> &Path {
        match self {
            Self::Chart(_, target) | Self::UnresolvedChart(_, target) => &target.path,
            Self::Image(_, target) => &target.path,
            Self::Unchecked(_, declaration) => &declaration.path,
        }
    }
}

enum ResolutionOutcome {
    Planned(PlannedUpdate),
    Skipped(SkippedUpdate),
    Noop,
}

fn resolve_targets(
    inventory: &Inventory,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
    options: PlanOptions,
    progress_callback: Option<&ProgressCallback<'_>>,
) -> Vec<(usize, ResolutionOutcome)> {
    let chart_count = inventory.chart_targets.len();
    let unresolved_chart_count = inventory.unresolved_chart_targets.len();
    let tasks = inventory
        .chart_targets
        .iter()
        .enumerate()
        .map(|(index, target)| ResolutionTask::Chart(index, target))
        .chain(
            inventory
                .unresolved_chart_targets
                .iter()
                .enumerate()
                .map(|(index, target)| {
                    ResolutionTask::UnresolvedChart(chart_count + index, target)
                }),
        )
        .chain(
            inventory
                .image_bindings
                .iter()
                .enumerate()
                .map(|(index, target)| {
                    ResolutionTask::Image(chart_count + unresolved_chart_count + index, target)
                }),
        )
        .chain(
            inventory
                .unchecked_version_declarations
                .iter()
                .enumerate()
                .map(|(index, declaration)| {
                    ResolutionTask::Unchecked(
                        chart_count
                            + unresolved_chart_count
                            + inventory.image_bindings.len()
                            + index,
                        declaration,
                    )
                }),
        )
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        return Vec::new();
    }

    let worker_count = options.max_workers.max(1).min(tasks.len());
    if worker_count == 1 {
        return tasks
            .iter()
            .enumerate()
            .map(|(completed, task)| {
                let outcome = resolve_task(inventory, task, chart_resolver, image_resolver);
                if let Some(callback) = progress_callback {
                    callback(completed + 1, tasks.len(), task.path());
                }
                outcome
            })
            .collect();
    }

    let completed_tasks = Mutex::new(0usize);
    let pool = ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .expect("rayon thread pool");

    pool.install(|| {
        tasks
            .par_iter()
            .map(|task| {
                let outcome = resolve_task(inventory, task, chart_resolver, image_resolver);
                if let Some(callback) = progress_callback {
                    let mut completed = completed_tasks.lock().expect("progress lock");
                    *completed += 1;
                    callback(*completed, tasks.len(), task.path());
                }
                outcome
            })
            .collect()
    })
}

fn resolve_task(
    inventory: &Inventory,
    task: &ResolutionTask<'_>,
    chart_resolver: &dyn ChartVersionResolver,
    image_resolver: &dyn ImageVersionResolver,
) -> (usize, ResolutionOutcome) {
    let (index, mut outcome) = match task {
        ResolutionTask::Chart(index, target) => (
            *index,
            resolve_chart_target(inventory, target, chart_resolver),
        ),
        ResolutionTask::UnresolvedChart(index, target) => (
            *index,
            ResolutionOutcome::Skipped(SkippedUpdate::unresolved_chart(target)),
        ),
        ResolutionTask::Image(index, target) => (
            *index,
            resolve_image_binding(inventory, target, image_resolver),
        ),
        ResolutionTask::Unchecked(index, declaration) => (
            *index,
            ResolutionOutcome::Skipped(SkippedUpdate::unchecked_declaration(declaration)),
        ),
    };
    if let ResolutionOutcome::Skipped(skipped) = &mut outcome {
        let (document_index, yaml_path) = match task {
            ResolutionTask::Chart(_, target) | ResolutionTask::UnresolvedChart(_, target) => {
                (target.document_index, "spec.chart.spec.version")
            }
            ResolutionTask::Image(_, target) => (target.document_index, target.yaml_path.as_str()),
            ResolutionTask::Unchecked(_, declaration) => {
                (declaration.document_index, declaration.yaml_path.as_str())
            }
        };
        skipped.document_index = Some(document_index);
        skipped.yaml_path = Some(yaml_path.into());
        skipped.current_value = inventory
            .manifest_documents
            .get(&(task.path().to_path_buf(), document_index))
            .and_then(|document| value_at_path(document, yaml_path))
            .and_then(yaml_serde::Value::as_str)
            .map(str::to_string)
            .or_else(|| skipped.current_value.clone());
    }
    (index, outcome)
}

fn resolve_chart_target(
    inventory: &Inventory,
    target: &HelmReleaseTarget,
    resolver: &dyn ChartVersionResolver,
) -> ResolutionOutcome {
    let repo_name = target.repo_name.as_deref().unwrap_or_default();
    if let Some(sources) = inventory.ambiguous_repositories.get(repo_name) {
        return ResolutionOutcome::Skipped(
            SkippedUpdate::with_reason(
                Some(target.path.clone()),
                SkipReason::new(
                    format!(
                        "ambiguous HelmRepository {repo_name}: {} manifests share this name; inspect inventory --json for source locations",
                        sources.len()
                    ),
                    SkipReasonCode::AmbiguousHelmRepository,
                    false,
                    None,
                ),
            )
            .with_target_identity(
                format!(
                    "HelmRelease:{}:{}:{}",
                    target.document_index,
                    target.resource_id.name,
                    target.current_version.as_deref().unwrap_or_default()
                ),
                "spec.chart.spec.version",
            ),
        );
    }
    let Some(repository) = inventory.repositories.get(repo_name) else {
        return ResolutionOutcome::Skipped(
            SkippedUpdate::missing_helm_repository(Some(target.path.clone()), repo_name)
                .with_target_identity(
                    format!(
                        "HelmRelease:{}:{}:{}",
                        target.document_index,
                        target.resource_id.name,
                        target.current_version.as_deref().unwrap_or_default()
                    ),
                    "spec.chart.spec.version",
                ),
        );
    };

    let expected_namespace = inventory
        .manifest_documents
        .get(&(target.path.clone(), target.document_index))
        .and_then(|document| value_at_path(document, "spec.chart.spec.sourceRef.namespace"))
        .and_then(yaml_serde::Value::as_str)
        .or(target.resource_id.namespace.as_deref());
    if let (Some(expected), Some(found)) = (expected_namespace, repository.namespace.as_deref())
        && expected != found
    {
        return ResolutionOutcome::Skipped(
            SkippedUpdate::with_reason(
                Some(target.path.clone()),
                SkipReason::new(
                    format!("HelmRepository namespace mismatch: expected {expected}/{repo_name}, found {found}/{repo_name}"),
                    SkipReasonCode::MissingHelmRepository,
                    false,
                    None,
                ),
            ).with_target_identity(
                format!("HelmRelease:{}:{}:{}", target.document_index, target.resource_id.name, target.current_version.as_deref().unwrap_or_default()),
                "spec.chart.spec.version",
            ),
        );
    }

    let latest_version = match resolver.resolve(
        repository,
        target.chart_name.as_deref().unwrap_or_default(),
        target.current_version.as_deref(),
    ) {
        Ok(version) => version,
        Err(error) => {
            return ResolutionOutcome::Skipped(SkippedUpdate::from_chart_resolution_error(
                target, error,
            ));
        }
    };
    let current_version = target.current_version.clone().unwrap_or_default();
    if !is_newer_version(&current_version, &latest_version) {
        return ResolutionOutcome::Noop;
    }

    let mut manifest_identity = ManifestIdentity::capture(
        inventory,
        &target.path,
        target.document_index,
        &target.resource_id,
        &[
            "spec.chart.spec.chart".into(),
            "spec.chart.spec.sourceRef".into(),
        ],
    );
    if let Some(identity) = &mut manifest_identity {
        identity.sources = inventory
            .repository_sources(repo_name)
            .into_iter()
            .map(|source| {
                let document = inventory
                    .manifest_documents
                    .get(&(source.path.clone(), source.document_index))?;
                Some(SourceIdentity {
                    path: source.path.clone(),
                    document_index: source.document_index,
                    document: document.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
    }
    ResolutionOutcome::Planned(PlannedUpdate::Chart(PlannedChartUpdate {
        manifest_identity,
        path: target.path.clone(),
        document_index: target.document_index,
        target_name: target.resource_id.name.clone(),
        chart_name: target.chart_name.clone().unwrap_or_default(),
        repo_name: target.repo_name.clone().unwrap_or_default(),
        current_version,
        latest_version,
    }))
}

fn resolve_image_binding(
    inventory: &Inventory,
    target: &ImageBinding,
    resolver: &dyn ImageVersionResolver,
) -> ResolutionOutcome {
    if target.value_kind == ImageBindingValueKind::UnsupportedSchema {
        return ResolutionOutcome::Skipped(SkippedUpdate::unsupported_image_schema(target));
    }
    let latest_image = match resolver.resolve(&target.image) {
        Ok(image) => image,
        Err(error) => {
            return ResolutionOutcome::Skipped(SkippedUpdate::from_image_resolution_error(
                target, error,
            ));
        }
    };
    if latest_image == target.image {
        return ResolutionOutcome::Noop;
    }
    let Some(current_version) = parse_image_reference(&target.image)
        .ok()
        .and_then(|reference| reference.tag)
    else {
        return ResolutionOutcome::Skipped(SkippedUpdate::new(
            Some(target.path.clone()),
            format!(
                "could not determine comparable image tags for {}",
                target.image
            ),
        ));
    };
    let Some(latest_version) = parse_image_reference(&latest_image)
        .ok()
        .and_then(|reference| reference.tag)
    else {
        return ResolutionOutcome::Skipped(SkippedUpdate::new(
            Some(target.path.clone()),
            format!(
                "could not determine comparable image tags for {}",
                target.image
            ),
        ));
    };
    if !is_newer_image_tag(&current_version, &latest_version) {
        return ResolutionOutcome::Noop;
    }
    let identity_paths = if target.value_kind == ImageBindingValueKind::Tag {
        let parent = target
            .yaml_path
            .strip_suffix(".tag")
            .expect("mapping tag path");
        ["repository", "registry", "tag", "digest", "sha", "sha256"]
            .into_iter()
            .map(|key| format!("{parent}.{key}"))
            .collect::<Vec<_>>()
    } else {
        let mut paths = vec![target.yaml_path.clone()];
        if target.resource_id.kind != "HelmRelease"
            && let Some(parent) = target.yaml_path.strip_suffix(".image")
        {
            paths.push(format!("{parent}.name"));
        }
        paths
    };
    ResolutionOutcome::Planned(PlannedUpdate::Image(PlannedImageUpdate {
        manifest_identity: ManifestIdentity::capture(
            inventory,
            &target.path,
            target.document_index,
            &target.resource_id,
            &identity_paths,
        ),
        path: target.path.clone(),
        document_index: target.document_index,
        target_name: target.resource_id.name.clone(),
        yaml_path: target.yaml_path.clone(),
        current_image: target.image.clone(),
        latest_image,
        current_version,
        latest_version,
        value_kind: target.value_kind,
    }))
}

#[cfg(test)]
pub fn apply_updates(report: &UpdateReport) -> Result<usize> {
    apply_planned_updates(&report.planned).map(|paths| paths.len())
}

pub(crate) fn apply_planned_updates<'a>(
    updates: impl IntoIterator<Item = &'a PlannedUpdate>,
) -> Result<Vec<PathBuf>> {
    let mut updates_by_path: BTreeMap<PathBuf, Vec<&PlannedUpdate>> = BTreeMap::new();
    for update in updates {
        updates_by_path
            .entry(update.path().to_path_buf())
            .or_default()
            .push(update);
    }
    let mut prepared_files = Vec::with_capacity(updates_by_path.len());
    for (path, updates) in updates_by_path {
        let original = fs::read_to_string(&path).with_context(|| {
            format!("failed to read {} while preparing updates", path.display())
        })?;
        let yaml_file: YamlFile = original.parse().with_context(|| {
            format!("failed to parse {} while preparing updates", path.display())
        })?;
        let documents = yaml_file.documents().collect::<Vec<_>>();
        let semantic_documents = parse_yaml_documents(&original)?;
        let mut expected_documents = semantic_documents.clone();
        let mut edits = Vec::new();
        for update in updates {
            let document = documents.get(update.document_index()).ok_or_else(|| {
                anyhow!(
                    "document index {} not found in {}",
                    update.document_index(),
                    path.display()
                )
            })?;
            let (yaml_path, expected, replacement, identity) = match update {
                PlannedUpdate::Chart(chart) => {
                    ensure_editable_chart_spec(document)?;
                    (
                        "spec.chart.spec.version",
                        &chart.current_version,
                        &chart.latest_version,
                        &chart.manifest_identity,
                    )
                }
                PlannedUpdate::Image(image) => {
                    let (expected, replacement) = match image.value_kind {
                        ImageBindingValueKind::ImageReference => {
                            (&image.current_image, &image.latest_image)
                        }
                        ImageBindingValueKind::Tag => {
                            (&image.current_version, &image.latest_version)
                        }
                        ImageBindingValueKind::UnsupportedSchema => {
                            return Err(anyhow!("unsupported image schema cannot be applied"));
                        }
                    };
                    (
                        image.yaml_path.as_str(),
                        expected,
                        replacement,
                        &image.manifest_identity,
                    )
                }
            };
            let semantic_document = semantic_documents
                .get(update.document_index())
                .ok_or_else(|| anyhow!("semantic document missing"))?;
            if let Some(identity) = identity {
                identity.verify(semantic_document)?;
            }
            edits.push(checked_scalar_edit(
                document,
                yaml_path,
                expected,
                replacement,
            )?);
            let expected_document = expected_documents
                .get_mut(update.document_index())
                .ok_or_else(|| anyhow!("semantic document missing"))?;
            let expected_node =
                value_at_path_mut(expected_document, yaml_path).ok_or_else(|| {
                    anyhow!("missing YAML path {yaml_path}; target changed after planning")
                })?;
            *expected_node = yaml_serde::Value::String(replacement.clone());
        }
        edits.sort_by_key(|edit| edit.range.start);
        if edits
            .windows(2)
            .any(|pair| pair[0].range.end > pair[1].range.start)
        {
            return Err(anyhow!("overlapping YAML updates in {}", path.display()));
        }
        let mut text = original;
        for edit in edits.into_iter().rev() {
            text.replace_range(edit.range, &edit.replacement);
        }
        let actual_documents = parse_yaml_documents(&text)
            .with_context(|| format!("invalid YAML after preparing {}", path.display()))?;
        if actual_documents != expected_documents {
            return Err(anyhow!(
                "prepared YAML does not match the approved changes in {}",
                path.display()
            ));
        }
        prepared_files.push((path, text));
    }
    for (written_count, (path, text)) in prepared_files.iter().enumerate() {
        fs::write(path, text).with_context(|| format!("failed to write {} after {written_count} file(s) were applied; inspect the working tree with Git", path.display()))?;
    }
    Ok(prepared_files.into_iter().map(|(path, _)| path).collect())
}

fn ensure_editable_chart_spec(document: &EditDocument) -> Result<()> {
    document
        .as_mapping()
        .ok_or_else(|| anyhow!("HelmRelease document must be a YAML mapping"))?;

    for path in ["spec", "spec.chart", "spec.chart.spec"] {
        let Some(node) = edit_node_at_path(document, path) else {
            continue;
        };
        if node.as_mapping().is_none() {
            let key = path.rsplit('.').next().expect("non-empty path");
            return Err(anyhow!("HelmRelease {key} must be a YAML mapping"));
        }
    }

    Ok(())
}

struct ScalarEdit {
    range: Range<usize>,
    replacement: String,
}

fn checked_scalar_edit(
    document: &EditDocument,
    yaml_path: &str,
    expected: &str,
    value: &str,
) -> Result<ScalarEdit> {
    let node = edit_node_at_path(document, yaml_path)
        .ok_or_else(|| anyhow!("missing YAML path {yaml_path}; target changed after planning"))?;
    let scalar = node
        .as_scalar()
        .ok_or_else(|| anyhow!("Expected YAML scalar at {yaml_path}"))?;
    let actual = scalar.as_string();
    if actual != expected {
        return Err(anyhow!(
            "YAML scalar at {yaml_path} changed after planning: expected {expected:?}, found {actual:?}"
        ));
    }
    let range = scalar.byte_range();
    Ok(ScalarEdit {
        range: range.start as usize..range.end as usize,
        replacement: render_scalar_preserving_style(scalar, value)?,
    })
}

fn render_scalar_preserving_style(scalar: &EditScalar, value: &str) -> Result<String> {
    let raw = scalar.value();
    Ok(match raw.as_str() {
        text if text.starts_with('"') && text.ends_with('"') => {
            ScalarValue::double_quoted(value).to_string()
        }
        text if text.starts_with('\'') && text.ends_with('\'') => {
            ScalarValue::single_quoted(value).to_string()
        }
        text if text.starts_with('|') || text.starts_with('>') => {
            let (header, body) = text
                .split_once('\n')
                .ok_or_else(|| anyhow!("block scalar has no content line"))?;
            let indent = &body[..body.len() - body.trim_start_matches(' ').len()];
            let trailing = &body[body.trim_end_matches(char::is_whitespace).len()..];
            format!("{header}\n{indent}{value}{trailing}")
        }
        _ => value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_structured_skips_without_reparsing_display_text() {
        let skipped = SkippedUpdate::new(
            Some(PathBuf::from("/repo/apps/demo.yaml")),
            "network: timeout: still structured",
        );

        assert_eq!(
            crate::cli::skip_json(&skipped, Path::new("/repo")),
            json!({
                "id": "v1:apps%2Fdemo.yaml:unresolved:unclassified",
                "path": "apps/demo.yaml",
                "yaml_path": null,
                "reason": "network: timeout: still structured",
                "reason_code": "unclassified",
                "retryable": false,
                "source_url": null
            })
        );
    }
}
