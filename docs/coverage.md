# Coverage

The [kubeflux audit](kubeflux-coverage.md) maps this scope to real declarations and
executable examples of remaining gaps.

`fluxrepo-update` scans user-authored `.yaml` and `.yml` manifests. Each manifest
is interpreted independently; the tool does not render Kustomize graphs or inherit chart
identity and values from another file. An explicit base version can therefore be updated
even when an overlay overrides it or no environment includes it. The scope is all eligible
repository manifests, including untracked files, not effective deployed versions.

## Updated

- Explicit `HelmRelease.spec.chart.spec.version` values when chart and source identity are
  present in the same manifest
- `containers[*].image` and `initContainers[*].image` in Deployments, StatefulSets,
  DaemonSets, Jobs, CronJobs, and Pods
- Recursive scalar image values under `HelmRelease.spec.values`
- Recursive `image.repository`/`image.tag` mappings, with optional `image.registry`
- Standard public Helm HTTP indexes, generic public OCI chart registries, and public
  container registries, including anonymous bearer-token challenges

Latest stable version selection supports semantic, calendar, numeric, and recognized
numeric-pattern releases. It excludes prereleases and mutable tags, permits cross-major
updates, and never proposes a downgrade. This is version selection, not a compatibility
check for chart values, application migrations, or Kubernetes versions.

Image suffixes such as `-alpine`, `-slim-bookworm`, and `-arm64` are retained exactly,
including variant version numbers. Unfamiliar suffixes on numeric versions are treated
conservatively as variants; explicit prerelease markers remain excluded. Chart versions
use chart SemVer rules, including OCI registry `_` encoding for `+` build metadata.

Repeated `HelmRepository` definitions resolve together only when their name, raw namespace,
and complete `spec` match. Paths and descriptive metadata do not affect equivalence;
even harmless spec differences such as polling intervals remain conflicts. Different
definitions with the same name remain ambiguous, including across namespaces. Known
namespace mismatches are also skipped: `sourceRef.namespace`, or the release namespace
when omitted, must match the source namespace when both are explicit. Missing namespaces
are not inferred from Kustomize configuration.

All equivalent source copies participate in selection identity and are rechecked before
applying updates. Kustomize namespace transformations remain unsupported.

Within a run, targets share fetched metadata. Registry tag lists follow `Link` headers
with a `next` relation. Pagination cycles or more than 1,000 pages are reported as
resolution failures rather than using a partial tag list.

## Left unchanged

- Missing chart versions (supported image bindings can still update)
- Manifest fragments whose own chart or source identity is incomplete
- Chart versions referenced through `HelmRelease.spec.chartRef` or `OCIRepository`
- Blank or omitted tags inherited from chart defaults
- Templated, mutable, tagless, digest-pinned images and recognized commit tags
- Unknown image schemas and ambiguous `tag` or `version` fields
- Private-source credential management
- Generated `gotk-*` manifests directly inside a directory named `flux-system`
- Files in hidden directories, cache directories, symlinks, and non-YAML files

Recognizable unresolved targets appear in the Update Plan with stable identities and
reason codes. They do not block independently resolvable updates. Malformed YAML and
runtime or write failures remain errors.

The scanner also reports these declarations without checking or editing them:

- `image.tag` mappings under Helm values without a local image repository
- CloudNativePG `Cluster.spec.imageName`
- HTTP(S) URLs in Kustomization `resources`, including release paths and `?ref=` pins

These entries include their file, document, field, current value, and reason. Digest-pinned
image bindings continue to be reported as unchecked, even when they contain a version tag.
Chart defaults and remote resources are not fetched to resolve these declarations.
Other custom resource fields, arbitrary tag/version keys, non-HTTP resource references,
and inherited values without explicit declarations remain outside discovery.

`discovered_count` counts recognized declarations, not every dependency in the repository.
In an update report it equals `checked_count + unchecked_count`; successful checks include
declarations with no available update. Unchecked counts include unsupported declarations
and failed resolution attempts. Counts cover the whole run, including in selected apply mode.

Apply mode prepares and validates every selected transformation before writing, including
checking the scalar, resource identity, chart/source identity, and image/container identity
as applicable. It parses the prepared YAML and rejects unintended semantic changes,
including effects through aliases. Supported block scalars and literal keys containing
punctuation are handled without changing surrounding comments, quoting, document
separators, line endings, or unrelated formatting.

Known limitation: applying some valid indentless block lists reports a missing YAML
path after planning. The [U15 examples](kubeflux-coverage.md#u15) retain the expected
edits and currently report TODO.

No automatic rollback, deployment, or reconciliation is performed. If an operating-system
write fails after earlier files were written, the error reports partial application and
Git remains the recovery boundary.
