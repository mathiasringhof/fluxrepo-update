# Coverage

This page describes what `fluxrepo-update` can update directly, what it only inventories,
and what it deliberately skips.

## Updated

- `HelmRelease.spec.chart.spec.version` in base manifests such as
  `apps/base/paperless-ngx/release.yaml`
- `HelmRelease.spec.chart.spec.version` in patch manifests that only carry the version,
  such as `apps/production/paperless/release-patch.yaml`, when chart metadata can be
  inherited from a matching base `HelmRelease`
- `Deployment.spec.template.spec.containers[*].image` and
  `Deployment.spec.template.spec.initContainers[*].image` when the image tag is versioned
  and a newer comparable tag exists in the registry
- chart versions resolved from standard Helm repository `index.yaml`
- the existing TrueCharts OCI special case
- public registry tags resolved through the OCI registry HTTP API

## Inventoried Only

- `HelmRelease` overlays that only carry `spec.values`, such as
  `apps/production/audiobookshelf/release-patch.yaml`
- values files represented as `HelmRelease` resources, such as
  `apps/production/uptimekuma-values.yaml`
- image references in StatefulSets, DaemonSets, Jobs, CronJobs, Pods, and nested `image`
  fields inside other manifests
- image references inside `HelmRelease.spec.values`, such as
  `apps/production/jellyfin/release.yaml`
- bundled or vendored manifests where versioning is not driven by a
  `HelmRepository` and `HelmRelease` pair

## Skipped

- generated Flux bootstrap manifests under `clusters/*/flux-system/gotk-*`
- hidden and local tool cache directories such as `.git`, `.venv`, `.uv-cache`,
  `.pytest_cache`, `.ruff_cache`, and `__pycache__`

## Unsupported

- generic OCI repositories beyond the TrueCharts special case
- mutable image tags such as `latest` and `main`
- digest-pinned or bare image references without an explicit tag
- any mutation outside `HelmRelease.spec.chart.spec.version` and direct `Deployment` image fields
- non-YAML files

## How Inheritance Works

Some overlay patches contain only the version field and omit chart metadata. The scanner
links those overlays back to a matching `HelmRelease` with the same kind, name, and
namespace. When a complete source manifest is found, the patch becomes an update target.

In practice this is what lets the tool update:

- `apps/base/paperless-ngx/release.yaml`
- `apps/production/paperless/release-patch.yaml`
- `apps/staging/paperless-ngx/release-patch.yaml`

even though the overlay files do not repeat the chart name and repository in full.

## Gap Vs `kubeflux/updatehelm.sh`

The legacy shell script only updates manifests that contain chart name, repository name,
and chart version in the same file. In this repository that misses at least:

- patch manifests that carry only the chart version
- values-only `HelmRelease` overlays, which still matter for inventory
- image-based manifests that are not direct `Deployment` image fields
