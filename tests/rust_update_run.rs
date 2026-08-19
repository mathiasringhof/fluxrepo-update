use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use fluxrepo_update::models::{ImageBinding, ImageBindingValueKind, Inventory, ResourceId};
use fluxrepo_update::resolvers::{StaticImageVersionResolver, StaticVersionResolver};
use fluxrepo_update::update_run::{
    UpdateReview, UpdateRun, UpdateRunMode, UpdateRunRejection, UpdateRunStatus,
    UpdateSelectionIdentity,
};

#[test]
fn no_updates_has_an_explicit_status_and_does_not_request_approval() {
    let fixture = Fixture::new();
    let images = StaticImageVersionResolver::new(HashMap::from([
        (
            "example/first:1.0.0".to_string(),
            "example/first:1.0.0".to_string(),
        ),
        (
            "example/second:1.0.0".to_string(),
            "example/second:1.0.0".to_string(),
        ),
    ]));
    let mut approval_calls = 0;
    let mut approval = |_reviews: &[UpdateReview]| {
        approval_calls += 1;
        Ok(Vec::new())
    };

    let outcome = UpdateRun::new(&fixture.inventory, &fixture.chart_resolver, &images)
        .execute(UpdateRunMode::PlanOnly, Some(&mut approval))
        .expect("run update");

    assert_eq!(outcome.status(), UpdateRunStatus::NoUpdates);
    assert!(outcome.plan().planned().is_empty());
    assert_eq!(approval_calls, 0);
}

#[test]
fn review_does_not_request_approval_when_nothing_is_planned() {
    let fixture = Fixture::new();
    let images = StaticImageVersionResolver::new(HashMap::from([
        (
            "example/first:1.0.0".to_string(),
            "example/first:1.0.0".to_string(),
        ),
        (
            "example/second:1.0.0".to_string(),
            "example/second:1.0.0".to_string(),
        ),
    ]));
    let mut approval_calls = 0;
    let mut approval = |_reviews: &[UpdateReview]| {
        approval_calls += 1;
        Ok(Vec::new())
    };

    let outcome = UpdateRun::new(&fixture.inventory, &fixture.chart_resolver, &images)
        .execute(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect("run update");

    assert_eq!(outcome.status(), UpdateRunStatus::NoUpdates);
    assert_eq!(approval_calls, 0);
}

#[test]
fn apply_all_applies_every_update_and_deduplicates_changed_paths() {
    let fixture = Fixture::new();

    let outcome = fixture
        .run(UpdateRunMode::ApplyAll, None)
        .expect("run update");

    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesApplied);
    assert_eq!(outcome.approved().len(), 2);
    assert_eq!(outcome.applied().len(), 2);
    assert_eq!(outcome.changed_paths(), [PathBuf::from("workload.yaml")]);
    let written = fixture.contents();
    assert!(written.contains("example/first:2.0.0"));
    assert!(written.contains("example/second:2.0.0"));
}

#[test]
fn plan_only_retains_the_complete_plan_without_approval_or_writes() {
    let fixture = Fixture::new();
    let approvals = AtomicUsize::new(0);
    let mut approval = |_: &[fluxrepo_update::update_run::UpdateReview]| {
        approvals.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    };

    let outcome = fixture
        .run(UpdateRunMode::PlanOnly, Some(&mut approval))
        .expect("plan update run");

    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesPlanned);
    assert_eq!(outcome.plan().planned().len(), 2);
    assert!(outcome.approved().is_empty());
    assert!(outcome.applied().is_empty());
    assert!(outcome.changed_paths().is_empty());
    assert_eq!(approvals.load(Ordering::SeqCst), 0);
    assert!(fixture.contents().contains(":1.0.0"));
}

#[test]
fn review_invokes_one_callback_and_retains_rejected_planned_updates() {
    let fixture = Fixture::new();
    let approvals = AtomicUsize::new(0);
    let mut approval = |reviews: &[fluxrepo_update::update_run::UpdateReview]| {
        approvals.fetch_add(1, Ordering::SeqCst);
        Ok(vec![reviews[1].identity().clone()])
    };

    let outcome = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect("review update run");

    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesApplied);
    assert_eq!(outcome.plan().planned().len(), 2);
    assert_eq!(outcome.approved(), outcome.applied());
    assert_eq!(outcome.applied().len(), 1);
    assert_eq!(outcome.changed_paths(), &[PathBuf::from("workload.yaml")]);
    let contents = fixture.contents();
    assert!(contents.contains("example/first:1.0.0"));
    assert!(contents.contains("example/second:2.0.0"));
}

