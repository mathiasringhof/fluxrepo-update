mod common;

use common::{ResponseSpec, TestHttpServer};
use fluxrepo_update::github::{
    GitHubReleaseResolver, GitHubResolverError, RemoteResourceVersionResolver,
    parse_github_resource,
};
use serde_json::{Value, json};

#[test]
fn github_resource_parser_preserves_url_spelling_and_replaces_only_the_pin() {
    let cases = [
        (
            "https://github.com/Owner/Project/config/default?ref=v1.2.3",
            "Owner",
            "Project",
            "v1.2.3",
            None,
            "https://github.com/Owner/Project/config/default?ref=v2.0.0",
        ),
        (
            "GitHub.COM/Owner/Project.git//Config/Base/?ref=1.2",
            "Owner",
            "Project",
            "1.2",
            None,
            "GitHub.COM/Owner/Project.git//Config/Base/?ref=v2.0.0",
        ),
        (
            "https://GitHub.COM/Owner/Project/releases/download/v1.2.3/Install.yaml",
            "Owner",
            "Project",
            "v1.2.3",
            Some("Install.yaml"),
            "https://GitHub.COM/Owner/Project/releases/download/v2.0.0/Install.yaml",
        ),
        (
            "https://github.com/owner/project?ref=v1.2.3",
            "owner",
            "project",
            "v1.2.3",
            None,
            "https://github.com/owner/project?ref=v2.0.0",
        ),
    ];

    for (value, owner, repository, current, asset, proposed) in cases {
        let reference = parse_github_resource(value).expect("parse GitHub pin");
        assert_eq!(reference.owner, owner, "{value}");
        assert_eq!(reference.repository, repository, "{value}");
        assert_eq!(reference.current_version, current, "{value}");
        assert_eq!(reference.required_asset.as_deref(), asset, "{value}");
        assert_eq!(reference.with_version("v2.0.0"), proposed, "{value}");
    }
}

#[test]
fn github_resource_parser_rejects_unsupported_or_ambiguous_values() {
    for value in [
        "../base",
        "https://example.com/owner/project/base?ref=v1.2.3",
        "https://github.example.com/owner/project/base?ref=v1.2.3",
        "http://github.com/owner/project/base?ref=v1.2.3",
        "git::https://github.com/owner/project/base?ref=v1.2.3",
        "ssh://git@github.com/owner/project/base?ref=v1.2.3",
        "https://token@github.com/owner/project/base?ref=v1.2.3",
        "https://github.com:443/owner/project/base?ref=v1.2.3",
        "https://github.com/owner/project/base",
        "https://github.com/owner/project/base?ref=main",
        "https://github.com/owner/project/base?ref=deadbeef",
        "https://github.com/owner/project/base?ref=1",
        "https://github.com/owner/project/base?ref=v2.0.0-rc.1",
        "https://github.com/owner/project/base?ref=v2.0.0+build.1",
        "https://github.com/owner/project/base?ref=v1.2.3&ref=v2.0.0",
        "https://github.com/owner/project/base?ref=v1.2.3&timeout=60s",
        "https://github.com/owner/project/base?ref=v1.2.3#fragment",
        "https://github.com/owner/project/base?ref=v1%2e2%2e3",
        "https://github.com/owner/project/../other?ref=v1.2.3",
        "https://github.com/owner/project/releases/download/main/install.yaml",
        "https://github.com/owner/project/releases/download/v1.2.3/install.yaml?raw=1",
        "https://github.com/owner/project/releases/download/v1.2.3/dir/install.yaml",
        "https://github.com/owner/project/releases/download/v1.2.3/",
        "https://github.com//project/base?ref=v1.2.3",
        " https://github.com/owner/project/base?ref=v1.2.3",
    ] {
        assert!(parse_github_resource(value).is_none(), "{value}");
    }
}

