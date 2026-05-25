use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use yaml_edit::path::YamlPath;
use yaml_edit::{Document as EditDocument, Scalar as EditScalar, ScalarValue, YamlFile};

use crate::models::{DeploymentImageTarget, HelmReleaseTarget, Inventory, TargetKind};
use crate::resolvers::{
    ChartVersionResolver, ImageVersionResolver, is_newer_version, parse_image_reference,
};

#[derive(Debug, Clone, Serialize)]
pub struct PlannedChartUpdate {
    pub path: PathBuf,
    pub document_index: usize,
    pub target_name: String,
    pub chart_name: String,
    pub repo_name: String,
    pub current_version: String,
    pub latest_version: String,
    pub inherited_source: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedDeploymentUpdate {
    pub path: PathBuf,
    pub document_index: usize,
    pub target_name: String,
    pub yaml_path: String,
    pub current_image: String,
    pub latest_image: String,
    pub current_version: String,
    pub latest_version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "target_kind")]
pub enum PlannedUpdate {
    #[serde(rename = "HelmRelease")]
    Chart(PlannedChartUpdate),
    #[serde(rename = "Deployment")]
    Deployment(PlannedDeploymentUpdate),
}

impl PlannedUpdate {
    pub fn path(&self) -> &Path {
        match self {
            Self::Chart(update) => &update.path,
            Self::Deployment(update) => &update.path,
        }
    }

    pub fn document_index(&self) -> usize {
        match self {
            Self::Chart(update) => update.document_index,
            Self::Deployment(update) => update.document_index,
        }
    }

    pub fn target_kind(&self) -> &str {
        self.kind().as_str()
    }

    pub fn kind(&self) -> TargetKind {
        match self {
            Self::Chart(_) => TargetKind::HelmRelease,
            Self::Deployment(_) => TargetKind::Deployment,
        }
    }

    pub fn target_name(&self) -> &str {
        match self {
            Self::Chart(update) => &update.target_name,
            Self::Deployment(update) => &update.target_name,
        }
    }

    pub fn current_version(&self) -> &str {
        match self {
            Self::Chart(update) => &update.current_version,
            Self::Deployment(update) => &update.current_version,
        }
    }