#[test]
fn review_with_no_approvals_is_successful_and_does_not_write() {
    let fixture = Fixture::new();
    let mut approval = |_: &[fluxrepo_update::update_run::UpdateReview]| Ok(Vec::new());

    let outcome = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect("decline updates");

    assert_eq!(outcome.status(), UpdateRunStatus::NoUpdatesApproved);
    assert_eq!(outcome.plan().planned().len(), 2);
    assert!(outcome.rejection().is_none());
    assert!(fixture.contents().contains(":1.0.0"));
}

#[test]
fn review_with_all_approvals_applies_the_complete_plan() {
    let fixture = Fixture::new();
    let mut approval = |reviews: &[UpdateReview]| {
        Ok(reviews
            .iter()
            .map(|review| review.identity().clone())
            .collect())
    };

    let outcome = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect("approve all updates");

    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesApplied);
    assert_eq!(outcome.approved(), outcome.plan().identities());
    assert_eq!(outcome.applied(), outcome.plan().identities());
    assert_eq!(fixture.contents().matches(":2.0.0").count(), 2);
}

#[test]
fn approval_failure_produces_an_error_instead_of_an_outcome() {
    let fixture = Fixture::new();
    let mut approval = |_reviews: &[UpdateReview]| Err(anyhow::anyhow!("approval failed"));

    let error = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect_err("approval failure must be operational");

    assert!(error.to_string().contains("approval failed"));
    assert!(fixture.contents().contains(":1.0.0"));
}

#[test]
fn a_stale_plan_produces_an_error_instead_of_an_outcome() {
    let fixture = Fixture::new();
    let path = fixture.path.clone();
    let mut approval = move |reviews: &[UpdateReview]| {
        fs::write(
            &path,
            "kind: Pod\nmetadata: {name: demo}\nspec:\n  containers:\n    - {name: first, image: example/first:9.0.0}\n    - {name: second, image: example/second:1.0.0}\n",
        )?;
        Ok(reviews
            .iter()
            .map(|review| review.identity().clone())
            .collect())
    };

    let error = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect_err("stale plan must be operational");

    assert!(error.to_string().contains("changed after planning"));
}

#[test]
fn a_read_failure_produces_an_error_instead_of_an_outcome() {
    let fixture = Fixture::new();
    let path = fixture.path.clone();
    let mut approval = move |reviews: &[UpdateReview]| {
        fs::remove_file(&path)?;
        Ok(reviews
            .iter()
            .map(|review| review.identity().clone())
            .collect())
    };

    let error = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect_err("read failure must be operational");

    assert!(error.to_string().contains("failed to read"));
}