#[test]
fn github_release_resolver_selects_the_newest_numeric_stable_release() {
    let mut draft = release("v9.0.0", &[]);
    draft["draft"] = json!(true);
    let mut prerelease = release("v8.0.0", &[]);
    prerelease["prerelease"] = json!(true);
    let server = release_server(&[
        release("v1.9.0", &[]),
        draft,
        release("v1.10.0", &[]),
        prerelease,
        release("v7.0.0-rc.1", &[]),
        release("release-6.0.0", &[]),
    ]);
    let resolver = resolver(&server);

    assert_eq!(
        resolver
            .resolve("Owner", "Project", "v1.2.0", &[])
            .expect("stable release"),
        "v1.10.0"
    );
    let requests = server.finish();
    assert_eq!(
        requests[0].path,
        "/repos/Owner/Project/releases?per_page=100"
    );
    assert_eq!(
        requests[0].header("Accept"),
        Some("application/vnd.github+json")
    );
    assert!(requests[0].header("User-Agent").is_some());
}

#[test]
fn github_release_resolver_requires_all_assets_and_caches_project_metadata() {
    let server = release_server(&[
        release("v3.0.0", &["Install.yaml"]),
        release("v2.0.0", &["Install.yaml", "Namespace.yaml"]),
        release("v1.0.0", &["Install.yaml", "Namespace.yaml"]),
    ]);
    let resolver = resolver(&server);
    let assets = vec!["Install.yaml".to_string(), "Namespace.yaml".to_string()];
    assert_eq!(
        resolver
            .resolve("Owner", "Project", "v1.0.0", &assets)
            .expect("common release"),
        "v2.0.0"
    );
    assert_eq!(
        resolver
            .resolve("owner", "project", "v1.0.0", &[])
            .expect("cached metadata for another target"),
        "v3.0.0"
    );
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn github_release_resolver_distinguishes_missing_assets_from_current_assets() {
    let server = release_server(&[
        release("v2.0.0", &["different.yaml"]),
        release("v1.0.0", &["install.yaml"]),
    ]);
    let resolver = resolver(&server);
    assert_eq!(
        resolver
            .resolve("owner", "project", "v1.0.0", &["install.yaml".to_string()])
            .expect("eligible current release"),
        "v1.0.0"
    );
    let error = resolver
        .resolve("owner", "project", "v1.0.0", &["Install.yaml".to_string()])
        .expect_err("asset names are case sensitive");
    let error = github_error(&error);
    assert_eq!(error.reason_code(), "github_release_assets_missing");
    assert!(!error.retryable());
    assert!(error.source_url().is_some());
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn github_release_resolver_never_downgrades_or_rewrites_an_equivalent_pin() {
    let server = release_server(&[release("v1.2.0", &[])]);
    let resolver = resolver(&server);
    for current in ["v9.0.0", "1.2", "v1.2.0"] {
        assert_eq!(
            resolver
                .resolve("owner", "project", current, &[])
                .expect("current pin"),
            current
        );
    }
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn github_release_resolver_follows_trusted_pagination_before_selecting_assets() {
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(200, json!([release("v3.0.0", &["first.yaml"])]).to_string())
            .header("Link", "<?per_page=100&page=2>; rel=next"),
        ResponseSpec::new(
            200,
            json!([release("v2.0.0", &["first.yaml", "second.yaml"])]).to_string(),
        ),
    ]);
    let resolver = resolver(&server);
    assert_eq!(
        resolver
            .resolve(
                "owner",
                "project",
                "v1.0.0",
                &["first.yaml".to_string(), "second.yaml".to_string()],
            )
            .expect("paginated common release"),
        "v2.0.0"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].path,
        "/repos/owner/project/releases?per_page=100&page=2"
    );
}

