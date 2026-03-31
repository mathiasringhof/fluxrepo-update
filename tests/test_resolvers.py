from __future__ import annotations

from dataclasses import dataclass, field

import pytest

from fluxrepo_update.models import HelmRepository
from fluxrepo_update.resolvers import (
    RegistryImageResolver,
    RepositoryChartResolver,
    is_newer_version,
    parse_next_link,
    select_comparable_tags,
)


class FakeResponse:
    def __init__(self, text: str) -> None:
        self.text = text

    def raise_for_status(self) -> None:
        return None


class FakeClient:
    def __init__(self, response_text: str) -> None:
        self._response_text = response_text
        self.calls: list[str] = []

    def get(self, url: str) -> FakeResponse:
        self.calls.append(url)
        return FakeResponse(self._response_text)


@dataclass
class FakeHTTPResponse:
    text: str = ""
    status_code: int = 200
    headers: dict[str, str] = field(default_factory=dict)
    json_body: dict[str, object] | None = None
    url: str = ""

    def raise_for_status(self) -> None:
        if self.status_code >= 400:
            raise ValueError(f"http {self.status_code}")

    def json(self) -> dict[str, object]:
        return self.json_body or {}


class FakeSequenceClient:
    def __init__(self, responses: list[FakeHTTPResponse]) -> None:
        self._responses = list(responses)
        self.calls: list[tuple[str, dict[str, str] | None, dict[str, str] | None]] = []

    def get(
        self,
        url: str,
        *,
        headers: dict[str, str] | None = None,
        params: dict[str, str] | None = None,
    ) -> FakeHTTPResponse:
        self.calls.append((url, headers, params))
        if not self._responses:
            raise AssertionError("no fake response configured")
        response = self._responses.pop(0)
        if not response.url:
            response.url = url
        return response


def test_repository_chart_resolver_caches_repository_index_between_resolves() -> None:
    resolver = RepositoryChartResolver()
    resolver._client = FakeClient(
        """
entries:
  chart-a:
    - version: "1.0.0"
  chart-b:
    - version: "2.0.0"
"""
    )
    repository = HelmRepository(
        name="example",
        url="https://charts.example.com",
        repo_type="default",
        path=None,  # type: ignore[arg-type]
        document_index=0,
    )

    first = resolver.resolve(repository, "chart-a")
    second = resolver.resolve(repository, "chart-b")

    assert first == "1.0.0"
    assert second == "2.0.0"
    assert resolver._client.calls == ["https://charts.example.com/index.yaml"]


def test_select_comparable_tags_keeps_numeric_series_within_same_track() -> None:
    comparable = select_comparable_tags(
        "3.22",
        ["3.22", "3.22.3", "3.23", "20260127", "latest"],
    )

    assert comparable == ["3.22", "3.22.3"]


def test_is_newer_version_supports_linuxserver_style_version_tags() -> None:
    assert is_newer_version("version-10.0_p1-r10", "version-10.2_p1-r0") is True


def test_is_newer_version_rejects_incompatible_chart_version_scheme_changes() -> None:
    assert is_newer_version("2.11.2-Chart6", "2.1.0") is False


def test_repository_chart_resolver_selects_latest_comparable_chart_version() -> None:
    resolver = RepositoryChartResolver()
    resolver._client = FakeClient(
        """
entries:
  unpoller:
    - version: "2.11.2-Chart5"
    - version: "2.11.2-Chart6"
    - version: "2.1.0"
"""
    )
    repository = HelmRepository(
        name="unpoller",
        url="https://charts.example.com",
        repo_type="default",
        path=None,  # type: ignore[arg-type]
        document_index=0,
    )

    latest = resolver.resolve(repository, "unpoller", current_version="2.11.2-Chart5")

    assert latest == "2.11.2-Chart6"


def test_repository_chart_resolver_rejects_when_no_comparable_chart_versions_exist() -> None:
    resolver = RepositoryChartResolver()
    resolver._client = FakeClient(
        """
entries:
  unpoller:
    - version: "2.1.0"
"""
    )
    repository = HelmRepository(
        name="unpoller",
        url="https://charts.example.com",
        repo_type="default",
        path=None,  # type: ignore[arg-type]
        document_index=0,
    )

    with pytest.raises(ValueError, match="no comparable chart versions found"):
        resolver.resolve(repository, "unpoller", current_version="2.11.2-Chart6")


