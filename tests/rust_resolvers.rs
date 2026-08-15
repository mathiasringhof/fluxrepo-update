#![allow(clippy::needless_raw_string_hashes)]

mod common;

use std::path::PathBuf;

use common::{ResponseSpec, TestHttpServer};
use fluxrepo_update::models::{HelmRepository, RepoType};
use fluxrepo_update::resolvers::{
    ChartVersionResolver, ImageVersionResolver, RegistryImageResolver, RepositoryChartResolver,
    ResolverError, ResolverErrorCode, is_newer_version, parse_image_reference, parse_next_link,
    select_comparable_tags,
};

#[test]
fn stable_tag_selection_can_cross_numeric_tracks_and_major_versions() {
    let comparable = select_comparable_tags(
        "3.22",
        &[
            "3.22",
            "3.22.3",
            "3.23",
            "4.0.0",
            "20260127",
            "latest",
            "5.0.0-rc.1",
        ],
    );

    assert_eq!(comparable, vec!["3.22", "3.22.3", "3.23", "4.0.0"]);
}

#[test]
fn newer_version_supports_linuxserver_style_tags() {
    assert!(is_newer_version(
        "version-10.0_p1-r10",
        "version-10.2_p1-r0"
    ));
}

#[test]
fn newer_version_rejects_incompatible_chart_scheme_changes() {
    assert!(!is_newer_version("2.11.2-Chart6", "2.1.0"));
}

#[test]
fn version_comparison_stays_within_comparable_families() {
    let cases = [
        ("20250101", "20250102", true),
        ("20250101", "20241231", false),
        ("3.22", "3.22.3", true),
        ("3.22", "3.23.0", true),
        ("3.22", "4.0.0", true),
        ("v1.2.3", "v1.2.4", true),
        ("1.0.0+build.1", "1.1.0", true),
        ("1.0.0-rc.1", "1.0.0", true),
        ("1.0.0-milestone.1", "1.0.0", true),
        ("1.0.0-0", "1.0.0", true),
        ("1.2.3-alpha.1", "1.2.3-alpha.2", false),
        ("1.2.3-alpha.1", "1.2.3-beta.1", false),
        ("version-10.0_p1-r10", "version-10.2_p1-r0", true),
    ];

    for (current, candidate, expected) in cases {
        assert_eq!(
            is_newer_version(current, candidate),
            expected,
            "{candidate} newer than {current}"
        );
    }
}

#[test]
fn repository_chart_resolver_rejects_unknown_repository_types() {
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("demo", "https://example.invalid/charts", "custom");

    let error = resolver
        .resolve(&repository, "demo", Some("1.0.0"))
        .expect_err("unsupported repository type");

    assert_eq!(
        error
            .downcast_ref::<ResolverError>()
            .expect("resolver error")
            .code(),
        ResolverErrorCode::UnsupportedRepositoryType
    );
}

#[test]
fn comparable_tag_selection_filters_mutable_commits_and_other_tracks() {
    let cases = [
        (
            "20250101",
            vec!["20250101", "20250102", "1.2.3", "latest"],
            vec!["20250101", "20250102"],
        ),
        (
            "1.2.3",
            vec!["1.2.3", "1.3.0-rc.1", "commit-deadbee"],
            vec!["1.2.3"],
        ),
    ];

    for (current, tags, expected) in cases {
        assert_eq!(select_comparable_tags(current, &tags), expected);
    }
}

#[test]
fn repository_chart_resolver_caches_repository_index_between_resolves() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        r#"
entries:
  chart-a:
    - version: "1.0.0"
  chart-b:
    - version: "2.0.0"
"#,
    )]);
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("example", &server.base_url, "default");

    let first = resolver
        .resolve(&repository, "chart-a", None)
        .expect("resolve chart-a");
    let second = resolver
        .resolve(&repository, "chart-b", None)
        .expect("resolve chart-b");

    assert_eq!(first, "1.0.0");
    assert_eq!(second, "2.0.0");
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/index.yaml");
}

#[test]
fn repository_chart_resolver_advances_prerelease_to_latest_stable_version() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        r#"
entries:
  demo:
    - version: "1.0.0-milestone.1"
    - version: "1.0.0"
    - version: "2.0.0-rc.1"
