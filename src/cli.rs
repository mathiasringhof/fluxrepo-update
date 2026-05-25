use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::resolvers::{
    ChartVersionResolver, ImageVersionResolver, RegistryImageResolver, RepositoryChartResolver,
};
use crate::scanner::scan_repo;
use crate::updater::{
    PlanOptions, PlannedUpdate, UpdateReport, apply_updates, plan_updates_with_options,
    plan_updates_with_progress,
};

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
    #[command(about = "Scan a Flux repository and list update targets")]
    Inventory {
        #[arg(value_name = "REPO_ROOT", help = "Flux repository root")]
        repo_root: PathBuf,
        #[arg(long = "json", help = "Emit the full inventory as JSON")]
        json_output: bool,
    },
    #[command(name = "update-helm")]
    #[command(about = "Plan or apply HelmRelease and Deployment image updates")]
    UpdateHelm {
        #[arg(value_name = "REPO_ROOT", help = "Flux repository root")]
        repo_root: PathBuf,
        #[arg(long = "json", help = "Emit the update report as JSON")]
        json_output: bool,
        #[arg(long, help = "Apply all planned updates; requires --non-interactive")]
        write: bool,
        #[arg(
            long,
            conflicts_with = "best_effort",
            help = "Exit with code 2 when any target is skipped"
        )]
        strict: bool,
        #[arg(long = "best-effort", help = "Allow skipped targets while planning")]
        best_effort: bool,
        #[arg(long = "non-interactive", help = "Disable prompts")]
        non_interactive: bool,
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
    let terminal_stderr = stderr.is_terminal();
    run_with_args_and_output(
        std::env::args_os(),
        stdin.lock(),
        &mut stdout,
        &mut stderr,
        &DefaultResolverFactory,
        PlanOptions::default(),
        HumanOutput {
            color: terminal_stderr,
            progress: terminal_stderr,
        },
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
    E: Write + Send,
    F: ResolverFactory + ?Sized,
{
    run_with_args_and_output(
        args,
        input,
        stdout,
        stderr,
        resolver_factory,
        plan_options,
        HumanOutput::plain(),
    )
}

fn run_with_args_and_output<I, T, R, W, E, F>(
    args: I,
    input: R,
    stdout: &mut W,
    stderr: &mut E,
    resolver_factory: &F,
    plan_options: PlanOptions,
    human_output: HumanOutput,
) -> Result<u8>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    R: BufRead,
    W: Write,
    E: Write + Send,
    F: ResolverFactory + ?Sized,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            write!(stderr, "{error}")?;
            return Ok(error.exit_code() as u8);
        }
    };

    let json_output = cli.command.json_output();
    let exit_code = match cli.command {
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
    human_output: HumanOutput,
) -> Result<u8>
where
    R: BufRead,
    W: Write,
    E: Write + Send,
    F: ResolverFactory + ?Sized,
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

    if !json_output {
        writeln!(stderr, "Scanning {}...", repo_root.display())?;
    }
    let inventory = scan_repo(&repo_root)?;
    let target_count = inventory.chart_targets.len() + inventory.deployment_targets.len();
    if !json_output {
        writeln!(stderr, "Resolving updates for {} targets...", target_count)?;
    }
    let chart_resolver = resolver_factory.chart_resolver();
    let image_resolver = resolver_factory.image_resolver();
    let report = if !json_output && human_output.progress && target_count > 0 {
        let progress = Mutex::new(ProgressRenderer::new(stderr, &repo_root, human_output));
        let progress_callback = |completed: usize, total: usize, path: &Path| {
            if let Ok(mut renderer) = progress.lock() {
                renderer.render(completed, total, path);
            }
        };
        let report = plan_updates_with_progress(
            &inventory,
            chart_resolver.as_ref(),
            image_resolver.as_ref(),
            plan_options,
            &progress_callback,
        );
        if let Ok(mut renderer) = progress.lock() {
            renderer.finish();
        }
        report
    } else {
        plan_updates_with_options(
            &inventory,
            chart_resolver.as_ref(),
            image_resolver.as_ref(),
            plan_options,
        )
    };

    if strict && !report.skipped.is_empty() {
        emit_update_output(
            &report,
            OutputContext::new(
                &repo_root,
                json_output,
                "plan",
                strict,
                non_interactive,
                human_output,
            ),
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
            select_updates_for_apply(report, &repo_root, human_output, input, stderr)?
        };
        if approved_report.planned.is_empty() {
            emit_update_output(
                &approved_report,
                OutputContext::new(
                    &repo_root,
                    json_output,
                    "apply",
                    strict,
                    non_interactive,
                    human_output,
                ),
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
            OutputContext::new(
                &repo_root,
                json_output,
                "apply",
                strict,
                non_interactive,
                human_output,
            ),
            approved_report.planned.len(),
            changed_files,
            stdout,
            stderr,
        )?;
        return Ok(EXIT_UPDATES_APPLIED);
    }

    emit_update_output(
        &report,
        OutputContext::new(
            &repo_root,
            json_output,
            "plan",
            strict,
            non_interactive,
            human_output,
        ),
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
    repo_root: &Path,
    human_output: HumanOutput,
    mut input: R,
    stderr: &mut E,
) -> Result<UpdateReport> {
    let mut approved = Vec::new();
    for update in report.planned {
        write!(
            stderr,
            "{}",
            render_prompt(&update, repo_root, human_output)
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
    human_output: HumanOutput,
}

impl<'a> OutputContext<'a> {
    fn new(
        repo_root: &'a Path,
        json_output: bool,
        mode: &'a str,
        strict: bool,
        non_interactive: bool,
        human_output: HumanOutput,
    ) -> Self {
        Self {
            repo_root,
            json_output,
            mode,
            strict,
            non_interactive,
            human_output,
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
            "{}",
            render_update_line(update, context.repo_root, context.human_output)
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
    update: &PlannedUpdate,
    repo_root: &Path,
    human_output: HumanOutput,
) -> String {
    let path = human_output.styled(&relative_path(update.path(), repo_root), AnsiStyle::Cyan);
    let current = human_output.styled(update.current_version(), AnsiStyle::Yellow);
    let latest = human_output.styled(update.latest_version(), AnsiStyle::Green);

    match update {
        PlannedUpdate::Chart(chart_update) => {
            let mut line = format!(
                "{path}: HelmRelease {} {} {current} -> {latest}",
                chart_update.target_name, chart_update.chart_name
            );
            if chart_update.inherited_source {
                line.push_str(" inherited-source");
            }
            line
        }
        PlannedUpdate::Deployment(deployment_update) => format!(
            "{path}: Deployment {} {} {current} -> {latest}",
            deployment_update.target_name, deployment_update.yaml_path
        ),
    }
}

fn render_prompt(update: &PlannedUpdate, repo_root: &Path, human_output: HumanOutput) -> String {
    let path = human_output.styled(&relative_path(update.path(), repo_root), AnsiStyle::Cyan);
    let current = human_output.styled(update.current_version(), AnsiStyle::Yellow);
    let latest = human_output.styled(update.latest_version(), AnsiStyle::Green);
    let label = match update {
        PlannedUpdate::Chart(_) => "chart",
        PlannedUpdate::Deployment(_) => "image",
    };
    format!("Update {path} ({label} {current} -> {latest})? [y/N] ")
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
