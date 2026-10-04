use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;

use fluxrepo_update::github::RemoteResourceVersionResolver;
use fluxrepo_update::resolvers::{StaticImageVersionResolver, StaticVersionResolver};
use fluxrepo_update::scanner::scan_repo;
use fluxrepo_update::update_run::{UpdateRun, UpdateRunMode, UpdateRunStatus};

type RemoteResolutionCall = (String, String, String, Vec<String>);

#[derive(Default)]
struct RemoteResolver {
    calls: Mutex<Vec<RemoteResolutionCall>>,
    fail: bool,
}

impl RemoteResourceVersionResolver for RemoteResolver {
    fn resolve(
        &self,
        owner: &str,
        repository: &str,
        current: &str,
        assets: &[String],
    ) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push((
            owner.into(),
            repository.into(),
            current.into(),
            assets.to_vec(),
        ));
        if self.fail {
            anyhow::bail!("release metadata unavailable")
        }
        Ok("v2.0.0".into())
    }
}

fn manifests(text: &str) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("kustomization.yaml"), text).unwrap();
    temp
}

const GROUP: &str = "kind: Kustomization\nresources:\n- 'https://github.com/Example/Operator/releases/download/v1.0.0/operator.yaml' # operator\n- github.com/example/operator/deploy/crds?ref=v1.0.0\n- local.yaml\n# - github.com/example/operator/disabled?ref=v9.0.0\n---\nkind: Kustomization\nresources: [\"https://github.com/example/operator/releases/download/v1.0.0/crds.yaml\"]\n";

