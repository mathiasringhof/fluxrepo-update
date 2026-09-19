use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use fluxrepo_update::resolvers::{StaticImageVersionResolver, StaticVersionResolver};
use fluxrepo_update::scanner::scan_repo;
use fluxrepo_update::update_run::{
    UpdateRun, UpdateRunMode, UpdateRunStatus, UpdateSelectionIdentity,
};

fn fixture() -> (
    tempfile::TempDir,
    StaticVersionResolver,
    StaticImageVersionResolver,
) {
    let temp = tempfile::tempdir().expect("temp dir");
    for file in ["a", "b"] {
        fs::write(temp.path().join(format!("{file}.yaml")), format!(
            "kind: Pod\nmetadata:\n  name: {file}\nspec:\n  containers:\n  - name: app\n    image: example/{file}:1.0.0\n  - name: sidecar\n    image: example/{file}:1.0.0\n"
        )).expect("write fixture");
    }
    let images = StaticImageVersionResolver::new(HashMap::from([
        ("example/a:1.0.0".into(), "example/a:2.0.0".into()),
        ("example/b:1.0.0".into(), "example/b:2.0.0".into()),
    ]));
    (temp, StaticVersionResolver::new(HashMap::new()), images)
}

fn contents(root: &Path) -> Vec<String> {
    ["a.yaml", "b.yaml"]
        .map(|file| fs::read_to_string(root.join(file)).expect("read fixture"))
        .to_vec()
}

#[test]
fn planning_retains_all_updates_without_review_or_writes() {
    let (temp, charts, images) = fixture();
    let original = contents(temp.path());
    let inventory = scan_repo(temp.path()).expect("scan");
    let mut progress = Vec::new();
    let outcome = UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| panic!("planning must not review"),
            Some(&mut |event| progress.push((event.completed, event.total))),
        )
        .expect("plan");
    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesPlanned);
    assert_eq!(outcome.plan().review().len(), 4);
    assert!(outcome.approved().is_empty());
    assert!(outcome.applied().is_empty());
    assert!(outcome.changed_paths().is_empty());
    assert_eq!(progress, vec![(1, 4), (2, 4), (3, 4), (4, 4)]);
    assert_eq!(contents(temp.path()), original);
}

#[test]
fn review_runs_once_and_retains_declined_updates() {
    let (temp, charts, images) = fixture();
    let inventory = scan_repo(temp.path()).expect("scan");
    let mut reviews = 0;
    let outcome = UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::ReviewAndApply,
            &mut |items| {
                reviews += 1;
                assert_eq!(items.len(), 4);
                Ok(vec![
                    items[2].identity().clone(),
                    items[0].identity().clone(),
                    items[0].identity().clone(),
                ])
            },
            None,
        )
        .expect("review and apply");
    assert_eq!(reviews, 1);
    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesApplied);
    let items = outcome.plan().review();
    assert_eq!(items.len(), 4);
    assert_eq!(
        outcome.approved(),
        &[items[0].identity().clone(), items[2].identity().clone()]
    );
    assert_eq!(outcome.applied(), outcome.approved());
    assert_eq!(
        outcome.changed_paths(),
        &[PathBuf::from("a.yaml"), PathBuf::from("b.yaml")]
    );
    for text in contents(temp.path()) {
        assert_eq!(text.matches("2.0.0").count(), 1);
        assert_eq!(text.matches("1.0.0").count(), 1);
    }
}

#[test]
fn applying_all_deduplicates_changed_files() {
    let (temp, charts, images) = fixture();
    let inventory = scan_repo(temp.path()).expect("scan");
    let outcome = UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::ApplyAll,
            &mut |_| panic!("apply all must not review"),
            None,
        )
        .expect("apply all");
    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesApplied);
    assert_eq!(outcome.applied().len(), 4);
    assert_eq!(
        outcome.changed_paths(),
        &[PathBuf::from("a.yaml"), PathBuf::from("b.yaml")]
    );
    assert!(
        contents(temp.path())
            .iter()
            .all(|text| !text.contains("1.0.0"))
    );
}

