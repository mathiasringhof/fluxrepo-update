use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use yaml_serde::{Deserializer, Mapping, Value};

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

    fn to_json_value(&self, repo_root: &Path) -> JsonValue {
        let path = self
            .path()
            .strip_prefix(repo_root)
            .unwrap_or(self.path())
            .to_string_lossy()
            .to_string();
        match self {
            Self::Chart(update) => json!({
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

    let next_task = AtomicUsize::new(0);
    let completed_tasks = AtomicUsize::new(0);
    let outcomes = Mutex::new(Vec::with_capacity(tasks.len()));
    thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    let task_index = next_task.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(task_index) else {
                        break;
                    };
                    let outcome = resolve_task(inventory, task, chart_resolver, image_resolver);
                    outcomes.lock().expect("outcome lock").push(outcome);
                    let completed = completed_tasks.fetch_add(1, Ordering::Relaxed) + 1;
                    if let Some(callback) = progress_callback {
                        callback(completed, tasks.len(), task.path());
                    }
                }
            });
        }
    });
    outcomes.into_inner().expect("outcome lock")
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
        let mut documents = Deserializer::from_str(&text)
            .map(Value::deserialize)
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for update in updates {
            let document = documents.get_mut(update.document_index()).ok_or_else(|| {
                anyhow!(
                    "document index {} not found in {}",
                    update.document_index(),
                    path.display()
                )
            })?;
            match update {
                PlannedUpdate::Chart(chart_update) => {
                    ensure_chart_spec(document)?;
                    set_nested_string(
                        document,
                        &["spec", "chart", "spec", "version"],
                        &chart_update.latest_version,
                    )?;
                }
                PlannedUpdate::Deployment(deployment_update) => {
                    set_yaml_path_value(
                        document,
                        &deployment_update.yaml_path,
                        &deployment_update.latest_image,
                    )?;
                }
            }
        }

        let rendered = documents
            .iter()
            .map(yaml_serde::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .join("---\n");
        fs::write(&path, rendered)?;
        changed_files += 1;
    }
    Ok(changed_files)
}

fn ensure_chart_spec(document: &mut Value) -> Result<()> {
    let mapping = document
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("HelmRelease document must be a YAML mapping"))?;
    ensure_mapping_child(mapping, "spec")?;
    let spec = mapping
        .get_mut(Value::String("spec".to_string()))
        .and_then(Value::as_mapping_mut)
        .expect("spec mapping exists");
    ensure_mapping_child(spec, "chart")?;
    let chart = spec
        .get_mut(Value::String("chart".to_string()))
        .and_then(Value::as_mapping_mut)
        .expect("chart mapping exists");
    ensure_mapping_child(chart, "spec")?;
    Ok(())
}

fn ensure_mapping_child(mapping: &mut Mapping, key: &str) -> Result<()> {
    let key_value = Value::String(key.to_string());
    if !mapping.contains_key(&key_value) {
        mapping.insert(key_value.clone(), Value::Mapping(Mapping::new()));
    }
    if !mapping.get(&key_value).is_some_and(Value::is_mapping) {
        return Err(anyhow!("HelmRelease {key} must be a YAML mapping"));
    }
    Ok(())
}

fn set_nested_string(document: &mut Value, path: &[&str], value: &str) -> Result<()> {
    let mut current = document;
    for key in &path[..path.len() - 1] {
        current = current
            .as_mapping_mut()
            .and_then(|mapping| mapping.get_mut(Value::String((*key).to_string())))
            .ok_or_else(|| anyhow!("missing YAML mapping key {key}"))?;
    }
    let last_key = path.last().expect("non-empty path");
    current
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("expected YAML mapping"))?
        .insert(
            Value::String((*last_key).to_string()),
            Value::String(value.to_string()),
        );
    Ok(())
}

fn set_yaml_path_value(document: &mut Value, yaml_path: &str, value: &str) -> Result<()> {
    let parts = parse_yaml_path(yaml_path);
    let mut current = document;
    for part in &parts[..parts.len() - 1] {
        current = match part {
            YamlPathPart::Key(key) => current
                .as_mapping_mut()
                .and_then(|mapping| mapping.get_mut(Value::String(key.clone())))
                .ok_or_else(|| anyhow!("Expected YAML mapping while traversing {yaml_path}"))?,
            YamlPathPart::Index(index) => current
                .as_sequence_mut()
                .and_then(|sequence| sequence.get_mut(*index))
                .ok_or_else(|| anyhow!("Expected YAML sequence while traversing {yaml_path}"))?,
        };
    }

    match parts.last().expect("non-empty YAML path") {
        YamlPathPart::Key(key) => {
            current
                .as_mapping_mut()
                .ok_or_else(|| anyhow!("Expected YAML mapping at {yaml_path}"))?
                .insert(Value::String(key.clone()), Value::String(value.to_string()));
        }
        YamlPathPart::Index(index) => {
            let sequence = current
                .as_sequence_mut()
                .ok_or_else(|| anyhow!("Expected YAML sequence at {yaml_path}"))?;
            sequence[*index] = Value::String(value.to_string());
        }
    }
    Ok(())
}

fn parse_yaml_path(yaml_path: &str) -> Vec<YamlPathPart> {
    let mut parts = Vec::new();
    for segment in yaml_path.split('.').filter(|segment| !segment.is_empty()) {
        let key = segment.split('[').next().unwrap_or_default();
        if !key.is_empty() {
            parts.push(YamlPathPart::Key(key.to_string()));
        }
        for index_text in segment.split('[').skip(1) {
            if let Some((index, _)) = index_text.split_once(']')
                && let Ok(index) = index.parse()
            {
                parts.push(YamlPathPart::Index(index));
            }
        }
    }
    parts
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum YamlPathPart {
    Key(String),
    Index(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yaml_paths_with_terminal_indexes() {
        assert_eq!(
            parse_yaml_path("spec.template.spec.containers[0]"),
            vec![
                YamlPathPart::Key("spec".to_string()),
                YamlPathPart::Key("template".to_string()),
                YamlPathPart::Key("spec".to_string()),
                YamlPathPart::Key("containers".to_string()),
                YamlPathPart::Index(0),
            ]
        );
    }

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
