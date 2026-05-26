mod common;

use std::path::PathBuf;

use common::{ResponseSpec, TestHttpServer};
use fluxrepo_update::models::{HelmRepository, RepoType};
use fluxrepo_update::resolvers::{
    ChartVersionResolver, ImageVersionResolver, RegistryImageResolver, RepositoryChartResolver,
    is_newer_version, parse_image_reference, parse_next_link, select_comparable_tags,
};

#[test]
fn comparable_tags_stay_within_numeric_track() {
    let comparable =
        select_comparable_tags("3.22", &["3.22", "3.22.3", "3.23", "20260127", "latest"]);

    assert_eq!(comparable, vec!["3.22", "3.22.3"]);
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
        ("3.22", "3.23.0", false),
        ("v1.2.3", "v1.2.4", true),
        ("1.2.3-alpha.1", "1.2.3-alpha.2", true),
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
fn comparable_tag_selection_filters_mutable_commits_and_other_tracks() {
    let cases = [
        (
            "20250101",
            vec!["20250101", "20250102", "1.2.3", "latest"],
            vec!["20250101", "20250102"],
        ),
        (
            "1.2.3-alpha.1",
            vec![
                "1.2.3-alpha.1",
                "1.2.3-alpha.2",
                "1.2.3-beta.1",
                "commit-deadbee",
            ],
            vec!["1.2.3-alpha.1", "1.2.3-alpha.2"],
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
fn repository_chart_resolver_selects_latest_comparable_chart_version() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(
        200,
        r#"
entries:
  unpoller:
    - version: "2.11.2-Chart5"
    - version: "2.11.2-Chart6"
    - version: "2.1.0"
"#,
    )]);
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("unpoller", &server.base_url, "default");

    let latest = resolver
        .resolve(&repository, "unpoller", Some("2.11.2-Chart5"))
        .expect("resolve comparable chart");

    assert_eq!(latest, "2.11.2-Chart6");
    server.finish();
}

#[test]
fn repository_chart_resolver_rejects_when_no_comparable_chart_versions_exist() {
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
        .expect_err("no comparable chart versions");

    assert!(
        error
            .to_string()
            .contains("no comparable chart versions found")
    );
    server.finish();
}

#[test]
fn repository_chart_resolver_uses_truecharts_oci_special_case() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(200, "version: \"13.3.0\"\n")]);
    let resolver = RepositoryChartResolver::with_truecharts_base_url(&server.base_url);
    let repository = helm_repository("truecharts", "oci://ghcr.io/truecharts/charts", "oci");

    let latest = resolver
        .resolve(&repository, "paperless-ngx", None)
        .expect("resolve truecharts chart");

    assert_eq!(latest, "13.3.0");
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/paperless-ngx/Chart.yaml");
}

#[test]
fn repository_chart_resolver_default_truecharts_url_matches_contract() {
    let resolver = RepositoryChartResolver::default();

    assert_eq!(
        resolver.truecharts_base_url(),
        "https://raw.githubusercontent.com/truecharts/public/refs/heads/master/charts/stable"
    );
}

#[test]
fn repository_chart_resolver_rejects_unsupported_oci_repository() {
    let resolver = RepositoryChartResolver::default();
    let repository = helm_repository("example", "oci://registry.example.com/charts", "oci");

    let error = resolver
        .resolve(&repository, "demo", None)
        .expect_err("unsupported OCI repository");

    assert!(
        error
            .to_string()
            .contains("OCI repository support is not implemented")
    );
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
            "{}/demo/app:3.22.4",
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
