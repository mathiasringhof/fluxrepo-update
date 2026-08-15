# fluxrepo-update

`fluxrepo-update` is a Rust CLI for inspecting a FluxCD repository and updating
explicit Helm chart and container-image versions without depending on `helm` or `yq`.

It discovers manifest-local update targets through supported schemas:

- explicit `HelmRelease.spec.chart.spec.version` values with source identity in the same manifest
- `containers` and `initContainers` images in Deployments, StatefulSets, DaemonSets, Jobs,
  CronJobs, and Pods
- recursive scalar and `repository`/`tag` image bindings under `HelmRelease.spec.values`
- public HTTP Helm repositories, generic public OCI chart repositories, and public container registries

## What It Does

The CLI has two commands:

- `inventory`: scan a repository and report what the tool sees
- `update-helm`: resolve latest stable chart and image versions, show a deterministic plan, and optionally apply it

The tool edits:

- `HelmRelease.spec.chart.spec.version`
- standard workload PodSpec `containers` and `initContainers` image scalars
- concrete scalar images and `image.repository`/`image.tag` mappings under `HelmRelease.spec.values`

It leaves inherited, templated, mutable, tagless, digest-pinned, and unknown image schemas
unchanged. Generated Flux bootstrap manifests are never edited.

## Requirements

Prebuilt Linux binaries do not require Rust. Download the archive for your architecture
from the [latest release](https://github.com/mathiasringhof/fluxrepo-update/releases/latest):

```bash
# x86_64; use aarch64-unknown-linux-gnu on 64-bit ARM
curl -LO https://github.com/mathiasringhof/fluxrepo-update/releases/latest/download/fluxrepo-update-x86_64-unknown-linux-gnu.tar.gz
tar -xzf fluxrepo-update-x86_64-unknown-linux-gnu.tar.gz
sudo install fluxrepo-update /usr/local/bin/
```

Building from source requires:

- Rust `>=1.95`
- Cargo
- network access for `update-helm`, which fetches Helm indexes and OCI/container registry tags

## Quick Start

Inspect a Flux repository:

```bash
cargo run -- inventory /path/to/flux-repo
cargo run -- inventory /path/to/flux-repo --json
```

JSON inventory includes discovered `HelmRepository` sources, update targets, image
references, unresolved targets, and skipped generated manifests.

Preview available updates without changing files:

```bash
cargo run -- update-helm /path/to/flux-repo --non-interactive
cargo run -- update-helm /path/to/flux-repo --json --non-interactive
```

Human `update-helm` output goes to stderr, uses relative paths, includes target details
for each planned update, and shows terminal color/progress when stderr is interactive.
`--json` keeps stdout to indented JSON and disables human progress/color output.
Runtime failures after parsing are JSON objects on stderr when `--json` is present.

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

Apply selected planned updates non-interactively:

```bash
cargo run -- update-helm /path/to/flux-repo --json --non-interactive
cargo run -- update-helm /path/to/flux-repo --write --non-interactive --apply-id '<id-from-plan>'
```

The tests include `tests/fixtures/kubeflux/`, a small fixture distilled from a real Flux
repository. It is used for fixture-backed tests and local examples:

```bash
cargo run -- inventory tests/fixtures/kubeflux --json
cargo run -- update-helm tests/fixtures/kubeflux --json --non-interactive
```

Run the Rust test suite:

```bash
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Enable the local pre-commit hook to run those same checks before each commit:

```bash
git config core.hooksPath .githooks
```

## Safety And Modes

`update-helm` has two distinct modes:

- default human mode is interactive and writes the updates you approve
- `--non-interactive` is agent mode and only prints the plan unless you also pass `--write`

Mode summary:

- Interactive apply:
  - `cargo run -- update-helm /path/to/repo`
  - prompts once per planned update; press `y` or `n` to answer, default is `No`
- Non-interactive plan:
  - `cargo run -- update-helm /path/to/repo --non-interactive`
  - no prompts, no file changes
- Non-interactive apply-all:
  - `cargo run -- update-helm /path/to/repo --write --non-interactive`
  - applies all planned updates without prompts
- Non-interactive apply-selected:
  - inspect `--json --non-interactive`, then pass one or more `--apply-id` values with `--write --non-interactive`
  - applies only the selected planned updates without prompts

Invalid combinations:

- `--write` requires `--non-interactive`
- `--apply-id` requires `--write`

## Exit Codes

- `0`: no updates applied, no updates available, or no updates approved
- `2`: invalid arguments or a runtime/write failure
- `10`: planning mode found updates
- `20`: updates were applied

See [docs/output.md](docs/output.md) for the JSON success and error shapes.

## Current Coverage

The scanner accepts `.yaml` and `.yml`, excludes hidden/cache paths and generated
`flux-system/gotk-*` files, and treats every manifest independently. Resolution is
best-effort: unresolved targets retain stable IDs and reason codes while other updates
remain available. Latest stable selection supports semantic, calendar, and numeric versions,
can cross major versions, excludes prereleases, and never proposes a downgrade.

## Docs

- [Usage](docs/usage.md)
- [Output](docs/output.md)
- [Coverage](docs/coverage.md)
- [Docs Index](docs/README.md)
