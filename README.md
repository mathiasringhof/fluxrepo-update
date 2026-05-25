# fluxrepo-update

`fluxrepo-update` is a Rust CLI for inspecting a FluxCD repository and updating
Helm chart versions and Deployment image tags without depending on `helm` or `yq`.

It currently updates two manifest types directly:

- discover `HelmRepository` sources and `HelmRelease` update targets
- detect patch manifests that inherit chart metadata from a matching base release
- discover versioned `Deployment` image fields in `containers` and `initContainers`
- update `spec.chart.spec.version` when a newer chart version is available
- update `Deployment` image tags when a newer comparable tag is available

## What It Does

The CLI has two commands:

- `inventory`: scan a repository and report what the tool sees
- `update-helm`: resolve latest chart versions and Deployment image tags, show planned updates, and optionally apply them

The tool edits:

- `HelmRelease.spec.chart.spec.version`
- `Deployment.spec.template.spec.{containers,initContainers}[*].image`

It does not rewrite `spec.values`, mutable image channels such as `latest` or `main`,
digest-pinned image references, or generated Flux bootstrap manifests.

## Requirements

- Rust `>=1.95`
- Cargo
- network access for `update-helm`, which fetches chart metadata from Helm repository `index.yaml`
  files, the TrueCharts GitHub-backed special case, and container registry tag APIs

## Quick Start

Inspect a Flux repository:

```bash
cargo run -- inventory /path/to/flux-repo
cargo run -- inventory /path/to/flux-repo --json
```

Preview available updates without changing files:

```bash
cargo run -- update-helm /path/to/flux-repo --non-interactive
cargo run -- update-helm /path/to/flux-repo --json --non-interactive
```

Human `update-helm` output goes to stderr, uses relative paths, includes target details
for each planned update, and shows terminal color/progress when stderr is interactive.
`--json` keeps stdout to indented JSON and disables human progress/color output.

Apply updates interactively:

```bash
cargo run -- update-helm /path/to/flux-repo
```

Apply all planned updates non-interactively:

```bash
cargo run -- update-helm /path/to/flux-repo --write --non-interactive
```

Apply mode updates the targeted YAML scalar values in place, preserving surrounding
formatting, comments, quote style, and multi-document separators where possible.

The tests include `tests/fixtures/kubeflux/`, a small fixture distilled from a real Flux
repository. It is used for fixture-backed tests and local examples:

```bash
cargo run -- inventory tests/fixtures/kubeflux --json
cargo run -- update-helm tests/fixtures/kubeflux --json --non-interactive
```

Run the Rust test suite:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

## Safety And Modes

`update-helm` has two distinct modes:

- default human mode is interactive and writes the updates you approve
- `--non-interactive` is agent mode and only prints the plan unless you also pass `--write`

Mode summary:

- Interactive apply:
  - `cargo run -- update-helm /path/to/repo`
  - prompts once per planned update, default answer is `No`
- Non-interactive plan:
  - `cargo run -- update-helm /path/to/repo --non-interactive`
  - no prompts, no file changes
- Non-interactive apply-all:
  - `cargo run -- update-helm /path/to/repo --write --non-interactive`
  - applies all planned updates without prompts

Invalid combinations:

- `--write` requires `--non-interactive`

## Exit Codes

- `0`: no updates applied, no updates available, or no updates approved
- `2`: invalid option combination or `--strict` failed because some targets were skipped
- `10`: planning mode found updates
- `20`: updates were applied

## Current Coverage

Updated today:

- `HelmRelease.spec.chart.spec.version` in base manifests such as
  `apps/base/*/release.yaml`
- `HelmRelease.spec.chart.spec.version` in patch manifests such as
  `apps/production/*/release-patch.yaml` when chart metadata is inherited from a matching
  base `HelmRelease`
- `Deployment` image tags in direct workload manifests such as
  `apps/base/sonarr/deployment.yaml` and `apps/production/openssh/deployment.yaml`
- standard Helm repositories resolved through `index.yaml`
- the existing TrueCharts OCI special case
- public registry tags resolved through the OCI registry HTTP API for comparable versioned tags

Not updated today:

- values-only `HelmRelease` overlays
- image references outside `Deployment` container and initContainer fields
- image references inside `HelmRelease.spec.values`
- mutable image tags such as `latest`, `main`, and bare image references without an explicit tag
- digest-pinned image references
- generic OCI repositories other than the TrueCharts special case
- generated Flux manifests under `clusters/*/flux-system/gotk-*`

## Docs

- [Usage](docs/usage.md)
- [Output](docs/output.md)
- [Coverage](docs/coverage.md)
- [Docs Index](docs/README.md)