#[test]
fn a_parse_failure_produces_an_error_instead_of_an_outcome() {
    let fixture = Fixture::new();
    let path = fixture.path.clone();
    let mut approval = move |reviews: &[UpdateReview]| {
        fs::write(&path, "key: [\n")?;
        Ok(reviews
            .iter()
            .map(|review| review.identity().clone())
            .collect())
    };

    let error = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect_err("parse failure must be operational");

    assert!(
        error.to_string().contains("failed to parse"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn a_write_failure_produces_an_error_instead_of_an_outcome() {
    let fixture = Fixture::new();
    let path = fixture.path.clone();
    let mut approval = move |reviews: &[UpdateReview]| {
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions)?;
        Ok(reviews
            .iter()
            .map(|review| review.identity().clone())
            .collect())
    };

    let error = fixture
        .run(UpdateRunMode::ReviewAndApply, Some(&mut approval))
        .expect_err("write failure must be operational");

    assert!(error.to_string().contains("failed to write"));
}

#[test]
fn selected_mode_deduplicates_requests_and_preserves_plan_order() {
    let planned = Fixture::new();
    let plan = planned
        .run(UpdateRunMode::PlanOnly, None)
        .expect("discover identities");
    let first = plan.plan().identities()[0].clone();
    let second = plan.plan().identities()[1].clone();
    let fixture = Fixture::new();

    let outcome = fixture
        .run(
            UpdateRunMode::ApplySelected(vec![second.clone(), first.clone(), second]),
            None,
        )
        .expect("apply selected updates");

    assert_eq!(
        outcome.applied(),
        &[first, plan.plan().identities()[1].clone()]
    );
    assert_eq!(outcome.changed_paths(), &[PathBuf::from("workload.yaml")]);
    assert_eq!(fixture.contents().matches(":2.0.0").count(), 2);
}

#[test]
fn a_mixed_known_and_unknown_selection_rejects_the_complete_request_without_writes() {
    let fixture = Fixture::new();
    let known = fixture
        .run(UpdateRunMode::PlanOnly, None)
        .expect("discover identities")
        .plan()
        .identities()[0]
        .clone();
    let unknown = UpdateSelectionIdentity::new("v1:unknown");

    let outcome = fixture
        .run(
            UpdateRunMode::ApplySelected(vec![known, unknown.clone(), unknown.clone()]),
            None,
        )
        .expect("expected rejection is an outcome");

    assert_eq!(outcome.status(), UpdateRunStatus::RunRejected);
    assert_eq!(outcome.plan().planned().len(), 2);
    assert!(outcome.approved().is_empty());
    assert!(outcome.applied().is_empty());
    assert!(outcome.changed_paths().is_empty());
    assert_eq!(
        outcome.rejection(),
        Some(&UpdateRunRejection::UnknownSelections(vec![unknown]))
    );
    assert!(fixture.contents().contains(":1.0.0"));
}

#[test]
fn progress_observation_is_monotonic_and_best_effort() {
    let fixture = Fixture::new();
    let events = std::sync::Mutex::new(Vec::new());
    let observer = |completed, total, _path: &std::path::Path| {
        events
            .lock()
            .expect("progress lock")
            .push((completed, total));
    };
    let outcome = UpdateRun::new(
        &fixture.inventory,
        &fixture.chart_resolver,
        &fixture.image_resolver,
    )
    .with_progress(&observer)
    .execute(UpdateRunMode::PlanOnly, None)
    .expect("run with progress");

    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesPlanned);
    let events = events.into_inner().expect("progress lock");
    assert_eq!(events.len(), 2);
    assert!(events.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(events.iter().all(|(_, total)| *total == 2));
}

struct Fixture {
    _temp: tempfile::TempDir,
    path: PathBuf,
    inventory: Inventory,
    chart_resolver: StaticVersionResolver,
    image_resolver: StaticImageVersionResolver,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let repo_root = temp.path().canonicalize().expect("canonical temp dir");
        let path = repo_root.join("workload.yaml");
        fs::write(
            &path,
            "kind: Pod\nmetadata: {name: demo}\nspec:\n  containers:\n    - {name: first, image: example/first:1.0.0}\n    - {name: second, image: example/second:1.0.0}\n",
        )
        .expect("write manifest");
        let mut inventory = Inventory::new(repo_root);
        inventory.image_bindings = vec![
            binding(
                &path,
                "first",
                "spec.containers[0].image",
                "example/first:1.0.0",
            ),
            binding(
                &path,
                "second",
                "spec.containers[1].image",
                "example/second:1.0.0",
            ),
        ];
        Self {
            _temp: temp,
            path,
            inventory,
            chart_resolver: StaticVersionResolver::new(HashMap::new()),
            image_resolver: StaticImageVersionResolver::new(HashMap::from([
                (
                    "example/first:1.0.0".to_string(),
                    "example/first:2.0.0".to_string(),
                ),
                (
                    "example/second:1.0.0".to_string(),
                    "example/second:2.0.0".to_string(),
                ),
            ])),
        }
    }

    fn run(
        &self,
        mode: UpdateRunMode,
        approval: Option<&mut fluxrepo_update::update_run::ApprovalCallback<'_>>,
    ) -> anyhow::Result<fluxrepo_update::update_run::UpdateRunOutcome> {
        UpdateRun::new(&self.inventory, &self.chart_resolver, &self.image_resolver)
            .execute(mode, approval)
    }

    fn contents(&self) -> String {
        fs::read_to_string(&self.path).expect("read manifest")
    }
}

fn binding(path: &std::path::Path, name: &str, yaml_path: &str, image: &str) -> ImageBinding {
    ImageBinding {
        path: path.to_path_buf(),
        document_index: 0,
        resource_id: ResourceId {
            kind: "Pod".to_string(),
            name: name.to_string(),
            namespace: None,
        },
        yaml_path: yaml_path.to_string(),
        image: image.to_string(),
        value_kind: ImageBindingValueKind::ImageReference,
    }
}
