use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::resolvers::{
    ChartVersionResolver, ImageVersionResolver, RegistryImageResolver, RepositoryChartResolver,
};
use crate::scanner::scan_repo;
use crate::updater::{PlanOptions, UpdateReport, apply_updates, plan_updates_with_options};

pub const EXIT_OK: u8 = 0;
pub const EXIT_STRICT_FAILURE: u8 = 2;
pub const EXIT_UPDATES_AVAILABLE: u8 = 10;
pub const EXIT_UPDATES_APPLIED: u8 = 20;

pub trait ResolverFactory {
    fn chart_resolver(&self) -> Box<dyn ChartVersionResolver + Sync>;
    fn image_resolver(&self) -> Box<dyn ImageVersionResolver + Sync>;
}

#[derive(Debug, Default)]
pub struct DefaultResolverFactory;

impl ResolverFactory for DefaultResolverFactory {
    fn chart_resolver(&self) -> Box<dyn ChartVersionResolver + Sync> {
        Box::new(RepositoryChartResolver::default())
    }

    fn image_resolver(&self) -> Box<dyn ImageVersionResolver + Sync> {
        Box::new(RegistryImageResolver::default())
    }
}

#[derive(Debug, Parser)]
#[command(name = "fluxrepo-update")]
#[command(about = "Inspect and update FluxCD manifest versions")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Inventory {
        repo_root: PathBuf,
        #[arg(long = "json")]
        json_output: bool,
    },
    #[command(name = "update-helm")]
    UpdateHelm {
        repo_root: PathBuf,
        #[arg(long = "json")]
        json_output: bool,
        #[arg(long)]
        write: bool,
        #[arg(long, conflicts_with = "best_effort")]
        strict: bool,
        #[arg(long = "best-effort")]
        best_effort: bool,
        #[arg(long = "non-interactive")]
        non_interactive: bool,
    },
}

pub fn run() -> Result<u8> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    run_with_args(
        std::env::args_os(),
        stdin.lock(),
        &mut stdout,
        &mut stderr,
        &DefaultResolverFactory,
        PlanOptions::default(),
    )
}

pub fn run_with_args<I, T, R, W, E, F>(
    args: I,
    input: R,
    stdout: &mut W,
    stderr: &mut E,
    resolver_factory: &F,
    plan_options: PlanOptions,
) -> Result<u8>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    R: BufRead,
    W: Write,
    E: Write,
    F: ResolverFactory + ?Sized,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            write!(stderr, "{error}")?;
            return Ok(error.exit_code() as u8);
        }
    };

    match cli.command {
        Commands::Inventory {
            repo_root,
            json_output,
        } => inventory_command(repo_root, json_output, stdout),
        Commands::UpdateHelm {
            repo_root,
            json_output,
            write,
            strict,
            best_effort: _,
            non_interactive,
        } => update_helm_command(
            repo_root,
            json_output,
            write,
            strict,
            non_interactive,
            input,
            stdout,
            stderr,
            resolver_factory,
            plan_options,
        ),
    }
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
        writeln!(stdout, "Repositories: {}", inventory.repositories.len())?;
        writeln!(stdout, "Chart targets: {}", inventory.chart_targets.len())?;
        writeln!(
            stdout,
            "Deployment targets: {}",
            inventory.deployment_targets.len()
        )?;
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
fn update_helm_command<R, W, E, F>(
    repo_root: PathBuf,
    json_output: bool,
    write: bool,
    strict: bool,
    non_interactive: bool,
    input: R,
    stdout: &mut W,
    stderr: &mut E,
    resolver_factory: &F,
    plan_options: PlanOptions,
) -> Result<u8>
where
    R: BufRead,
    W: Write,
    E: Write,
    F: ResolverFactory + ?Sized,
{
    let repo_root = repo_root.canonicalize()?;
    if write && !non_interactive {
        writeln!(
            stderr,
            "Using --write requires --non-interactive. Interactive mode already writes approved changes."
        )?;
        return Ok(EXIT_STRICT_FAILURE);
    }

    if !json_output {
        writeln!(stderr, "Scanning {}...", repo_root.display())?;
    }
    let inventory = scan_repo(&repo_root)?;
    if !json_output {
        writeln!(
            stderr,
            "Resolving updates for {} targets...",
            inventory.chart_targets.len() + inventory.deployment_targets.len()
        )?;
    }
    let chart_resolver = resolver_factory.chart_resolver();
    let image_resolver = resolver_factory.image_resolver();
    let report = plan_updates_with_options(
        &inventory,
        chart_resolver.as_ref(),
        image_resolver.as_ref(),
        plan_options,
    );

    if strict && !report.skipped.is_empty() {
        emit_update_output(
            &report,
            OutputContext::new(&repo_root, json_output, "plan", strict, non_interactive),
            0,
            0,
            stdout,
            stderr,
        )?;
        return Ok(EXIT_STRICT_FAILURE);
    }

    if !report.planned.is_empty() && (write || !non_interactive) {
        let approved_report = if non_interactive {
            report
        } else {
            select_updates_for_apply(report, input, stderr)?
        };
        if approved_report.planned.is_empty() {
            emit_update_output(
                &approved_report,
                OutputContext::new(&repo_root, json_output, "apply", strict, non_interactive),
                0,
                0,
                stdout,
                stderr,
            )?;
            return Ok(EXIT_OK);
        }
        let changed_files = apply_updates(&approved_report)?;
        emit_update_output(
            &approved_report,
            OutputContext::new(&repo_root, json_output, "apply", strict, non_interactive),
            approved_report.planned.len(),
            changed_files,
            stdout,
            stderr,
        )?;
        return Ok(EXIT_UPDATES_APPLIED);
    }

    emit_update_output(
        &report,
        OutputContext::new(&repo_root, json_output, "plan", strict, non_interactive),
        0,
        0,
        stdout,
        stderr,
    )?;
    if report.planned.is_empty() {
        Ok(EXIT_OK)
    } else {
        Ok(EXIT_UPDATES_AVAILABLE)
    }
}

