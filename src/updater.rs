use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use serde_json::{Value as JsonValue, json};
use yaml_edit::path::YamlPath;
use yaml_edit::{Document as EditDocument, Scalar as EditScalar, ScalarValue, YamlFile};

use crate::models::{
    HelmReleaseTarget, ImageBinding, ImageBindingValueKind, Inventory, TargetKind,
};
use crate::resolvers::{
    ChartVersionResolver, ImageVersionResolver, ResolverError, ResolverErrorCode, is_newer_version,
    parse_image_reference,
};

#[derive(Debug, Clone)]
struct PlannedChartUpdate {
    path: PathBuf,
    document_index: usize,
    target_name: String,
    chart_name: String,
    repo_name: String,
    current_version: String,
    latest_version: String,
    inherited_source: bool,
}

#[derive(Debug, Clone)]
struct PlannedImageUpdate {
    path: PathBuf,
    document_index: usize,
    target_name: String,
    yaml_path: String,
    current_image: String,
    latest_image: String,
    current_version: String,
    latest_version: String,
    value_kind: ImageBindingValueKind,
}

#[derive(Debug, Clone)]
pub struct PlannedUpdate {
    variant: PlannedUpdateVariant,
}

#[derive(Debug, Clone)]
enum PlannedUpdateVariant {
    Chart(PlannedChartUpdate),
    Image(PlannedImageUpdate),
}

impl PlannedUpdate {
    #[allow(clippy::too_many_arguments)]
    pub fn chart(
        path: PathBuf,
        document_index: usize,
        target_name: String,
        chart_name: String,
        repo_name: String,
        current_version: String,
        latest_version: String,
        inherited_source: bool,
    ) -> Self {
        Self {
            variant: PlannedUpdateVariant::Chart(PlannedChartUpdate {
                path,
                document_index,
                target_name,
                chart_name,
                repo_name,
                current_version,
                latest_version,
                inherited_source,
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn image(
        path: PathBuf,
        document_index: usize,
        target_name: String,
        yaml_path: String,
        current_image: String,
        latest_image: String,
        current_version: String,
        latest_version: String,
        value_kind: ImageBindingValueKind,
    ) -> Self {
        Self {
            variant: PlannedUpdateVariant::Image(PlannedImageUpdate {
                path,
                document_index,
                target_name,
                yaml_path,
                current_image,
                latest_image,
                current_version,
                latest_version,
                value_kind,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => &update.path,
            PlannedUpdateVariant::Image(update) => &update.path,
        }
    }

    pub fn document_index(&self) -> usize {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => update.document_index,
            PlannedUpdateVariant::Image(update) => update.document_index,
        }
    }

    pub fn target_kind(&self) -> &str {
        self.kind().as_str()
    }

    pub fn kind(&self) -> TargetKind {
        match &self.variant {
            PlannedUpdateVariant::Chart(_) => TargetKind::HelmRelease,
            PlannedUpdateVariant::Image(_) => TargetKind::ImageBinding,
        }
    }

    pub fn target_name(&self) -> &str {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => &update.target_name,
            PlannedUpdateVariant::Image(update) => &update.target_name,
        }
    }

    pub fn current_version(&self) -> &str {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => &update.current_version,
            PlannedUpdateVariant::Image(update) => &update.current_version,
        }
    }

    pub fn latest_version(&self) -> &str {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => &update.latest_version,
            PlannedUpdateVariant::Image(update) => &update.latest_version,
        }
    }

    pub fn chart_name(&self) -> Option<&str> {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => Some(&update.chart_name),
            PlannedUpdateVariant::Image(_) => None,
        }
    }

    pub fn repo_name(&self) -> Option<&str> {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => Some(&update.repo_name),
            PlannedUpdateVariant::Image(_) => None,
        }
    }

