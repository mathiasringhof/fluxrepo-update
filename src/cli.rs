//! Command parsing and dispatch; presentation and terminal interaction are private modules.

mod interactive_approval;
mod output;
#[cfg(test)]
mod tests;

use output::{HumanOutput, OutputContext, ProgressRenderer, emit_update_output};
#[cfg(test)]
pub(crate) use output::{plan_payload, skip_json};

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use serde_json::json;

use crate::resolvers::{
    ChartVersionResolver, ImageVersionResolver, RegistryImageResolver, RepositoryChartResolver,
};
use crate::scanner::scan_repo;
use crate::update_run::{
    ResolutionProgress, UpdateReview, UpdateRun, UpdateRunMode, UpdateRunStatus,
};

pub const EXIT_OK: u8 = 0;
pub const EXIT_STRICT_FAILURE: u8 = 2;
pub const EXIT_UPDATES_AVAILABLE: u8 = 10;
pub const EXIT_UPDATES_APPLIED: u8 = 20;

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
    UpdateHelm(UpdateHelmArgs),
}

#[derive(Debug, Args)]
struct UpdateHelmArgs {
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
}

impl UpdateHelmArgs {
    fn mode(&self) -> std::result::Result<UpdateRunMode, &'static str> {
        if self.write && !self.non_interactive {
            return Err(
                "Using --write requires --non-interactive. Interactive mode already writes approved changes.",
            );
        }
        if !self.apply_ids.is_empty() && !self.write {
            return Err("--apply-id requires --write.");
        }
        Ok(if !self.non_interactive {
            UpdateRunMode::ReviewAndApply
        } else if !self.write {
            UpdateRunMode::PlanOnly
        } else if self.apply_ids.is_empty() {
            UpdateRunMode::ApplyAll
        } else {
            UpdateRunMode::ApplySelected(self.apply_ids.iter().cloned().map(Into::into).collect())
        })
    }
}

impl Commands {
    fn json_output(&self) -> bool {
        match self {
            Self::Inventory { json_output, .. } => *json_output,
            Self::UpdateHelm(args) => args.json_output,
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
        Commands::UpdateHelm(args) => update_helm_command(
            args,
            approval,
            stdout,
            stderr,
            &UpdateRun::new(chart_resolver, image_resolver),
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

fn invalid_arguments<E: Write>(stderr: &mut E, json_output: bool, message: &str) -> Result<u8> {
    if json_output {
        emit_json_error_message(stderr, "invalid_arguments", message, EXIT_STRICT_FAILURE)?;
    } else {
        writeln!(stderr, "{message}")?;
    }
    Ok(EXIT_STRICT_FAILURE)
}

fn update_helm_command<W: Write, E: Write + Send>(
    args: UpdateHelmArgs,
    approval: interactive_approval::InteractiveApproval<'_>,
    stdout: &mut W,
    stderr: &mut E,
    run: &UpdateRun<'_>,
    human_output: HumanOutput,
) -> Result<u8> {
    let mode = match args.mode() {
        Ok(mode) => mode,
        Err(message) => return invalid_arguments(stderr, args.json_output, message),
    };
    let repo_root = args.repo_root.canonicalize()?;
    let json_output = args.json_output;
    let non_interactive = args.non_interactive;
    if !json_output {
        writeln!(stderr, "Scanning {}...", repo_root.display())?;
    }
    let inventory = scan_repo(&repo_root)?;
    let target_count = inventory.declaration_count();
    if !json_output {
        writeln!(stderr, "Resolving updates for {target_count} targets...")?;
    }
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
        let result = run.execute(
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
        return invalid_arguments(stderr, json_output, &message);
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

fn format_unknown_apply_ids(ids: &[String]) -> String {
    if ids.len() == 1 {
        format!("Unknown apply id: {}", ids[0])
    } else {
        format!("Unknown apply ids: {}", ids.join(", "))
    }
}
