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
| Plan, selection, and application | [`UpdateRun::execute`](../src/update_run.rs) | [Update Run tests](../tests/rust_update_run.rs) |
| Arguments, approval, and output | [`cli::run_with_args`](../src/cli.rs) | [CLI tests](../tests/rust_cli.rs) |
| Full CLI workflows and YAML preservation | [Workflow tests](../tests/rust_workflows.rs) | Local HTTP fixtures and exact file comparisons |

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

The [synthetic corpus](../coverage/kubeflux/README.md) is an opt-in public-CLI exercise
using local HTTP fixtures. It is separate from Cargo tests and CI, and includes desired
capabilities that intentionally report TODO. See its guide for commands, case format,
and runner tests; see the [audit](kubeflux-coverage.md) for provenance and open categories.
Promote a fixed gap with a red/green Rust regression test and the corresponding corpus
expectation. Keep expected results independent of actual CLI output.