"#,
    )]);
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("demo", &server.base_url, "default");

    let latest = resolver
        .resolve(&repository, "demo", Some("1.0.0-milestone.1"))
        .expect("resolve comparable chart");

    assert_eq!(latest, "1.0.0");
    server.finish();
}

#[test]
fn repository_chart_resolver_reports_unfamiliar_version_schemes() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        "entries:\n  demo:\n    - version: foo\n    - version: bar\n",
    )]);
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("demo", &server.base_url, "default");

    let error = resolver
        .resolve(&repository, "demo", Some("foo"))
        .expect_err("unfamiliar version scheme");

    assert_eq!(
        error
            .downcast_ref::<ResolverError>()
            .expect("resolver error")
            .code(),
        ResolverErrorCode::IncompatibleVersionScheme
    );
    server.finish();
}

#[test]
fn repository_chart_resolver_reports_a_newer_semver_prerelease() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        r#"
entries:
  unpoller:
    - version: "2.1.0"
"#,
    )]);
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("unpoller", &server.base_url, "default");

    let error = resolver
        .resolve(&repository, "unpoller", Some("2.11.2-Chart6"))
        .expect_err("current prerelease is newer than the source");

    assert_eq!(
        error
            .downcast_ref::<ResolverError>()
            .expect("resolver error")
            .code(),
        ResolverErrorCode::CurrentVersionNewerThanSource
    );
    server.finish();
}

#[test]
fn repository_chart_resolver_reports_a_current_version_missing_from_the_source() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        "entries:\n  demo:\n    - version: 1.0.0\n    - version: 2.0.0\n",
    )]);
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("demo", &server.base_url, "default");

    let error = resolver
        .resolve(&repository, "demo", Some("1.5.0"))
        .expect_err("missing current version");

    assert_eq!(
        error
            .downcast_ref::<ResolverError>()
            .expect("resolver error")
            .code(),
        ResolverErrorCode::CurrentVersionNotFound
    );
    server.finish();
}

#[test]
fn repository_chart_resolver_resolves_generic_oci_repositories_by_protocol() {
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(200, r#"{"tags":["1.0.0","1.2.0","2.0.0-rc.1","2.0.0"]}"#)
            .header("Content-Type", "application/json"),
    ]);
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository(
        "not-a-vendor-name",
        &format!(
            "oci://{}/charts",
            server.base_url.trim_start_matches("http://")
        ),
        "oci",
    );

    let latest = resolver
        .resolve(&repository, "demo", Some("1.0.0"))
        .expect("resolve generic OCI chart");

    assert_eq!(latest, "2.0.0");
    let requests = server.finish();
    assert_eq!(requests[0].path, "/v2/charts/demo/tags/list?n=1000");
}

#[test]
fn image_reference_parsing_matches_registry_defaults() {
    let cases = [
        (
            "alpine:3.22",
            "docker.io",
            "library/alpine",
            Some("3.22"),
            None,
            false,
            "alpine:3.0.0",
        ),
        (
            "docker.io/library/alpine:3.22",
            "docker.io",
            "library/alpine",
            Some("3.22"),
            None,
            true,
            "docker.io/library/alpine:3.0.0",
        ),
        (
            "registry.example.com/demo/app:1.2.3",
            "registry.example.com",
            "demo/app",
            Some("1.2.3"),
            None,
            true,
            "registry.example.com/demo/app:3.0.0",
        ),
        (
            "localhost:5000/demo/app:1.2.3",
            "localhost:5000",
            "demo/app",
            Some("1.2.3"),
            None,
            true,
            "localhost:5000/demo/app:3.0.0",
        ),
        (
            "example/app:1.2.3@sha256:deadbeef",
            "docker.io",
            "example/app",
            Some("1.2.3"),
            Some("sha256:deadbeef"),
            false,
            "example/app:3.0.0",
        ),
    ];

    for (image, registry, repository, tag, digest, explicit_registry, retagged) in cases {
        let reference = parse_image_reference(image).expect("parse image");
        assert_eq!(reference.registry, registry, "{image} registry");
        assert_eq!(reference.repository, repository, "{image} repository");
        assert_eq!(reference.tag.as_deref(), tag, "{image} tag");
        assert_eq!(reference.digest.as_deref(), digest, "{image} digest");
        assert_eq!(
            reference.explicit_registry, explicit_registry,
            "{image} explicit registry"
        );
        assert_eq!(reference.with_tag("3.0.0"), retagged, "{image} retag");
    }
}

