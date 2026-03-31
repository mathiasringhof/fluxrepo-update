from __future__ import annotations

import re
import threading
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any
from urllib.parse import urljoin, urlparse

import httpx
from packaging.version import InvalidVersion, Version
from ruamel.yaml import YAML

from fluxrepo_update.models import HelmRepository

_SAFE_YAML = YAML(typ="safe")


class ChartVersionResolver:
    def resolve(
        self,
        repository: HelmRepository,
        chart_name: str,
        current_version: str | None = None,
    ) -> str:
        raise NotImplementedError


class ImageVersionResolver:
    def resolve(self, image: str) -> str:
        raise NotImplementedError


class RepositoryChartResolver(ChartVersionResolver):
    def __init__(self, timeout_seconds: float = 20.0) -> None:
        self._client = httpx.Client(timeout=timeout_seconds, follow_redirects=True)
        self._version_cache: dict[tuple[str, str, str, str, str | None], str] = {}
        self._index_cache: dict[tuple[str, str, str], dict[str, Any]] = {}
        self._cache_lock = threading.Lock()

    def resolve(
        self,
        repository: HelmRepository,
        chart_name: str,
        current_version: str | None = None,
    ) -> str:
        cache_key = (repository.repo_type, repository.name, repository.url, chart_name, current_version)
        with self._cache_lock:
            cached_version = self._version_cache.get(cache_key)
        if cached_version is not None:
            return cached_version

        if repository.repo_type == "oci":
            if repository.name == "truecharts":
                version = self._resolve_truecharts(chart_name)
                with self._cache_lock:
                    self._version_cache[cache_key] = version
                return version
            raise ValueError(f"OCI repository support is not implemented for {repository.name}")

        version = self._resolve_index(repository, chart_name, current_version=current_version)
        with self._cache_lock:
            self._version_cache[cache_key] = version
        return version

    def _resolve_truecharts(self, chart_name: str) -> str:
        url = f"https://raw.githubusercontent.com/truecharts/public/refs/heads/master/charts/stable/{chart_name}/Chart.yaml"
        response = self._client.get(url)
        response.raise_for_status()
        document = _SAFE_YAML.load(response.text)
        version = document.get("version") if isinstance(document, dict) else None
        if not isinstance(version, str):
            raise ValueError(f"Unable to resolve TrueCharts version for {chart_name}")
        return version

    def _resolve_index(
        self,
        repository: HelmRepository,
        chart_name: str,
        current_version: str | None = None,
    ) -> str:
        document = self._load_repository_index(repository)
        if not isinstance(document, dict):
            raise ValueError(f"Repository index for {repository.name} is not a YAML mapping")

        entries = document.get("entries")
        if not isinstance(entries, dict):
            raise ValueError(f"Repository index for {repository.name} does not contain entries")

        chart_entries = entries.get(chart_name)
        if not isinstance(chart_entries, list) or not chart_entries:
            raise ValueError(f"Chart {chart_name} was not found in repository {repository.name}")

        versions = [
            str(entry["version"])
            for entry in chart_entries
            if isinstance(entry, dict) and isinstance(entry.get("version"), str)
        ]
        if not versions:
            raise ValueError(f"Chart {chart_name} in repository {repository.name} has no versions")

        if current_version is not None:
            comparable_versions = select_comparable_versions(current_version, versions)
            if not comparable_versions:
                raise ValueError(f"no comparable chart versions found for current version {current_version}")
            return max(comparable_versions, key=comparable_version_sort_key)

        return max(versions, key=version_sort_key)

    def _load_repository_index(self, repository: HelmRepository) -> dict[str, Any]:
        cache_key = (repository.repo_type, repository.name, repository.url)
        with self._cache_lock:
            cached_index = self._index_cache.get(cache_key)
        if cached_index is not None:
            return cached_index

        index_url = repository.url.rstrip("/") + "/index.yaml"
        response = self._client.get(index_url)
        response.raise_for_status()
        document = _SAFE_YAML.load(response.text)
        if not isinstance(document, dict):
            raise ValueError(f"Repository index for {repository.name} is not a YAML mapping")

        with self._cache_lock:
            self._index_cache[cache_key] = document
        return document


