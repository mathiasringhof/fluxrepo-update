use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::models::TargetKind;
use crate::resolvers::{
    ChartVersionResolver, ImageVersionResolver, RegistryImageResolver, RepositoryChartResolver,
};
use crate::scanner::scan_repo;
use crate::update_run::{
    ResolutionProgress, SkippedUpdate, UpdateReview, UpdateRun, UpdateRunMode, UpdateRunOutcome,
    UpdateRunStatus, UpdateSelectionIdentity,
};

pub const EXIT_OK: u8 = 0;
pub const EXIT_STRICT_FAILURE: u8 = 2;
pub const EXIT_UPDATES_AVAILABLE: u8 = 10;
pub const EXIT_UPDATES_APPLIED: u8 = 20;

impl std::fmt::Display for SkippedUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(path) => write!(formatter, "{}: {}", path.display(), self.reason),
            None => formatter.write_str(&self.reason),
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "fluxrepo-update")]
#[command(version)]
#[command(about = "Inspect and update FluxCD manifest versions")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Scan a Flux repository and list update targets")]
    Inventory {
        #[arg(value_name = "REPO_ROOT", help = "Flux repository root")]
        repo_root: PathBuf,
        #[arg(long = "json", help = "Emit the full inventory as JSON")]
        json_output: bool,
    },
    #[command(name = "update-helm")]
    #[command(about = "Plan or apply HelmRelease and image binding updates")]
    UpdateHelm {
        #[arg(value_name = "REPO_ROOT", help = "Flux repository root")]
        repo_root: PathBuf,
        #[arg(long = "json", help = "Emit the update report as JSON")]
        json_output: bool,
        #[arg(long, help = "Apply all planned updates; requires --non-interactive")]
        write: bool,
        #[arg(long = "non-interactive", help = "Disable prompts")]
        non_interactive: bool,
        #[arg(long = "apply-id", value_name = "ID")]
        apply_ids: Vec<String>,
    },
}

impl Commands {
    fn json_output(&self) -> bool {
        match self {
            Self::Inventory { json_output, .. } | Self::UpdateHelm { json_output, .. } => {
                *json_output
            }
        }
    }
}

pub fn run() -> Result<u8> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let terminal_stderr = std::io::IsTerminal::is_terminal(&stderr);
    run_with_args_and_output(
        std::env::args_os(),
        interactive_approval::InteractiveApproval::classified(stdin.lock()),
        &mut stdout,
        &mut stderr,
        &RepositoryChartResolver::default(),
        &RegistryImageResolver::default(),
        HumanOutput {
            color: terminal_stderr,
            progress: terminal_stderr,
        },
    )
}

pub fn run_with_args<I, T, R, W, E>(
    args: I,
    input: R,
    stdout: &mut W,
    stderr: &mut E,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
) -> Result<u8>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    R: Read,
    W: Write,
    E: Write + Send,
{
    run_with_args_and_output(
        args,
        interactive_approval::InteractiveApproval::plain(input),
        stdout,
        stderr,
        chart_resolver,
        image_resolver,
        HumanOutput::plain(),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_with_args_and_output<I, T, W, E>(
    args: I,
    approval: interactive_approval::InteractiveApproval<'_>,
    stdout: &mut W,
    stderr: &mut E,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
    human_output: HumanOutput,
) -> Result<u8>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    W: Write,
    E: Write + Send,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                write!(stderr, "{error}")?;
            } else {
                write!(stdout, "{error}")?;
            }
            return Ok(u8::try_from(error.exit_code()).unwrap_or(EXIT_STRICT_FAILURE));
        }
    };

    let json_output = cli.command.json_output();
    let human_output = if json_output {
        HumanOutput::plain()
    } else {
        human_output
    };
    let exit_code = match cli.command {
        Commands::Inventory {
            repo_root,
            json_output,
        } => inventory_command(repo_root, json_output, stdout),
        Commands::UpdateHelm {
            repo_root,
            json_output,
            write,
            non_interactive,
            apply_ids,
        } => update_helm_command(
            repo_root,
            json_output,
            write,
            non_interactive,
            apply_ids,
            approval,
            stdout,
            stderr,
            chart_resolver,
            image_resolver,
            human_output,
        ),
    };

    match exit_code {
        Ok(code) => Ok(code),
        Err(error) if json_output => {
            emit_json_error(stderr, &error, EXIT_STRICT_FAILURE)?;
            Ok(EXIT_STRICT_FAILURE)
        }
        Err(error) => Err(error),
    }
}