#[test]
fn next_link_supports_relative_query_string() {
    let next_url = parse_next_link(
        Some("</v2/demo/app/tags/list?n=1000&last=1.2.3>; rel=\"next\""),
        "https://registry.example.com/v2/demo/app/tags/list?n=1000",
    );

    assert_eq!(
        next_url.as_deref(),
        Some("https://registry.example.com/v2/demo/app/tags/list?n=1000&last=1.2.3")
    );
}

#[test]
fn registry_resolver_rejects_unsupported_image_references_before_network() {
    let resolver = RegistryImageResolver::default();

    assert!(
        resolver
            .resolve("alpine@sha256:deadbeef")
            .expect_err("digest rejection")
            .to_string()
            .contains("image digests are not supported")
    );
    assert!(
        resolver
            .resolve("pugmatt/bedrock-connect")
            .expect_err("missing tag rejection")
            .to_string()
            .contains("image tag is missing")
    );
    assert!(
        resolver
            .resolve("lscr.io/linuxserver/smokeping:latest")
            .expect_err("mutable tag rejection")
            .to_string()
            .contains("image tag latest is mutable")
    );
    assert!(
        resolver
            .resolve("example/app:commit-deadbee")
            .expect_err("commit tag rejection")
            .to_string()
            .contains("does not look versioned")
    );
}

#[test]
fn registry_resolver_reports_templated_images_without_network_access() {
    let resolver = RegistryImageResolver::default();

    let error = resolver
        .resolve("registry.example/{{ .Values.image }}:1.0.0")
        .expect_err("templated image must be unresolved");

    assert_eq!(
        error
            .downcast_ref::<ResolverError>()
            .expect("resolver error")
            .code(),
        ResolverErrorCode::TemplatedImageReference
    );
}

#[test]
fn registry_resolver_uses_bearer_token_and_paginates_tags() {
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(401, "").header(
            "WWW-Authenticate",
            "Bearer realm=\"{base_url}/token\",service=\"{host}\",scope=\"repository:demo/app:pull\"",
        ),
        ResponseSpec::new(200, r#"{"token":"secret-token"}"#)
            .header("Content-Type", "application/json"),
        ResponseSpec::new(200, r#"{"tags":["3.22","latest"]}"#)
            .header("Content-Type", "application/json")
            .header("Link", "</v2/demo/app/tags/list?n=1000&last=3.22>; rel=\"next\""),
        ResponseSpec::new(200, r#"{"tags":["3.22.4","3.23.0"]}"#)
            .header("Content-Type", "application/json"),
    ]);
    let resolver = RegistryImageResolver::default();
    let image = format!(
        "{}/demo/app:3.22",
        server.base_url.trim_start_matches("http://")
    );

    let latest = resolver.resolve(&image).expect("resolve image");

    assert_eq!(
        latest,
        format!(
            "{}/demo/app:3.23.0",
            server.base_url.trim_start_matches("http://")
        )
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].path, "/v2/demo/app/tags/list?n=1000");
    assert!(requests[1].path.starts_with("/token?"));
    assert!(requests[1].path.contains("service="));
    assert!(requests[1].path.contains("scope="));
    assert_eq!(
        requests[2].header("Authorization"),
        Some("Bearer secret-token")
    );
    assert_eq!(requests[3].path, "/v2/demo/app/tags/list?n=1000&last=3.22");
}

#[test]
fn registry_resolver_caches_resolved_image() {
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(200, r#"{"tags":["1.2.3","1.2.4"]}"#)
            .header("Content-Type", "application/json"),
    ]);
    let resolver = RegistryImageResolver::default();
    let image = format!(
        "{}/demo/app:1.2.3",
        server.base_url.trim_start_matches("http://")
    );

    let first = resolver.resolve(&image).expect("resolve image");
    let second = resolver.resolve(&image).expect("resolve cached image");

    assert_eq!(
        first,
        format!(
            "{}/demo/app:1.2.4",
            server.base_url.trim_start_matches("http://")
        )
    );
    assert_eq!(second, first);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v2/demo/app/tags/list?n=1000");
}

fn helm_repository(name: &str, url: &str, repo_type: &str) -> HelmRepository {
    HelmRepository {
        name: name.to_string(),
        url: url.to_string(),
        repo_type: RepoType::from(repo_type),
        path: PathBuf::new(),
        document_index: 0,
    }
}