class StaticVersionResolver(ChartVersionResolver):
    def __init__(self, versions: Mapping[tuple[str, str], str]) -> None:
        self._versions = dict(versions)

    def resolve(
        self,
        repository: HelmRepository,
        chart_name: str,
        current_version: str | None = None,
    ) -> str:
        key = (repository.name, chart_name)
        try:
            return self._versions[key]
        except KeyError as exc:
            raise ValueError(f"No static version configured for {repository.name}/{chart_name}") from exc


class StaticImageVersionResolver(ImageVersionResolver):
    def __init__(self, versions: Mapping[str, str]) -> None:
        self._versions = dict(versions)

    def resolve(self, image: str) -> str:
        try:
            return self._versions[image]
        except KeyError as exc:
            raise ValueError(f"No static image version configured for {image}") from exc


@dataclass(frozen=True)
class ParsedImageReference:
    registry: str
    repository: str
    tag: str | None
    digest: str | None
    explicit_registry: bool

    @property
    def api_registry(self) -> str:
        if self.registry == "docker.io":
            return "registry-1.docker.io"
        return self.registry

    def with_tag(self, tag: str) -> str:
        repository = self.repository
        if self.registry == "docker.io" and not self.explicit_registry and repository.startswith("library/"):
            repository = repository.removeprefix("library/")

        base = repository if not self.explicit_registry else f"{self.registry}/{repository}"
        return f"{base}:{tag}"


@dataclass(frozen=True)
class WwwAuthenticateChallenge:
    realm: str
    service: str | None
    scope: str | None


@dataclass(frozen=True)
class TagSignature:
    prefix: str
    core: str
    suffix: str


@dataclass(frozen=True)
class ComparableVersion:
    family: str
    literal_parts: tuple[str, ...]
    numeric_parts: tuple[int, ...]


class RegistryImageResolver(ImageVersionResolver):
    def __init__(self, timeout_seconds: float = 20.0) -> None:
        self._client = httpx.Client(timeout=timeout_seconds, follow_redirects=True)
        self._resolution_cache: dict[str, str] = {}
        self._tag_cache: dict[tuple[str, str], list[str]] = {}
        self._token_cache: dict[tuple[str, str | None, str | None], str] = {}
        self._cache_lock = threading.Lock()

    def resolve(self, image: str) -> str:
        with self._cache_lock:
            cached = self._resolution_cache.get(image)
        if cached is not None:
            return cached

        reference = parse_image_reference(image)
        if reference.digest:
            raise ValueError(f"image digests are not supported for {image}")
        if reference.tag is None:
            raise ValueError(f"image tag is missing for {image}")
        if is_mutable_image_tag(reference.tag):
            raise ValueError(f"image tag {reference.tag} is mutable")
        if looks_like_commit_tag(reference.tag):
            raise ValueError(f"image tag {reference.tag} does not look versioned")

        tags = self._list_tags(reference)
        comparable_tags = select_comparable_tags(reference.tag, tags)
        if not comparable_tags:
            raise ValueError(f"no comparable tags found for {image}")

        latest_tag = max(comparable_tags, key=image_tag_sort_key)
        resolved = reference.with_tag(latest_tag)
        with self._cache_lock:
            self._resolution_cache[image] = resolved
        return resolved

    def _list_tags(self, reference: ParsedImageReference) -> list[str]:
        cache_key = (reference.api_registry, reference.repository)
        with self._cache_lock:
            cached = self._tag_cache.get(cache_key)
        if cached is not None:
            return cached

        url = f"https://{reference.api_registry}/v2/{reference.repository}/tags/list?n=1000"
        tags: list[str] = []
        while url:
            response = self._get_registry(url)
            response.raise_for_status()
            payload = response.json()
            page_tags = payload.get("tags")
            if isinstance(page_tags, list):
                tags.extend(str(tag) for tag in page_tags if isinstance(tag, str))
            url = parse_next_link(response.headers.get("Link"), str(response.url))

        with self._cache_lock:
            self._tag_cache[cache_key] = tags
        return tags

    def _get_registry(self, url: str) -> httpx.Response:
        response = self._client.get(url)
        if response.status_code != 401:
            return response

        challenge = parse_www_authenticate(response.headers.get("WWW-Authenticate"))
        if challenge is None:
            return response

        token = self._get_bearer_token(challenge)
        return self._client.get(url, headers={"Authorization": f"Bearer {token}"})

    def _get_bearer_token(self, challenge: WwwAuthenticateChallenge) -> str:
        cache_key = (challenge.realm, challenge.service, challenge.scope)
        with self._cache_lock:
            cached = self._token_cache.get(cache_key)
        if cached is not None:
            return cached

        params = {}
        if challenge.service:
            params["service"] = challenge.service
        if challenge.scope:
            params["scope"] = challenge.scope

        response = self._client.get(challenge.realm, params=params)
        response.raise_for_status()
        payload = response.json()
        token = payload.get("token") or payload.get("access_token")
        if not isinstance(token, str) or not token:
            raise ValueError(f"token response from {challenge.realm} did not include a bearer token")

        with self._cache_lock:
            self._token_cache[cache_key] = token
        return token