fn emit_json_error<E: Write>(stderr: &mut E, error: &anyhow::Error, exit_code: u8) -> Result<()> {
    emit_json_error_message(stderr, "runtime_error", &format!("{error:#}"), exit_code)
}

fn emit_json_error_message<E: Write>(
    stderr: &mut E,
    error: &str,
    message: &str,
    exit_code: u8,
) -> Result<()> {
    writeln!(
        stderr,
        "{}",
        serde_json::to_string_pretty(&json!({
            "error": error,
            "message": message,
            "exit_code": exit_code,
        }))?
    )?;
    Ok(())
}

fn inventory_command<W: Write>(
    repo_root: PathBuf,
    json_output: bool,
    stdout: &mut W,
) -> Result<u8> {
    let inventory = scan_repo(&repo_root)?;
    if json_output {
        writeln!(
            stdout,
            "{}",
            serde_json::to_string_pretty(&inventory.to_json_value())?
        )?;
    } else {
        writeln!(stdout, "Repositories: {}", inventory.repository_count())?;
        writeln!(
            stdout,
            "Scope: repository manifests (deployment state unknown)"
        )?;
        writeln!(
            stdout,
            "Version declarations: {}",
            inventory.declaration_count()
        )?;
        writeln!(
            stdout,
            "Unsupported declarations: {}",
            inventory.unchecked_version_declarations.len()
        )?;
        writeln!(stdout, "Chart targets: {}", inventory.chart_targets.len())?;
        writeln!(stdout, "Image bindings: {}", inventory.image_bindings.len())?;
        writeln!(
            stdout,
            "HelmReleases without chart version: {}",
            inventory.helmreleases_without_chart_version.len()
        )?;
        writeln!(
            stdout,
            "Unresolved chart targets: {}",
            inventory.unresolved_chart_targets.len()
        )?;
        writeln!(
            stdout,
            "Image references: {}",
            inventory.image_references.len()
        )?;
        writeln!(
            stdout,
            "Skipped generated files: {}",
            inventory.skipped_paths.len()
        )?;
    }
    Ok(EXIT_OK)
}

