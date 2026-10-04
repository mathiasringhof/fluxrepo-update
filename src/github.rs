use std::cmp::Ordering;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use anyhow::Result;
use reqwest::blocking::Client;
use serde_json::Value;
use url::Url;

use crate::resolvers::{MetadataCache, ResolverError, ResolverErrorCode, next_link_target};

const MAX_RELEASE_PAGES: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubResourceReference {
    pub owner: String,
    pub repository: String,
    pub current_version: String,
    pub required_asset: Option<String>,
    original: String,
    version_range: Range<usize>,
}

impl GitHubResourceReference {
    pub fn with_version(&self, version: &str) -> String {
        let mut result = self.original.clone();
        result.replace_range(self.version_range.clone(), version);
        result
    }
}

pub fn parse_github_resource(value: &str) -> Option<GitHubResourceReference> {
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
        || value.contains(['\\', '%', '#'])
    {
        return None;
    }
    let (address, prefix_length) = if let Some((scheme, address)) = value.split_once("://") {
        if !scheme.eq_ignore_ascii_case("https") {
            return None;
        }
        (address, scheme.len() + 3)
    } else {
        (value, 0)
    };
    let (host, path_and_query) = address.split_once('/')?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path_start = prefix_length + host.len() + 1;
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    let segments = path.split('/').collect::<Vec<_>>();
    let owner = *segments.first()?;
    let raw_repository = *segments.get(1)?;
    let repository = raw_repository
        .strip_suffix(".git")
        .unwrap_or(raw_repository);
    if !valid_project_identity(owner, repository)
        || segments
            .iter()
            .any(|segment| matches!(*segment, "." | ".."))
    {
        return None;
    }
    let (current_version, required_asset, version_start) = if let Some(query) = query {
        let version = query.strip_prefix("ref=")?;
        if segments.get(2) == Some(&"releases") && segments.get(3) == Some(&"download") {
            return None;
        }
        (version, None, value.len() - version.len())
    } else {
        if prefix_length == 0
            || segments.len() != 6
            || segments[2] != "releases"
            || segments[3] != "download"
            || segments[5].is_empty()
        {
            return None;
        }
        let version_start =
            path_start + owner.len() + 1 + raw_repository.len() + "/releases/download/".len();
        (segments[4], Some(segments[5].to_string()), version_start)
    };
    parse_stable_version(current_version)?;
    Some(GitHubResourceReference {
        owner: owner.to_string(),
        repository: repository.to_string(),
        current_version: current_version.to_string(),
        required_asset,
        original: value.to_string(),
        version_range: version_start..version_start + current_version.len(),
    })
}

fn parse_stable_version(value: &str) -> Option<Vec<u64>> {
    let components = value.strip_prefix('v').unwrap_or(value).split('.');
    let numbers = components
        .map(|component| {
            if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            component.parse().ok()
        })
        .collect::<Option<Vec<_>>>()?;
    (numbers.len() >= 2).then_some(numbers)
}

pub trait RemoteResourceVersionResolver {
    fn resolve(
        &self,
        owner: &str,
        repository: &str,
        current_version: &str,
        required_assets: &[String],
    ) -> Result<String>;
}

/// Compatibility name for the shared typed resolver error.
pub type GitHubResolverError = ResolverError;

pub struct GitHubReleaseResolver {
    client: Client,
    api_base_url: String,
    release_cache: MetadataCache<(String, String), Vec<Release>>,
}

impl Default for GitHubReleaseResolver {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl GitHubReleaseResolver {
    pub fn builder() -> GitHubReleaseResolverBuilder {
        GitHubReleaseResolverBuilder::default()
    }

    fn releases_url(&self, owner: &str, repository: &str) -> Result<Url> {
        let source_url = format!(
            "{}/repos/{owner}/{repository}/releases?per_page=100",
            self.api_base_url.trim_end_matches('/')
        );
        let url = Url::parse(&source_url)
            .map_err(|_| metadata_error("invalid GitHub API URL", &self.api_base_url))?;
        if !matches!(url.scheme(), "https" | "http")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query() != Some("per_page=100")
        {
            return Err(metadata_error("invalid GitHub API URL", &self.api_base_url).into());
        }
        Ok(url)
    }

    fn releases(
        &self,
        owner: &str,
        repository: &str,
        source_url: &Url,
    ) -> Result<Arc<Vec<Release>>> {
        let key = (owner.to_ascii_lowercase(), repository.to_ascii_lowercase());
        self.release_cache
            .get_or_fetch(key, || Ok(self.fetch_releases(source_url)?))
    }

