mod assertions;
mod server;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use fluxrepo_update::resolvers::{RegistryImageResolver, RepositoryChartResolver};
use fluxrepo_update::scanner::scan_repo;
use fluxrepo_update::update_run::{UpdateRun, UpdateRunMode};
use serde_json::Value;

use server::FixtureServer;

type Files = BTreeMap<PathBuf, Vec<u8>>;

fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("coverage/kubeflux/cases")
}

pub fn registered_cases_match_passing_corpus(registered: &[&str]) -> Result<()> {
    let mut passing = BTreeSet::new();
    for entry in fs::read_dir(cases_root())? {
        let path = entry?.path().join("case.json");
        let spec: Value = serde_json::from_slice(&fs::read(&path)?)?;
        match spec["expectation"].as_str() {
            Some("pass") => {
                let id = spec["id"].as_str().context("case needs an id")?;
                ensure!(
                    path.parent().and_then(Path::file_name) == Some(id.as_ref()),
                    "case id must match its directory: {}",
                    path.display()
                );
                ensure!(passing.insert(id.to_owned()), "duplicate case id: {id}");
            }
            Some("todo") => {}
            _ => bail!("invalid case expectation: {}", path.display()),
        }
    }
    let registered_set = registered
        .iter()
        .map(|id| id.replace('_', "-"))
        .collect::<BTreeSet<_>>();
    ensure!(
        registered_set.len() == registered.len(),
        "duplicate registered case"
    );
    ensure!(!passing.is_empty(), "passing corpus must not be empty");
    ensure!(
        passing == registered_set,
        "register every passing case in tests/rust_corpus.rs; missing: {:?}; no longer passing: {:?}",
        passing.difference(&registered_set).collect::<Vec<_>>(),
        registered_set.difference(&passing).collect::<Vec<_>>()
    );
    Ok(())
}

pub fn run(id: &str) -> Result<()> {
    run_case(&cases_root().join(id)).with_context(|| format!("corpus case {id}"))
}

fn run_case(case_dir: &Path) -> Result<()> {
    let spec: Value = serde_json::from_slice(&fs::read(case_dir.join("case.json"))?)?;
    ensure!(
        spec["expectation"] == "pass",
        "only passing cases belong in the normal suite"
    );
    let id = spec["id"].as_str().context("case needs an id")?;
    ensure!(
        case_dir.file_name() == Some(id.as_ref()),
        "case id must match its directory"
    );
    ensure!(
        spec.get("known_failure").is_none() && !case_dir.join("known_actual").exists(),
        "passing cases cannot have known failure baselines"
    );
    let responses = spec
        .get("responses")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let server = FixtureServer::new(&responses)?;
    let spec: Value = serde_json::from_str(&server.expand(&spec.to_string()))?;
    let original = expanded_files(&case_dir.join("repo"), &server)?;
    let mut expected = original.clone();
    let overlay = case_dir.join("expected");
    if overlay.exists() {
        for (path, content) in expanded_files(&overlay, &server)? {
            ensure!(
                original.contains_key(&path),
                "expected overlay adds unknown file: {}",
                path.display()
            );
            expected.insert(path, content);
        }
    }
    let temporary = tempfile::tempdir()?;
    let repo = temporary.path().join("repo");
    fs::create_dir(&repo)?;
    for (path, content) in &original {
        let target = repo.join(path);
        fs::create_dir_all(target.parent().context("file needs parent")?)?;
        fs::write(target, content)?;
    }

    let inventory = scan_repo(&repo)?;
    let command = spec["command"].as_str().context("case needs a command")?;
    if command == "inventory" {
        server.finish()?;
        assertions::check_inventory(&spec, &inventory)?;
    } else {
        let mode = match command {
            "plan" => UpdateRunMode::PlanOnly,
            "apply" => UpdateRunMode::ApplyAll,
            _ => bail!("unsupported corpus command: {command}"),
        };
        let client = server.client()?;
        let charts = RepositoryChartResolver::builder()
            .client(client.clone())
            .build();
        let images = RegistryImageResolver::builder().client(client).build();
        let outcome = UpdateRun::new(&charts, &images).execute(
            &inventory,
            mode,
            &mut |_| bail!("non-interactive corpus case requested approval"),
            None,
        );
        // Check fixture errors even when a resolver converts an unexpected request to a skip.
        server.finish()?;
        assertions::check_outcome(&spec, &inventory, &outcome?)?;
    }
    compare_files(&expected, &read_files(&repo)?)
}

fn expanded_files(root: &Path, server: &FixtureServer) -> Result<Files> {
    read_files(root)?
        .into_iter()
        .map(|(path, content)| {
            let text = String::from_utf8(content)
                .with_context(|| format!("fixture is not UTF-8: {}", path.display()))?;
            Ok((path, server.expand(&text).into_bytes()))
        })
        .collect()
}

fn read_files(root: &Path) -> Result<Files> {
    fn visit(root: &Path, directory: &Path, files: &mut Files) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(root, &path, files)?;
            } else {
                ensure!(
                    kind.is_file(),
                    "fixture must contain regular files: {}",
                    path.display()
                );
                files.insert(path.strip_prefix(root)?.to_owned(), fs::read(&path)?);
            }
        }
        Ok(())
    }
    let mut files = Files::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn compare_files(expected: &Files, actual: &Files) -> Result<()> {
    let paths = expected
        .keys()
        .chain(actual.keys())
        .collect::<BTreeSet<_>>();
    let differences = paths
        .into_iter()
        .filter(|path| expected.get(*path) != actual.get(*path))
        .collect::<Vec<_>>();
    ensure!(
        differences.is_empty(),
        "repository files differ from expected bytes: {differences:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_comparison_rejects_modified_missing_and_added_files() {
        let expected = Files::from([(PathBuf::from("pod.yaml"), b"original\r\n".to_vec())]);
        compare_files(&expected, &expected).unwrap();
        for actual in [
            Files::from([(PathBuf::from("pod.yaml"), b"original\n".to_vec())]),
            Files::new(),
            Files::from([
                (PathBuf::from("pod.yaml"), b"original\r\n".to_vec()),
                (PathBuf::from("unexpected.yaml"), Vec::new()),
            ]),
        ] {
            assert!(compare_files(&expected, &actual).is_err());
        }
    }

    #[test]
    fn an_incorrect_expected_file_fails_the_case() {
        let temporary = tempfile::tempdir().unwrap();
        let case_dir = temporary.path().join("charts-http");
        for (path, content) in read_files(&cases_root().join("charts-http")).unwrap() {
            let target = case_dir.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, content).unwrap();
        }
        fs::write(
            case_dir.join("expected/release.yaml"),
            "incorrect expected bytes\n",
        )
        .unwrap();
        let error = run_case(&case_dir).expect_err("incorrect expected files must fail");
        assert!(
            error.to_string().contains("repository files differ"),
            "{error:#}"
        );
    }

    #[test]
    fn registration_rejects_missing_passing_cases() {
        assert!(registered_cases_match_passing_corpus(&[]).is_err());
    }
}