#[allow(clippy::too_many_arguments)]
fn update_helm_command<W, E>(
    repo_root: PathBuf,
    json_output: bool,
    write: bool,
    non_interactive: bool,
    apply_ids: Vec<String>,
    approval: interactive_approval::InteractiveApproval<'_>,
    stdout: &mut W,
    stderr: &mut E,
    chart_resolver: &(dyn ChartVersionResolver + Sync),
    image_resolver: &(dyn ImageVersionResolver + Sync),
    human_output: HumanOutput,
) -> Result<u8>
where
    W: Write,
    E: Write + Send,
{
    let repo_root = repo_root.canonicalize()?;
    if write && !non_interactive {
        let message = "Using --write requires --non-interactive. Interactive mode already writes approved changes.";
        if json_output {
            emit_json_error_message(stderr, "invalid_arguments", message, EXIT_STRICT_FAILURE)?;
        } else {
            writeln!(stderr, "{message}")?;
        }
        return Ok(EXIT_STRICT_FAILURE);
    }
    if !apply_ids.is_empty() && !write {
        let message = "--apply-id requires --write.";
        if json_output {
            emit_json_error_message(stderr, "invalid_arguments", message, EXIT_STRICT_FAILURE)?;
        } else {
            writeln!(stderr, "{message}")?;
        }
        return Ok(EXIT_STRICT_FAILURE);
    }

    if !json_output {
        writeln!(stderr, "Scanning {}...", repo_root.display())?;
    }
    let inventory = scan_repo(&repo_root)?;
    let target_count = inventory.declaration_count();
    if !json_output {
        writeln!(stderr, "Resolving updates for {target_count} targets...")?;
    }
    let mode = if !non_interactive {
        UpdateRunMode::ReviewAndApply
    } else if !write {
        UpdateRunMode::PlanOnly
    } else if apply_ids.is_empty() {
        UpdateRunMode::ApplyAll
    } else {
        UpdateRunMode::ApplySelected(apply_ids.into_iter().map(Into::into).collect())
    };
    let outcome = {
        let output = Mutex::new(ProgressRenderer::new(stderr, &repo_root, human_output));
        let mut approval = Some(approval);
        let mut review = |items: &[UpdateReview<'_>]| {
            let mut output = output.lock().expect("terminal output lock");
            output.finish();
            approval.take().expect("one review per run").review(
                items,
                output.stderr,
                &repo_root,
                human_output,
            )
        };
        let mut progress = |event: ResolutionProgress<'_>| {
            if let Ok(mut output) = output.lock() {
                output.render(event.completed, event.total, event.path);
            }
        };
        let result = UpdateRun::new(chart_resolver, image_resolver).execute(
            &inventory,
            mode,
            &mut review,
            if !json_output && human_output.progress && target_count > 0 {
                Some(&mut progress)
            } else {
                None
            },
        );
        if let Ok(mut output) = output.lock() {
            output.finish();
        }
        result?
    };
    if let Some(rejection) = outcome.rejection() {
        let ids = rejection
            .unknown_identities()
            .iter()
            .map(|id| id.as_str().to_string())
            .collect::<Vec<_>>();
        let message = format_unknown_apply_ids(&ids);
        if json_output {
            emit_json_error_message(stderr, "invalid_arguments", &message, EXIT_STRICT_FAILURE)?;
        } else {
            writeln!(stderr, "{message}")?;
        }
        return Ok(EXIT_STRICT_FAILURE);
    }
    let mode = match outcome.status() {
        UpdateRunStatus::UpdatesApplied | UpdateRunStatus::NoUpdatesApproved => "apply",
        _ => "plan",
    };
    emit_update_output(
        &outcome,
        OutputContext::new(&repo_root, json_output, mode, non_interactive, human_output),
        stdout,
        stderr,
    )?;
    Ok(match outcome.status() {
        UpdateRunStatus::UpdatesApplied => EXIT_UPDATES_APPLIED,
        UpdateRunStatus::UpdatesPlanned => EXIT_UPDATES_AVAILABLE,
        UpdateRunStatus::RunRejected => EXIT_STRICT_FAILURE,
        UpdateRunStatus::NoUpdates | UpdateRunStatus::NoUpdatesApproved => EXIT_OK,
    })
}

mod interactive_approval {
    use std::error::Error;
    use std::fmt;
    use std::io::{IsTerminal, Read, Write};
    use std::os::fd::AsFd;
    use std::path::Path;

    use anyhow::{Result, anyhow};
    use rustix::termios::{OptionalActions, Termios, tcgetattr, tcsetattr};

    use super::{
        AnsiStyle, HumanOutput, TargetKind, UpdateReview, UpdateSelectionIdentity, relative_path,
    };