#[test]
fn selected_and_reviewed_unknown_identities_reject_the_whole_selection() {
    for review in [false, true] {
        let (temp, charts, images) = fixture();
        let original = contents(temp.path());
        let inventory = scan_repo(temp.path()).expect("scan");
        let run = UpdateRun::new(&charts, &images);
        let plan = run
            .execute(
                &inventory,
                UpdateRunMode::PlanOnly,
                &mut |_| unreachable!(),
                None,
            )
            .expect("plan");
        let requested = vec![
            plan.plan().review()[0].identity().clone(),
            "unknown-b".to_string().into(),
            "unknown-a".to_string().into(),
        ];
        let mode = if review {
            UpdateRunMode::ReviewAndApply
        } else {
            UpdateRunMode::ApplySelected(requested.clone())
        };
        let outcome = run
            .execute(&inventory, mode, &mut |_| Ok(requested.clone()), None)
            .expect("rejection outcome");
        assert_eq!(outcome.status(), UpdateRunStatus::RunRejected);
        assert_eq!(outcome.plan().review().len(), 4);
        assert!(outcome.approved().is_empty());
        assert!(outcome.applied().is_empty());
        assert!(outcome.changed_paths().is_empty());
        let unknown = outcome.rejection().expect("rejection").unknown_identities();
        assert_eq!(
            unknown
                .iter()
                .map(UpdateSelectionIdentity::as_str)
                .collect::<Vec<_>>(),
            ["unknown-a", "unknown-b"]
        );
        assert_eq!(contents(temp.path()), original);
    }
}

#[test]
fn declining_all_preserves_the_plan_and_does_not_write() {
    let (temp, charts, images) = fixture();
    let original = contents(temp.path());
    let inventory = scan_repo(temp.path()).expect("scan");
    let outcome = UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::ReviewAndApply,
            &mut |_| Ok(vec![]),
            None,
        )
        .expect("review");
    assert_eq!(outcome.status(), UpdateRunStatus::NoUpdatesApproved);
    assert_eq!(outcome.plan().review().len(), 4);
    assert!(outcome.approved().is_empty());
    assert_eq!(contents(temp.path()), original);
}

#[test]
fn empty_plans_never_request_approval_but_stale_selections_are_rejected() {
    let temp = tempfile::tempdir().expect("temp");
    let inventory = scan_repo(temp.path()).expect("scan");
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(HashMap::new());
    let run = UpdateRun::new(&charts, &images);
    for mode in [
        UpdateRunMode::PlanOnly,
        UpdateRunMode::ReviewAndApply,
        UpdateRunMode::ApplyAll,
    ] {
        let outcome = run
            .execute(
                &inventory,
                mode,
                &mut |_| panic!("empty plan must not review"),
                None,
            )
            .expect("empty plan");
        assert_eq!(outcome.status(), UpdateRunStatus::NoUpdates);
        assert!(outcome.plan().review().is_empty());
    }
    let outcome = run
        .execute(
            &inventory,
            UpdateRunMode::ApplySelected(vec!["stale".to_string().into()]),
            &mut |_| unreachable!(),
            None,
        )
        .expect("rejection");
    assert_eq!(outcome.status(), UpdateRunStatus::RunRejected);
}

#[test]
fn approval_and_stale_plan_failures_produce_errors_without_writes() {
    let (temp, charts, images) = fixture();
    let original = contents(temp.path());
    let inventory = scan_repo(temp.path()).expect("scan");
    let run = UpdateRun::new(&charts, &images);
    let error = run
        .execute(
            &inventory,
            UpdateRunMode::ReviewAndApply,
            &mut |_| Err(anyhow!("review interrupted")),
            None,
        )
        .expect_err("approval failure");
    assert!(error.to_string().contains("review interrupted"));
    assert_eq!(contents(temp.path()), original);
    let error = run
        .execute(
            &inventory,
            UpdateRunMode::ReviewAndApply,
            &mut |items| {
                fs::write(
                    temp.path().join("b.yaml"),
                    original[1].replace("1.0.0", "1.5.0"),
                )?;
                Ok(items.iter().map(|item| item.identity().clone()).collect())
            },
            None,
        )
        .expect_err("stale plan");
    assert!(error.to_string().contains("changed"));
    assert_eq!(
        fs::read_to_string(temp.path().join("a.yaml")).expect("read a"),
        original[0]
    );
}

