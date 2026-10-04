//! Human and JSON reports, terminal styling, and resolution progress.

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use serde_json::json;

use crate::models::TargetKind;
use crate::update_run::{SkippedUpdate, UpdateReview, UpdateRunOutcome};

impl std::fmt::Display for SkippedUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(path) => write!(formatter, "{}: {}", path.display(), self.reason),
            None => formatter.write_str(&self.reason),
        }
    }
}

pub(super) struct OutputContext<'a> {
    repo_root: &'a Path,
    json_output: bool,
    mode: &'a str,
    non_interactive: bool,
    human_output: HumanOutput,
}

impl<'a> OutputContext<'a> {
    pub(super) fn new(
        repo_root: &'a Path,
        json_output: bool,
        mode: &'a str,
        non_interactive: bool,
        human_output: HumanOutput,
    ) -> Self {
        Self {
            repo_root,
            json_output,
            mode,
            non_interactive,
            human_output,
        }
    }
}

pub(super) fn emit_update_output<W: Write, E: Write>(
    outcome: &UpdateRunOutcome,
    context: OutputContext<'_>,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<()> {
    let planned = outcome
        .plan()
        .review()
        .into_iter()
        .filter(|item| context.mode == "plan" || outcome.applied().contains(item.identity()))
        .collect::<Vec<_>>();
    let skipped = outcome.plan().skipped();
    let applied_count = outcome.applied().len();
    let changed_file_count = outcome.changed_paths().len();
    if context.json_output {
        let report = update_output_json(
            &planned,
            skipped,
            &context,
            applied_count,
            changed_file_count,
            outcome.plan().checked_count(),
        );
        writeln!(stdout, "{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(());
    }
    writeln!(
        stderr,
        "Coverage (repository manifests): {} discovered, {} checked, {} unchecked.",
        outcome.plan().checked_count() + skipped.len(),
        outcome.plan().checked_count(),
        skipped.len()
    )?;
    for item in skipped {
        write!(
            stderr,
            "skip: {}",
            item.path
                .as_ref()
                .map(|path| relative_path(path, context.repo_root))
                .unwrap_or_default()
        )?;
        if let Some(index) = item.document_index() {
            write!(stderr, " (document {})", index + 1)?;
        }
        if let Some(field) = item.yaml_path() {
            write!(stderr, " {field}")?;
        }
        if let Some(value) = item.current_value() {
            write!(stderr, " = {value}")?;
        }
        writeln!(stderr, ": {}", item.reason)?;
    }
    if planned.is_empty() {
        if context.mode == "apply" {
            writeln!(stderr, "No updates were approved.")?;
        } else if !skipped.is_empty() {
            let count = skipped.len();
            let targets = if count == 1 { "target" } else { "targets" };
            writeln!(stderr, "No updates planned; {count} {targets} skipped.")?;
        } else {
            writeln!(stderr, "No updates required.")?;
        }
        return Ok(());
    }
    for update in &planned {
        writeln!(
            stderr,
            "{}",
            render_update_line(update, context.repo_root, context.human_output)
        )?;
    }
    if context.mode == "plan" {
        writeln!(
            stderr,
            "Plan only. Re-run without --non-interactive for prompts, add --non-interactive --write to apply all changes, or pass --apply-id with --write to apply selected updates."
        )?;
    } else {
        writeln!(
            stderr,
            "Updated {applied_count} targets across {changed_file_count} files."
        )?;
    }
    Ok(())
}

fn update_output_json(
    planned: &[UpdateReview<'_>],
    skipped: &[SkippedUpdate],
    context: &OutputContext<'_>,
    applied_count: usize,
    changed_file_count: usize,
    checked_count: usize,
) -> serde_json::Value {
    json!({
        "mode": context.mode, "non_interactive": context.non_interactive,
        "scope": "repository_manifests",
        "summary": { "planned_count": planned.len(), "applied_count": applied_count,
            "skipped_count": skipped.len(), "changed_file_count": changed_file_count,
            "discovered_count": checked_count + skipped.len(),
            "checked_count": checked_count, "unchecked_count": skipped.len() },
        "planned": planned.iter().map(|item| update_json(item, context.repo_root)).collect::<Vec<_>>(),
        "skipped": skipped.iter().map(|item| skip_json(item, context.repo_root)).collect::<Vec<_>>(),
    })
}

pub(crate) fn skip_json(skipped: &SkippedUpdate, repo_root: &Path) -> serde_json::Value {
    let mut value = json!({
        "id": skipped.selection_id(repo_root),
        "path": skipped.path.as_ref().map(|path| relative_path(path, repo_root)).unwrap_or_default(),
        "yaml_path": skipped.yaml_path(), "reason": skipped.reason,
        "reason_code": skipped.reason_code.as_str(), "retryable": skipped.retryable,
        "source_url": skipped.source_url,
    });
    if let Some(current) = skipped.current_value() {
        value["current_value"] = json!(current);
    }
    if let Some(index) = skipped.document_index() {
        value["document_index"] = json!(index);
    }
    value
}

#[cfg(test)]
pub(crate) fn plan_payload(
    report: &crate::update_run::implementation::UpdateReport,
) -> serde_json::Value {
    let root = Path::new("/repo");
    let plan = crate::update_run::UpdatePlan::from_report(report.clone(), root);
    update_output_json(
        &plan.review(),
        plan.skipped(),
        &OutputContext::new(root, true, "plan", true, HumanOutput::plain()),
        0,
        0,
        plan.checked_count(),
    )
}

fn update_json(update: &UpdateReview<'_>, repo_root: &Path) -> serde_json::Value {
    let mut value = json!({
        "id": update.identity().as_str(), "path": relative_path(update.path(), repo_root),
        "document_index": update.document_index(), "target_kind": update.kind().as_str(),
        "target_name": update.target_name(), "yaml_path": update.yaml_path(),
        "current_version": update.current_version(), "latest_version": update.latest_version(),
        "inherited_source": false,
    });
    match update.kind() {
        TargetKind::HelmRelease => {
            value["chart_name"] = json!(update.chart_name());
            value["repo_name"] = json!(update.repo_name());
            value["sources"] = json!(
                update
                    .source_locations()
                    .iter()
                    .map(|(path, index)| json!({
                        "path": relative_path(path, repo_root), "document_index": index,
                    }))
                    .collect::<Vec<_>>()
            );
        }
        TargetKind::ImageBinding => {
            value["current_image"] = json!(update.current_image());
            value["latest_image"] = json!(update.latest_image());
        }
        TargetKind::RemoteResource => {
            value["resource_changes"] = json!(update.remote_resource_changes().iter().map(|change| json!({
                "document_index": change.document_index, "yaml_path": change.yaml_path,
                "current_resource": change.current_resource, "latest_resource": change.latest_resource,
            })).collect::<Vec<_>>());
        }
    }
    value
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HumanOutput {
    pub(super) color: bool,
    pub(super) progress: bool,
}

impl HumanOutput {
    pub(super) fn plain() -> Self {
        Self {
            color: false,
            progress: false,
        }
    }

    pub(super) fn styled(self, text: &str, style: AnsiStyle) -> String {
        if !self.color {
            return text.to_string();
        }
        format!("\x1b[{}m{text}\x1b[0m", style.code())
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum AnsiStyle {
    Cyan,
    Yellow,
    Green,
}

impl AnsiStyle {
    fn code(self) -> &'static str {
        match self {
            Self::Cyan => "36",
            Self::Yellow => "33",
            Self::Green => "32",
        }
    }
}

fn render_update_line(
    update: &UpdateReview<'_>,
    repo_root: &Path,
    human_output: HumanOutput,
) -> String {
    let path = human_output.styled(&relative_path(update.path(), repo_root), AnsiStyle::Cyan);
    let current = human_output.styled(update.current_version(), AnsiStyle::Yellow);
    let latest = human_output.styled(update.latest_version(), AnsiStyle::Green);
    match update.kind() {
        TargetKind::HelmRelease => format!(
            "{path}: HelmRelease {} {} {current} -> {latest}",
            update.target_name(),
            update.chart_name().unwrap_or_default()
        ),
        TargetKind::ImageBinding => format!(
            "{path}: ImageBinding {} {} {current} -> {latest}",
            update.target_name(),
            update.yaml_path()
        ),
        TargetKind::RemoteResource => format!(
            "{path}: RemoteResource {} ({} references) {current} -> {latest}",
            update.target_name(),
            update.remote_resource_changes().len()
        ),
    }
}

pub(super) fn relative_path(path: &Path, repo_root: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

pub(super) struct ProgressRenderer<'a, E: Write> {
    pub(super) stderr: &'a mut E,
    repo_root: &'a Path,
    human_output: HumanOutput,
    started_at: Instant,
    last_width: usize,
}

impl<'a, E: Write> ProgressRenderer<'a, E> {
    pub(super) fn new(stderr: &'a mut E, repo_root: &'a Path, human_output: HumanOutput) -> Self {
        Self {
            stderr,
            repo_root,
            human_output,
            started_at: Instant::now(),
            last_width: 0,
        }
    }

    pub(super) fn render(&mut self, completed: usize, total: usize, path: &Path) {
        let spinner = ["|", "/", "-", "\\"][completed % 4];
        let bar = render_progress_bar(completed, total, 24);
        let elapsed = self.started_at.elapsed().as_secs();
        let path = ellipsize(&relative_path(path, self.repo_root), 48);
        let path = self.human_output.styled(&path, AnsiStyle::Cyan);
        let line = format!("{spinner} Resolving {completed}/{total} [{bar}] {elapsed}s {path}");
        let padding = self.last_width.saturating_sub(line.len());
        let _ = write!(self.stderr, "\r{line}{}", " ".repeat(padding));
        let _ = self.stderr.flush();
        self.last_width = line.len();
    }

    pub(super) fn finish(&mut self) {
        if self.last_width == 0 {
            return;
        }
        let _ = write!(self.stderr, "\r{}\r", " ".repeat(self.last_width));
        let _ = self.stderr.flush();
        self.last_width = 0;
    }
}

fn render_progress_bar(completed: usize, total: usize, width: usize) -> String {
    if total == 0 {
        return "-".repeat(width);
    }
    let filled = width
        .saturating_mul(completed)
        .checked_div(total)
        .unwrap_or(0);
    format!("{}{}", "=".repeat(filled), "-".repeat(width - filled))
}

fn ellipsize(text: &str, max_width: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let head_len = (max_width - 3) / 2;
    let tail_len = max_width - 3 - head_len;
    let head = chars.iter().take(head_len).collect::<String>();
    let tail = chars
        .iter()
        .skip(chars.len() - tail_len)
        .collect::<String>();
    format!("{head}...{tail}")
}
