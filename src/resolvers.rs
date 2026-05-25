use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use anyhow::{Result, anyhow};
use regex::Regex;
use reqwest::blocking::Client;
use serde_yaml::Value;
use url::Url;

use crate::models::{HelmRepository, RepoType};

type ChartVersionCacheKey = (RepoType, String, String, String, Option<String>);
type ChartIndexCacheKey = (RepoType, String, String);
type BearerTokenCacheKey = (String, Option<String>, Option<String>);

static AUTH_PAIR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"([A-Za-z]+)="([^"]*)""#).expect("auth regex"));
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<([^>]+)>\s*;\s*rel="next""#).expect("link regex"));
static DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{8}$").expect("date regex"));
static PARTS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)").expect("parts regex"));
static COMMIT_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(sha|commit|rev)[-_][0-9a-f]{7,}$").expect("commit regex"));

pub trait ChartVersionResolver {
    fn resolve(
        &self,
        repository: &HelmRepository,
        chart_name: &str,
        current_version: Option<&str>,
    ) -> Result<String>;
}

pub trait ImageVersionResolver {
    fn resolve(&self, image: &str) -> Result<String>;
}

pub struct StaticVersionResolver {
    versions: HashMap<(String, String), String>,
}

impl StaticVersionResolver {
    pub fn new(versions: HashMap<(String, String), String>) -> Self {
        Self { versions }
    }
}

impl ChartVersionResolver for StaticVersionResolver {
    fn resolve(
        &self,
        repository: &HelmRepository,
        chart_name: &str,
        _current_version: Option<&str>,
    ) -> Result<String> {
        self.versions
            .get(&(repository.name.clone(), chart_name.to_string()))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "No static version configured for {}/{}",
                    repository.name,
                    chart_name
                )
            })
    }
}

pub struct StaticImageVersionResolver {
    versions: HashMap<String, String>,
}

impl StaticImageVersionResolver {
    pub fn new(versions: HashMap<String, String>) -> Self {
        Self { versions }
    }
}

impl ImageVersionResolver for StaticImageVersionResolver {
    fn resolve(&self, image: &str) -> Result<String> {
        self.versions
            .get(image)
            .cloned()
            .ok_or_else(|| anyhow!("No static image version configured for {image}"))
    }
}

pub struct RepositoryChartResolver {
    client: Client,
    truecharts_base_url: String,
    version_cache: Mutex<HashMap<ChartVersionCacheKey, String>>,
    index_cache: Mutex<HashMap<ChartIndexCacheKey, Value>>,
}

impl Default for RepositoryChartResolver {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl ChartVersionResolver for RepositoryChartResolver {
    fn resolve(
        &self,
        repository: &HelmRepository,
        chart_name: &str,
        current_version: Option<&str>,
    ) -> Result<String> {
        let cache_key = (
            repository.repo_type.clone(),
            repository.name.clone(),
            repository.url.clone(),
            chart_name.to_string(),
            current_version.map(str::to_string),
        );
        if let Some(version) = self
            .version_cache
            .lock()
            .expect("cache lock")
            .get(&cache_key)
        {
            return Ok(version.clone());
        }

        let version = if repository.repo_type == RepoType::Oci {
            if repository.name == "truecharts" {
                self.resolve_truecharts(chart_name)?
            } else {
                return Err(anyhow!(
                    "OCI repository support is not implemented for {}",
                    repository.name
                ));
            }
        } else {
            self.resolve_index(repository, chart_name, current_version)?
        };
        self.version_cache
            .lock()
            .expect("cache lock")
            .insert(cache_key, version.clone());
        Ok(version)
    }
}

impl RepositoryChartResolver {
    pub fn builder() -> RepositoryChartResolverBuilder {
        RepositoryChartResolverBuilder::default()
    }

    pub fn with_truecharts_base_url(base_url: impl Into<String>) -> Self {
        Self::builder().truecharts_base_url(base_url).build()
    }

    pub fn truecharts_base_url(&self) -> &str {
        &self.truecharts_base_url
    }

    fn resolve_truecharts(&self, chart_name: &str) -> Result<String> {
        let url = format!("{}/{chart_name}/Chart.yaml", self.truecharts_base_url);
        let text = self.client.get(url).send()?.error_for_status()?.text()?;
        let document: Value = serde_yaml::from_str(&text)?;
        document
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Unable to resolve TrueCharts version for {chart_name}"))
    }

