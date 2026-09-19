# Coverage

`fluxrepo-update` scans user-authored `.yaml` and `.yml` Managed Manifests. Each manifest
is interpreted independently; the tool does not render Kustomize graphs or inherit chart
identity and values from another file. An explicit base version can therefore be updated
even when an overlay overrides it; the plan does not describe effective deployed versions.

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
updates, and never proposes a downgrade. This is version selection, not a compatibility
check for chart values, application migrations, or Kubernetes versions.

Image suffixes such as `-alpine`, `-slim-bookworm`, and `-arm64` are retained exactly,
including variant version numbers. Unfamiliar suffixes on numeric versions are treated
conservatively as variants; explicit prerelease markers remain excluded. Chart versions
use chart SemVer rules, including OCI registry `_` encoding for `+` build metadata.

Source names must be unique across the scanned tree. Duplicate `HelmRepository` names
are reported as ambiguous, even across namespaces; affected charts are skipped. Known
namespace mismatches are also skipped: `sourceRef.namespace`, or the release namespace
when omitted, must match the source namespace when both are explicit. Missing namespaces
are not inferred from Kustomize configuration.

Within a run, targets share fetched metadata. Registry pagination cycles or more than
1,000 pages are reported as resolution failures rather than using a partial tag list.

## Left unchanged

- Missing chart versions (supported image bindings can still update)
- Manifest fragments whose own chart or source identity is incomplete
- Chart versions referenced through `HelmRelease.spec.chartRef` or `OCIRepository`
- Blank or omitted tags inherited from chart defaults
- Templated, mutable, tagless, digest-pinned images and recognized commit tags
- Unknown image schemas and ambiguous `tag` or `version` fields
- Private-source credential management
- Generated Flux bootstrap manifests under `clusters/*/flux-system/gotk-*`
- Files in hidden directories, cache directories, symlinks, and non-YAML files

Recognizable unresolved targets appear in the Update Plan with stable identities and
reason codes. They do not block independently resolvable updates. Malformed YAML and
runtime or write failures remain errors.

Apply mode prepares and validates every selected transformation before writing, including
checking the scalar, resource identity, chart/source identity, and image/container identity
as applicable. It parses the prepared YAML and rejects unintended semantic changes,
including effects through aliases. Supported block scalars and literal keys containing
punctuation are handled without changing surrounding comments, quoting, document
separators, line endings, or unrelated formatting.

No automatic rollback, deployment, or reconciliation is performed. If an operating-system
write fails after earlier files were written, the error reports partial application and
Git remains the recovery boundary.
