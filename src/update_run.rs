//! One complete planning, review, selection, and application attempt.

mod implementation;
#[cfg(test)]
mod tests;

pub use self::implementation::{SkipReasonCode, SkippedUpdate};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub(crate) use self::implementation::PlannedUpdate;
pub use self::implementation::RemoteResourceChange;
use self::implementation::{PlanOptions, apply_planned_updates, plan_updates_with_resources};
use crate::github::{GitHubReleaseResolver, RemoteResourceVersionResolver};
use crate::models::Inventory;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckCompleteness {
    Complete,
    Incomplete,
    Failed,
    NotChecked,
}

impl CheckCompleteness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
            Self::NotChecked => "not_checked",
        }
    }
}

/// Version Declaration counts for the full check, independent of approval and application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckSummary {
    pub checked_unchanged_count: usize,
    pub update_available_count: usize,
    pub unsupported_count: usize,
    pub failed_count: usize,
}

impl CheckSummary {
    pub fn completeness(self) -> CheckCompleteness {
        if self.checked_unchanged_count + self.update_available_count == 0 {
            if self.failed_count > 0 {
                CheckCompleteness::Failed
            } else {
                CheckCompleteness::NotChecked
            }
        } else if self.unsupported_count > 0 || self.failed_count > 0 {
            CheckCompleteness::Incomplete
        } else {
            CheckCompleteness::Complete
        }
    }
}

#[derive(Debug)]
pub struct UpdatePlan {
    planned: Vec<PlannedUpdate>,
    skipped: Vec<SkippedUpdate>,
    checked_count: usize,
}

impl UpdatePlan {
    pub(crate) fn new(
        planned: Vec<PlannedUpdate>,
        skipped: Vec<SkippedUpdate>,
        checked_count: usize,
    ) -> Self {
        Self {
            planned,
            skipped,
            checked_count,
        }
    }

    pub fn review(&self) -> Vec<UpdateReview<'_>> {
        self.planned.iter().map(PlannedUpdate::review).collect()
    }

    pub fn skipped(&self) -> &[SkippedUpdate] {
        &self.skipped
    }

    pub fn checked_count(&self) -> usize {
        self.checked_count
    }

    pub fn check_summary(&self) -> CheckSummary {
        let failed_count = self
            .skipped
            .iter()
            .filter(|item| item.reason_code.is_failed_check())
            .count();
        let update_available_count = self
            .planned
            .iter()
            .map(PlannedUpdate::declaration_count)
            .sum::<usize>();
        CheckSummary {
            checked_unchanged_count: self.checked_count - update_available_count,
            update_available_count,
            unsupported_count: self.skipped.len() - failed_count,
            failed_count,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UpdateReview<'a> {
    update: &'a PlannedUpdate,
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
    charts: &'a (dyn ChartVersionResolver + Sync),
    images: &'a (dyn ImageVersionResolver + Sync),
    remote_resources: Option<&'a (dyn RemoteResourceVersionResolver + Sync)>,
}

impl<'a> UpdateRun<'a> {
    pub fn new(
        chart_resolver: &'a (dyn ChartVersionResolver + Sync),
        image_resolver: &'a (dyn ImageVersionResolver + Sync),
    ) -> Self {
        Self {
            charts: chart_resolver,
            images: image_resolver,
            remote_resources: None,
        }
    }

    #[must_use]
    pub fn with_remote_resource_resolver(
        mut self,
        resolver: &'a (dyn RemoteResourceVersionResolver + Sync),
    ) -> Self {
        self.remote_resources = Some(resolver);
        self
    }

    pub fn execute(
        &self,
        inventory: &Inventory,
        mode: UpdateRunMode,
        approval: &mut Approval<'_>,
        progress: Option<&mut ProgressObserver<'_>>,
    ) -> Result<UpdateRunOutcome> {
        let default_remote_resolver = GitHubReleaseResolver::default();
        let plan = plan_updates_with_resources(
            inventory,
            self.charts,
            self.images,
            self.remote_resources.unwrap_or(&default_remote_resolver),
            PlanOptions::default(),
            progress,
        );
        for update in &plan.planned {
            if !update.has_original_identity() {
                bail!(
                    "inventory lacks original manifest or source identity for {}; scan the repository before running updates",
                    update.path().display()
                );
            }
        }
        let mut outcome = UpdateRunOutcome {
            status: if plan.planned.is_empty() {
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
            UpdateRunMode::ReviewAndApply if !outcome.plan.planned.is_empty() => {
                approval(&outcome.plan.review())?
            }
            UpdateRunMode::ReviewAndApply => Vec::new(),
            UpdateRunMode::ApplyAll => outcome
                .plan
                .planned
                .iter()
                .map(|update| update.identity().clone())
                .collect(),
            UpdateRunMode::ApplySelected(identities) => identities,
        };
        let mut requested = requested.into_iter().collect::<BTreeSet<_>>();
        let approved = outcome
            .plan
            .planned
            .iter()
            .map(PlannedUpdate::identity)
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
            if !outcome.plan.planned.is_empty() {
                outcome.status = UpdateRunStatus::NoUpdatesApproved;
            }
            return Ok(outcome);
        }
        let approved_set = approved.iter().collect::<BTreeSet<_>>();
        let updates = outcome
            .plan
            .planned
            .iter()
            .filter(|update| approved_set.contains(update.identity()));
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