    pub(super) struct InteractiveApproval<'a> {
        input: Box<dyn ApprovalInput + 'a>,
    }

    impl<'a> InteractiveApproval<'a> {
        pub(super) fn classified<R>(input: R) -> Self
        where
            R: Read + AsFd + IsTerminal + 'a,
        {
            let terminal = input.is_terminal();
            Self {
                input: Box::new(ClassifiedInput { input, terminal }),
            }
        }

        pub(super) fn plain<R: Read + 'a>(input: R) -> Self {
            Self {
                input: Box::new(PlainInput(input)),
            }
        }

        pub(super) fn review<E: Write>(
            mut self,
            updates: &[UpdateReview<'_>],
            output: &mut E,
            repo_root: &Path,
            human_output: HumanOutput,
        ) -> Result<Vec<UpdateSelectionIdentity>> {
            let original = self.input.enter_raw_mode()?;
            let review_result = self.review_updates(updates, output, repo_root, human_output);
            let restoration_result = original
                .as_ref()
                .map_or(Ok(()), |settings| self.input.restore(settings));
            match (review_result, restoration_result) {
                (Ok(report), Ok(())) => Ok(report),
                (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
                (Err(primary), Err(restoration)) => Err(ApprovalAndRestorationError {
                    primary,
                    restoration,
                }
                .into()),
            }
        }

        fn review_updates<E: Write>(
            &mut self,
            updates: &[UpdateReview<'_>],
            output: &mut E,
            repo_root: &Path,
            human_output: HumanOutput,
        ) -> Result<Vec<UpdateSelectionIdentity>> {
            let mut approved = Vec::new();
            for update in updates {
                write!(output, "{}", render_prompt(update, repo_root, human_output))?;
                output.flush()?;
                let mut buffer = [0; 1];
                let count = self.input.read(&mut buffer)?;
                if count != 0 && buffer[0] == 0x03 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "prompt interrupted",
                    )
                    .into());
                }
                let choice = count.checked_sub(1).map_or('\n', |_| char::from(buffer[0]));
                let displayed = if matches!(choice, 'y' | 'Y') {
                    'y'
                } else {
                    'n'
                };
                if self.input.is_terminal() {
                    write!(output, "{displayed}\r\n")?;
                } else {
                    writeln!(output, "{displayed}")?;
                }
                if matches!(choice, 'y' | 'Y') {
                    approved.push(update.identity().clone());
                }
            }
            Ok(approved)
        }
    }

    trait ApprovalInput: Read {
        fn is_terminal(&self) -> bool;
        fn enter_raw_mode(&self) -> Result<Option<Termios>>;
        fn restore(&self, settings: &Termios) -> Result<()>;
    }

    struct PlainInput<R>(R);

    impl<R: Read> Read for PlainInput<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl<R: Read> ApprovalInput for PlainInput<R> {
        fn is_terminal(&self) -> bool {
            false
        }

        fn enter_raw_mode(&self) -> Result<Option<Termios>> {
            Ok(None)
        }

        fn restore(&self, _settings: &Termios) -> Result<()> {
            Ok(())
        }
    }

    struct ClassifiedInput<R> {
        input: R,
        terminal: bool,
    }

    impl<R: Read> Read for ClassifiedInput<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl<R: Read + AsFd> ApprovalInput for ClassifiedInput<R> {
        fn is_terminal(&self) -> bool {
            self.terminal
        }

        fn enter_raw_mode(&self) -> Result<Option<Termios>> {
            if !self.terminal {
                return Ok(None);
            }
            let original = tcgetattr(&self.input)?;
            let mut raw = original.clone();
            raw.make_raw();
            tcsetattr(&self.input, OptionalActions::Now, &raw)?;
            Ok(Some(original))
        }

        fn restore(&self, settings: &Termios) -> Result<()> {
            tcsetattr(&self.input, OptionalActions::Now, settings).map_err(|error| anyhow!(error))
        }
    }

    #[derive(Debug)]
    struct ApprovalAndRestorationError {
        primary: anyhow::Error,
        restoration: anyhow::Error,
    }

    impl fmt::Display for ApprovalAndRestorationError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "{:#}; additionally, terminal restoration failed: {:#}",
                self.primary, self.restoration
            )
        }
    }

    impl Error for ApprovalAndRestorationError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(self.primary.as_ref())
        }
    }

    fn render_prompt(
        update: &UpdateReview<'_>,
        repo_root: &Path,
        human_output: HumanOutput,
    ) -> String {
        let path = human_output.styled(&relative_path(update.path(), repo_root), AnsiStyle::Cyan);
        let document = update.document_index() + 1;
        let details = match update.kind() {
            TargetKind::HelmRelease => {
                let current = human_output.styled(update.current_version(), AnsiStyle::Yellow);
                let latest = human_output.styled(update.latest_version(), AnsiStyle::Green);
                format!(
                    "HelmRelease {}, {}, chart {}/{} {current} -> {latest}",
                    update.target_name(),
                    update.yaml_path(),
                    update.repo_name().unwrap_or_default(),
                    update.chart_name().unwrap_or_default()
                )
            }
            TargetKind::ImageBinding => {
                let current = human_output.styled(
                    update.current_image().unwrap_or_default(),
                    AnsiStyle::Yellow,
                );
                let latest = human_output
                    .styled(update.latest_image().unwrap_or_default(), AnsiStyle::Green);
                format!(
                    "{} {}, {}, image {current} -> {latest}",
                    update.resource_kind(),
                    update.target_name(),
                    update.yaml_path()
                )
            }
        };
        format!("Update {path} (document {document}, {details})? [y/N] ")
    }
}