def test_repository_chart_resolver_uses_truecharts_oci_special_case() -> None:
    resolver = RepositoryChartResolver()
    resolver._client = FakeClient('version: "13.3.0"\n')
    repository = HelmRepository(
        name="truecharts",
        url="oci://ghcr.io/truecharts/charts",
        repo_type="oci",
        path=None,  # type: ignore[arg-type]
        document_index=0,
    )

    latest = resolver.resolve(repository, "paperless-ngx")

    assert latest == "13.3.0"
    assert resolver._client.calls == [
        "https://raw.githubusercontent.com/truecharts/public/refs/heads/master/charts/stable/paperless-ngx/Chart.yaml"
    ]


def test_repository_chart_resolver_rejects_unsupported_oci_repository() -> None:
    resolver = RepositoryChartResolver()
    repository = HelmRepository(
        name="example",
        url="oci://registry.example.com/charts",
        repo_type="oci",
        path=None,  # type: ignore[arg-type]
        document_index=0,
    )

    with pytest.raises(ValueError, match="OCI repository support is not implemented"):
        resolver.resolve(repository, "demo")


@pytest.mark.parametrize(
    ("image", "message"),
    [
        ("alpine@sha256:deadbeef", "image digests are not supported"),
        ("pugmatt/bedrock-connect", "image tag is missing"),
        ("lscr.io/linuxserver/smokeping:latest", "image tag latest is mutable"),
        ("example/app:commit-deadbee", "image tag commit-deadbee does not look versioned"),
    ],
)
def test_registry_image_resolver_rejects_unsupported_image_references(image: str, message: str) -> None:
    resolver = RegistryImageResolver()

    with pytest.raises(ValueError, match=message):
        resolver.resolve(image)


def test_registry_image_resolver_uses_bearer_token_and_paginates_tags() -> None:
    resolver = RegistryImageResolver()
    resolver._client = FakeSequenceClient(
        [
            FakeHTTPResponse(
                status_code=401,
                headers={
                    "WWW-Authenticate": 'Bearer realm="https://auth.example.com/token",service="registry.example.com",scope="repository:demo/app:pull"'
                },
            ),
            FakeHTTPResponse(json_body={"token": "secret-token"}),
            FakeHTTPResponse(
                json_body={"tags": ["3.22", "latest"]},
                headers={"Link": '</v2/demo/app/tags/list?n=1000&last=3.22>; rel="next"'},
            ),
            FakeHTTPResponse(json_body={"tags": ["3.22.4", "3.23.0"]}),
        ]
    )

    latest = resolver.resolve("registry.example.com/demo/app:3.22")

    assert latest == "registry.example.com/demo/app:3.22.4"
    assert resolver._client.calls == [
        ("https://registry.example.com/v2/demo/app/tags/list?n=1000", None, None),
        (
            "https://auth.example.com/token",
            None,
            {"service": "registry.example.com", "scope": "repository:demo/app:pull"},
        ),
        (
            "https://registry.example.com/v2/demo/app/tags/list?n=1000",
            {"Authorization": "Bearer secret-token"},
            None,
        ),
        ("https://registry.example.com/v2/demo/app/tags/list?n=1000&last=3.22", None, None),
    ]


def test_registry_image_resolver_caches_resolved_image() -> None:
    resolver = RegistryImageResolver()
    resolver._client = FakeSequenceClient(
        [
            FakeHTTPResponse(json_body={"tags": ["1.2.3", "1.2.4"]}),
        ]
    )

    first = resolver.resolve("registry.example.com/demo/app:1.2.3")
    second = resolver.resolve("registry.example.com/demo/app:1.2.3")

    assert first == "registry.example.com/demo/app:1.2.4"
    assert second == first
    assert resolver._client.calls == [
        ("https://registry.example.com/v2/demo/app/tags/list?n=1000", None, None),
    ]


def test_parse_next_link_supports_relative_query_string() -> None:
    next_url = parse_next_link(
        '</v2/demo/app/tags/list?n=1000&last=1.2.3>; rel="next"',
        "https://registry.example.com/v2/demo/app/tags/list?n=1000",
    )

    assert next_url == "https://registry.example.com/v2/demo/app/tags/list?n=1000&last=1.2.3"