    fn fetch_releases(
        &self,
        initial_url: &Url,
    ) -> std::result::Result<Vec<Release>, ResolverError> {
        let mut current_url = initial_url.clone();
        let mut visited = HashSet::new();
        let mut releases = Vec::new();
        loop {
            if !trusted_page(&current_url, initial_url) {
                return Err(metadata_error(
                    "GitHub pagination points outside the trusted release endpoint",
                    current_url.as_str(),
                ));
            }
            begin_page(&mut visited, &current_url)?;
            let response = self
                .client
                .get(current_url.clone())
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .send()
                .map_err(|error| {
                    ResolverError::request_failed(
                        ResolverErrorCode::GitHubRequestFailed,
                        current_url.as_str(),
                        error,
                    )
                })?;
            if response.url() != &current_url {
                return Err(metadata_error(
                    "GitHub API redirects are unsupported",
                    current_url.as_str(),
                ));
            }
            let status = response.status();
            if !status.is_success() {
                let rate_limited = status == reqwest::StatusCode::FORBIDDEN
                    && (response
                        .headers()
                        .contains_key(reqwest::header::RETRY_AFTER)
                        || response
                            .headers()
                            .get("X-RateLimit-Remaining")
                            .and_then(|value| value.to_str().ok())
                            .is_some_and(|remaining| remaining.trim() == "0"));
                return Err(ResolverError::with_source_url(
                    ResolverErrorCode::GitHubRequestFailed,
                    format!("GitHub release request returned HTTP {status}"),
                    rate_limited
                        || status.is_server_error()
                        || status == reqwest::StatusCode::REQUEST_TIMEOUT
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS,
                    current_url.as_str(),
                ));
            }
            let link = response
                .headers()
                .get(reqwest::header::LINK)
                .map(|value| value.to_str())
                .transpose()
                .map_err(|_| {
                    metadata_error("invalid GitHub pagination header", current_url.as_str())
                })?;
            let next_url = next_link_target(link)
                .map(|target| current_url.join(target))
                .transpose()
                .map_err(|_| {
                    metadata_error("invalid GitHub next-page URL", current_url.as_str())
                })?;
            let body = response.bytes().map_err(|error| {
                ResolverError::request_failed(
                    ResolverErrorCode::GitHubRequestFailed,
                    current_url.as_str(),
                    error,
                )
            })?;
            let metadata = serde_json::from_slice::<Value>(&body).map_err(|error| {
                metadata_error(
                    format!("invalid GitHub release JSON: {error}"),
                    current_url.as_str(),
                )
            })?;
            releases.extend(parse_releases(&metadata, current_url.as_str())?);
            let Some(next_url) = next_url else {
                break;
            };
            current_url = next_url;
        }
        Ok(releases)
    }
}

impl RemoteResourceVersionResolver for GitHubReleaseResolver {
    fn resolve(
        &self,
        owner: &str,
        repository: &str,
        current_version: &str,
        required_assets: &[String],
    ) -> Result<String> {
        let current = parse_stable_version(current_version).ok_or_else(|| {
            ResolverError::permanent(
                ResolverErrorCode::IncompatibleVersionScheme,
                format!("GitHub resource pin {current_version} is not a stable numeric version"),
            )
        })?;
        if !valid_project_identity(owner, repository) {
            return Err(ResolverError::permanent(
                ResolverErrorCode::GitHubReleaseMetadataInvalid,
                "invalid GitHub project identity",
            )
            .into());
        }
        let source_url = self.releases_url(owner, repository)?;
        let releases = self.releases(owner, repository, &source_url)?;
        if releases.is_empty() {
            return Err(ResolverError::with_source_url(
                ResolverErrorCode::GitHubReleaseVersionUnavailable,
                format!("no stable numeric GitHub releases found for {owner}/{repository}"),
                false,
                source_url.as_str(),
            )
            .into());
        }
        let selected = releases
            .iter()
            .filter(|release| required_assets.iter().all(|asset| release.assets.contains(asset)))
            .max_by(|left, right| {
                compare_numeric_versions(&left.version, &right.version)
                    .then_with(|| left.tag.cmp(&right.tag))
            })
            .ok_or_else(|| {
                ResolverError::with_source_url(
                    ResolverErrorCode::GitHubReleaseAssetsMissing,
                    format!("no stable GitHub release includes every required asset for {owner}/{repository}"),
                    false,
                    source_url.as_str(),
                )
            })?;
        if compare_numeric_versions(&selected.version, &current).is_gt() {
            Ok(selected.tag.clone())
        } else {
            Ok(current_version.to_string())
        }
    }
}

#[derive(Default)]
pub struct GitHubReleaseResolverBuilder {
    client: Option<Client>,
    api_base_url: Option<String>,
}

impl GitHubReleaseResolverBuilder {
    /// Supplied clients should disable redirects, as the default client does.
    /// Responses whose final URL changed are rejected after the request completes.
    #[must_use]
    pub fn client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    #[must_use]
    pub fn api_base_url(mut self, api_base_url: String) -> Self {
        self.api_base_url = Some(api_base_url);
        self
    }