fn format_unknown_apply_ids(ids: &[String]) -> String {
    if ids.len() == 1 {
        format!("Unknown apply id: {}", ids[0])
    } else {
        format!("Unknown apply ids: {}", ids.join(", "))
    }
}

struct OutputContext<'a> {
    repo_root: &'a Path,
    json_output: bool,
    mode: &'a str,
    non_interactive: bool,
    human_output: HumanOutput,
}

impl<'a> OutputContext<'a> {
    fn new(
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

fn emit_update_output<W: Write, E: Write>(
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
    }
    value
}

#[derive(Debug, Clone, Copy)]
struct HumanOutput {
    color: bool,
    progress: bool,
}

impl HumanOutput {
    fn plain() -> Self {
        Self {
            color: false,
            progress: false,
        }
    }

    fn styled(self, text: &str, style: AnsiStyle) -> String {
        if !self.color {
            return text.to_string();
        }
        format!("\x1b[{}m{text}\x1b[0m", style.code())
    }
}

#[derive(Debug, Clone, Copy)]
enum AnsiStyle {
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
    }
}

fn relative_path(path: &Path, repo_root: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

struct ProgressRenderer<'a, E: Write> {
    stderr: &'a mut E,
    repo_root: &'a Path,
    human_output: HumanOutput,
    started_at: Instant,
    last_width: usize,
}

impl<'a, E: Write> ProgressRenderer<'a, E> {
    fn new(stderr: &'a mut E, repo_root: &'a Path, human_output: HumanOutput) -> Self {
        Self {
            stderr,
            repo_root,
            human_output,
            started_at: Instant::now(),
            last_width: 0,
        }
    }

