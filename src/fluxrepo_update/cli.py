from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Annotated

import click
import typer
from rich.console import Console
from rich.markup import escape
from rich.progress import (
    BarColumn,
    MofNCompleteColumn,
    Progress,
    SpinnerColumn,
    TextColumn,
    TimeElapsedColumn,
)
from rich.table import Column
from typer.core import TyperCommand

from fluxrepo_update.models import DeploymentImageTarget, HelmReleaseTarget
from fluxrepo_update.resolvers import (
    ChartVersionResolver,
    ImageVersionResolver,
    RegistryImageResolver,
    RepositoryChartResolver,
)
from fluxrepo_update.scanner import scan_repo
from fluxrepo_update.updater import (
    PlannedChartUpdate,
    PlannedDeploymentUpdate,
    UpdateReport,
    apply_updates,
    plan_updates,
)

app = typer.Typer(no_args_is_help=True, add_completion=False)
EXIT_OK = 0
EXIT_STRICT_FAILURE = 2
EXIT_UPDATES_AVAILABLE = 10
EXIT_UPDATES_APPLIED = 20
_ERR_CONSOLE = Console(stderr=True)
_ACTIVE_PROGRESS: Progress | None = None
RepoRootArgument = Annotated[
    Path,
    typer.Argument(..., exists=True, file_okay=False, dir_okay=True, readable=True),
]
JsonOption = Annotated[bool, typer.Option("--json", help="Render machine-readable JSON output.")]
WriteOption = Annotated[
    bool,
    typer.Option(
        "--write",
        help="Apply all planned updates without prompts. Requires --non-interactive.",
    ),
]
StrictOption = Annotated[
    bool,
    typer.Option("--strict/--best-effort", help="Fail if any chart resolution step is skipped."),
]
NonInteractiveOption = Annotated[
    bool,
    typer.Option("--non-interactive", help="Disable any future interactive behavior for automation use."),
]


class HelpOnMissingArgumentCommand(TyperCommand):
    def parse_args(self, ctx: click.Context, args: list[str]) -> list[str]:
        try:
            return super().parse_args(ctx, args)
        except click.MissingParameter as exc:
            if ctx.resilient_parsing:
                raise

            typer.echo(ctx.get_help(), err=True)
            typer.echo(err=True)
            typer.echo(f"Error: {exc.format_message()}", err=True)
            raise typer.Exit(exc.exit_code) from exc


@app.command("inventory", cls=HelpOnMissingArgumentCommand)
def inventory_command(
    repo_root: RepoRootArgument,
    json_output: JsonOption = False,
) -> None:
    inventory = scan_repo(repo_root)

    if json_output:
        typer.echo(json.dumps(inventory.to_dict(), indent=2))
        return

    typer.echo(f"Repositories: {len(inventory.repositories)}")
    typer.echo(f"Chart targets: {len(inventory.chart_targets)}")
    typer.echo(f"Deployment targets: {len(inventory.deployment_targets)}")
    typer.echo(
        "HelmReleases without chart version: "
        f"{len(inventory.helmreleases_without_chart_version)}"
    )
    typer.echo(f"Unresolved chart targets: {len(inventory.unresolved_chart_targets)}")
    typer.echo(f"Image references: {len(inventory.image_references)}")
    typer.echo(f"Skipped generated files: {len(inventory.skipped_paths)}")


