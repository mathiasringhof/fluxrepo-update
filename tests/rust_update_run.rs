use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use fluxrepo_update::resolvers::{StaticImageVersionResolver, StaticVersionResolver};
use fluxrepo_update::scanner::scan_repo;
use fluxrepo_update::update_run::{
    UpdateReview, UpdateRun, UpdateRunMode, UpdateRunStatus, UpdateSelectionIdentity,
};

fn chart_copies() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temp dir");
    for environment in ["production", "testing"] {
        let directory = temp.path().join(environment);
        fs::create_dir(&directory).expect("environment directory");
        fs::write(directory.join("source.yaml"), format!(
            "kind: HelmRepository\nmetadata:\n  name: shared\n  namespace: apps\n  labels: {{environment: {environment}}}\nspec:\n  url: https://charts.example.org\n  interval: 1h\n"
        )).expect("source");
        fs::write(directory.join("release.yaml"), format!(
            "kind: HelmRelease\nmetadata: {{name: {environment}, namespace: apps}}\nspec:\n  chart:\n    spec:\n      chart: demo\n      version: 1.0.0\n      sourceRef: {{kind: HelmRepository, name: shared}}\n"
        )).expect("release");
    }
    temp
}

#[test]
fn equivalent_source_copies_allow_each_manifest_to_update() {
    let temp = chart_copies();
    let inventory = scan_repo(temp.path()).expect("scan");
    let charts = StaticVersionResolver::new(HashMap::from([(
        ("shared".into(), "demo".into()),
        "2.0.0".into(),
    )]));
    let images = StaticImageVersionResolver::new(HashMap::new());
    let outcome = UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::ApplyAll,
            &mut |_| unreachable!(),
            None,
        )
        .expect("apply equivalent sources");
    assert_eq!(outcome.applied().len(), 2);
    assert!(outcome.plan().skipped().is_empty());
    for environment in ["production", "testing"] {
        let text = fs::read_to_string(temp.path().join(environment).join("release.yaml")).unwrap();
        assert!(text.contains("version: 2.0.0"));
    }
}

#[test]
fn changing_any_equivalent_source_during_review_prevents_all_writes() {
    for environment in ["production", "testing"] {
        let temp = chart_copies();
        let inventory = scan_repo(temp.path()).expect("scan");
        let charts = StaticVersionResolver::new(HashMap::from([(
            ("shared".into(), "demo".into()),
            "2.0.0".into(),
        )]));
        let images = StaticImageVersionResolver::new(HashMap::new());
        let result = UpdateRun::new(&charts, &images).execute(
            &inventory,
            UpdateRunMode::ReviewAndApply,
            &mut |items| {
                let path = temp.path().join(environment).join("source.yaml");
                let text = fs::read_to_string(&path).unwrap();
                fs::write(
                    path,
                    text.replace("charts.example.org", "different.example.org"),
                )
                .unwrap();
                Ok(items.iter().map(|item| item.identity().clone()).collect())
            },
            None,
        );
        assert!(
            result
                .expect_err("changed copy must reject application")
                .to_string()
                .contains("changed after planning")
        );
        for directory in ["production", "testing"] {
            assert!(
                fs::read_to_string(temp.path().join(directory).join("release.yaml"))
                    .unwrap()
                    .contains("version: 1.0.0")
            );
        }
    }
}

#[test]
fn inventory_explains_equivalent_source_locations_without_losing_the_count() {
    let temp = chart_copies();
    let inventory = scan_repo(temp.path()).expect("scan").to_json_value();
    assert_eq!(inventory["repository_count"], 2);
    assert_eq!(inventory["ambiguous_repositories"], serde_json::json!([]));
    let sources = inventory["equivalent_repositories"][0]["sources"]
        .as_array()
        .expect("equivalent sources");
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["path"], "production/source.yaml");
    assert_eq!(sources[1]["path"], "testing/source.yaml");
    assert!(sources.iter().all(
        |source| source["namespace"] == "apps" && source["url"] == "https://charts.example.org"
    ));
}