fn select_updates_for_apply<R: BufRead, E: Write>(
    report: UpdateReport,
    mut input: R,
    stderr: &mut E,
) -> Result<UpdateReport> {
    let mut approved = Vec::new();
    for update in report.planned {
        write!(
            stderr,
            "Update {} {} -> {}? [y/N] ",
            update.path().display(),
            update.current_version(),
            update.latest_version()
        )?;
        let mut buffer = String::new();
        input.read_line(&mut buffer)?;
        let choice = buffer.chars().next().unwrap_or('\n');
        if matches!(choice, 'y' | 'Y') {
            approved.push(update);
        }
    }
    Ok(UpdateReport {
        planned: approved,
        skipped: report.skipped,
    })
}

struct OutputContext<'a> {
    repo_root: &'a Path,
    json_output: bool,
    mode: &'a str,
    strict: bool,
    non_interactive: bool,
}

impl<'a> OutputContext<'a> {
    fn new(
        repo_root: &'a Path,
        json_output: bool,
        mode: &'a str,
        strict: bool,
        non_interactive: bool,
    ) -> Self {
        Self {
            repo_root,
            json_output,
            mode,
            strict,
            non_interactive,
        }
    }
}

fn emit_update_output<W: Write, E: Write>(
    report: &UpdateReport,
    context: OutputContext<'_>,
    applied_count: usize,
    changed_file_count: usize,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<()> {
    if context.json_output {
        writeln!(
            stdout,
            "{}",
            serde_json::to_string_pretty(&report.to_json_value(
                context.repo_root,
                context.mode,
                context.strict,
                context.non_interactive,
                applied_count,
                changed_file_count
            ))?
        )?;
        return Ok(());
    }

    for item in &report.skipped {
        writeln!(stderr, "skip: {item}")?;
    }
    if report.planned.is_empty() {
        if context.mode == "apply" {
            writeln!(stderr, "No updates were approved.")?;
        } else {
            writeln!(stderr, "No updates required.")?;
        }
        return Ok(());
    }
    for update in &report.planned {
        writeln!(
            stderr,
            "{}: {} {} -> {}",
            update
                .path()
                .strip_prefix(context.repo_root)
                .unwrap_or(update.path())
                .display(),
            update.target_kind(),
            update.current_version(),
            update.latest_version()
        )?;
    }
    if context.mode == "plan" {
        writeln!(
            stderr,
            "Plan only. Re-run without --non-interactive for prompts, or add --non-interactive --write to apply all changes."
        )?;
    } else {
        writeln!(
            stderr,
            "Updated {applied_count} targets across {changed_file_count} files."
        )?;
    }
    Ok(())
}