@app.command("update-helm", cls=HelpOnMissingArgumentCommand)
def update_helm_command(
    repo_root: RepoRootArgument,
    json_output: JsonOption = False,
    write: WriteOption = False,
    strict: StrictOption = False,
    non_interactive: NonInteractiveOption = False,
) -> None:
    resolved_repo_root = repo_root.resolve()
    if not json_output:
        typer.echo(f"Scanning {resolved_repo_root}...", err=True)
    inventory = scan_repo(resolved_repo_root)
    chart_resolver = build_chart_resolver()
    image_resolver = build_image_resolver()
    total_targets = len(inventory.chart_targets) + len(inventory.deployment_targets)
    if not json_output and total_targets:
        typer.echo(
            f"Resolving updates for {total_targets} targets...",
            err=True,
        )

    progress_enabled = not json_output and bool(total_targets)
    try:
        report = plan_updates(
            inventory,
            chart_resolver,
            image_resolver,
            progress_callback=build_progress_callback(
                repo_root=resolved_repo_root,
                enabled=progress_enabled,
            ),
        )
    except KeyboardInterrupt:
        finish_progress(enabled=progress_enabled)
        typer.echo("Cancelled.", err=True)
        raise typer.Exit(130) from None
    else:
        finish_progress(enabled=progress_enabled)
    has_strict_failure = strict and bool(report.skipped)

    if write and not non_interactive:
        typer.echo(
            "Using --write requires --non-interactive. Interactive mode already writes approved changes.",
            err=True,
        )
        raise typer.Exit(EXIT_STRICT_FAILURE)

    if has_strict_failure:
        emit_update_output(
            report,
            repo_root=resolved_repo_root,
            json_output=json_output,
            mode="plan",
            strict=strict,
            non_interactive=non_interactive,
            applied_count=0,
            changed_file_count=0,
        )
        raise typer.Exit(EXIT_STRICT_FAILURE)

    if should_apply_updates(
        report,
        json_output=json_output,
        write=write,
        non_interactive=non_interactive,
    ):
        approved_report = select_updates_for_apply(
            report,
            repo_root=resolved_repo_root,
            non_interactive=non_interactive,
        )
        if not approved_report.planned:
            emit_update_output(
                approved_report,
                repo_root=resolved_repo_root,
                json_output=json_output,
                mode="apply",
                strict=strict,
                non_interactive=non_interactive,
                applied_count=0,
                changed_file_count=0,
            )
            raise typer.Exit(EXIT_OK)

        changed_files = apply_updates(approved_report)
        emit_update_output(
            approved_report,
            repo_root=resolved_repo_root,
            json_output=json_output,
            mode="apply",
            strict=strict,
            non_interactive=non_interactive,
            applied_count=len(approved_report.planned),
            changed_file_count=changed_files,
        )
        raise typer.Exit(EXIT_UPDATES_APPLIED)

    emit_update_output(
        report,
        repo_root=resolved_repo_root,
        json_output=json_output,
        mode="plan",
        strict=strict,
        non_interactive=non_interactive,
        applied_count=0,
        changed_file_count=0,
    )

    if report.planned:
        raise typer.Exit(EXIT_UPDATES_AVAILABLE)
    raise typer.Exit(EXIT_OK)


def build_chart_resolver() -> ChartVersionResolver:
    return RepositoryChartResolver()


def build_image_resolver() -> ImageVersionResolver:
    return RegistryImageResolver()


def build_progress_callback(*, repo_root: Path, enabled: bool):
    if not enabled:
        return None

    global _ACTIVE_PROGRESS
    progress = Progress(
        SpinnerColumn(),
        TextColumn("{task.description}", table_column=Column(width=9)),
        TextColumn("{task.fields[count]}", table_column=Column(width=5)),
        BarColumn(bar_width=24),
        MofNCompleteColumn(),
        TimeElapsedColumn(),
        TextColumn(
            "{task.fields[path]}",
            table_column=Column(ratio=1, overflow="ellipsis", no_wrap=True),
        ),
        console=_ERR_CONSOLE,
        transient=False,
    )
    progress.start()
    task_id = progress.add_task("Resolving", total=None, count="0/0", path="")
    _ACTIVE_PROGRESS = progress

    def emit_progress(current: int, total: int, target: HelmReleaseTarget | DeploymentImageTarget) -> None:
        try:
            target_path = target.path.relative_to(repo_root)
        except ValueError:
            target_path = target.path

        progress.update(
            task_id,
            total=total,
            advance=1,
            description="Resolving",
            count=f"{current}/{total}",
            path=str(target_path),
        )

    return emit_progress


def finish_progress(*, enabled: bool) -> None:
    global _ACTIVE_PROGRESS
    if enabled and _ACTIVE_PROGRESS is not None:
        _ACTIVE_PROGRESS.stop()
        _ACTIVE_PROGRESS = None


