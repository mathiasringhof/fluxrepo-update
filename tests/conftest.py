from __future__ import annotations

import shutil
from pathlib import Path

import pytest

_IGNORED_COPY_PATTERNS = shutil.ignore_patterns(
    ".uv-cache",
    ".venv",
    ".pytest_cache",
    ".ruff_cache",
    "__pycache__",
)


@pytest.fixture
def fixture_repo_root() -> Path:
    return Path(__file__).resolve().parent / "fixtures" / "kubeflux"


@pytest.fixture
def copied_fixture_repo(tmp_path: Path, fixture_repo_root: Path) -> Path:
    repo_root = tmp_path / "kubeflux"
    shutil.copytree(fixture_repo_root, repo_root, ignore=_IGNORED_COPY_PATTERNS)
    return repo_root
