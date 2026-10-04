use anyhow::{Context, Result, bail, ensure};
use fluxrepo_update::models::Inventory;
use fluxrepo_update::update_run::{
    RemoteResourceChange, SkippedUpdate, UpdateReview, UpdateRunOutcome, UpdateRunStatus,
};
use serde_json::{Map, Value, json};

// These assertions intentionally cover corpus domain expectations, rather than the
// CLI report schema. New pointers and nonempty list equality need an explicit
// implementation when a future case is promoted, so unsupported checks fail loudly.
pub fn check_outcome(
    spec: &Value,
    inventory: &Inventory,
    outcome: &UpdateRunOutcome,
) -> Result<()> {
    ensure!(
        matches!(
            spec.get("command").and_then(Value::as_str),
            Some("plan" | "apply")
        ),
        "outcome assertions require command plan or apply"
    );
    let checks = checks(spec)?;
    let expected_status = match exit_code(checks)? {
        0 => UpdateRunStatus::NoUpdates,
        10 => UpdateRunStatus::UpdatesPlanned,
        20 => UpdateRunStatus::UpdatesApplied,
        code => bail!("unsupported exit_code {code}; use 0, 10, or 20 for application status"),
    };
    ensure!(
        outcome.status() == expected_status,
        "application status: expected {expected_status:?}, got {:?}",
        outcome.status()
    );
    let planned = outcome.plan().review();
    let skipped = outcome.plan().skipped();
    ensure!(
        outcome.plan().checked_count() + skipped.len() == inventory.declaration_count(),
        "inventory declarations: {} discovered, but {} checked + {} skipped",
        inventory.declaration_count(),
        outcome.plan().checked_count(),
        skipped.len()
    );
    assertions(checks, |pointer, assertion| match pointer {
        "/planned" => match assertion {
            Assertion::Equals(expected) => empty_list(pointer, expected, planned.len()),
            Assertion::Contains(expected) => {
                validate_fields(expected, RowKind::Planned)?;
                ensure!(
                    planned
                        .iter()
                        .any(|update| expected.iter().all(|(field, value)| {
                            planned_field(update, field, inventory).as_ref() == Some(value)
                        })),
                    "{pointer}: no planned update matches {expected:?}"
                );
                Ok(())
            }
        },
        "/skipped" => match assertion {
            Assertion::Equals(expected) => empty_list(pointer, expected, skipped.len()),
            Assertion::Contains(expected) => {
                validate_fields(expected, RowKind::Skipped)?;
                ensure!(
                    skipped
                        .iter()
                        .any(|update| expected.iter().all(|(field, value)| {
                            skipped_field(update, field, inventory) == *value
                        })),
                    "{pointer}: no skipped declaration matches {expected:?}"
                );
                Ok(())
            }
        },
        _ => {
            if let Some(raw_index) = pointer
                .strip_prefix("/planned/")
                .and_then(|path| path.strip_suffix("/resource_changes"))
            {
                let index: usize = raw_index.parse().context("invalid planned array index")?;
                ensure!(
                    raw_index == index.to_string(),
                    "invalid planned array index {raw_index:?}"
                );
                let update = planned
                    .get(index)
                    .with_context(|| format!("{pointer}: planned index out of bounds"))?;
                return check_resource_changes(
                    pointer,
                    assertion,
                    update.remote_resource_changes(),
                );
            }
            let actual = match pointer {
                "/summary/discovered_count" => inventory.declaration_count(),
                "/summary/checked_count" => outcome.plan().checked_count(),
                "/summary/unchecked_count" | "/summary/skipped_count" => skipped.len(),
                "/summary/planned_count" => planned.len(),
                "/summary/applied_count" => outcome.applied().len(),
                "/summary/changed_file_count" => outcome.changed_paths().len(),
                _ => bail!("unsupported outcome pointer {pointer:?}"),
            };
            let Assertion::Equals(expected) = assertion else {
                bail!("{pointer}: summary counts require equals");
            };
            ensure!(
                expected.as_u64().is_some(),
                "{pointer}: count must be a nonnegative integer"
            );
            ensure!(
                json!(actual) == *expected,
                "{pointer}: expected {expected}, got {actual}"
            );
            Ok(())
        }
    })
}

