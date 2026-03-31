from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fluxrepo_update import cli
from fluxrepo_update.cli import app
from fluxrepo_update.models import HelmRepository
from fluxrepo_update.resolvers import ChartVersionResolver, ImageVersionResolver


class StubResolver(ChartVersionResolver):
    def __init__(self, versions: dict[tuple[str, str], str]) -> None:
        self._versions = versions

    def resolve(
        self,
        repository: HelmRepository,
        chart_name: str,
        current_version: str | None = None,
    ) -> str:
        key = (repository.name, chart_name)
        if key not in self._versions:
            raise ValueError(f"no resolver fixture for {repository.name}/{chart_name}")
        return self._versions[key]


class StubImageResolver(ImageVersionResolver):
    def __init__(self, versions: dict[str, str]) -> None:
        self._versions = versions

    def resolve(self, image: str) -> str:
        if image not in self._versions:
            raise ValueError(f"no resolver fixture for {image}")
        return self._versions[image]


@pytest.fixture
def cli_runner() -> CliRunner:
    return CliRunner()


def test_inventory_command_supports_json_output(fixture_repo_root) -> None:
    runner = CliRunner()
    result = runner.invoke(app, ["inventory", str(fixture_repo_root), "--json"])

    assert result.exit_code == 0
    payload = json.loads(result.stdout)
    assert payload["repository_count"] > 0
    assert payload["chart_target_count"] > 0
    assert payload["image_reference_count"] > 0


def test_inventory_command_prints_human_summary(fixture_repo_root: Path, cli_runner: CliRunner) -> None:
    result = cli_runner.invoke(app, ["inventory", str(fixture_repo_root)])

    assert result.exit_code == 0
    assert "Repositories:" in result.output
    assert "Chart targets:" in result.output
    assert "Unresolved chart targets:" in result.output


def test_inventory_command_prints_help_when_repo_root_is_missing(cli_runner: CliRunner) -> None:
    result = cli_runner.invoke(app, ["inventory"])

    assert result.exit_code == 2
    assert "Usage:" in result.output
    assert "inventory [OPTIONS] REPO_ROOT" in result.output
    assert "--help" in result.output
    assert "Missing argument 'REPO_ROOT'." in result.output


def test_update_helm_json_dry_run_returns_agent_friendly_payload(
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(
        cli,
        "build_image_resolver",
        lambda: StubImageResolver({"linuxserver/sonarr:version-4.0.16.2944": "linuxserver/sonarr:version-4.0.17.3000"}),
    )

    result = cli_runner.invoke(
        app,
        ["update-helm", str(fixture_repo_root), "--json", "--non-interactive"],
    )

    assert result.exit_code == 10
    payload = json.loads(result.stdout)
    assert payload["mode"] == "plan"
    assert payload["strict"] is False
    assert payload["non_interactive"] is True
    assert payload["summary"]["planned_count"] >= 1
    assert payload["summary"]["applied_count"] == 0
    assert payload["summary"]["skipped_count"] >= 1
    assert "path" in payload["skipped"][0]
    assert "reason" in payload["skipped"][0]
    assert any(
        item["path"] == "apps/production/paperless/release-patch.yaml" and item["inherited_source"] is True
        for item in payload["planned"]
    )
    assert any(
        item["path"] == "apps/base/sonarr/deployment.yaml"
        and item["target_kind"] == "Deployment"
        and item["yaml_path"] == "spec.template.spec.containers[0].image"
        and item["latest_image"] == "linuxserver/sonarr:version-4.0.17.3000"
        for item in payload["planned"]
    )


def test_update_helm_json_write_returns_applied_exit_code(
    tmp_path: Path,
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo_root = copy_fixture_repo(tmp_path, fixture_repo_root)
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(
        cli,
        "build_image_resolver",
        lambda: StubImageResolver({"linuxserver/sonarr:version-4.0.16.2944": "linuxserver/sonarr:version-4.0.17.3000"}),
    )

    result = cli_runner.invoke(
        app,
        ["update-helm", str(repo_root), "--json", "--write", "--non-interactive"],
    )

    assert result.exit_code == 20
    payload = json.loads(result.stdout)
    assert payload["mode"] == "apply"
    assert payload["summary"]["applied_count"] >= 1
    assert payload["summary"]["changed_file_count"] >= 1
    assert 'version: "12.1.0"' in (repo_root / "apps/production/paperless/release-patch.yaml").read_text(
        encoding="utf-8"
    )
    assert "image: linuxserver/sonarr:version-4.0.17.3000" in (
        repo_root / "apps/base/sonarr/deployment.yaml"
    ).read_text(encoding="utf-8")


def test_update_helm_interactive_mode_prompts_per_release_and_applies_selected_only(
    tmp_path: Path,
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo_root = copy_fixture_repo(tmp_path, fixture_repo_root)
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(repo_root)],
        input="ynnn",
    )

    assert result.exit_code == 20
    assert 'version: "12.1.0"' in (repo_root / "apps/base/paperless-ngx/release.yaml").read_text(encoding="utf-8")
    assert 'version: "11.29.10"' in (repo_root / "apps/production/paperless/release-patch.yaml").read_text(
        encoding="utf-8"
    )


def test_update_helm_interactive_mode_defaults_empty_answer_to_no(
    tmp_path: Path,
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo_root = copy_fixture_repo(tmp_path, fixture_repo_root)
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(repo_root)],
        input="\n\n\n\n",
    )

    assert result.exit_code == 0
    assert 'version: "12.0.0"' in (repo_root / "apps/base/paperless-ngx/release.yaml").read_text(encoding="utf-8")
    assert 'version: "11.29.10"' in (repo_root / "apps/production/paperless/release-patch.yaml").read_text(
        encoding="utf-8"
    )


