# fluxrepo-update

Inspect FluxCD repositories and update explicit Helm chart and container-image versions.
No `helm` or `yq` required.

The CLI finds chart versions in `HelmRelease` manifests, images in standard Kubernetes
workloads, and image values under `HelmRelease.spec.values`. It resolves public HTTP/OCI
sources and edits approved YAML scalars while preserving comments and formatting.

## Install

Download a prebuilt Linux binary from the
[latest release](https://github.com/mathiasringhof/fluxrepo-update/releases/latest):

```sh
# x86_64; use aarch64-unknown-linux-gnu on 64-bit ARM
curl -LO https://github.com/mathiasringhof/fluxrepo-update/releases/latest/download/fluxrepo-update-x86_64-unknown-linux-gnu.tar.gz
tar -xzf fluxrepo-update-x86_64-unknown-linux-gnu.tar.gz
sudo install fluxrepo-update /usr/local/bin/
```

To build from source, see [development](docs/development.md).
`cargo test --locked` includes the passing synthetic scenarios, using temporary
repositories and local HTTP fixtures without contacting public registries.

## Start with a preview

```sh
fluxrepo-update inventory /path/to/flux-repo
fluxrepo-update update-helm /path/to/flux-repo --non-interactive
```

Inventory works offline. Update planning needs network access for chart indexes and
registry tags. Both commands accept `--json` for structured output.

To review and approve each update interactively:

```sh
fluxrepo-update update-helm /path/to/flux-repo
```

For automation, `--non-interactive` only prints the plan. Add `--write` to apply it;
use `--apply-id` to select reviewed items. See [usage](docs/usage.md) for these workflows
and [output](docs/output.md#exit-codes) for exit codes (`10` means updates are available;
`20` means updates were applied).

## Scope

The tool reads individual repository manifests, including inactive bases and untracked
YAML; it does not render Kustomize overlays or inspect deployed resources. Latest stable
selection can cross major versions and does not check upgrade compatibility.

Generated Flux bootstrap manifests and unsupported declarations remain unchanged.
Inspect skipped targets: no planned updates does not mean every dependency was checked.
See [coverage](docs/coverage.md) for supported forms, skip boundaries, and write recovery.

## Reference

- [Usage](docs/usage.md): modes, selected updates, and recovery
- [Output](docs/output.md): JSON fields, reason codes, and exit codes
- [Coverage](docs/coverage.md): supported inputs and safety limits
- [Development](docs/development.md): build, test, and code navigation
- [kubeflux audit](docs/kubeflux-coverage.md): observed gaps and synthetic examples