pub fn check_inventory(spec: &Value, inventory: &Inventory) -> Result<()> {
    ensure!(
        spec.get("command").and_then(Value::as_str) == Some("inventory"),
        "inventory assertions require command inventory"
    );
    let checks = checks(spec)?;
    ensure!(
        exit_code(checks)? == 0,
        "unsupported inventory exit_code; successful inventory requires 0"
    );
    let inventory = inventory.to_json_value();
    assertions(checks, |pointer, assertion| {
        let field = pointer
            .strip_prefix('/')
            .context("inventory pointer must begin with /")?;
        let actual = inventory
            .get(field)
            .with_context(|| format!("unsupported inventory pointer {pointer:?}"))?;
        match assertion {
            Assertion::Equals(expected) => {
                ensure!(
                    actual == expected,
                    "{pointer}: expected {expected}, got {actual}"
                );
            }
            Assertion::Contains(expected) => {
                let rows = actual
                    .as_array()
                    .context("contains requires an inventory declaration array")?;
                let fields = rows
                    .first()
                    .and_then(Value::as_object)
                    .with_context(|| format!("{pointer}: no inventory declarations to match"))?;
                for field in expected.keys() {
                    ensure!(
                        fields.contains_key(field),
                        "{pointer}: unsupported inventory field {field:?}"
                    );
                }
                ensure!(
                    rows.iter().any(|row| expected
                        .iter()
                        .all(|(field, value)| row.get(field) == Some(value))),
                    "{pointer}: no inventory declaration matches {expected:?}"
                );
            }
        }
        Ok(())
    })
}

fn checks(spec: &Value) -> Result<&Map<String, Value>> {
    let checks = spec
        .get("checks")
        .and_then(Value::as_object)
        .context("checks must be an object")?;
    for field in checks.keys() {
        ensure!(
            matches!(field.as_str(), "exit_code" | "json" | "invariants"),
            "unsupported checks field {field:?}"
        );
    }
    exit_code(checks)?;
    let json = checks
        .get("json")
        .and_then(Value::as_array)
        .context("checks.json must be an array")?;
    let invariants = checks
        .get("invariants")
        .map(|value| {
            value
                .as_array()
                .context("checks.invariants must be an array")
        })
        .transpose()?;
    ensure!(
        !json.is_empty() || invariants.is_some_and(|checks| !checks.is_empty()),
        "at least one JSON or invariant assertion is required"
    );
    Ok(checks)
}

fn exit_code(checks: &Map<String, Value>) -> Result<u64> {
    checks
        .get("exit_code")
        .and_then(Value::as_u64)
        .context("checks.exit_code must be a nonnegative integer")
}