    pub fn inherited_source(&self) -> bool {
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => update.inherited_source,
            PlannedUpdateVariant::Image(_) => false,
        }
    }

    pub fn yaml_path(&self) -> &str {
        match &self.variant {
            PlannedUpdateVariant::Chart(_) => "spec.chart.spec.version",
            PlannedUpdateVariant::Image(update) => &update.yaml_path,
        }
    }

    pub fn selection_id(&self, repo_root: &Path) -> String {
        let path = self
            .path()
            .strip_prefix(repo_root)
            .unwrap_or(self.path())
            .to_string_lossy()
            .to_string();
        let mut parts = vec![
            self.target_kind().to_string(),
            path,
            self.document_index().to_string(),
            self.target_name().to_string(),
        ];
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => {
                parts.extend([
                    "spec.chart.spec.version".to_string(),
                    update.repo_name.clone(),
                    update.chart_name.clone(),
                    update.current_version.clone(),
                    update.latest_version.clone(),
                ]);
            }
            PlannedUpdateVariant::Image(update) => {
                parts.extend([
                    update.yaml_path.clone(),
                    update.current_image.clone(),
                    update.latest_image.clone(),
                    update.current_version.clone(),
                    update.latest_version.clone(),
                ]);
            }
        }
        format!(
            "v1:{}",
            parts
                .iter()
                .map(|part| encode_selection_id_part(part))
                .collect::<Vec<_>>()
                .join(":")
        )
    }

    fn to_json_value(&self, repo_root: &Path) -> JsonValue {
        let path = self
            .path()
            .strip_prefix(repo_root)
            .unwrap_or(self.path())
            .to_string_lossy()
            .to_string();
        match &self.variant {
            PlannedUpdateVariant::Chart(update) => json!({
                "id": self.selection_id(repo_root),
                "path": path,
                "document_index": update.document_index,
                "target_kind": "HelmRelease",
                "target_name": update.target_name,
                "yaml_path": "spec.chart.spec.version",
                "chart_name": update.chart_name,
                "repo_name": update.repo_name,
                "current_version": update.current_version,
                "latest_version": update.latest_version,
                "inherited_source": update.inherited_source,
            }),
            PlannedUpdateVariant::Image(update) => json!({
                "id": self.selection_id(repo_root),
                "path": path,
                "document_index": update.document_index,
                "target_kind": "ImageBinding",
                "target_name": update.target_name,
                "yaml_path": update.yaml_path,
                "current_image": update.current_image,
                "latest_image": update.latest_image,
                "current_version": update.current_version,
                "latest_version": update.latest_version,
                "inherited_source": false,
            }),
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
}

impl UpdateReport {
    pub fn to_json_value(
        &self,
        repo_root: &Path,
        mode: &str,
        non_interactive: bool,
        applied_count: usize,
        changed_file_count: usize,
    ) -> JsonValue {
        json!({
            "mode": mode,
            "non_interactive": non_interactive,
            "summary": {
                "planned_count": self.planned.len(),
                "applied_count": applied_count,
                "skipped_count": self.skipped.len(),
                "changed_file_count": changed_file_count,
            },
            "planned": self.planned.iter().map(|item| item.to_json_value(repo_root)).collect::<Vec<_>>(),
            "skipped": self.skipped.iter().map(|item| item.to_json_value(repo_root)).collect::<Vec<_>>(),
        })
    }
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
}

impl SkippedUpdate {
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
        }
    }

    fn to_json_value(&self, repo_root: &Path) -> JsonValue {
        let path = self
            .path
            .as_ref()
            .map(|path| {
                path.strip_prefix(repo_root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_default();
        json!({
            "id": format!(
                "v1:{}:{}:{}",
                encode_selection_id_part(&path),
                encode_selection_id_part(self.identity_suffix.as_deref().unwrap_or("unresolved")),
                self.reason_code.as_str()
            ),
            "path": path,
            "yaml_path": self.yaml_path,
            "reason": self.reason,
            "reason_code": self.reason_code.as_str(),
            "retryable": self.retryable,
            "source_url": self.source_url,
        })
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
    Unclassified,
}

impl SkipReasonCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::MissingHelmRepository => "missing_helm_repository",
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

impl fmt::Display for SkippedUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(formatter, "{}: {}", path.display(), self.reason),
            None => formatter.write_str(&self.reason),
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

pub(crate) fn plan_updates_with_optional_progress(
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
            item.yaml_path().to_string(),
        )
    });
    let skipped = indexed_outcomes
        .into_iter()
        .filter_map(|(_, outcome)| match outcome {
            ResolutionOutcome::Skipped(reason) => Some(reason),
            _ => None,
        })
        .collect();

    UpdateReport { planned, skipped }
}