def test_update_helm_standard_interactive_mode_prompts_without_write_flag(
    tmp_path: Path,
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo_root = copy_fixture_repo(tmp_path, fixture_repo_root)
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(repo_root)],
        input="ynnn",
    )

    assert result.exit_code == 20
    assert 'version: "12.1.0"' in (repo_root / "apps/base/paperless-ngx/release.yaml").read_text(encoding="utf-8")
    assert 'version: "11.29.10"' in (repo_root / "apps/production/paperless/release-patch.yaml").read_text(
        encoding="utf-8"
    )


def test_update_helm_non_interactive_plans_without_writing(
    tmp_path: Path,
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo_root = copy_fixture_repo(tmp_path, fixture_repo_root)
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(repo_root), "--json", "--non-interactive"],
    )

    assert result.exit_code == 10
    payload = json.loads(result.stdout)
    assert payload["mode"] == "plan"
    assert payload["summary"]["applied_count"] == 0
    assert 'version: "12.0.0"' in (repo_root / "apps/base/paperless-ngx/release.yaml").read_text(encoding="utf-8")


def test_update_helm_write_requires_non_interactive(
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(fixture_repo_root), "--write"],
    )

    assert result.exit_code == 2
    assert "--non-interactive" in result.stderr


def test_update_helm_prints_help_when_repo_root_is_missing(cli_runner: CliRunner) -> None:
    result = cli_runner.invoke(app, ["update-helm"])

    assert result.exit_code == 2
    assert "Usage:" in result.output
    assert "update-helm [OPTIONS] REPO_ROOT" in result.output
    assert "--help" in result.output
    assert "Missing argument 'REPO_ROOT'." in result.output


def test_update_helm_strict_fails_on_skipped_resolution(
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(fixture_repo_root), "--json", "--strict", "--non-interactive"],
    )

    assert result.exit_code == 2
    payload = json.loads(result.stdout)
    assert payload["strict"] is True
    assert payload["summary"]["skipped_count"] >= 1
    assert payload["summary"]["planned_count"] >= 1


def test_update_helm_returns_zero_when_no_updates_needed(
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "11.29.10"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(fixture_repo_root), "--json", "--non-interactive"],
    )

    assert result.exit_code == 0
    payload = json.loads(result.stdout)
    assert payload["summary"]["planned_count"] == 0
    assert payload["summary"]["applied_count"] == 0


def test_update_helm_non_json_plan_output_includes_skip_reasons_and_plan_hint(
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        cli,
        "build_chart_resolver",
        lambda: StubResolver({("truecharts", "paperless-ngx"): "12.1.0"}),
    )
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))

    result = cli_runner.invoke(
        app,
        ["update-helm", str(fixture_repo_root), "--non-interactive"],
    )

    assert result.exit_code == 10
    assert "skip: " in result.stderr
    assert "no resolver fixture for truecharts/audiobookshelf" in result.stderr
    assert "Plan only. Re-run without --non-interactive" in result.stderr


def test_build_progress_callback_uses_rich_progress(monkeypatch: pytest.MonkeyPatch) -> None:
    lifecycle: list[object] = []
    updates: list[tuple[int, int, str | None, dict[str, object]]] = []

    class FakeProgress:
        def __init__(self, *columns: object, **kwargs: object) -> None:
            lifecycle.append(("init", columns, kwargs))

        def start(self) -> None:
            lifecycle.append("start")

        def add_task(self, description: str, total: int, **fields: object) -> int:
            lifecycle.append(("add_task", description, total, fields))
            return 7

        def update(
            self,
            task_id: int,
            *,
            total: int | None = None,
            advance: int = 0,
            description: str | None = None,
            **fields: object,
        ) -> None:
            updates.append((task_id, total, advance, description, fields))

        def stop(self) -> None:
            lifecycle.append("stop")

    monkeypatch.setattr(cli, "Progress", FakeProgress)
    monkeypatch.setattr(cli, "_ACTIVE_PROGRESS", None, raising=False)

    callback = cli.build_progress_callback(repo_root=Path("/repo"), enabled=True)
    assert callback is not None

    callback(1, 3, type("Target", (), {"path": Path("/repo/apps/demo.yaml")})())
    callback(2, 3, type("Target", (), {"path": Path("/repo/apps/other.yaml")})())
    cli.finish_progress(enabled=True)

    init_columns = next(item[1] for item in lifecycle if isinstance(item, tuple) and item[0] == "init")
    assert any(isinstance(column, cli.BarColumn) and column.bar_width == 24 for column in init_columns)
    assert ("add_task", "Resolving", None, {"count": "0/0", "path": ""}) in lifecycle
    assert updates == [
        (7, 3, 1, "Resolving", {"count": "1/3", "path": "apps/demo.yaml"}),
        (7, 3, 1, "Resolving", {"count": "2/3", "path": "apps/other.yaml"}),
    ]
    assert lifecycle[-1] == "stop"