def version_sort_key(version: str) -> tuple[int, Version | tuple[tuple[int, int | str], ...]]:
    normalized = normalize_pep440_candidate(version)
    try:
        return (1, Version(normalized))
    except InvalidVersion:
        return (0, natural_sort_key(normalized))


def normalize_pep440_candidate(version: str) -> str:
    if version[:1].lower() == "v" and len(version) > 1 and version[1].isdigit():
        return version[1:]
    return version


def parse_image_reference(image: str) -> ParsedImageReference:
    name_part, _, digest = image.partition("@")
    last_slash = name_part.rfind("/")
    last_colon = name_part.rfind(":")
    if last_colon > last_slash:
        repository_part = name_part[:last_colon]
        tag = name_part[last_colon + 1 :]
    else:
        repository_part = name_part
        tag = None

    first_segment, _, remainder = repository_part.partition("/")
    explicit_registry = bool(remainder) and (
        "." in first_segment or ":" in first_segment or first_segment == "localhost"
    )

    if explicit_registry:
        registry = first_segment
        repository = remainder
    else:
        registry = "docker.io"
        repository = repository_part

    if registry == "docker.io" and "/" not in repository:
        repository = f"library/{repository}"

    return ParsedImageReference(
        registry=registry,
        repository=repository,
        tag=tag or None,
        digest=digest or None,
        explicit_registry=explicit_registry,
    )


def parse_www_authenticate(header_value: str | None) -> WwwAuthenticateChallenge | None:
    if not header_value:
        return None

    scheme, _, remainder = header_value.partition(" ")
    if scheme.lower() != "bearer" or not remainder:
        return None

    pairs = {
        key: value
        for key, value in re.findall(r'([A-Za-z]+)="([^"]*)"', remainder)
    }
    realm = pairs.get("realm")
    if not realm:
        return None

    return WwwAuthenticateChallenge(
        realm=realm,
        service=pairs.get("service"),
        scope=pairs.get("scope"),
    )


def parse_next_link(link_header: str | None, current_url: str) -> str | None:
    if not link_header:
        return None

    match = re.search(r'<([^>]+)>\s*;\s*rel="next"', link_header)
    if match is None:
        return None

    next_url = match.group(1)
    if next_url.startswith("http://") or next_url.startswith("https://"):
        return next_url

    base_parts = urlparse(current_url)
    if next_url.startswith("/"):
        return f"{base_parts.scheme}://{base_parts.netloc}{next_url}"

    if "?" in next_url and not next_url.startswith("."):
        return f"{base_parts.scheme}://{base_parts.netloc}{base_parts.path}{next_url}"

    return urljoin(current_url, next_url)


def natural_sort_key(value: str) -> tuple[tuple[int, int | str], ...]:
    parts = re.split(r"(\d+)", value.lower())
    return tuple((1, int(part)) if part.isdigit() else (0, part) for part in parts if part)