    pub fn build(self) -> GitHubReleaseResolver {
        GitHubReleaseResolver {
            client: self.client.unwrap_or_else(|| {
                Client::builder()
                    .user_agent(concat!("fluxrepo-update/", env!("CARGO_PKG_VERSION")))
                    .timeout(std::time::Duration::from_secs(20))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("GitHub HTTP client")
            }),
            api_base_url: self
                .api_base_url
                .unwrap_or_else(|| "https://api.github.com".to_string()),
            release_cache: MetadataCache::default(),
        }
    }
}

struct Release {
    tag: String,
    version: Vec<u64>,
    assets: HashSet<String>,
}

fn parse_releases(
    metadata: &Value,
    source_url: &str,
) -> std::result::Result<Vec<Release>, ResolverError> {
    let entries = metadata
        .as_array()
        .ok_or_else(|| metadata_error("GitHub release metadata must be an array", source_url))?;
    let mut releases = Vec::new();
    for entry in entries {
        let tag = entry["tag_name"].as_str();
        let draft = entry["draft"].as_bool();
        let prerelease = entry["prerelease"].as_bool();
        let (Some(tag), Some(draft), Some(prerelease)) = (tag, draft, prerelease) else {
            return Err(metadata_error(
                "invalid GitHub release metadata",
                source_url,
            ));
        };
        if draft || prerelease {
            continue;
        }
        let Some(version) = parse_stable_version(tag) else {
            continue;
        };
        let assets = entry["assets"].as_array().ok_or_else(|| {
            metadata_error(
                "GitHub release metadata is missing its asset list",
                source_url,
            )
        })?;
        let mut names = HashSet::new();
        for asset in assets {
            let Some(name) = asset["name"].as_str().filter(|name| !name.is_empty()) else {
                return Err(metadata_error(
                    "invalid GitHub release asset metadata",
                    source_url,
                ));
            };
            names.insert(name.to_string());
            if let Some(download_url) = asset.get("browser_download_url") {
                let download_url = download_url.as_str().ok_or_else(|| {
                    metadata_error("invalid GitHub asset download URL", source_url)
                })?;
                names.insert(download_url.to_string());
            }
        }
        releases.push(Release {
            tag: tag.to_string(),
            version,
            assets: names,
        });
    }
    Ok(releases)
}

fn metadata_error(message: impl Into<String>, source_url: &str) -> ResolverError {
    ResolverError::with_source_url(
        ResolverErrorCode::GitHubReleaseMetadataInvalid,
        message,
        false,
        source_url,
    )
}

fn trusted_page(candidate: &Url, initial_url: &Url) -> bool {
    candidate.origin() == initial_url.origin()
        && candidate.path() == initial_url.path()
        && candidate.username().is_empty()
        && candidate.password().is_none()
        && candidate.fragment().is_none()
}

fn begin_page(
    visited: &mut HashSet<String>,
    current_url: &Url,
) -> std::result::Result<(), ResolverError> {
    if visited.len() >= MAX_RELEASE_PAGES {
        return Err(metadata_error(
            "GitHub release pagination exceeded its page limit",
            current_url.as_str(),
        ));
    }
    if !visited.insert(current_url.to_string()) {
        return Err(metadata_error(
            "GitHub release pagination cycle",
            current_url.as_str(),
        ));
    }
    Ok(())
}

fn valid_project_identity(owner: &str, repository: &str) -> bool {
    !owner.is_empty()
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !repository.is_empty()
        && !matches!(repository, "." | "..")
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn compare_numeric_versions(left: &[u64], right: &[u64]) -> Ordering {
    (0..left.len().max(right.len()))
        .map(|index| {
            left.get(index)
                .unwrap_or(&0)
                .cmp(right.get(index).unwrap_or(&0))
        })
        .find(|order| !order.is_eq())
        .unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::{HashSet, Url, begin_page};

    #[test]
    fn release_pagination_has_a_finite_page_budget() {
        let mut visited = HashSet::new();
        for page in 1..=1_000 {
            let url = Url::parse(&format!(
                "https://api.github.com/repos/owner/project/releases?page={page}"
            ))
            .expect("release page");
            begin_page(&mut visited, &url).expect("page within budget");
        }
        let next = "https://api.github.com/repos/owner/project/releases?page=1001";
        let error = begin_page(&mut visited, &Url::parse(next).expect("next page"))
            .expect_err("page budget exhausted before another request");
        assert_eq!(error.reason_code(), "github_release_metadata_invalid");
        assert!(!error.retryable());
        assert_eq!(error.source_url(), Some(next));
        assert_eq!(visited.len(), 1_000);
    }
}