def test_render_update_line_colors_chart_filename_and_new_version() -> None:
    rendered = cli.render_update_line(
        cli.PlannedChartUpdate(
            path=Path("/repo/apps/base/paperless-ngx/release.yaml"),
            document_index=0,
            target_name="paperless-ngx",
            chart_name="paperless-ngx",
            repo_name="truecharts",
            current_version="12.0.0",
            latest_version="12.1.0",
            inherited_source=False,
        ),
        repo_root=Path("/repo"),
    )

    assert "[cyan]apps/base/paperless-ngx/release.yaml[/cyan]" in rendered
    assert "[green]12.1.0[/green]" in rendered


def test_render_update_line_colors_deployment_filename_and_new_version() -> None:
    rendered = cli.render_update_line(
        cli.PlannedDeploymentUpdate(
            path=Path("/repo/apps/base/sonarr/deployment.yaml"),
            document_index=0,
            target_name="sonarr",
            yaml_path="spec.template.spec.containers[0].image",
            current_image="linuxserver/sonarr:4.0.16",
            latest_image="linuxserver/sonarr:4.0.17",
            current_version="4.0.16",
            latest_version="4.0.17",
        ),
        repo_root=Path("/repo"),
    )

    assert "[cyan]apps/base/sonarr/deployment.yaml[/cyan]" in rendered
    assert "[green]4.0.17[/green]" in rendered


def test_build_update_prompt_colors_filename_and_new_version() -> None:
    prompt = cli.build_update_prompt(
        cli.PlannedChartUpdate(
            path=Path("/repo/apps/base/radarr/release.yaml"),
            document_index=0,
            target_name="radarr",
            chart_name="radarr",
            repo_name="truecharts",
            current_version="27.0.0",
            latest_version="27.0.2",
            inherited_source=False,
        ),
        repo_root=Path("/repo"),
    )

    assert "[cyan]apps/base/radarr/release.yaml[/cyan]" in prompt
    assert "[green]27.0.2[/green]" in prompt


def test_prompt_for_update_approval_renders_with_rich_console(monkeypatch: pytest.MonkeyPatch) -> None:
    printed: list[tuple[str, str]] = []

    class FakeConsole:
        def print(self, text: str, *, end: str = "\n") -> None:
            printed.append((text, end))

    monkeypatch.setattr(cli, "_ERR_CONSOLE", FakeConsole())
    monkeypatch.setattr(cli, "read_single_keypress", lambda: "\n")

    approved = cli.prompt_for_update_approval("[cyan]demo[/cyan]? [y/N] ")

    assert approved is False
    assert printed == [("[cyan]demo[/cyan]? [y/N] ", "")]


def test_read_single_keypress_uses_click_when_stdin_is_a_tty(monkeypatch: pytest.MonkeyPatch) -> None:
    class FakeStdin:
        def isatty(self) -> bool:
            return True

    monkeypatch.setattr(cli.sys, "stdin", FakeStdin())
    monkeypatch.setattr(cli.click, "getchar", lambda: "y")

    assert cli.read_single_keypress() == "y"


def test_update_helm_single_interrupt_stops_progress_and_exits_cleanly(
    fixture_repo_root: Path,
    cli_runner: CliRunner,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    finished: list[bool] = []

    monkeypatch.setattr(cli, "build_chart_resolver", lambda: StubResolver({}))
    monkeypatch.setattr(cli, "build_image_resolver", lambda: StubImageResolver({}))
    monkeypatch.setattr(cli, "plan_updates", lambda *args, **kwargs: (_ for _ in ()).throw(KeyboardInterrupt()))
    monkeypatch.setattr(
        cli,
        "finish_progress",
        lambda *, enabled: finished.append(enabled),
    )

    result = cli_runner.invoke(app, ["update-helm", str(fixture_repo_root)])

    assert result.exit_code == 130
    assert finished == [True]
    assert "Cancelled." in result.stderr


def copy_fixture_repo(tmp_path: Path, fixture_repo_root: Path) -> Path:
    import shutil

    repo_root = tmp_path / "kubeflux"
    shutil.copytree(
        fixture_repo_root,
        repo_root,
        ignore=shutil.ignore_patterns(".uv-cache", ".venv", ".pytest_cache", ".ruff_cache", "__pycache__"),
    )
    return repo_root