enum ResolutionTask<'a> {
    Chart(usize, &'a HelmReleaseTarget),
    UnresolvedChart(usize, &'a HelmReleaseTarget),
    Image(usize, &'a ImageBinding),
}

impl ResolutionTask<'_> {
    fn path(&self) -> &Path {
        match self {
            Self::Chart(_, target) | Self::UnresolvedChart(_, target) => &target.path,
            Self::Image(_, target) => &target.path,
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
    match task {
        ResolutionTask::Chart(index, target) => (
            *index,
            resolve_chart_target(inventory, target, chart_resolver),
        ),
        ResolutionTask::UnresolvedChart(index, target) => (
            *index,
            ResolutionOutcome::Skipped(SkippedUpdate::unresolved_chart(target)),
        ),
        ResolutionTask::Image(index, target) => {
            (*index, resolve_image_binding(target, image_resolver))
        }
    }
}

fn resolve_chart_target(
    inventory: &Inventory,
    target: &HelmReleaseTarget,
    resolver: &dyn ChartVersionResolver,
) -> ResolutionOutcome {
    let repo_name = target.repo_name.as_deref().unwrap_or_default();
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

    ResolutionOutcome::Planned(PlannedUpdate::chart(
        target.path.clone(),
        target.document_index,
        target.resource_id.name.clone(),
        target.chart_name.clone().unwrap_or_default(),
        target.repo_name.clone().unwrap_or_default(),
        current_version,
        latest_version,
        target.source_is_inherited,
    ))
}

fn resolve_image_binding(
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
    if !is_newer_version(&current_version, &latest_version) {
        return ResolutionOutcome::Noop;
    }

    ResolutionOutcome::Planned(PlannedUpdate::image(
        target.path.clone(),
        target.document_index,
        target.resource_id.name.clone(),
        target.yaml_path.clone(),
        target.image.clone(),
        latest_image,
        current_version,
        latest_version,
        target.value_kind,
    ))
}

pub fn apply_updates(report: &UpdateReport) -> Result<usize> {
    Ok(apply_updates_with_paths(report)?.len())
}

pub(crate) fn apply_updates_with_paths(report: &UpdateReport) -> Result<Vec<PathBuf>> {
    let updates = report.planned.iter().collect::<Vec<_>>();
    apply_planned_updates_with_paths(&updates)
}

pub(crate) fn apply_planned_updates_with_paths(updates: &[&PlannedUpdate]) -> Result<Vec<PathBuf>> {
    let mut updates_by_path: BTreeMap<PathBuf, Vec<&PlannedUpdate>> = BTreeMap::new();
    for update in updates {
        updates_by_path
            .entry(update.path().to_path_buf())
            .or_default()
            .push(*update);
    }

    let mut prepared_files = Vec::with_capacity(updates_by_path.len());
    for (path, updates) in updates_by_path {
        let text = fs::read_to_string(&path).with_context(|| {
            format!("failed to read {} while preparing updates", path.display())
        })?;
        let yaml_file: YamlFile = text.parse().with_context(|| {
            format!("failed to parse {} while preparing updates", path.display())
        })?;
        let documents = yaml_file.documents().collect::<Vec<_>>();

        for update in updates {
            let document = documents.get(update.document_index()).ok_or_else(|| {
                anyhow!(
                    "document index {} not found in {}",
                    update.document_index(),
                    path.display()
                )
            })?;
            match &update.variant {
                PlannedUpdateVariant::Chart(chart_update) => {
                    ensure_editable_chart_spec(document)?;
                    set_checked_yaml_scalar_value(
                        document,
                        "spec.chart.spec.version",
                        &chart_update.current_version,
                        &chart_update.latest_version,
                    )?;
                }
                PlannedUpdateVariant::Image(image_update) => {
                    let (expected, replacement) = match image_update.value_kind {
                        ImageBindingValueKind::ImageReference => {
                            (&image_update.current_image, &image_update.latest_image)
                        }
                        ImageBindingValueKind::Tag => {
                            (&image_update.current_version, &image_update.latest_version)
                        }
                        ImageBindingValueKind::UnsupportedSchema => {
                            return Err(anyhow!("unsupported image schema cannot be applied"));
                        }
                    };
                    set_checked_yaml_scalar_value(
                        document,
                        &image_update.yaml_path,
                        expected,
                        replacement,
                    )?;
                }
            }
        }

        prepared_files.push((path, yaml_file.to_string()));
    }

    for (written_count, (path, text)) in prepared_files.iter().enumerate() {
        fs::write(path, text).with_context(|| {
            format!(
                "failed to write {} after {written_count} file(s) were applied; inspect the working tree with Git",
                path.display()
            )
        })?;
    }
    Ok(prepared_files.into_iter().map(|(path, _)| path).collect())
}

fn ensure_editable_chart_spec(document: &EditDocument) -> Result<()> {
    document
        .as_mapping()
        .ok_or_else(|| anyhow!("HelmRelease document must be a YAML mapping"))?;

    for path in ["spec", "spec.chart", "spec.chart.spec"] {
        let Some(node) = document.get_path(path) else {
            continue;
        };
        if node.as_mapping().is_none() {
            let key = path.rsplit('.').next().expect("non-empty path");
            return Err(anyhow!("HelmRelease {key} must be a YAML mapping"));
        }
    }

    Ok(())
}

fn set_checked_yaml_scalar_value(
    document: &EditDocument,
    yaml_path: &str,
    expected: &str,
    value: &str,
) -> Result<()> {
    let node = document
        .get_path(yaml_path)
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
    set_scalar_preserving_style(scalar, value);
    Ok(())
}

fn set_scalar_preserving_style(scalar: &EditScalar, value: &str) {
    let rendered = match scalar.value().as_str() {
        text if text.starts_with('"') && text.ends_with('"') => {
            ScalarValue::double_quoted(value).to_string()
        }
        text if text.starts_with('\'') && text.ends_with('\'') => {
            ScalarValue::single_quoted(value).to_string()
        }
        _ => value.to_string(),
    };
    scalar.set_value(&rendered);
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
            skipped.to_json_value(Path::new("/repo")),
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