    pub fn latest_version(&self) -> &str {
        match self {
            Self::Chart(update) => &update.latest_version,
            Self::Deployment(update) => &update.latest_version,
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
            Self::Deployment(update) => {
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
        match self {
            Self::Chart(update) => json!({
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
            Self::Deployment(update) => json!({
                "id": self.selection_id(repo_root),
                "path": path,
                "document_index": update.document_index,
                "target_kind": "Deployment",
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
        strict: bool,
        non_interactive: bool,
        applied_count: usize,
        changed_file_count: usize,
    ) -> JsonValue {
        json!({
            "mode": mode,
            "strict": strict,
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
}

impl SkippedUpdate {
    pub fn new(path: Option<PathBuf>, reason: impl Into<String>) -> Self {
        Self {
            path,
            reason: reason.into(),
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
        json!({"path": path, "reason": self.reason})
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
                PlannedUpdate::Deployment(update) => update.yaml_path.clone(),
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

    UpdateReport { planned, skipped }
}

enum ResolutionTask<'a> {
    Chart(usize, &'a HelmReleaseTarget),
    Deployment(usize, &'a DeploymentImageTarget),
}

impl ResolutionTask<'_> {
    fn path(&self) -> &Path {
        match self {
            Self::Chart(_, target) => &target.path,
            Self::Deployment(_, target) => &target.path,
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
    let tasks = inventory
        .chart_targets
        .iter()
        .enumerate()
        .map(|(index, target)| ResolutionTask::Chart(index, target))
        .chain(
            inventory
                .deployment_targets
                .iter()
                .enumerate()
                .map(|(index, target)| ResolutionTask::Deployment(chart_count + index, target)),
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
                    let completed = {
                        let mut count = completed_tasks.lock().expect("progress lock");
                        *count += 1;
                        *count
                    };
                    callback(completed, tasks.len(), task.path());
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
        ResolutionTask::Deployment(index, target) => {
            (*index, resolve_deployment_target(target, image_resolver))
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
        return ResolutionOutcome::Skipped(SkippedUpdate::new(
            Some(target.path.clone()),
            format!("missing HelmRepository {repo_name}"),
        ));
    };

    let latest_version = match resolver.resolve(
        repository,
        target.chart_name.as_deref().unwrap_or_default(),
        target.current_version.as_deref(),
    ) {
        Ok(version) => version,
        Err(error) => {
            return ResolutionOutcome::Skipped(SkippedUpdate::new(
                Some(target.path.clone()),
                error.to_string(),
            ));
        }
    };
    let current_version = target.current_version.clone().unwrap_or_default();
    if !is_newer_version(&current_version, &latest_version) {
        return ResolutionOutcome::Noop;
    }

    ResolutionOutcome::Planned(PlannedUpdate::Chart(PlannedChartUpdate {
        path: target.path.clone(),
        document_index: target.document_index,
        target_name: target.resource_id.name.clone(),
        chart_name: target.chart_name.clone().unwrap_or_default(),
        repo_name: target.repo_name.clone().unwrap_or_default(),
        current_version,
        latest_version,
        inherited_source: target.source_is_inherited,
    }))
}

fn resolve_deployment_target(
    target: &DeploymentImageTarget,
    resolver: &dyn ImageVersionResolver,
) -> ResolutionOutcome {
    let latest_image = match resolver.resolve(&target.image) {
        Ok(image) => image,
        Err(error) => {
            return ResolutionOutcome::Skipped(SkippedUpdate::new(
                Some(target.path.clone()),
                error.to_string(),
            ));
        }
    };
    if latest_image == target.image {
        return ResolutionOutcome::Noop;
    }
    let current_version = match parse_image_reference(&target.image)
        .ok()
        .and_then(|reference| reference.tag)
    {
        Some(tag) => tag,
        None => {
            return ResolutionOutcome::Skipped(SkippedUpdate::new(
                Some(target.path.clone()),
                format!(
                    "could not determine comparable image tags for {}",
                    target.image
                ),
            ));
        }
    };
    let latest_version = match parse_image_reference(&latest_image)
        .ok()
        .and_then(|reference| reference.tag)
    {
        Some(tag) => tag,
        None => {
            return ResolutionOutcome::Skipped(SkippedUpdate::new(
                Some(target.path.clone()),
                format!(
                    "could not determine comparable image tags for {}",
                    target.image
                ),
            ));
        }
    };
    if !is_newer_version(&current_version, &latest_version) {
        return ResolutionOutcome::Noop;
    }

    ResolutionOutcome::Planned(PlannedUpdate::Deployment(PlannedDeploymentUpdate {
        path: target.path.clone(),
        document_index: target.document_index,
        target_name: target.resource_id.name.clone(),
        yaml_path: target.yaml_path.clone(),
        current_image: target.image.clone(),
        latest_image,
        current_version,
        latest_version,
    }))
}

pub fn apply_updates(report: &UpdateReport) -> Result<usize> {
    let mut updates_by_path: BTreeMap<PathBuf, Vec<&PlannedUpdate>> = BTreeMap::new();
    for update in &report.planned {
        updates_by_path
            .entry(update.path().to_path_buf())
            .or_default()
            .push(update);
    }

    let mut changed_files = 0;
    for (path, updates) in updates_by_path {
        let text = fs::read_to_string(&path)?;
        let yaml_file: YamlFile = text.parse()?;
        let documents = yaml_file.documents().collect::<Vec<_>>();

        for update in updates {
            let document = documents.get(update.document_index()).ok_or_else(|| {
                anyhow!(
                    "document index {} not found in {}",
                    update.document_index(),
                    path.display()
                )
            })?;
            match update {
                PlannedUpdate::Chart(chart_update) => {
                    ensure_editable_chart_spec(document)?;
                    set_yaml_scalar_value(
                        document,
                        "spec.chart.spec.version",
                        &chart_update.latest_version,
                        true,
                    )?;
                }
                PlannedUpdate::Deployment(deployment_update) => {
                    set_yaml_scalar_value(
                        document,
                        &deployment_update.yaml_path,
                        &deployment_update.latest_image,
                        false,
                    )?;
                }
            }
        }

        fs::write(&path, yaml_file.to_string())?;
        changed_files += 1;
    }
    Ok(changed_files)
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

fn set_yaml_scalar_value(
    document: &EditDocument,
    yaml_path: &str,
    value: &str,
    create_missing: bool,
) -> Result<()> {
    if let Some(node) = document.get_path(yaml_path) {
        let scalar = node
            .as_scalar()
            .ok_or_else(|| anyhow!("Expected YAML scalar at {yaml_path}"))?;
        set_scalar_preserving_style(scalar, value);
    } else if create_missing {
        document.set_path(yaml_path, ScalarValue::string(value));
    } else {
        return Err(anyhow!("missing YAML path {yaml_path}"));
    }
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
            json!({"path": "apps/demo.yaml", "reason": "network: timeout: still structured"})
        );
    }
}