enum Assertion<'a> {
    Equals(&'a Value),
    Contains(&'a Map<String, Value>),
}

fn assertions(
    checks: &Map<String, Value>,
    mut evaluate: impl FnMut(&str, Assertion<'_>) -> Result<()>,
) -> Result<()> {
    for group in ["json", "invariants"] {
        let Some(checks) = checks.get(group).and_then(Value::as_array) else {
            continue;
        };
        for (index, check) in checks.iter().enumerate() {
            let mut evaluate_check = || -> Result<()> {
                let check = check
                    .as_object()
                    .context("each assertion must be an object")?;
                for field in check.keys() {
                    ensure!(
                        matches!(field.as_str(), "at" | "equals" | "contains"),
                        "unsupported assertion field {field:?}"
                    );
                }
                let pointer = check
                    .get("at")
                    .and_then(Value::as_str)
                    .context("assertion at must be a string pointer")?;
                let assertion = match (check.get("equals"), check.get("contains")) {
                    (Some(expected), None) => Assertion::Equals(expected),
                    (None, Some(expected)) => {
                        let expected = expected
                            .as_object()
                            .filter(|value| !value.is_empty())
                            .context("contains must select a nonempty object")?;
                        Assertion::Contains(expected)
                    }
                    _ => bail!("choose exactly one equals or contains operator"),
                };
                evaluate(pointer, assertion)
            };
            evaluate_check().with_context(|| format!("checks.{group}[{index}]"))?;
        }
    }
    Ok(())
}

fn empty_list(pointer: &str, expected: &Value, actual_len: usize) -> Result<()> {
    let expected = expected
        .as_array()
        .context("list equals requires an array")?;
    ensure!(
        expected.is_empty(),
        "{pointer}: nonempty list equality is unsupported; use contains and summary counts"
    );
    ensure!(
        actual_len == 0,
        "{pointer}: expected an empty list, got {actual_len} declarations"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum RowKind {
    Planned,
    Skipped,
    ResourceChange,
}

fn validate_fields(expected: &Map<String, Value>, kind: RowKind) -> Result<()> {
    for (field, value) in expected {
        let valid = match (kind, field.as_str()) {
            (RowKind::Planned | RowKind::Skipped | RowKind::ResourceChange, "document_index") => {
                value.as_u64().is_some()
            }
            (RowKind::Planned, "sources") => {
                let sources = value.as_array().context("sources must be an array")?;
                for source in sources {
                    let source = source
                        .as_object()
                        .context("sources entries must be objects")?;
                    ensure!(
                        source.len() == 2
                            && source.get("path").is_some_and(Value::is_string)
                            && source
                                .get("document_index")
                                .and_then(Value::as_u64)
                                .is_some(),
                        "sources entries require exactly path and document_index"
                    );
                }
                true
            }
            (
                RowKind::Planned,
                "id" | "path" | "target_kind" | "target_name" | "yaml_path" | "current_version"
                | "latest_version" | "chart_name" | "repo_name" | "current_image" | "latest_image",
            )
            | (RowKind::Skipped, "id" | "path" | "reason" | "reason_code")
            | (RowKind::ResourceChange, "yaml_path" | "current_resource" | "latest_resource") => {
                value.is_string()
            }
            (RowKind::Skipped, "yaml_path" | "current_value" | "source_url") => {
                value.is_string() || value.is_null()
            }
            (RowKind::Skipped, "retryable") => value.is_boolean(),
            _ => bail!("unsupported declaration field {field:?}"),
        };
        ensure!(
            valid,
            "invalid value for declaration field {field:?}: {value}"
        );
    }
    Ok(())
}

fn check_resource_changes(
    pointer: &str,
    assertion: Assertion<'_>,
    changes: &[RemoteResourceChange],
) -> Result<()> {
    match assertion {
        Assertion::Equals(expected) => empty_list(pointer, expected, changes.len()),
        Assertion::Contains(expected) => {
            validate_fields(expected, RowKind::ResourceChange)?;
            ensure!(
                changes
                    .iter()
                    .any(|change| expected.iter().all(|(field, value)| {
                        let actual = match field.as_str() {
                            "document_index" => json!(change.document_index),
                            "yaml_path" => json!(change.yaml_path),
                            "current_resource" => json!(change.current_resource),
                            "latest_resource" => json!(change.latest_resource),
                            _ => {
                                unreachable!("resource change fields are validated before matching")
                            }
                        };
                        actual == *value
                    })),
                "{pointer}: no resource change matches {expected:?}"
            );
            Ok(())
        }
    }
}

fn planned_field(update: &UpdateReview<'_>, field: &str, inventory: &Inventory) -> Option<Value> {
    Some(match field {
        "id" => json!(update.identity().as_str()),
        "path" => json!(inventory.relative(update.path())),
        "document_index" => json!(update.document_index()),
        "target_kind" => json!(update.kind().as_str()),
        "target_name" => json!(update.target_name()),
        "yaml_path" => json!(update.yaml_path()),
        "current_version" => json!(update.current_version()),
        "latest_version" => json!(update.latest_version()),
        "chart_name" => json!(update.chart_name()?),
        "repo_name" => json!(update.repo_name()?),
        "current_image" => json!(update.current_image()?),
        "latest_image" => json!(update.latest_image()?),
        "sources" => json!(update.source_locations().iter().map(|(path, index)| json!({"path": inventory.relative(path), "document_index": index})).collect::<Vec<_>>()),
        _ => unreachable!("planned fields are validated before matching"),
    })
}

fn skipped_field(update: &SkippedUpdate, field: &str, inventory: &Inventory) -> Value {
    match field {
        "id" => json!(update.selection_id(&inventory.repo_root)),
        "path" => json!(
            update
                .path
                .as_ref()
                .map(|path| inventory.relative(path))
                .unwrap_or_default()
        ),
        "document_index" => json!(update.document_index()),
        "yaml_path" => json!(update.yaml_path()),
        "current_value" => json!(update.current_value()),
        "reason" => json!(update.reason),
        "reason_code" => json!(update.reason_code.as_str()),
        "retryable" => json!(update.retryable),
        "source_url" => json!(update.source_url),
        _ => unreachable!("skipped fields are validated before matching"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    use fluxrepo_update::github::RemoteResourceVersionResolver;
    use fluxrepo_update::resolvers::{
        StaticImageVersionResolver, StaticVersionResolver, parse_image_reference,
    };
    use fluxrepo_update::scanner::scan_repo;
    use fluxrepo_update::update_run::{UpdateRun, UpdateRunMode};
    use serde_json::json;

    use super::*;

    fn copy_repository(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_repository(&entry.path(), &destination);
            } else {
                let text = fs::read_to_string(entry.path()).unwrap();
                fs::write(destination, expand(&text)).unwrap();
            }
        }
    }

    fn expand(text: &str) -> String {
        text.replace("{{server}}", "https://charts.example")
            .replace("{{registry}}", "registry.example")
    }

    struct StaticRemoteResolver<'a>(&'a str);

    impl RemoteResourceVersionResolver for StaticRemoteResolver<'_> {
        fn resolve(
            &self,
            _owner: &str,
            _repository: &str,
            _current_version: &str,
            _required_assets: &[String],
        ) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    fn case_run(
        id: &str,
        mode: UpdateRunMode,
        latest: &str,
    ) -> (tempfile::TempDir, Value, Inventory, UpdateRunOutcome) {
        let case = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("coverage/kubeflux/cases")
            .join(id);
        let temp = tempfile::tempdir().unwrap();
        copy_repository(&case.join("repo"), temp.path());
        let spec = serde_json::from_str(&expand(
            &fs::read_to_string(case.join("case.json")).unwrap(),
        ))
        .unwrap();
        let inventory = scan_repo(temp.path()).unwrap();
        let charts = StaticVersionResolver::new(
            inventory
                .chart_targets
                .iter()
                .map(|target| {
                    (
                        (
                            target.repo_name.clone().unwrap(),
                            target.chart_name.clone().unwrap(),
                        ),
                        latest.to_string(),
                    )
                })
                .collect::<HashMap<_, _>>(),
        );
        let images = StaticImageVersionResolver::new(
            inventory
                .image_bindings
                .iter()
                .map(|binding| {
                    (
                        binding.image.clone(),
                        parse_image_reference(&binding.image)
                            .unwrap()
                            .with_tag(latest),
                    )
                })
                .collect(),
        );
        let remote = StaticRemoteResolver(latest);
        let outcome = UpdateRun::new(&charts, &images)
            .with_remote_resource_resolver(&remote)
            .execute(&inventory, mode, &mut |_| unreachable!(), None)
            .unwrap();
        (temp, spec, inventory, outcome)
    }

    fn assertion(exit_code: u64, check: Value) -> Value {
        json!({"command": "apply", "checks": {"exit_code": exit_code, "json": [check]}})
    }

    fn rejects(spec: &Value, inventory: &Inventory, outcome: &UpdateRunOutcome, message: &str) {
        let error = check_outcome(spec, inventory, outcome).expect_err(message);
        assert!(
            format!("{error:#}").contains(message),
            "expected diagnostic containing {message:?}, got {error:#}"
        );
    }

    #[test]
    fn accepts_independent_corpus_expectations_for_apply_plan_and_no_updates() {
        for (id, latest) in [
            ("chart-source-equivalence", "2.0.0"),
            ("version-variant", "1.29.0-alpine3.21"),
            ("image-tag-only-digest", "2.0.0"),
            ("report-fully-current", "1.0.0"),
        ] {
            let (_temp, spec, inventory, outcome) = case_run(id, UpdateRunMode::ApplyAll, latest);
            check_outcome(&spec, &inventory, &outcome).unwrap();
        }
        let (_temp, mut spec, inventory, outcome) =
            case_run("chart-source-equivalence", UpdateRunMode::PlanOnly, "2.0.0");
        spec["command"] = json!("plan");
        spec["checks"]["exit_code"] = json!(10);
        for check in spec["checks"]["json"].as_array_mut().unwrap() {
            if check["at"] == "/summary/applied_count" {
                check["equals"] = json!(0);
            }
        }
        check_outcome(&spec, &inventory, &outcome).unwrap();
    }

    #[test]
    fn accepts_remote_corpus_changes_and_counts_declarations_separately_from_groups() {
        for (id, latest) in [
            ("remote-git-ref-update", "v2.0.0"),
            ("remote-release-resource-update", "v2.0.0"),
            ("schemeless-kustomize-resource", "v0.16.0"),
        ] {
            let (_temp, spec, inventory, outcome) = case_run(id, UpdateRunMode::ApplyAll, latest);
            check_outcome(&spec, &inventory, &outcome).unwrap();
        }
    }

    #[test]
    fn remote_change_assertions_reject_wrong_fields_indices_and_group_counts() {
        let (_temp, spec, inventory, outcome) =
            case_run("remote-git-ref-update", UpdateRunMode::ApplyAll, "v2.0.0");
        check_outcome(&spec, &inventory, &outcome).unwrap();
        let pointer = "/planned/0/resource_changes";
        for (field, wrong) in [
            ("document_index", json!(1)),
            ("yaml_path", json!("resources[9]")),
            (
                "current_resource",
                json!("https://github.com/wrong/project?ref=v1.0.0"),
            ),
            (
                "latest_resource",
                json!("https://github.com/wrong/project?ref=v9.0.0"),
            ),
        ] {
            let spec = assertion(20, json!({"at": pointer, "contains": {field: wrong}}));
            rejects(&spec, &inventory, &outcome, pointer);
        }
        for field in ["planned_count", "applied_count"] {
            let pointer = format!("/summary/{field}");
            let spec = assertion(20, json!({"at": pointer, "equals": 3}));
            rejects(&spec, &inventory, &outcome, &pointer);
        }
        for check in [
            json!({"at": pointer, "equals": []}),
            json!({"at": pointer, "contains": {"unknown": "value"}}),
            json!({"at": pointer, "contains": {"document_index": "0"}}),
            json!({"at": pointer, "contains": {"latest_resource": null}}),
            json!({"at": "/planned/1/resource_changes", "contains": {"yaml_path": "resources[0]"}}),
            json!({"at": "/planned/01/resource_changes", "contains": {"yaml_path": "resources[0]"}}),
            json!({"at": "/planned/+0/resource_changes", "contains": {"yaml_path": "resources[0]"}}),
            json!({"at": "/planned/-1/resource_changes", "contains": {"yaml_path": "resources[0]"}}),
            json!({"at": "/planned/0/resource_change", "contains": {"yaml_path": "resources[0]"}}),
            json!({"at": "/planned/0/resource_changes/0", "contains": {"yaml_path": "resources[0]"}}),
        ] {
            assert!(
                check_outcome(&assertion(20, check.clone()), &inventory, &outcome).is_err(),
                "invalid remote assertion accepted: {check}"
            );
        }
    }

    #[test]
    fn remote_change_pointer_can_select_a_later_planned_group() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("kustomization.yaml"),
            "resources:\n- github.com/example/first/deploy?ref=v1.0.0\n- github.com/example/second/deploy?ref=v1.0.0\n").unwrap();
        let inventory = scan_repo(temp.path()).unwrap();
        let charts = StaticVersionResolver::new(HashMap::new());
        let images = StaticImageVersionResolver::new(HashMap::new());
        let remote = StaticRemoteResolver("v2.0.0");
        let outcome = UpdateRun::new(&charts, &images)
            .with_remote_resource_resolver(&remote)
            .execute(
                &inventory,
                UpdateRunMode::ApplyAll,
                &mut |_| unreachable!(),
                None,
            )
            .unwrap();
        let spec = assertion(
            20,
            json!({"at": "/planned/1/resource_changes", "contains": {
                "yaml_path": "resources[1]",
                "latest_resource": "github.com/example/second/deploy?ref=v2.0.0",
            }}),
        );
        check_outcome(&spec, &inventory, &outcome).unwrap();
    }

    #[test]
    fn rejects_incorrect_summary_counts_and_statuses() {
        let (_temp, spec, inventory, outcome) =
            case_run("chart-source-equivalence", UpdateRunMode::ApplyAll, "2.0.0");
        check_outcome(&spec, &inventory, &outcome).unwrap();
        for (field, wrong) in [
            ("discovered_count", 3),
            ("checked_count", 3),
            ("unchecked_count", 1),
            ("planned_count", 3),
            ("applied_count", 3),
            ("skipped_count", 1),
            ("changed_file_count", 3),
        ] {
            let pointer = format!("/summary/{field}");
            let spec = assertion(20, json!({"at": pointer, "equals": wrong}));
            rejects(&spec, &inventory, &outcome, &pointer);
        }
        for code in [0, 10] {
            let mut spec = spec.clone();
            spec["checks"]["exit_code"] = json!(code);
            rejects(&spec, &inventory, &outcome, "status");
        }
        let mut spec = spec;
        spec["checks"]["exit_code"] = json!(2);
        rejects(&spec, &inventory, &outcome, "unsupported exit_code");
    }

    #[test]
    fn discovery_is_cross_checked_against_inventory_even_without_a_count_assertion() {
        let (_temp, _spec, mut inventory, outcome) = case_run(
            "version-variant",
            UpdateRunMode::ApplyAll,
            "1.29.0-alpine3.21",
        );
        let spec = assertion(20, json!({"at": "/skipped", "equals": []}));
        check_outcome(&spec, &inventory, &outcome).unwrap();
        inventory
            .image_bindings
            .push(inventory.image_bindings[0].clone());
        rejects(&spec, &inventory, &outcome, "inventory declarations");
    }

    #[test]
    fn rejects_incorrect_planned_fields_and_nonempty_list_equality() {
        let (_temp, _spec, inventory, outcome) =
            case_run("chart-source-equivalence", UpdateRunMode::ApplyAll, "2.0.0");
        for (field, wrong) in [
            ("id", json!("wrong")),
            ("path", json!("wrong.yaml")),
            ("document_index", json!(1)),
            ("target_kind", json!("ImageBinding")),
            ("target_name", json!("wrong")),
            ("yaml_path", json!("spec.wrong")),
            ("current_version", json!("9.0.0")),
            ("latest_version", json!("9.0.0")),
            ("chart_name", json!("wrong")),
            ("repo_name", json!("wrong")),
        ] {
            let spec = assertion(20, json!({"at": "/planned", "contains": {field: wrong}}));
            rejects(&spec, &inventory, &outcome, "/planned");
        }
        let (_temp, _spec, inventory, outcome) = case_run(
            "version-variant",
            UpdateRunMode::ApplyAll,
            "1.29.0-alpine3.21",
        );
        for field in ["current_image", "latest_image"] {
            let spec = assertion(
                20,
                json!({"at": "/planned", "contains": {field: "wrong:9"}}),
            );
            rejects(&spec, &inventory, &outcome, "/planned");
        }
        let spec = assertion(20, json!({"at": "/planned", "equals": []}));
        rejects(&spec, &inventory, &outcome, "/planned");
        let spec = assertion(
            20,
            json!({"at": "/planned", "equals": [{"path": "pod.yaml"}]}),
        );
        rejects(&spec, &inventory, &outcome, "nonempty list equality");
    }

    #[test]
    fn sources_are_exact_ordered_arrays_even_with_partial_object_contains() {
        let (_temp, _spec, inventory, outcome) =
            case_run("chart-source-equivalence", UpdateRunMode::ApplyAll, "2.0.0");
        let sources = json!([
            {"path": "sources/a.yaml", "document_index": 0},
            {"path": "sources/b.yaml", "document_index": 0},
        ]);
        let spec = assertion(
            20,
            json!({"at": "/planned", "contains": {"sources": sources}}),
        );
        check_outcome(&spec, &inventory, &outcome).unwrap();
        for sources in [
            json!([{ "path": "sources/a.yaml", "document_index": 0 }]),
            json!([
                {"path": "sources/b.yaml", "document_index": 0},
                {"path": "sources/a.yaml", "document_index": 0},
            ]),
            json!([
                {"path": "sources/a.yaml", "document_index": 1},
                {"path": "sources/b.yaml", "document_index": 0},
            ]),
            json!([
                {"path": "sources/a.yaml"},
                {"path": "sources/b.yaml", "document_index": 0},
            ]),
        ] {
            let spec = assertion(
                20,
                json!({"at": "/planned", "contains": {"sources": sources}}),
            );
            rejects(&spec, &inventory, &outcome, "sources");
        }
    }

    #[test]
    fn rejects_incorrect_skip_fields_and_checks_invariants() {
        let (_temp, spec, inventory, outcome) =
            case_run("image-tag-only-digest", UpdateRunMode::ApplyAll, "2.0.0");
        check_outcome(&spec, &inventory, &outcome).unwrap();
        for (field, wrong) in [
            ("path", json!("wrong.yaml")),
            ("document_index", json!(1)),
            ("yaml_path", json!("spec.wrong")),
            ("current_value", json!("wrong:latest")),
            ("reason_code", json!("chart_not_found")),
            ("retryable", json!(true)),
        ] {
            let spec = assertion(0, json!({"at": "/skipped", "contains": {field: wrong}}));
            rejects(&spec, &inventory, &outcome, "/skipped");
        }
        let spec = assertion(0, json!({"at": "/skipped", "equals": []}));
        rejects(&spec, &inventory, &outcome, "/skipped");
        let mut spec = spec;
        spec["checks"]["json"] = json!([]);
        spec["checks"]["invariants"] = json!([
            {"at": "/skipped", "contains": {"retryable": false}},
        ]);
        check_outcome(&spec, &inventory, &outcome).unwrap();
        spec["checks"]["invariants"][0]["contains"]["retryable"] = json!(true);
        rejects(&spec, &inventory, &outcome, "invariants[0]");
    }

    #[test]
    fn inventory_checks_use_the_public_model_and_reject_incorrect_expectations() {
        let (_temp, _spec, inventory, _outcome) =
            case_run("chart-source-equivalence", UpdateRunMode::PlanOnly, "2.0.0");
        let mut spec = json!({"command": "inventory", "checks": {"exit_code": 0, "json": [
            {"at": "/discovered_count", "equals": 2},
            {"at": "/repository_count", "equals": 2},
            {"at": "/chart_targets", "contains": {"path": "apps/a.yaml", "current_version": "1.0.0"}},
        ]}});
        check_inventory(&spec, &inventory).unwrap();
        spec["checks"]["json"][0]["equals"] = json!(3);
        assert!(check_inventory(&spec, &inventory).is_err());
        spec["checks"]["json"][0]["equals"] = json!(2);
        spec["checks"]["json"][2]["contains"]["current_version"] = json!("9.0.0");
        assert!(check_inventory(&spec, &inventory).is_err());
        spec["checks"]["json"][2]["contains"] = json!({"unknown": false});
        assert!(check_inventory(&spec, &inventory).is_err());
    }

    #[test]
    fn rejects_unknown_pointers_fields_and_malformed_checks_without_skipping_them() {
        let (_temp, _spec, inventory, outcome) = case_run(
            "version-variant",
            UpdateRunMode::ApplyAll,
            "1.29.0-alpine3.21",
        );
        for check in [
            json!({"at": "/summary/missing", "equals": 0}),
            json!({"at": "/mode", "equals": "apply"}),
            json!({"at": "/planned", "contains": {"latest_versoin": "2.0.0"}}),
            json!({"at": "/skipped", "contains": {"unknown": false}}),
            json!({"at": "/planned", "contains": {}}),
            json!({"at": "/planned", "contains": []}),
            json!({"at": "/planned", "equals": [], "contains": {"path": "pod.yaml"}}),
            json!({"at": "/planned", "equals": [], "unknown": true}),
            json!({"at": "/planned"}),
            json!({"at": 1, "equals": []}),
            json!({"at": "/summary/checked_count", "contains": {"count": 1}}),
            json!({"at": "/summary/checked_count", "equals": true}),
            json!({"at": "/summary/checked_count", "equals": -1}),
            json!({"at": "/planned", "contains": {"document_index": "0"}}),
            json!({"at": "/skipped", "contains": {"retryable": "false"}}),
            json!({"at": "/planned", "contains": {"sources": {"path": "source.yaml"}}}),
        ] {
            let spec = assertion(20, check.clone());
            assert!(
                check_outcome(&spec, &inventory, &outcome).is_err(),
                "invalid assertion was accepted: {check}"
            );
        }
        for checks in [
            json!({"exit_code": 20, "json": []}),
            json!({"exit_code": 20, "json": {}, "invariants": []}),
            json!({"exit_code": 20, "json": [{"at": "/skipped", "equals": []}], "invariants": {}}),
            json!({"exit_code": "20", "json": [{"at": "/skipped", "equals": []}]}),
            json!({"exit_code": 20, "exit_code_not": 0, "json": [{"at": "/skipped", "equals": []}]}),
        ] {
            let spec = json!({"command": "apply", "checks": checks});
            assert!(check_outcome(&spec, &inventory, &outcome).is_err());
        }
    }
}
