# Coverage

`fluxrepo-update` scans user-authored `.yaml` and `.yml` Managed Manifests. Each manifest
is interpreted independently; the tool does not render Kustomize graphs or inherit chart
identity and values from another file.

## Updated

- Explicit `HelmRelease.spec.chart.spec.version` values when chart and source identity are
  present in the same manifest
- `containers[*].image` and `initContainers[*].image` in Deployments, StatefulSets,
  DaemonSets, Jobs, CronJobs, and Pods
- Recursive scalar image values under `HelmRelease.spec.values`
- Recursive `image.repository`/`image.tag` mappings, with optional `image.registry`
- Standard public Helm HTTP indexes, generic public OCI chart registries, and public
  container registries, including anonymous bearer-token challenges

Latest Stable Version selection supports semantic, calendar, numeric, and recognized
numeric-pattern releases. It excludes prereleases and mutable tags, permits cross-major
updates, and never proposes a downgrade.

## Left unchanged

- Helm releases without an explicit chart version
- Manifest fragments whose own chart or source identity is incomplete
- Blank or omitted tags inherited from chart defaults
- Templated, mutable, tagless, digest- or SHA-pinned image references
- Unknown image schemas and ambiguous `tag` or `version` fields
- Generated Flux bootstrap manifests under `clusters/*/flux-system/gotk-*`
- Files in hidden directories, cache directories, symlinks, and non-YAML files

Recognizable unresolved targets appear in the Update Plan with stable identities and
reason codes. They do not block independently resolvable updates. Malformed YAML and
runtime or write failures remain errors.

Apply mode prepares and validates every selected transformation before writing, including
checking that each targeted scalar still equals the value in the approved plan. It edits
only the explicit scalar and preserves surrounding comments, quoting, document separators,
line endings, and unrelated formatting. If an operating-system write fails after earlier
files were written, the error reports partial application and Git remains the recovery
boundary.