#[test]
fn github_release_resolver_rejects_untrusted_or_cyclic_pagination() {
    for link in [
        "<https://example.invalid/releases?page=2>; rel=next",
        "<{base_url}/repos/owner/project/releases?per_page=100>; rel=next",
        "<http://user@{host}/repos/owner/project/releases?page=2>; rel=next",
    ] {
        let server = TestHttpServer::new(vec![
            ResponseSpec::new(200, json!([release("v1.0.0", &[])]).to_string())
                .header("Link", link),
        ]);
        let error = resolver(&server)
            .resolve("owner", "project", "v1.0.0", &[])
            .expect_err("untrusted pagination");
        let error = github_error(&error);
        assert_eq!(error.reason_code(), "github_release_metadata_invalid");
        assert!(!error.retryable());
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn github_release_resolver_default_client_does_not_follow_redirects() {
    let destination = TestHttpServer::new(Vec::new());
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(302, "redirect").header("Location", &destination.base_url),
    ]);
    let error = resolver(&server)
        .resolve("owner", "project", "v1.0.0", &[])
        .expect_err("redirect rejection");
    assert_eq!(github_error(&error).reason_code(), "github_request_failed");
    assert!(!github_error(&error).retryable());
    assert_eq!(server.finish().len(), 1);
    assert!(destination.finish().is_empty());
}

#[test]
fn github_release_resolver_caches_typed_request_failures_case_insensitively() {
    for (status, rate_limited, retryable) in [
        (401, false, false),
        (403, false, false),
        (403, true, true),
        (404, false, false),
        (408, false, true),
        (429, false, true),
        (500, false, true),
    ] {
        let response = ResponseSpec::new(status, "request failed");
        let response = if rate_limited {
            response.header("X-RateLimit-Remaining", "0")
        } else {
            response
        };
        let server = TestHttpServer::new(vec![response]);
        let resolver = resolver(&server);
        for (owner, project) in [("Owner", "Project"), ("owner", "project")] {
            let error = resolver
                .resolve(owner, project, "v1.0.0", &[])
                .expect_err("HTTP failure");
            let error = github_error(&error);
            assert_eq!(error.reason_code(), "github_request_failed");
            assert_eq!(error.retryable(), retryable, "HTTP {status}");
            assert_eq!(
                error.source_url(),
                Some(
                    format!(
                        "{}/repos/Owner/Project/releases?per_page=100",
                        server.base_url
                    )
                    .as_str()
                )
            );
        }
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn github_release_resolver_reports_malformed_or_absent_metadata_as_unchecked() {
    for (body, code) in [
        ("not JSON", "github_release_metadata_invalid"),
        ("{}", "github_release_metadata_invalid"),
        ("[null]", "github_release_metadata_invalid"),
        (
            r#"[{"tag_name":"v1.0.0"}]"#,
            "github_release_metadata_invalid",
        ),
        ("[]", "github_release_version_unavailable"),
    ] {
        let server = TestHttpServer::new(vec![ResponseSpec::new(200, body)]);
        let error = resolver(&server)
            .resolve("owner", "project", "v1.0.0", &[])
            .expect_err("unusable metadata");
        let error = github_error(&error);
        assert_eq!(error.reason_code(), code, "{body}");
        assert!(!error.retryable());
        assert!(error.source_url().is_some());
        server.finish();
    }
}

#[test]
fn github_release_resolver_rejects_unsupported_pins_before_network() {
    let server = TestHttpServer::new(Vec::new());
    let resolver = resolver(&server);
    for current in ["main", "v1.0.0-rc.1", "deadbeef", "1"] {
        let error = resolver
            .resolve("owner", "project", current, &[])
            .expect_err("unsupported pin");
        assert_eq!(
            github_error(&error).reason_code(),
            "incompatible_version_scheme"
        );
        assert!(github_error(&error).source_url().is_none());
    }
    assert!(server.finish().is_empty());
}

#[test]
fn concurrent_github_resolves_share_one_project_download() {
    let server = release_server(&[release("v2.0.0", &[])]);
    let resolver = resolver(&server);
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    resolver.resolve("owner", "project", "v1.0.0", &[])
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            assert_eq!(handle.join().expect("worker").expect("release"), "v2.0.0");
        }
    });
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn github_release_resolver_uses_the_injected_http_client() {
    let server = release_server(&[release("v2.0.0", &[])]);
    let client = reqwest::blocking::Client::builder()
        .user_agent("fixture-client")
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("fixture client");
    let resolver = GitHubReleaseResolver::builder()
        .client(client)
        .api_base_url(server.base_url.clone())
        .build();
    assert_eq!(
        resolver
            .resolve("owner", "project", "v1.0.0", &[])
            .expect("injected client"),
        "v2.0.0"
    );
    assert_eq!(
        server.finish()[0].header("User-Agent"),
        Some("fixture-client")
    );
}