    fn resolve_index(
        &self,
        repository: &HelmRepository,
        chart_name: &str,
        current_version: Option<&str>,
    ) -> Result<String> {
        let document = self.load_repository_index(repository)?;
        let entries = document
            .get("entries")
            .and_then(Value::as_mapping)
            .ok_or_else(|| {
                anyhow!(
                    "Repository index for {} does not contain entries",
                    repository.name
                )
            })?;
        let chart_entries = entries
            .get(Value::String(chart_name.to_string()))
            .and_then(Value::as_sequence)
            .ok_or_else(|| {
                anyhow!(
                    "Chart {chart_name} was not found in repository {}",
                    repository.name
                )
            })?;
        let versions = chart_entries
            .iter()
            .filter_map(|entry| {
                entry
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();
        if versions.is_empty() {
            return Err(anyhow!(
                "Chart {chart_name} in repository {} has no versions",
                repository.name
            ));
        }

        let candidates = if let Some(current_version) = current_version {
            let comparable = select_comparable_versions(current_version, &versions);
            if comparable.is_empty() {
                return Err(anyhow!(
                    "no comparable chart versions found for current version {current_version}"
                ));
            }
            comparable
        } else {
            versions
        };

        candidates
            .into_iter()
            .max_by(|left, right| compare_versions(left, right))
            .ok_or_else(|| anyhow!("no chart versions available for {chart_name}"))
    }

    fn load_repository_index(&self, repository: &HelmRepository) -> Result<Value> {
        let cache_key = (
            repository.repo_type.clone(),
            repository.name.clone(),
            repository.url.clone(),
        );
        if let Some(document) = self.index_cache.lock().expect("cache lock").get(&cache_key) {
            return Ok(document.clone());
        }

        let url = format!("{}/index.yaml", repository.url.trim_end_matches('/'));
        let text = self.client.get(url).send()?.error_for_status()?.text()?;
        let document: Value = serde_yaml::from_str(&text)?;
        self.index_cache
            .lock()
            .expect("cache lock")
            .insert(cache_key, document.clone());
        Ok(document)
    }
}

pub struct RepositoryChartResolverBuilder {
    client: Option<Client>,
    truecharts_base_url: String,
}

impl Default for RepositoryChartResolverBuilder {
    fn default() -> Self {
        Self {
            client: None,
            truecharts_base_url:
                "https://raw.githubusercontent.com/truecharts/public/refs/heads/master/charts/stable"
                    .to_string(),
        }
    }
}

impl RepositoryChartResolverBuilder {
    pub fn client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    pub fn truecharts_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.truecharts_base_url = base_url.into();
        self
    }

    pub fn build(self) -> RepositoryChartResolver {
        RepositoryChartResolver {
            client: self.client.unwrap_or_else(default_http_client),
            truecharts_base_url: self.truecharts_base_url.trim_end_matches('/').to_string(),
            version_cache: Mutex::new(HashMap::new()),
            index_cache: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedImageReference {
    pub registry: String,
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
    pub explicit_registry: bool,
}

impl ParsedImageReference {
    pub fn api_registry(&self) -> &str {
        if self.registry == "docker.io" {
            "registry-1.docker.io"
        } else {
            &self.registry
        }
    }

    pub fn with_tag(&self, tag: &str) -> String {
        let mut repository = self.repository.clone();
        if self.registry == "docker.io"
            && !self.explicit_registry
            && repository.starts_with("library/")
        {
            repository = repository.trim_start_matches("library/").to_string();
        }
        let base = if self.explicit_registry {
            format!("{}/{}", self.registry, repository)
        } else {
            repository
        };
        format!("{base}:{tag}")
    }
}

pub fn parse_image_reference(image: &str) -> Result<ParsedImageReference> {
    let (name_part, digest) = image
        .split_once('@')
        .map_or((image, None), |(name, digest)| {
            (name, Some(digest.to_string()))
        });
    let last_slash = name_part.rfind('/');
    let last_colon = name_part.rfind(':');
    let (repository_part, tag) = if last_colon > last_slash {
        let colon = last_colon.expect("colon exists");
        (
            &name_part[..colon],
            Some(name_part[colon + 1..].to_string()),
        )
    } else {
        (name_part, None)
    };

    let (first_segment, remainder) = repository_part
        .split_once('/')
        .map_or((repository_part, ""), |(first, rest)| (first, rest));
    let explicit_registry = !remainder.is_empty()
        && (first_segment.contains('.')
            || first_segment.contains(':')
            || first_segment == "localhost");

    let (registry, mut repository) = if explicit_registry {
        (first_segment.to_string(), remainder.to_string())
    } else {
        ("docker.io".to_string(), repository_part.to_string())
    };
    if registry == "docker.io" && !repository.contains('/') {
        repository = format!("library/{repository}");
    }

    Ok(ParsedImageReference {
        registry,
        repository,
        tag,
        digest,
        explicit_registry,
    })
}

pub struct RegistryImageResolver {
    client: Client,
    resolution_cache: Mutex<HashMap<String, String>>,
    tag_cache: Mutex<HashMap<(String, String), Vec<String>>>,
    token_cache: Mutex<HashMap<BearerTokenCacheKey, String>>,
}

impl Default for RegistryImageResolver {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl ImageVersionResolver for RegistryImageResolver {
    fn resolve(&self, image: &str) -> Result<String> {
        if let Some(cached) = self
            .resolution_cache
            .lock()
            .expect("cache lock")
            .get(image)
            .cloned()
        {
            return Ok(cached);
        }

        let reference = parse_image_reference(image)?;
        if reference.digest.is_some() {
            return Err(anyhow!("image digests are not supported for {image}"));
        }
        let tag = reference
            .tag
            .as_deref()
            .ok_or_else(|| anyhow!("image tag is missing for {image}"))?;
        if is_mutable_image_tag(tag) {
            return Err(anyhow!("image tag {tag} is mutable"));
        }
        if looks_like_commit_tag(tag) {
            return Err(anyhow!("image tag {tag} does not look versioned"));
        }

        let tags = self.list_tags(&reference)?;
        let comparable_tags =
            select_comparable_tags(tag, &tags.iter().map(String::as_str).collect::<Vec<_>>());
        if comparable_tags.is_empty() {
            return Err(anyhow!("no comparable tags found for {image}"));
        }
        let latest_tag = comparable_tags
            .into_iter()
            .max_by(|left, right| compare_versions(left, right))
            .expect("non-empty comparable tags");
        let resolved = reference.with_tag(latest_tag);
        self.resolution_cache
            .lock()
            .expect("cache lock")
            .insert(image.to_string(), resolved.clone());
        Ok(resolved)
    }
}

impl RegistryImageResolver {
    pub fn builder() -> RegistryImageResolverBuilder {
        RegistryImageResolverBuilder::default()
    }

    fn list_tags(&self, reference: &ParsedImageReference) -> Result<Vec<String>> {
        let cache_key = (
            reference.api_registry().to_string(),
            reference.repository.clone(),
        );
        if let Some(cached) = self.tag_cache.lock().expect("cache lock").get(&cache_key) {
            return Ok(cached.clone());
        }

        let mut url = Some(format!(
            "{}://{}/v2/{}/tags/list?n=1000",
            registry_scheme(reference.api_registry()),
            reference.api_registry(),
            reference.repository
        ));
        let mut tags = Vec::new();
        while let Some(current_url) = url {
            let response = self.get_registry(&current_url)?;
            let next_url = parse_next_link(
                response
                    .headers()
                    .get(reqwest::header::LINK)
                    .and_then(|value| value.to_str().ok()),
                response.url().as_str(),
            );
            let payload: serde_json::Value = response.error_for_status()?.json()?;
            if let Some(page_tags) = payload.get("tags").and_then(serde_json::Value::as_array) {
                tags.extend(
                    page_tags
                        .iter()
                        .filter_map(|tag| tag.as_str().map(str::to_string)),
                );
            }
            url = next_url;
        }

        self.tag_cache
            .lock()
            .expect("cache lock")
            .insert(cache_key, tags.clone());
        Ok(tags)
    }

    fn get_registry(&self, url: &str) -> Result<reqwest::blocking::Response> {
        let response = self.client.get(url).send()?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        let Some(challenge) = parse_www_authenticate(
            response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
        ) else {
            return Ok(response);
        };
        let token = self.get_bearer_token(&challenge)?;
        Ok(self.client.get(url).bearer_auth(token).send()?)
    }

    fn get_bearer_token(&self, challenge: &WwwAuthenticateChallenge) -> Result<String> {
        let cache_key = (
            challenge.realm.clone(),
            challenge.service.clone(),
            challenge.scope.clone(),
        );
        if let Some(token) = self.token_cache.lock().expect("cache lock").get(&cache_key) {
            return Ok(token.clone());
        }

        let mut request = self.client.get(&challenge.realm);
        if let Some(service) = &challenge.service {
            request = request.query(&[("service", service)]);
        }
        if let Some(scope) = &challenge.scope {
            request = request.query(&[("scope", scope)]);
        }
        let payload: serde_json::Value = request.send()?.error_for_status()?.json()?;
        let token = payload
            .get("token")
            .or_else(|| payload.get("access_token"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                anyhow!(
                    "token response from {} did not include a bearer token",
                    challenge.realm
                )
            })?
            .to_string();
        self.token_cache
            .lock()
            .expect("cache lock")
            .insert(cache_key, token.clone());
        Ok(token)
    }
}

#[derive(Default)]
pub struct RegistryImageResolverBuilder {
    client: Option<Client>,
}

impl RegistryImageResolverBuilder {
    pub fn client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    pub fn build(self) -> RegistryImageResolver {
        RegistryImageResolver {
            client: self.client.unwrap_or_else(default_http_client),
            resolution_cache: Mutex::new(HashMap::new()),
            tag_cache: Mutex::new(HashMap::new()),
            token_cache: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone)]
struct WwwAuthenticateChallenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

fn parse_www_authenticate(header_value: Option<&str>) -> Option<WwwAuthenticateChallenge> {
    let header_value = header_value?;
    let (scheme, remainder) = header_value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let pairs = AUTH_PAIR_RE
        .captures_iter(remainder)
        .map(|capture| (capture[1].to_string(), capture[2].to_string()))
        .collect::<HashMap<_, _>>();
    Some(WwwAuthenticateChallenge {
        realm: pairs.get("realm")?.clone(),
        service: pairs.get("service").cloned(),
        scope: pairs.get("scope").cloned(),
    })
}

fn registry_scheme(registry: &str) -> &'static str {
    let host = registry
        .strip_prefix('[')
        .and_then(|value| value.split_once(']').map(|(host, _)| host))
        .unwrap_or_else(|| registry.split(':').next().unwrap_or(registry));
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        "http"
    } else {
        "https"
    }
}

pub fn parse_next_link(link_header: Option<&str>, current_url: &str) -> Option<String> {
    let link_header = link_header?;
    let next_url = LINK_RE.captures(link_header)?.get(1)?.as_str();
    if next_url.starts_with("http://") || next_url.starts_with("https://") {
        return Some(next_url.to_string());
    }

    let base = Url::parse(current_url).ok()?;
    if next_url.starts_with('/') {
        return base.join(next_url).ok().map(|url| url.to_string());
    }
    if next_url.contains('?') && !next_url.starts_with('.') {
        return base.join(next_url).ok().map(|url| url.to_string());
    }
    base.join(next_url).ok().map(|url| url.to_string())
}

pub fn select_comparable_tags<'a>(current_tag: &str, tags: &[&'a str]) -> Vec<&'a str> {
    tags.iter()
        .copied()
        .filter(|tag| {
            !is_mutable_image_tag(tag)
                && !looks_like_commit_tag(tag)
                && is_comparable_version(current_tag, tag)
        })
        .collect()
}

pub fn select_comparable_versions(current_version: &str, versions: &[String]) -> Vec<String> {
    versions
        .iter()
        .filter(|version| is_comparable_version(current_version, version))
        .cloned()
        .collect()
}

pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    is_comparable_version(current, candidate) && compare_versions(candidate, current).is_gt()
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left = comparable_version_sort_key(left);
    let right = comparable_version_sort_key(right);
    left.cmp(&right)
}

fn comparable_version_sort_key(version: &str) -> SortKey {
    if let Some(comparable) = parse_comparable_version(version) {
        return SortKey::Numeric(comparable.numeric_parts);
    }
    SortKey::Natural(natural_sort_key(version))
}

fn is_comparable_version(current: &str, candidate: &str) -> bool {
    let current_version = parse_comparable_version(current);
    let candidate_version = parse_comparable_version(candidate);
    let (Some(current_version), Some(candidate_version)) = (current_version, candidate_version)
    else {
        return current == candidate;
    };

    match current_version.family {
        VersionFamily::Date => candidate_version.family == VersionFamily::Date,
        VersionFamily::NumericSeries => {
            matches!(
                candidate_version.family,
                VersionFamily::NumericSeries | VersionFamily::Numeric
            ) && candidate_version.numeric_parts.len() >= 2
                && candidate_version.numeric_parts[..2] == current_version.numeric_parts[..2]
        }
        VersionFamily::Numeric => matches!(
            candidate_version.family,
            VersionFamily::NumericSeries | VersionFamily::Numeric
        ),
        VersionFamily::Pattern => {
            candidate_version.family == VersionFamily::Pattern
                && candidate_version.literal_parts == current_version.literal_parts
                && candidate_version.numeric_parts.len() == current_version.numeric_parts.len()
        }
    }
}

fn parse_comparable_version(version: &str) -> Option<ComparableVersion> {
    if DATE_RE.is_match(version) {
        return Some(ComparableVersion {
            family: VersionFamily::Date,
            literal_parts: vec![String::new(), String::new()],
            numeric_parts: vec![version.parse().ok()?],
        });
    }

    let mut numeric_parts = Vec::new();
    let mut literal_parts = Vec::new();
    let mut last_end = 0;
    for capture in PARTS_RE.find_iter(version) {
        literal_parts.push(version[last_end..capture.start()].to_lowercase());
        numeric_parts.push(capture.as_str().parse().ok()?);
        last_end = capture.end();
    }
    if numeric_parts.is_empty() {
        return None;
    }
    literal_parts.push(version[last_end..].to_lowercase());

    let family = if is_plain_numeric_version(&literal_parts) {
        if numeric_parts.len() == 2 {
            VersionFamily::NumericSeries
        } else {
            VersionFamily::Numeric
        }
    } else {
        VersionFamily::Pattern
    };
    Some(ComparableVersion {
        family,
        literal_parts,
        numeric_parts,
    })
}

fn is_plain_numeric_version(literal_parts: &[String]) -> bool {
    let Some(first_part) = literal_parts.first() else {
        return false;
    };
    if !first_part.is_empty() && first_part != "v" {
        return false;
    }
    literal_parts
        .iter()
        .skip(1)
        .all(|part| part.is_empty() || part == ".")
}

fn is_mutable_image_tag(tag: &str) -> bool {
    matches!(
        tag.to_lowercase().as_str(),
        "latest" | "main" | "master" | "edge" | "nightly" | "stable"
    )
}

fn looks_like_commit_tag(tag: &str) -> bool {
    COMMIT_TAG_RE.is_match(&tag.to_lowercase())
}

fn natural_sort_key(value: &str) -> Vec<NaturalPart> {
    let mut parts = Vec::new();
    let mut last_end = 0;
    for capture in PARTS_RE.find_iter(&value.to_lowercase()) {
        if capture.start() > last_end {
            parts.push(NaturalPart::Text(
                value[last_end..capture.start()].to_lowercase(),
            ));
        }
        if let Ok(number) = capture.as_str().parse() {
            parts.push(NaturalPart::Number(number));
        }
        last_end = capture.end();
    }
    if last_end < value.len() {
        parts.push(NaturalPart::Text(value[last_end..].to_lowercase()));
    }
    parts
}

fn default_http_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("reqwest client")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComparableVersion {
    family: VersionFamily,
    literal_parts: Vec<String>,
    numeric_parts: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionFamily {
    Date,
    NumericSeries,
    Numeric,
    Pattern,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    Numeric(Vec<u64>),
    Natural(Vec<NaturalPart>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum NaturalPart {
    Text(String),
    Number(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_challenges() {
        let challenge = parse_www_authenticate(Some(
            r#"Bearer realm="https://auth.example.test/token",service="registry.example.test",scope="repository:demo/app:pull""#,
        ))
        .expect("challenge");

        assert_eq!(challenge.realm, "https://auth.example.test/token");
        assert_eq!(challenge.service.as_deref(), Some("registry.example.test"));
        assert_eq!(challenge.scope.as_deref(), Some("repository:demo/app:pull"));
    }

    #[test]
    fn local_registries_use_http_for_testability() {
        assert_eq!(registry_scheme("127.0.0.1:5000"), "http");
        assert_eq!(registry_scheme("localhost:5000"), "http");
        assert_eq!(registry_scheme("registry.example.test"), "https");
    }

    #[test]
    fn relative_next_links_preserve_non_default_ports() {
        let next_url = parse_next_link(
            Some(r#"</v2/demo/app/tags/list?n=1000&last=1.2.3>; rel="next""#),
            "http://127.0.0.1:49152/v2/demo/app/tags/list?n=1000",
        );

        assert_eq!(
            next_url.as_deref(),
            Some("http://127.0.0.1:49152/v2/demo/app/tags/list?n=1000&last=1.2.3")
        );
    }
}