#[test]
fn plans_require_the_original_manifest_identity() {
    let (temp, charts, images) = fixture();
    let original = contents(temp.path());
    let mut inventory = scan_repo(temp.path()).expect("scan");
    inventory.manifest_documents.clear();
    UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::ApplyAll,
            &mut |_| unreachable!(),
            None,
        )
        .expect_err("unverified inventory must not produce an applicable plan");
    assert_eq!(contents(temp.path()), original);
}

#[test]
fn chart_plans_require_the_original_source_identity() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("release.yaml");
    let original = "kind: HelmRepository\nmetadata: {name: source}\nspec: {url: https://example.invalid}\n---\nkind: HelmRelease\nmetadata: {name: demo}\nspec:\n  chart:\n    spec:\n      chart: demo\n      version: 1.0.0\n      sourceRef: {kind: HelmRepository, name: source}\n";
    fs::write(&path, original).expect("write source and release");
    let mut inventory = scan_repo(temp.path()).expect("scan");
    inventory
        .manifest_documents
        .retain(|(_, document), _| *document != 0);
    let charts = StaticVersionResolver::new(HashMap::from([(
        ("source".into(), "demo".into()),
        "2.0.0".into(),
    )]));
    let images = StaticImageVersionResolver::new(HashMap::new());
    UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::ApplyAll,
            &mut |_| unreachable!(),
            None,
        )
        .expect_err("unverified chart source must not produce an applicable plan");
    assert_eq!(fs::read_to_string(path).expect("read"), original);
}

#[test]
fn selected_updates_are_deduplicated_and_applied_in_plan_order() {
    let (temp, charts, images) = fixture();
    let inventory = scan_repo(temp.path()).expect("scan");
    let run = UpdateRun::new(&charts, &images);
    let plan = run
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .expect("plan");
    let items = plan.plan().review();
    let first = items[0].identity().clone();
    let last = items[3].identity().clone();
    let outcome = run
        .execute(
            &inventory,
            UpdateRunMode::ApplySelected(vec![last.clone(), first.clone(), last.clone()]),
            &mut |_| panic!("selected must not review"),
            None,
        )
        .expect("apply selected");
    assert_eq!(outcome.status(), UpdateRunStatus::UpdatesApplied);
    assert_eq!(outcome.plan().review().len(), 4);
    assert_eq!(outcome.applied(), &[first, last]);
    for text in contents(temp.path()) {
        assert_eq!(text.matches("2.0.0").count(), 1);
        assert_eq!(text.matches("1.0.0").count(), 1);
    }
}

#[test]
fn skipped_attempts_survive_review_application_and_rejection() {
    for reject in [false, true] {
        let (temp, charts, images) = fixture();
        fs::write(temp.path().join("skipped.yaml"), "kind: HelmRelease\nmetadata: {name: incomplete}\nspec: {chart: {spec: {version: 1.0.0}}}\n").expect("write skip");
        let inventory = scan_repo(temp.path()).expect("scan");
        let run = UpdateRun::new(&charts, &images);
        let plan = run
            .execute(
                &inventory,
                UpdateRunMode::PlanOnly,
                &mut |_| unreachable!(),
                None,
            )
            .expect("plan");
        let outcome = run
            .execute(
                &inventory,
                UpdateRunMode::ReviewAndApply,
                &mut |items| {
                    if reject {
                        Ok(vec!["unknown".to_string().into()])
                    } else {
                        Ok(items.iter().map(|item| item.identity().clone()).collect())
                    }
                },
                None,
            )
            .expect("outcome");
        assert_eq!(outcome.plan().skipped(), plan.plan().skipped());
        assert_eq!(outcome.plan().skipped().len(), 1);
        assert_eq!(
            outcome.status(),
            if reject {
                UpdateRunStatus::RunRejected
            } else {
                UpdateRunStatus::UpdatesApplied
            }
        );
        assert_eq!(outcome.applied().len(), if reject { 0 } else { 4 });
    }
}

