//! One complete planning, review, selection, and application attempt.

pub(crate) mod implementation;
#[cfg(test)]
mod tests;

pub use self::implementation::{SkipReasonCode, SkippedUpdate};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use self::implementation::{
    PlanOptions, PlannedUpdate, UpdateReport, apply_planned_updates, plan_updates,
};
use crate::models::{Inventory, TargetKind};
use crate::resolvers::{ChartVersionResolver, ImageVersionResolver};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpdateSelectionIdentity(String);

impl UpdateSelectionIdentity {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for UpdateSelectionIdentity {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateRunMode {
    PlanOnly,
    ReviewAndApply,
    ApplyAll,
    ApplySelected(Vec<UpdateSelectionIdentity>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateRunStatus {
    NoUpdates,
    UpdatesPlanned,
    NoUpdatesApproved,
    UpdatesApplied,
    RunRejected,
}

#[derive(Debug)]
pub struct UpdatePlan {
    report: UpdateReport,
    identities: Vec<UpdateSelectionIdentity>,
}

impl UpdatePlan {
    pub(crate) fn from_report(report: UpdateReport, repo_root: &Path) -> Self {
        let identities = report
            .planned
            .iter()
            .map(|update| UpdateSelectionIdentity(update.selection_id(repo_root)))
            .collect();
        Self { report, identities }
    }

    pub fn review(&self) -> Vec<UpdateReview<'_>> {
        self.report
            .planned
            .iter()
            .zip(&self.identities)
            .map(|(update, identity)| UpdateReview { update, identity })
            .collect()
    }

    pub fn skipped(&self) -> &[SkippedUpdate] {
        &self.report.skipped
    }

    pub fn checked_count(&self) -> usize {
        self.report.checked_count
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UpdateReview<'a> {
    update: &'a PlannedUpdate,
    identity: &'a UpdateSelectionIdentity,
}

impl UpdateReview<'_> {
    pub fn identity(&self) -> &UpdateSelectionIdentity {
        self.identity
    }

    pub fn path(&self) -> &Path {
        self.update.path()
    }

    pub fn document_index(&self) -> usize {
        self.update.document_index()
    }

    pub fn target_name(&self) -> &str {
        self.update.target_name()
    }

    pub fn current_version(&self) -> &str {
        self.update.current_version()
    }

    pub fn latest_version(&self) -> &str {
        self.update.latest_version()
    }

    pub fn kind(&self) -> TargetKind {
        self.update.kind()
    }

    pub fn resource_kind(&self) -> &str {
        match self.update {
            PlannedUpdate::Chart(_) => "HelmRelease",
            PlannedUpdate::Image(update) => update
                .manifest_identity
                .as_ref()
                .map_or("ImageBinding", |identity| {
                    identity.resource_id.kind.as_str()
                }),
        }
    }

    pub fn yaml_path(&self) -> &str {
        match self.update {
            PlannedUpdate::Chart(_) => "spec.chart.spec.version",
            PlannedUpdate::Image(update) => &update.yaml_path,
        }
    }

    pub fn chart_name(&self) -> Option<&str> {
        match self.update {
            PlannedUpdate::Chart(update) => Some(&update.chart_name),
            PlannedUpdate::Image(_) => None,
        }
    }

    pub fn repo_name(&self) -> Option<&str> {
        match self.update {
            PlannedUpdate::Chart(update) => Some(&update.repo_name),
            PlannedUpdate::Image(_) => None,
        }
    }

    pub fn source_locations(&self) -> Vec<(&Path, usize)> {
        match self.update {
            PlannedUpdate::Chart(update) => update
                .manifest_identity
                .as_ref()
                .map_or_else(Vec::new, |identity| identity.source_locations()),
            PlannedUpdate::Image(_) => Vec::new(),
        }
    }

    pub fn current_image(&self) -> Option<&str> {
        match self.update {
            PlannedUpdate::Image(update) => Some(&update.current_image),
            PlannedUpdate::Chart(_) => None,
        }
    }

    pub fn latest_image(&self) -> Option<&str> {
        match self.update {
            PlannedUpdate::Image(update) => Some(&update.latest_image),
            PlannedUpdate::Chart(_) => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct UpdateRunRejection {
    unknown_identities: Vec<UpdateSelectionIdentity>,
}

impl UpdateRunRejection {
    pub fn unknown_identities(&self) -> &[UpdateSelectionIdentity] {
        &self.unknown_identities
    }
}

#[derive(Debug)]
pub struct UpdateRunOutcome {
    plan: UpdatePlan,
    status: UpdateRunStatus,
    approved: Vec<UpdateSelectionIdentity>,
    applied: Vec<UpdateSelectionIdentity>,
    changed_paths: Vec<PathBuf>,
    rejection: Option<UpdateRunRejection>,
}

impl UpdateRunOutcome {
    pub fn plan(&self) -> &UpdatePlan {
        &self.plan
    }

    pub fn status(&self) -> UpdateRunStatus {
        self.status
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

    pub fn rejection(&self) -> Option<&UpdateRunRejection> {
        self.rejection.as_ref()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResolutionProgress<'a> {
    pub completed: usize,
    pub total: usize,
    pub path: &'a Path,
}

pub type Approval<'a> = dyn FnMut(&[UpdateReview<'_>]) -> Result<Vec<UpdateSelectionIdentity>> + 'a;
pub type ProgressObserver<'a> = dyn FnMut(ResolutionProgress<'_>) + Send + 'a;

pub struct UpdateRun<'a> {
    chart_resolver: &'a (dyn ChartVersionResolver + Sync),
    image_resolver: &'a (dyn ImageVersionResolver + Sync),
}

impl<'a> UpdateRun<'a> {
    pub fn new(
        chart_resolver: &'a (dyn ChartVersionResolver + Sync),
        image_resolver: &'a (dyn ImageVersionResolver + Sync),
    ) -> Self {
        Self {
            chart_resolver,
            image_resolver,
        }
    }

    pub fn execute(
        &self,
        inventory: &Inventory,
        mode: UpdateRunMode,
        approval: &mut Approval<'_>,
        progress: Option<&mut ProgressObserver<'_>>,
    ) -> Result<UpdateRunOutcome> {
        let report = plan_updates(
            inventory,
            self.chart_resolver,
            self.image_resolver,
            PlanOptions::default(),
            progress,
        );
        for update in &report.planned {
            if !update.has_original_identity() {
                bail!(
                    "inventory lacks original manifest or source identity for {}; scan the repository before running updates",
                    update.path().display()
                );
            }
        }
        let plan = UpdatePlan::from_report(report, &inventory.repo_root);
        let mut outcome = UpdateRunOutcome {
            status: if plan.identities.is_empty() {
                UpdateRunStatus::NoUpdates
            } else {
                UpdateRunStatus::UpdatesPlanned
            },
            plan,
            approved: Vec::new(),
            applied: Vec::new(),
            changed_paths: Vec::new(),
            rejection: None,
        };
        let requested = match mode {
            UpdateRunMode::PlanOnly => return Ok(outcome),
            UpdateRunMode::ReviewAndApply if !outcome.plan.identities.is_empty() => {
                approval(&outcome.plan.review())?
            }
            UpdateRunMode::ReviewAndApply => Vec::new(),
            UpdateRunMode::ApplyAll => outcome.plan.identities.clone(),
            UpdateRunMode::ApplySelected(identities) => identities,
        };
        let mut requested = requested.into_iter().collect::<BTreeSet<_>>();
        let approved = outcome
            .plan
            .identities
            .iter()
            .filter(|identity| requested.remove(*identity))
            .cloned()
            .collect::<Vec<_>>();
        if !requested.is_empty() {
            outcome.status = UpdateRunStatus::RunRejected;
            outcome.rejection = Some(UpdateRunRejection {
                unknown_identities: requested.into_iter().collect(),
            });
            return Ok(outcome);
        }
        if approved.is_empty() {
            if !outcome.plan.identities.is_empty() {
                outcome.status = UpdateRunStatus::NoUpdatesApproved;
            }
            return Ok(outcome);
        }
        let approved_set = approved.iter().collect::<BTreeSet<_>>();
        let updates = outcome
            .plan
            .report
            .planned
            .iter()
            .zip(&outcome.plan.identities)
            .filter(|(_, identity)| approved_set.contains(identity))
            .map(|(update, _)| update);
        outcome.changed_paths = apply_planned_updates(updates)?
            .into_iter()
            .map(|path| {
                path.strip_prefix(&inventory.repo_root)
                    .unwrap_or(&path)
                    .to_path_buf()
            })
            .collect();
        outcome.applied.clone_from(&approved);
        outcome.approved = approved;
        outcome.status = UpdateRunStatus::UpdatesApplied;
        Ok(outcome)
    }
}