#[test]
fn inventory_promotes_supported_pins_and_locates_unsupported_remote_forms() {
    let temp = manifests(
        "resources:\n- github.com/example/operator/deploy?ref=v1.0.0\n- HTTPS://GITHUB.COM/example/operator/releases/download/v1.0.0/operator.yaml\n- https://github.com/example/operator/deploy?ref=main\n- github.com/example/operator/deploy?ref=abcdef1234\n- https://github.com/example/operator/deploy?ref=v1.0.0&ref=v2.0.0\n- https://example.org/v1.0.0/operator.yaml\n- local.yaml\n",
    );
    let inventory = scan_repo(temp.path()).unwrap().to_json_value();
    assert_eq!(inventory["discovered_count"], 6);
    assert_eq!(inventory["remote_resource_target_count"], 2);
    assert_eq!(
        inventory["remote_resource_targets"][0]["yaml_path"],
        "resources[0]"
    );
    assert_eq!(
        inventory["remote_resource_targets"][1]["current_version"],
        "v1.0.0"
    );
    assert_eq!(
        inventory["unchecked_version_declarations"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn one_selected_identity_applies_all_coupled_urls_and_preserves_yaml() {
    let temp = manifests(GROUP);
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(HashMap::new());
    let remote = RemoteResolver::default();
    let run = UpdateRun::new(&charts, &images).with_remote_resource_resolver(&remote);
    let inventory = scan_repo(temp.path()).unwrap();
    assert_eq!(inventory.declaration_count(), 3);
    let plan = run
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    assert_eq!(plan.plan().checked_count(), 3);
    let review = plan.plan().review();
    assert_eq!(review.len(), 1);
    assert_eq!(review[0].remote_resource_changes().len(), 3);
    let id = review[0].identity().clone();
    let applied = run
        .execute(
            &inventory,
            UpdateRunMode::ApplySelected(vec![id]),
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    assert_eq!(applied.applied().len(), 1);
    assert_eq!(
        applied.changed_paths(),
        [std::path::PathBuf::from("kustomization.yaml")]
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("kustomization.yaml")).unwrap(),
        GROUP.replace("v1.0.0", "v2.0.0")
    );
    let calls = remote.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        (
            "example".into(),
            "operator".into(),
            "v1.0.0".into(),
            vec!["crds.yaml".into(), "operator.yaml".into()]
        )
    );
}

#[test]
fn changed_or_new_group_members_invalidate_selected_identity() {
    for replacement in [
        GROUP.replace("deploy/crds", "deploy/other"),
        format!("{GROUP}---\nresources:\n- github.com/example/operator/added?ref=v1.0.0\n"),
    ] {
        let temp = manifests(GROUP);
        let charts = StaticVersionResolver::new(HashMap::new());
        let images = StaticImageVersionResolver::new(HashMap::new());
        let remote = RemoteResolver::default();
        let run = UpdateRun::new(&charts, &images).with_remote_resource_resolver(&remote);
        let plan = run
            .execute(
                &scan_repo(temp.path()).unwrap(),
                UpdateRunMode::PlanOnly,
                &mut |_| unreachable!(),
                None,
            )
            .unwrap();
        let id = plan.plan().review()[0].identity().clone();
        fs::write(temp.path().join("kustomization.yaml"), &replacement).unwrap();
        let outcome = run
            .execute(
                &scan_repo(temp.path()).unwrap(),
                UpdateRunMode::ApplySelected(vec![id]),
                &mut |_| unreachable!(),
                None,
            )
            .unwrap();
        assert_eq!(outcome.status(), UpdateRunStatus::RunRejected);
        assert_eq!(
            fs::read_to_string(temp.path().join("kustomization.yaml")).unwrap(),
            replacement
        );
    }
}

#[test]
fn changing_resources_during_review_prevents_group_writes() {
    let temp = manifests(GROUP);
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(HashMap::new());
    let remote = RemoteResolver::default();
    let run = UpdateRun::new(&charts, &images).with_remote_resource_resolver(&remote);
    let inventory = scan_repo(temp.path()).unwrap();
    let changed =
        format!("{GROUP}---\nresources: [github.com/example/operator/added?ref=v1.0.0]\n");
    let error = run
        .execute(
            &inventory,
            UpdateRunMode::ReviewAndApply,
            &mut |review| {
                fs::write(temp.path().join("kustomization.yaml"), &changed).unwrap();
                Ok(vec![review[0].identity().clone()])
            },
            None,
        )
        .unwrap_err();
    assert!(error.to_string().contains("changed after planning"));
    assert_eq!(
        fs::read_to_string(temp.path().join("kustomization.yaml")).unwrap(),
        changed
    );
}

#[test]
fn failed_group_reports_each_unchecked_declaration() {
    let temp = manifests(GROUP);
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(HashMap::new());
    let remote = RemoteResolver {
        fail: true,
        ..RemoteResolver::default()
    };
    let plan = UpdateRun::new(&charts, &images)
        .with_remote_resource_resolver(&remote)
        .execute(
            &scan_repo(temp.path()).unwrap(),
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    assert_eq!(plan.plan().checked_count(), 0);
    assert_eq!(plan.plan().skipped().len(), 3);
    assert!(
        plan.plan()
            .skipped()
            .iter()
            .all(|skip| skip.current_value().unwrap().contains("v1.0.0"))
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("kustomization.yaml")).unwrap(),
        GROUP
    );
}

#[test]
fn kindless_resources_with_following_siblings_apply_and_progress_counts_declarations() {
    let text = "resources:\n- https://github.com/example/operator/deploy?ref=v1.0.0\n- github.com/example/operator/crds?ref=v1.0.0\nnamespace: demo\nlabels:\n- pairs: {app: demo}\n";
    let temp = manifests(text);
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(HashMap::new());
    let remote = RemoteResolver::default();
    let mut progress = Vec::new();
    let outcome = UpdateRun::new(&charts, &images)
        .with_remote_resource_resolver(&remote)
        .execute(
            &scan_repo(temp.path()).unwrap(),
            UpdateRunMode::ApplyAll,
            &mut |_| unreachable!(),
            Some(&mut |event| progress.push((event.completed, event.total))),
        )
        .unwrap();
    assert_eq!(outcome.applied().len(), 1);
    assert_eq!(
        fs::read_to_string(temp.path().join("kustomization.yaml")).unwrap(),
        text.replace("v1.0.0", "v2.0.0")
    );
    assert_eq!(progress.last(), Some(&(2, 2)));
}

#[test]
fn different_files_projects_and_current_pins_remain_independently_selectable() {
    let original = "resources:\n  - github.com/example/operator/a?ref=v1.0.0\n  - github.com/example/operator/b?ref=v1.0.0\n  - github.com/example/operator/c?ref=v1.1.0\n  - github.com/example/other/a?ref=v1.0.0\n";
    let temp = manifests(original);
    let second = temp.path().join("second.yaml");
    fs::write(
        &second,
        "kind: Kustomization\nresources: [github.com/example/operator/second?ref=v1.0.0]\n",
    )
    .unwrap();
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(HashMap::new());
    let remote = RemoteResolver::default();
    let run = UpdateRun::new(&charts, &images).with_remote_resource_resolver(&remote);
    let inventory = scan_repo(temp.path()).unwrap();
    let plan = run
        .execute(
            &inventory,
            UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    assert_eq!(plan.plan().review().len(), 4);
    let identity = plan
        .plan()
        .review()
        .into_iter()
        .find(|item| item.remote_resource_changes().len() == 2)
        .unwrap()
        .identity()
        .clone();
    let outcome = run
        .execute(
            &inventory,
            UpdateRunMode::ApplySelected(vec![identity]),
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    assert_eq!(outcome.applied().len(), 1);
    let expected = original
        .replace("operator/a?ref=v1.0.0", "operator/a?ref=v2.0.0")
        .replace("operator/b?ref=v1.0.0", "operator/b?ref=v2.0.0");
    assert_eq!(
        fs::read_to_string(temp.path().join("kustomization.yaml")).unwrap(),
        expected
    );
    assert!(fs::read_to_string(second).unwrap().contains("v1.0.0"));
}