#[test]
fn unreadable_or_malformed_files_abort_before_any_writes() {
    for remove in [false, true] {
        let (temp, charts, images) = fixture();
        let original = contents(temp.path());
        let inventory = scan_repo(temp.path()).expect("scan");
        let result = UpdateRun::new(&charts, &images).execute(
            &inventory,
            UpdateRunMode::ReviewAndApply,
            &mut |items| {
                if remove {
                    fs::remove_file(temp.path().join("b.yaml"))?;
                } else {
                    fs::write(temp.path().join("b.yaml"), "spec: [")?;
                }
                Ok(items.iter().map(|item| item.identity().clone()).collect())
            },
            None,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(temp.path().join("a.yaml")).expect("read a"),
            original[0]
        );
    }
}

#[test]
fn skipped_only_runs_keep_unresolved_attempts_without_requesting_approval() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("release.yaml");
    let original = "kind: HelmRelease\nmetadata: {name: incomplete}\nspec: {chart: {spec: {version: 1.0.0}}}\n";
    fs::write(&path, original).expect("write unresolved release");
    let inventory = scan_repo(temp.path()).expect("scan");
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(HashMap::new());
    let run = UpdateRun::new(&charts, &images);

    for mode in [
        UpdateRunMode::PlanOnly,
        UpdateRunMode::ReviewAndApply,
        UpdateRunMode::ApplyAll,
        UpdateRunMode::ApplySelected(Vec::new()),
    ] {
        let mut progress = Vec::new();
        let outcome = run
            .execute(
                &inventory,
                mode,
                &mut |_| panic!("skipped targets cannot be approved"),
                Some(&mut |event| progress.push((event.completed, event.total))),
            )
            .expect("skipped-only outcome");

        assert_eq!(outcome.status(), UpdateRunStatus::NoUpdates);
        assert!(outcome.plan().review().is_empty());
        assert_eq!(outcome.plan().skipped().len(), 1);
        assert!(outcome.approved().is_empty());
        assert!(outcome.applied().is_empty());
        assert!(outcome.changed_paths().is_empty());
        assert!(outcome.rejection().is_none());
        assert_eq!(progress, vec![(1, 1)]);
        assert_eq!(fs::read_to_string(&path).expect("read release"), original);
    }
}

#[cfg(unix)]
#[test]
fn a_later_write_failure_returns_an_error_and_preserves_partial_write_diagnostics() {
    use std::os::unix::fs::PermissionsExt;

    let (temp, charts, images) = fixture();
    let original = contents(temp.path());
    let inventory = scan_repo(temp.path()).expect("scan");
    let second_path = temp.path().join("b.yaml");
    let permissions = fs::metadata(&second_path)
        .expect("file metadata")
        .permissions();
    fs::set_permissions(&second_path, fs::Permissions::from_mode(0o444))
        .expect("make second file read-only");

    let result = UpdateRun::new(&charts, &images).execute(
        &inventory,
        UpdateRunMode::ApplyAll,
        &mut |_| panic!("apply-all must not request approval"),
        None,
    );
    fs::set_permissions(&second_path, permissions).expect("restore file permissions");

    let error = result.expect_err("a partial write must not return an outcome");
    assert!(error.to_string().contains("after 1 file(s) were applied"));
    assert!(error.to_string().contains("b.yaml"));
    let written = contents(temp.path());
    assert_eq!(written[0], original[0].replace("1.0.0", "2.0.0"));
    assert_eq!(written[1], original[1]);
}
