use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::models::Inventory;
use crate::resolvers::{ChartVersionResolver, ImageVersionResolver};
use crate::updater::{
    PlanOptions, PlannedUpdate, ProgressCallback, SkippedUpdate, UpdateReport,
    apply_planned_updates_with_paths, plan_updates_with_optional_progress,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpdateSelectionIdentity(String);

impl UpdateSelectionIdentity {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for UpdateSelectionIdentity {
    fn from(value: String) -> Self {
        Self(value)
    }
}
impl From<&str> for UpdateSelectionIdentity {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateRunMode {
    PlanOnly,
    ReviewAndApply,
    ApplyAll,
    ApplySelected(Vec<UpdateSelectionIdentity>),
}

#[derive(Debug, Clone)]
pub struct UpdateReview {
    identity: UpdateSelectionIdentity,
    path: PathBuf,
    target_kind: String,
    current_version: String,
    latest_version: String,
}

impl UpdateReview {
    pub fn new(
        identity: UpdateSelectionIdentity,
        path: PathBuf,
        target_kind: impl Into<String>,
        current_version: impl Into<String>,
        latest_version: impl Into<String>,
    ) -> Self {
        Self {
            identity,
            path,
            target_kind: target_kind.into(),
            current_version: current_version.into(),
            latest_version: latest_version.into(),
        }
    }

    pub fn identity(&self) -> &UpdateSelectionIdentity {
        &self.identity
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn target_kind(&self) -> &str {
        &self.target_kind
    }
    pub fn current_version(&self) -> &str {
        &self.current_version
    }
    pub fn latest_version(&self) -> &str {
        &self.latest_version
    }
}

#[derive(Debug, Clone)]
pub struct UpdatePlan {
    report: UpdateReport,
    identities: Vec<UpdateSelectionIdentity>,
}

impl UpdatePlan {
    pub fn planned(&self) -> &[PlannedUpdate] {
        &self.report.planned
    }
    pub fn skipped(&self) -> &[SkippedUpdate] {
        &self.report.skipped
    }
    pub fn identities(&self) -> &[UpdateSelectionIdentity] {
        &self.identities
    }
    pub fn report(&self) -> &UpdateReport {
        &self.report
    }

    pub fn reviews(&self) -> Vec<UpdateReview> {
        self.report
            .planned
            .iter()
            .zip(&self.identities)
            .map(|(update, identity)| UpdateReview {
                identity: identity.clone(),
                path: update.path().to_path_buf(),
                target_kind: update.target_kind().to_string(),
                current_version: update.current_version().to_string(),
                latest_version: update.latest_version().to_string(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateRunStatus {
    NoUpdates,
    UpdatesPlanned,
    NoUpdatesApproved,
    UpdatesApplied,
    RunRejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateRunRejection {
    UnknownSelections(Vec<UpdateSelectionIdentity>),
}

impl UpdateRunRejection {
    pub fn unknown_selections(&self) -> &[UpdateSelectionIdentity] {
        match self {
            Self::UnknownSelections(identities) => identities,
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpdateRunOutcome {
    plan: UpdatePlan,
    approved: Vec<UpdateSelectionIdentity>,
    applied: Vec<UpdateSelectionIdentity>,
    changed_paths: Vec<PathBuf>,
    status: UpdateRunStatus,
    rejection: Option<UpdateRunRejection>,
}

impl UpdateRunOutcome {
    pub fn plan(&self) -> &UpdatePlan {
        &self.plan
    }
    pub fn approved(&self) -> &[UpdateSelectionIdentity] {
        &self.approved
    }
    pub fn applied(&self) -> &[UpdateSelectionIdentity] {
        &self.applied
    }
    pub fn changed_paths(&self) -> &[PathBuf] {
        &self.changed_paths
    }
    pub fn status(&self) -> UpdateRunStatus {
        self.status
    }
    pub fn rejection(&self) -> Option<&UpdateRunRejection> {
        self.rejection.as_ref()
    }
}

pub type ApprovalCallback<'a> =
    dyn FnMut(&[UpdateReview]) -> Result<Vec<UpdateSelectionIdentity>> + 'a;

pub struct UpdateRun<'a> {
    inventory: &'a Inventory,
    chart_resolver: &'a (dyn ChartVersionResolver + Sync),
    image_resolver: &'a (dyn ImageVersionResolver + Sync),
    progress: Option<&'a ProgressCallback<'a>>,
}

impl<'a> UpdateRun<'a> {
    pub fn new(
        inventory: &'a Inventory,
        chart_resolver: &'a (dyn ChartVersionResolver + Sync),
        image_resolver: &'a (dyn ImageVersionResolver + Sync),
    ) -> Self {
        Self {
            inventory,
            chart_resolver,
            image_resolver,
            progress: None,
        }
    }

    #[must_use]
    pub fn with_progress(mut self, progress: &'a ProgressCallback<'a>) -> Self {
        self.progress = Some(progress);
        self
    }

    pub fn execute(
        self,
        mode: UpdateRunMode,
        mut approval: Option<&mut ApprovalCallback<'_>>,
    ) -> Result<UpdateRunOutcome> {
        let report = plan_updates_with_optional_progress(
            self.inventory,
            self.chart_resolver,
            self.image_resolver,
            PlanOptions::default(),
            self.progress,
        );
        let identities = report
            .planned
            .iter()
            .map(|update| {
                UpdateSelectionIdentity::new(update.selection_id(&self.inventory.repo_root))
            })
            .collect::<Vec<_>>();
        let plan = UpdatePlan { report, identities };

        if plan.planned().is_empty() {
            let unknown = match &mode {
                UpdateRunMode::ApplySelected(requested) => {
                    normalize_selection(&plan, requested.clone()).err()
                }
                _ => None,
            };
            if let Some(unknown) = unknown {
                return Ok(outcome(
                    plan,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Some(UpdateRunRejection::UnknownSelections(unknown)),
                ));
            }
            return Ok(outcome(plan, Vec::new(), Vec::new(), Vec::new(), None));
        }

        let requested = match mode {
            UpdateRunMode::PlanOnly => return Ok(planned_outcome(plan)),
            UpdateRunMode::ApplyAll => plan.identities.clone(),
            UpdateRunMode::ApplySelected(requested) => requested,
            UpdateRunMode::ReviewAndApply => {
                let callback = approval.as_mut().ok_or_else(|| {
                    anyhow!("Review and Apply mode requires an approval callback")
                })?;
                callback(&plan.reviews())?
            }
        };

        let approved = match normalize_selection(&plan, requested) {
            Ok(approved) => approved,
            Err(unknown) => {
                return Ok(outcome(
                    plan,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Some(UpdateRunRejection::UnknownSelections(unknown)),
                ));
            }
        };
        if approved.is_empty() {
            return Ok(outcome(plan, approved, Vec::new(), Vec::new(), None));
        }

        let approved_set = approved.iter().cloned().collect::<HashSet<_>>();
        let selected = plan
            .report
            .planned
            .iter()
            .zip(&plan.identities)
            .filter(|(_, identity)| approved_set.contains(*identity))
            .map(|(update, _)| update)
            .collect::<Vec<_>>();
        let changed_paths = apply_planned_updates_with_paths(&selected)?
            .into_iter()
            .map(|path| {
                path.strip_prefix(&self.inventory.repo_root)
                    .unwrap_or(&path)
                    .to_path_buf()
            })
            .collect();
        Ok(outcome(
            plan,
            approved.clone(),
            approved,
            changed_paths,
            None,
        ))
    }
}

fn normalize_selection(
    plan: &UpdatePlan,
    requested: Vec<UpdateSelectionIdentity>,
) -> std::result::Result<Vec<UpdateSelectionIdentity>, Vec<UpdateSelectionIdentity>> {
    let known = plan.identities.iter().cloned().collect::<HashSet<_>>();
    let requested = requested.into_iter().collect::<BTreeSet<_>>();
    let unknown = requested
        .iter()
        .filter(|identity| !known.contains(*identity))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(unknown);
    }
    Ok(plan
        .identities
        .iter()
        .filter(|identity| requested.contains(*identity))
        .cloned()
        .collect())
}

fn planned_outcome(plan: UpdatePlan) -> UpdateRunOutcome {
    UpdateRunOutcome {
        plan,
        approved: Vec::new(),
        applied: Vec::new(),
        changed_paths: Vec::new(),
        status: UpdateRunStatus::UpdatesPlanned,
        rejection: None,
    }
}

fn outcome(
    plan: UpdatePlan,
    approved: Vec<UpdateSelectionIdentity>,
    applied: Vec<UpdateSelectionIdentity>,
    changed_paths: Vec<PathBuf>,
    rejection: Option<UpdateRunRejection>,
) -> UpdateRunOutcome {
    let status = if rejection.is_some() {
        UpdateRunStatus::RunRejected
    } else if !applied.is_empty() {
        UpdateRunStatus::UpdatesApplied
    } else if !plan.planned().is_empty() && approved.is_empty() {
        UpdateRunStatus::NoUpdatesApproved
    } else if !plan.planned().is_empty() {
        UpdateRunStatus::UpdatesPlanned
    } else {
        UpdateRunStatus::NoUpdates
    };
    UpdateRunOutcome {
        plan,
        approved,
        applied,
        changed_paths,
        status,
        rejection,
    }
}