#[test]
fn differing_source_specs_or_namespaces_remain_ambiguous() {
    for replacement in [
        "  interval: 2h\n",
        "  interval: 1h\n  secretRef: {name: credentials}\n",
        "  interval: 1h\n  type: oci\n",
    ] {
        let temp = chart_copies();
        let path = temp.path().join("testing/source.yaml");
        let original = fs::read_to_string(&path).unwrap();
        fs::write(&path, original.replace("  interval: 1h\n", replacement)).unwrap();
        let inventory = scan_repo(temp.path()).unwrap().to_json_value();
        assert_eq!(inventory["repository_count"], 2);
        assert_eq!(
            inventory["ambiguous_repositories"][0]["sources"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(inventory["equivalent_repositories"], serde_json::json!([]));
    }
    let temp = chart_copies();
    let path = temp.path().join("testing/source.yaml");
    let original = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        original.replace("namespace: apps", "namespace: other"),
    )
    .unwrap();
    assert_eq!(
        scan_repo(temp.path()).unwrap().ambiguous_repositories["shared"].len(),
        2
    );
}

#[test]
fn a_changed_equivalent_copy_invalidates_previously_selected_ids() {
    let temp = chart_copies();
    let charts = StaticVersionResolver::new(HashMap::from([(
        ("shared".into(), "demo".into()),
        "2.0.0".into(),
    )]));
    let images = StaticImageVersionResolver::new(HashMap::new());
    let run = UpdateRun::new(&charts, &images);
    let inventory = scan_repo(temp.path()).unwrap();
    let planned = run
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    let selected = planned
        .plan()
        .review()
        .iter()
        .map(|item| item.identity().clone())
        .collect();
    let path = temp.path().join("testing/source.yaml");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        text.replace("environment: testing", "environment: renamed"),
    )
    .unwrap();
    let refreshed = scan_repo(temp.path()).unwrap();
    let outcome = run
        .execute(
            &refreshed,
            UpdateRunMode::ApplySelected(selected),
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    assert_eq!(outcome.status(), UpdateRunStatus::RunRejected);
    assert!(outcome.applied().is_empty());
}

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
fn incomparable_image_results_retain_distinct_skipped_target_identities() {
    let (temp, charts, _) = fixture();
    let inventory = scan_repo(temp.path()).expect("scan");
    let images = StaticImageVersionResolver::new(HashMap::from([
        ("example/a:1.0.0".into(), "example/a".into()),
        ("example/b:1.0.0".into(), "example/b".into()),
    ]));
    let outcome = UpdateRun::new(&charts, &images)
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .expect("retain unresolved image results");
    let skipped = outcome.plan().skipped();
    assert_eq!(skipped.len(), 4);
    assert_ne!(
        skipped[0].selection_id(&inventory.repo_root),
        skipped[1].selection_id(&inventory.repo_root),
        "two containers in the same manifest remain independently identifiable"
    );
    assert_eq!(skipped[0].yaml_path(), Some("spec.containers[0].image"));
    assert_eq!(skipped[1].yaml_path(), Some("spec.containers[1].image"));
    assert!(skipped.iter().all(|item| item.document_index() == Some(0)));
    assert_eq!(skipped[0].current_value(), Some("example/a:1.0.0"));
    assert_eq!(skipped[2].current_value(), Some("example/b:1.0.0"));
}

#[test]
fn mixed_plans_keep_target_order_counts_and_complete_progress() {
    let temp = tempfile::tempdir().expect("temp dir");
    for (name, text) in [
        (
            "a.yaml",
            "kind: Pod\nmetadata: {name: pod}\nspec:\n  containers:\n  - {name: app, image: 'example/app:1.0.0'}\n  - {name: current, image: 'example/current:2.0.0'}\n  - {name: missing, image: 'example/missing:1.0.0'}\n",
        ),
        (
            "b.yaml",
            "kind: HelmRelease\nmetadata: {name: release}\nspec:\n  chart:\n    spec:\n      chart: demo\n      version: 1.0.0\n      sourceRef: {kind: HelmRepository, name: shared}\n  values: {image: 'example/values:1.0.0'}\n",
        ),
        (
            "c.yaml",
            "kind: HelmRelease\nmetadata: {name: incomplete}\nspec: {chart: {spec: {version: 7.0.0}}}\n",
        ),
        (
            "d.yaml",
            "kind: Kustomization\nresources: ['https://example.org/manifests/v1.0.0/install.yaml']\n",
        ),
        (
            "source.yaml",
            "kind: HelmRepository\nmetadata: {name: shared}\nspec: {url: 'https://charts.example.org'}\n",
        ),
    ] {
        fs::write(temp.path().join(name), text).expect("write manifest");
    }
    let inventory = scan_repo(temp.path()).expect("scan");
    let charts = StaticVersionResolver::new(HashMap::from([(
        ("shared".into(), "demo".into()),
        "2.0.0".into(),
    )]));
    let images = StaticImageVersionResolver::new(HashMap::from([
        ("example/app:1.0.0".into(), "example/app:2.0.0".into()),
        (
            "example/current:2.0.0".into(),
            "example/current:2.0.0".into(),
        ),
        ("example/values:1.0.0".into(), "example/values:2.0.0".into()),
    ]));
    let run = UpdateRun::new(&charts, &images);
    let mut progress = Vec::new();
    let outcome = run
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            Some(&mut |event| {
                progress.push((event.completed, event.total, inventory.relative(event.path)));
            }),
        )
        .expect("plan mixed targets");

    assert_eq!(outcome.plan().checked_count(), 4);
    assert_eq!(
        outcome
            .plan()
            .review()
            .iter()
            .map(|item| (
                inventory.relative(item.path()),
                item.yaml_path().to_string(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("a.yaml".into(), "spec.containers[0].image".into()),
            ("b.yaml".into(), "spec.chart.spec.version".into()),
            ("b.yaml".into(), "spec.values.image".into()),
        ]
    );
    assert_eq!(
        outcome
            .plan()
            .skipped()
            .iter()
            .map(|item| (
                inventory.relative(item.path.as_deref().expect("target path")),
                item.yaml_path().expect("target field"),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("c.yaml".into(), "spec.chart.spec.version"),
            ("a.yaml".into(), "spec.containers[2].image"),
            ("d.yaml".into(), "resources[0]"),
        ]
    );
    assert_eq!(
        progress
            .iter()
            .map(|(completed, total, _)| (*completed, *total))
            .collect::<Vec<_>>(),
        vec![(1, 7), (2, 7), (3, 7), (4, 7), (5, 7), (6, 7), (7, 7)]
    );
    let mut progress_paths = progress
        .into_iter()
        .map(|(_, _, path)| path)
        .collect::<Vec<_>>();
    progress_paths.sort();
    assert_eq!(
        progress_paths,
        [
            "a.yaml", "a.yaml", "a.yaml", "b.yaml", "b.yaml", "c.yaml", "d.yaml"
        ]
    );

    let without_progress = run
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .expect("plan without progress");
    assert_eq!(outcome.plan().skipped(), without_progress.plan().skipped());
    assert_eq!(
        outcome
            .plan()
            .review()
            .iter()
            .map(UpdateReview::identity)
            .collect::<Vec<_>>(),
        without_progress
            .plan()
            .review()
            .iter()
            .map(UpdateReview::identity)
            .collect::<Vec<_>>()
    );
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
            Some(&mut |event| {
                if event.completed == 1 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                progress.push((event.completed, event.total));
            }),
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
