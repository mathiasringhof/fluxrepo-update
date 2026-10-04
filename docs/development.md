# Development

## Build and check

Install Rust through rustup; [rust-toolchain.toml](../rust-toolchain.toml) selects the
project's toolchain and components. The minimum Rust version is declared in
[Cargo.toml](../Cargo.toml). No Helm or yq installation is needed.

```sh
cargo build --locked
cargo run -- inventory tests/fixtures/kubeflux --json
```

Use red/green TDD for code changes: reproduce the missing behavior with a failing test,
implement it, then rerun the relevant test. Before committing, run the CI checks:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

To run those checks automatically before each commit:

```sh
git config core.hooksPath .githooks
```

## Find code and tests

Read the [domain guidance](agents/domain.md) before changing architecture, terminology,
or behavior. The public workflow is scan → resolve and plan → review/select → apply.

| Area | Start here | Behavior tests |
| --- | --- | --- |
| Manifest discovery and source equivalence | [`scan_repo`](../src/scanner.rs), [inventory models](../src/models.rs) | [Scanner tests](../tests/rust_scanner.rs) |
| Remote metadata and version selection | [Resolver traits and implementations](../src/resolvers.rs) | [Resolver tests](../tests/rust_resolvers.rs) |
| GitHub resource pins and releases | [GitHub parser and resolver](../src/github.rs) | [GitHub tests](../tests/rust_github.rs), [resource workflow tests](../tests/rust_remote_resources.rs) |
| Plan, selection, and application | [`UpdateRun::execute`](../src/update_run.rs) | [Update Run tests](../tests/rust_update_run.rs) |
| Arguments, approval, and output | [`cli::run_with_args`](../src/cli.rs) | [CLI tests](../tests/rust_cli.rs) |
| Full CLI workflows and YAML preservation | [Workflow tests](../tests/rust_workflows.rs) | Local HTTP fixtures and exact file comparisons |
| Passing synthetic scenarios through the library | [Corpus tests](../tests/rust_corpus.rs) | Shared corpus inputs, local HTTP fixtures, and exact file comparisons |

`UpdateRun` owns workflow decisions and hides concurrency and editing. Keep terminal
presentation in the CLI and registry protocols behind resolver traits. The CLI separates
[approval input](../src/cli/interactive_approval.rs) from [reporting](../src/cli/output.rs).
For the JSON contract and the difference between the full Update Plan and the applied
output subset, see [output](output.md).

Use the existing [test helpers](../tests/common/mod.rs): static resolvers isolate
planning and selection; local HTTP servers exercise requests, authentication, and
pagination deterministically. Tests should cover behavior, not exact CLI help wording.
The small [kubeflux fixture](../tests/fixtures/kubeflux/) supports scanner and workflow
regressions without copying a production checkout.

## Explore coverage gaps

Every passing [synthetic corpus](../coverage/kubeflux/README.md) case runs in `cargo test`
and CI through `scan_repo` and `UpdateRun::execute`. The tests use real resolvers against
temporary local HTTP servers, check domain outcomes, and compare all repository files
byte-for-byte. No CLI process, Python installation, or public registry access is needed.
Run the suite with `cargo test --locked --test rust_corpus`; append a Rust test name
such as `charts_http` to select one case.

The opt-in Python runner still exercises the CLI contract across the entire corpus,
including capabilities that intentionally report TODO. See the corpus guide for its
commands and case format, and the [audit](kubeflux-coverage.md) for provenance and gaps.
When promoting a case to `pass`, add its name to `tests/rust_corpus.rs`; the registration
check fails if any passing case is missing or a registered case becomes TODO. Keep
expected results independent of actual output. New assertion forms must be supported
explicitly by the Rust harness; unsupported checks fail rather than being ignored.