    fn render(&mut self, completed: usize, total: usize, path: &Path) {
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

    fn finish(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::interactive_approval::InteractiveApproval;
    use super::{HumanOutput, UpdateSelectionIdentity};
    use crate::update_run::UpdatePlan;
    use crate::update_run::implementation::{
        PlannedChartUpdate, PlannedUpdate, SkippedUpdate, UpdateReport,
    };
    use std::io::Cursor;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    #[test]
    fn interactive_json_disables_terminal_color_and_progress() {
        let charts = crate::resolvers::StaticVersionResolver::new(std::collections::HashMap::new());
        let images =
            crate::resolvers::StaticImageVersionResolver::new(std::collections::HashMap::from([(
                "example/app:1.0.0".to_string(),
                "example/app:2.0.0".to_string(),
            )]));
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            temp.path().join("pod.yaml"),
            "kind: Pod\nmetadata: {name: demo}\nspec: {containers: [{image: example/app:1.0.0}]}\n",
        )
        .expect("write pod");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = super::run_with_args_and_output(
            [
                "fluxrepo-update",
                "update-helm",
                temp.path().to_str().expect("repo path"),
                "--json",
            ],
            InteractiveApproval::plain(Cursor::new(b"n")),
            &mut stdout,
            &mut stderr,
            &charts,
            &images,
            HumanOutput {
                color: true,
                progress: true,
            },
        )
        .expect("run interactive JSON command");

        assert_eq!(code, 0);
        let report: serde_json::Value = serde_json::from_slice(&stdout).expect("JSON report");
        assert_eq!(report["mode"], "apply");
        let stderr = String::from_utf8(stderr).expect("UTF-8 stderr");
        assert!(stderr.contains("[y/N]"));
        assert!(!stderr.contains('\u{1b}'));
        assert!(!stderr.contains("Resolving"));
    }

    #[test]
    fn plain_approval_filters_planned_updates_and_preserves_skips() {
        let report = report_with_updates(&[
            "first.yaml",
            "second.yaml",
            "third.yaml",
            "fourth.yaml",
            "fifth.yaml",
        ]);
        let mut output = Vec::new();
        let approval = InteractiveApproval::plain(Cursor::new(b"yYNx"));

        let approved = approval
            .review(
                &report.review(),
                &mut output,
                Path::new("/repo"),
                HumanOutput::plain(),
            )
            .expect("review updates");

        assert_eq!(approved.len(), 2);
        assert_eq!(approved[0], *report.review()[0].identity());
        assert_eq!(approved[1], *report.review()[1].identity());
        assert_eq!(report.skipped().len(), 1);
        let output = String::from_utf8(output).expect("utf-8");
        assert_eq!(output.matches("[y/N] y\n").count(), 2);
        assert_eq!(output.matches("[y/N] n\n").count(), 3);
        let prompt_positions = [
            "first.yaml",
            "second.yaml",
            "third.yaml",
            "fourth.yaml",
            "fifth.yaml",
        ]
        .map(|path| output.find(path).expect("prompt path"));
        assert!(prompt_positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn plain_approval_stops_on_ctrl_c_with_an_interrupted_error() {
        let report = report_with_updates(&["first.yaml", "second.yaml"]);
        let mut output = Vec::new();
        let approval = InteractiveApproval::plain(Cursor::new([0x03, b'y']));

        let error = approval
            .review(
                &report.review(),
                &mut output,
                Path::new("/repo"),
                HumanOutput::plain(),
            )
            .expect_err("Ctrl-C should interrupt");

        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::Interrupted)
        );
        assert_eq!(
            String::from_utf8(output)
                .expect("utf-8")
                .matches("[y/N]")
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_approval_reads_immediately_and_restores_the_same_pty() {
        let (result, output, original, restored) = review_through_pty(b'y');
        let approved = result.expect("review through PTY");

        assert_eq!(approved.len(), 1);
        assert!(output.ends_with(b"[y/N] y\r\n"));
        assert_eq!(restored.input_modes, original.input_modes);
        assert_eq!(restored.output_modes, original.output_modes);
        assert_eq!(restored.control_modes, original.control_modes);
        assert_eq!(
            restored.local_modes - rustix::termios::LocalModes::PENDIN,
            original.local_modes - rustix::termios::LocalModes::PENDIN
        );
        assert_eq!(restored.input_speed(), original.input_speed());
        assert_eq!(restored.output_speed(), original.output_speed());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_approval_reports_ctrl_c_and_restores_before_returning() {
        let (result, output, original, restored) = review_through_pty(0x03);
        let error = result.expect_err("Ctrl-C should interrupt");

        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::Interrupted)
        );
        assert_eq!(
            String::from_utf8(output)
                .expect("utf-8")
                .matches("[y/N]")
                .count(),
            1
        );
        assert_eq!(restored.input_modes, original.input_modes);
        assert_eq!(restored.output_modes, original.output_modes);
        assert_eq!(restored.control_modes, original.control_modes);
        assert_eq!(
            restored.local_modes - rustix::termios::LocalModes::PENDIN,
            original.local_modes - rustix::termios::LocalModes::PENDIN
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_approval_restores_the_pty_after_output_failure() {
        use std::fs::OpenOptions;

        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        use rustix::termios::tcgetattr;

        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open PTY master");
        grantpt(&master).expect("grant PTY");
        unlockpt(&master).expect("unlock PTY");
        let slave_path = ptsname(&master, Vec::new()).expect("PTY slave path");
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .open(slave_path.to_string_lossy().as_ref())
            .expect("open PTY slave");
        let observer = slave.try_clone().expect("clone PTY slave");
        let original = tcgetattr(&observer).expect("original terminal settings");
        let approval = InteractiveApproval::classified(slave);
        let mut output = FailingOutput;

        let error = approval
            .review(
                &report_with_updates(&["first.yaml"]).review(),
                &mut output,
                Path::new("/repo"),
                HumanOutput::plain(),
            )
            .expect_err("prompt output should fail");

        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::Other)
        );
        let restored = tcgetattr(&observer).expect("restored settings");
        assert_eq!(restored.input_modes, original.input_modes);
        assert_eq!(restored.output_modes, original.output_modes);
        assert_eq!(restored.control_modes, original.control_modes);
        assert_eq!(
            restored.local_modes - rustix::termios::LocalModes::PENDIN,
            original.local_modes - rustix::termios::LocalModes::PENDIN
        );
    }

    struct FailingOutput;

    impl std::io::Write for FailingOutput {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("output failed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    fn review_through_pty(
        decision: u8,
    ) -> (
        anyhow::Result<Vec<UpdateSelectionIdentity>>,
        Vec<u8>,
        rustix::termios::Termios,
        rustix::termios::Termios,
    ) {
        use std::fs::{File, OpenOptions};

        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        use rustix::termios::tcgetattr;

        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open PTY master");
        grantpt(&master).expect("grant PTY");
        unlockpt(&master).expect("unlock PTY");
        let slave_path = ptsname(&master, Vec::new()).expect("PTY slave path");
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .open(slave_path.to_string_lossy().as_ref())
            .expect("open PTY slave");
        let observer = slave.try_clone().expect("clone PTY slave");
        let original = tcgetattr(&observer).expect("original terminal settings");
        let terminal_observer = observer.try_clone().expect("clone PTY observer");
        let writer = std::thread::spawn(move || {
            while tcgetattr(&terminal_observer)
                .expect("active terminal settings")
                .local_modes
                .contains(rustix::termios::LocalModes::ICANON)
            {
                std::thread::yield_now();
            }
            let mut master = File::from(master);
            master.write_all(&[decision]).expect("send decision byte");
            while !tcgetattr(&terminal_observer)
                .expect("restored terminal settings")
                .local_modes
                .contains(rustix::termios::LocalModes::ICANON)
            {
                std::thread::yield_now();
            }
            master
        });
        let mut output = Vec::new();
        let result = InteractiveApproval::classified(slave).review(
            &report_with_updates(&["first.yaml"]).review(),
            &mut output,
            Path::new("/repo"),
            HumanOutput::plain(),
        );
        // Linux cannot query the slave's settings after the master is closed.
        let _master = writer.join().expect("PTY writer");
        let restored = tcgetattr(&observer).expect("restored settings");
        (result, output, original, restored)
    }

    fn report_with_updates(paths: &[&str]) -> UpdatePlan {
        let report = UpdateReport {
            checked_count: paths.len(),
            planned: paths
                .iter()
                .map(|path| {
                    PlannedUpdate::Chart(PlannedChartUpdate {
                        path: PathBuf::from("/repo").join(path),
                        document_index: 0,
                        target_name: path.to_string(),
                        chart_name: "chart".to_string(),
                        repo_name: "repo".to_string(),
                        current_version: "1.0.0".to_string(),
                        latest_version: "2.0.0".to_string(),
                        manifest_identity: None,
                    })
                })
                .collect(),
            skipped: vec![SkippedUpdate::new(
                Some(PathBuf::from("/repo/skipped.yaml")),
                "unresolved",
            )],
        };
        UpdatePlan::from_report(report, Path::new("/repo"))
    }
}
