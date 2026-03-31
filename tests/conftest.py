from __future__ import annotations

from pathlib import Path

import pytest


@pytest.fixture
def fixture_repo_root() -> Path:
    return Path(__file__).resolve().parents[1] / "kubeflux"