#[test]
fn github_release_resolver_does_not_treat_a_malformed_next_link_as_the_last_page() {
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(200, json!([release("v1.0.0", &[])]).to_string())
            .header("Link", r#"<http://[invalid]>; rel="next""#),
    ]);
    let error = resolver(&server)
        .resolve("owner", "project", "v1.0.0", &[])
        .expect_err("malformed next page must remain unchecked");
    assert_eq!(
        github_error(&error).reason_code(),
        "github_release_metadata_invalid"
    );
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn github_release_resolver_rejects_pagination_to_another_project_or_port() {
    for link in [
        "<{base_url}/repos/different/project/releases?page=2>; rel=next",
        "<http://127.0.0.1:1/repos/owner/project/releases?page=2>; rel=next",
    ] {
        let server = TestHttpServer::new(vec![
            ResponseSpec::new(200, json!([release("v1.0.0", &[])]).to_string())
                .header("Link", link),
        ]);
        let error = resolver(&server)
            .resolve("owner", "project", "v1.0.0", &[])
            .expect_err("release endpoint changed");
        assert_eq!(
            github_error(&error).reason_code(),
            "github_release_metadata_invalid"
        );
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn github_release_resolver_checks_metadata_before_reporting_current() {
    for malformed in [
        json!({"tag_name": "v1.0.0", "draft": false, "prerelease": false}),
        json!({"tag_name": "v1.0.0", "draft": false, "prerelease": false, "assets": null}),
        json!({"tag_name": "v1.0.0", "draft": false, "prerelease": false, "assets": [{}]}),
        json!({"tag_name": "v1.0.0", "draft": false, "prerelease": false, "assets": [{"name": ""}]}),
        json!({"tag_name": "v1.0.0", "draft": false, "prerelease": false, "assets": [{"name": "install.yaml", "browser_download_url": null}]}),
    ] {
        let server = release_server(&[release("v1.0.0", &[]), malformed]);
        let resolver = resolver(&server);
        for _ in 0..2 {
            let error = resolver
                .resolve("owner", "project", "v1.0.0", &[])
                .expect_err("incomplete release metadata");
            assert_eq!(
                github_error(&error).reason_code(),
                "github_release_metadata_invalid"
            );
            assert!(!github_error(&error).retryable());
        }
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn github_release_resolver_accepts_an_exact_asset_download_url() {
    let asset_url = "https://github.com/owner/project/releases/download/v2.0.0/install.yaml";
    let mut candidate = release("v2.0.0", &["install.yaml"]);
    candidate["assets"][0]["browser_download_url"] = json!(asset_url);
    let server = release_server(&[candidate]);
    assert_eq!(
        resolver(&server)
            .resolve("owner", "project", "v1.0.0", &[asset_url.to_string()])
            .expect("exact asset URL"),
        "v2.0.0"
    );
    server.finish();
}

#[test]
fn github_release_resolver_marks_retry_after_rate_limits_retryable() {
    let server = TestHttpServer::new(vec![
        ResponseSpec::new(403, "rate limited").header("Retry-After", "60"),
    ]);
    let error = resolver(&server)
        .resolve("owner", "project", "v1.0.0", &[])
        .expect_err("rate limit");
    assert!(github_error(&error).retryable());
    server.finish();
}

fn release(tag: &str, assets: &[&str]) -> Value {
    json!({
        "tag_name": tag,
        "draft": false,
        "prerelease": false,
        "assets": assets.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
    })
}

fn release_server(releases: &[Value]) -> TestHttpServer {
    TestHttpServer::new(vec![ResponseSpec::new(200, json!(releases).to_string())])
}

fn resolver(server: &TestHttpServer) -> GitHubReleaseResolver {
    GitHubReleaseResolver::builder()
        .api_base_url(server.base_url.clone())
        .build()
}

fn github_error(error: &anyhow::Error) -> &GitHubResolverError {
    error.downcast_ref().expect("typed GitHub resolver error")
}