def image_tag_sort_key(tag: str) -> tuple[int, Version | tuple[tuple[int, int | str], ...]]:
    comparable = parse_comparable_version(tag)
    if comparable is not None and comparable.family in {"date", "pattern"}:
        return (0, comparable.numeric_parts)
    return version_sort_key(tag)


def build_tag_signature(tag: str) -> TagSignature | None:
    digit_positions = [index for index, char in enumerate(tag) if char.isdigit()]
    if not digit_positions:
        return None

    first_digit = digit_positions[0]
    last_digit = digit_positions[-1]
    return TagSignature(
        prefix=tag[:first_digit],
        core=tag[first_digit : last_digit + 1],
        suffix=tag[last_digit + 1 :],
    )


def is_mutable_image_tag(tag: str) -> bool:
    return tag.lower() in {"latest", "main", "master", "edge", "nightly", "stable"}


def looks_like_commit_tag(tag: str) -> bool:
    lowered = tag.lower()
    return bool(re.fullmatch(r"(?:sha|commit|rev)[-_][0-9a-f]{7,}", lowered))


def select_comparable_tags(current_tag: str, tags: list[str]) -> list[str]:
    return [
        tag
        for tag in tags
        if not is_mutable_image_tag(tag)
        and not looks_like_commit_tag(tag)
        and is_comparable_version(current_tag, tag)
    ]


def is_newer_version(current: str, candidate: str) -> bool:
    if not is_comparable_version(current, candidate):
        return False
    return comparable_version_sort_key(candidate) > comparable_version_sort_key(current)


def select_comparable_versions(current_version: str, versions: list[str]) -> list[str]:
    return [version for version in versions if is_comparable_version(current_version, version)]


def comparable_version_sort_key(tag: str) -> Version | tuple[int, ...] | tuple[tuple[int, int | str], ...]:
    comparable = parse_comparable_version(tag)
    if comparable is None:
        return natural_sort_key(tag)
    if comparable.family in {"date", "pattern"}:
        return comparable.numeric_parts

    normalized = normalize_pep440_candidate(tag)
    try:
        return Version(normalized)
    except InvalidVersion:
        return natural_sort_key(normalized)


def is_comparable_version(current: str, candidate: str) -> bool:
    current_version = parse_comparable_version(current)
    candidate_version = parse_comparable_version(candidate)
    if current_version is None or candidate_version is None:
        return current == candidate

    if current_version.family == "date":
        return candidate_version.family == "date"

    if current_version.family == "numeric-series":
        return (
            candidate_version.family in {"numeric-series", "numeric"}
            and len(candidate_version.numeric_parts) >= 2
            and candidate_version.numeric_parts[:2] == current_version.numeric_parts[:2]
        )

    if current_version.family == "numeric":
        return candidate_version.family in {"numeric-series", "numeric"}

    if current_version.family == "pattern":
        return (
            candidate_version.family == "pattern"
            and candidate_version.literal_parts == current_version.literal_parts
            and len(candidate_version.numeric_parts) == len(current_version.numeric_parts)
        )

    return False


def parse_comparable_version(version: str) -> ComparableVersion | None:
    if re.fullmatch(r"\d{8}", version):
        return ComparableVersion(
            family="date",
            literal_parts=("", ""),
            numeric_parts=(int(version),),
        )

    parts = re.split(r"(\d+)", version)
    numeric_parts = tuple(int(part) for part in parts[1::2] if part)
    if not numeric_parts:
        return None

    literal_parts = tuple(part.lower() for part in parts[0::2])
    if is_plain_numeric_version(literal_parts):
        family = "numeric-series" if len(numeric_parts) == 2 else "numeric"
        return ComparableVersion(
            family=family,
            literal_parts=literal_parts,
            numeric_parts=numeric_parts,
        )

    return ComparableVersion(
        family="pattern",
        literal_parts=literal_parts,
        numeric_parts=numeric_parts,
    )


def is_plain_numeric_version(literal_parts: tuple[str, ...]) -> bool:
    if not literal_parts:
        return False

    first_part = literal_parts[0]
    if first_part not in {"", "v"}:
        return False

    return all(part in {"", "."} for part in literal_parts[1:])