def emit_update_output(
    report: UpdateReport,
    *,
    repo_root: Path,
    json_output: bool,
    mode: str,
    strict: bool,
    non_interactive: bool,
    applied_count: int,
    changed_file_count: int,
) -> None:
    if json_output:
        typer.echo(
            json.dumps(
                report.to_dict(
                    repo_root,
                    mode=mode,
                    strict=strict,
                    non_interactive=non_interactive,
                    applied_count=applied_count,
                    changed_file_count=changed_file_count,
                ),
                indent=2,
            )
        )
        return

    for item in report.skipped:
        typer.echo(f"skip: {item}", err=True)

    if not report.planned:
        if mode == "apply":
            typer.echo("No updates were approved.", err=True)
        else:
            typer.echo("No updates required.", err=True)
        return

    for update in report.planned:
        _ERR_CONSOLE.print(render_update_line(update, repo_root=repo_root))

    if mode == "plan":
        typer.echo(
            "Plan only. Re-run without --non-interactive for prompts, or add --non-interactive --write to apply all changes.",
            err=True,
        )
    else:
        typer.echo(f"Updated {applied_count} targets across {changed_file_count} files.", err=True)


def should_apply_updates(
    report: UpdateReport,
    *,
    json_output: bool,
    write: bool,
    non_interactive: bool,
) -> bool:
    if not report.planned:
        return False
    if non_interactive:
        return write
    return True


def select_updates_for_apply(
    report: UpdateReport,
    *,
    repo_root: Path,
    non_interactive: bool,
) -> UpdateReport:
    if non_interactive:
        return report

    approved: list = []
    for update in report.planned:
        approved_update = prompt_for_update_approval(build_update_prompt(update, repo_root=repo_root))
        if approved_update:
            approved.append(update)

    return UpdateReport(planned=approved, skipped=report.skipped)


def prompt_for_update_approval(prompt: str) -> bool:
    while True:
        _ERR_CONSOLE.print(prompt, end="")
        choice = read_single_keypress().lower()

        if choice in {"\r", "\n", ""}:
            typer.echo(err=True)
            return False
        if choice in {"y", "n"}:
            typer.echo(choice, err=True)
            return choice == "y"


def read_single_keypress() -> str:
    stdin = sys.stdin
    if stdin.isatty():
        return click.getchar()
    return stdin.read(1)


def render_update_line(
    update: PlannedChartUpdate | PlannedDeploymentUpdate,
    *,
    repo_root: Path,
) -> str:
    relative_path = escape(str(update.path.relative_to(repo_root)))
    if isinstance(update, PlannedChartUpdate):
        inherited = " inherited-source" if update.inherited_source else ""
        return (
            f"[cyan]{relative_path}[/cyan]: HelmRelease {escape(update.target_name)} "
            f"{escape(update.chart_name)} [yellow]{escape(update.current_version)}[/yellow] "
            f"-> [green]{escape(update.latest_version)}[/green]{inherited}"
        )

    return (
        f"[cyan]{relative_path}[/cyan]: Deployment {escape(update.target_name)} "
        f"{escape(update.yaml_path)} [yellow]{escape(update.current_version)}[/yellow] "
        f"-> [green]{escape(update.latest_version)}[/green]"
    )


def build_update_prompt(
    update: PlannedChartUpdate | PlannedDeploymentUpdate,
    *,
    repo_root: Path,
) -> str:
    relative_path = escape(str(update.path.relative_to(repo_root)))
    if isinstance(update, PlannedChartUpdate):
        detail = (
            f"{escape(update.chart_name)} [yellow]{escape(update.current_version)}[/yellow] "
            f"-> [green]{escape(update.latest_version)}[/green]"
        )
    else:
        detail = (
            f"{escape(update.yaml_path)} [yellow]{escape(update.current_version)}[/yellow] "
            f"-> [green]{escape(update.latest_version)}[/green]"
        )

    return f"Update [cyan]{relative_path}[/cyan] ({detail})? [y/N] "
