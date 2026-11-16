# Coverage

The [kubeflux audit](kubeflux-coverage.md) maps this scope to real declarations and
executable examples of remaining gaps.

`fluxrepo-update` scans user-authored `.yaml` and `.yml` manifests. Each manifest
retains its own explicit versions; the tool does not render Kustomize graphs or inherit chart
identity and values from another file. An explicit base version can therefore be updated
even when an overlay overrides it or no environment includes it. The scope is all eligible
repository manifests, including untracked files, not effective deployed versions.

## Updated

- Explicit `HelmRelease.spec.chart.spec.version` values when chart and source identity are
  present in the same manifest
- `containers[*].image` and `initContainers[*].image` in Deployments, StatefulSets,
  DaemonSets, Jobs, CronJobs, and Pods
- Recursive scalar image values under `HelmRelease.spec.values`
- Recursive scalar `repository`/`tag` mappings under Helm values, with optional `registry`
- Standard public Helm HTTP indexes, generic public OCI chart registries, and public
  container registries, including anonymous bearer-token challenges
- Public GitHub `Kustomization.resources` release assets such as
  `https://github.com/owner/repo/releases/download/v1.0.0/operator.yaml`, and HTTPS or
  scheme-less `github.com/owner/repo/subpath?ref=v1.0.0` references

GitHub pins use dotted numeric versions with an optional `v` prefix. Release metadata
must describe a stable, non-draft release; selection never downgrades. References with
the same file, case-insensitive project, and exact current pin select one common release
containing all required asset names and form one approval group. Only the pin is edited;
the asset path, subdirectory, `.git` suffix, and URL spelling are retained. The resources
lists across all documents participate in selection identity and are rechecked on apply.
GitHub pagination stays on the API origin; redirects are rejected by the default client.
Git subdirectory contents are not fetched or validated.

Latest stable version selection supports semantic, calendar, numeric, and recognized
numeric-pattern releases. It excludes prereleases and mutable tags, permits cross-major
updates, and never proposes a downgrade. This is version selection, not a compatibility
check for chart values, application migrations, or Kubernetes versions.

Compact calendar tags (`YYYYMMDD`, `YYYYMMDDHH`, `YYYYMMDDHHMM`, `YYYYMMDDHHMMSS`)
compare only within the same precision, including optional `v` prefixes and image variants.
They cannot replace dotted or ordinary numeric releases, so Jellyfin's timestamp build
channel stays separate from its stable releases. Unknown or incompatible version schemes
are reported as unresolved rather than used to guess an update.

Image suffixes such as `-alpine`, `-slim-bookworm`, and `-arm64` are retained exactly,
including variant version numbers. Unfamiliar suffixes on numeric versions are treated
conservatively as variants; explicit prerelease markers remain excluded. Chart versions
use chart SemVer rules, including OCI registry `_` encoding for `+` build metadata.

Repeated `HelmRepository` definitions resolve together only when their name, effective namespace,
and complete `spec` match. Paths and descriptive metadata do not affect equivalence;
even harmless spec differences such as polling intervals remain conflicts. Different
definitions with the same name remain ambiguous, including across namespaces. Known
namespace mismatches are also skipped: explicit `sourceRef.namespace`, or the release's
effective namespace when omitted, must match the source's effective namespace when both are known.

Effective namespaces follow local `resources` file/directory inclusions in
`kustomization.yaml`/`kustomization.yml`; the outermost namespace transformer wins.
Absent transforms retain raw namespaces. Disagreeing inclusion chains and cycles are
unresolved. Remote resources, `bases`, components, patches, generators, and Flux
Kustomization `targetNamespace` do not contribute namespace transforms.
All equivalent source copies and contributing inclusion-chain files bind selection
identity and are rechecked before applying chart updates; namespace discovery writes no files.

Within a run, targets share fetched metadata. Registry tag lists follow `Link` headers
with a `next` relation. Pagination cycles or more than 1,000 pages are reported as
resolution failures rather than using a partial tag list.

### Helm-values image bindings

Any recursively nested mapping under `spec.values`, including inside lists, qualifies
when `repository` and `tag` are non-empty scalars; no fixed parent key such as `image`
is required. Main, sidecar, metrics, test, and initialization values follow this same
rule. Each concrete tag is one binding and one target, even when a chart uses it for
multiple containers. Apply changes only that tag scalar, including in indentless lists.

```yaml
metrics:
  registry: ghcr.io
  repository: example/metrics
  tag: "1.2.3"
```

A nonblank sibling `registry` prefixes the repository as `registry/repository` unless
the repository already starts with that exact `registry/` prefix. Both `example/metrics`
and `ghcr.io/example/metrics` above resolve to `ghcr.io/example/metrics:1.2.3`.

Conservative forms remain unchanged; see the [output reason list](output.md#skipped):

| Form | Reason code or discovery boundary |
| --- | --- |
| Any nonblank sibling `digest`, `sha`, or `sha256`, or an image/tag containing `@digest` | `image_reference_pinned_by_digest`; earlier blank pin fields cannot hide later pins |
| Templated references (`{{…}}` or `${…}`) | `templated_image_reference` |
| Mutable tags (`latest`, `main`, `master`, `edge`, `nightly`, `stable`) | `mutable_image_tag` |
| Recognized commit tags or unknown version schemes | `incompatible_version_scheme` |
| Scalar image without a tag | `image_reference_missing_tag` |
| `image.tag` without a manifest-local repository | `unsupported_version_declaration` |
| Repository with `version` or `imageTag` instead of `tag` | `unsupported_image_schema` |
| Local repository with a blank/omitted tag | No binding or reason code; the tag is inherited |
| Other non-scalar fields or unknown shapes | Outside binding discovery unless recognized above; no guessing |

Empty or whitespace-only pin fields do not suppress an otherwise eligible tag update.

## Left unchanged

- Missing chart versions (supported image bindings can still update)
- Manifest fragments whose own chart or source identity is incomplete
- Chart versions referenced through `HelmRelease.spec.chartRef` or `OCIRepository`
- Blank or omitted tags inherited from chart defaults
- Templated, mutable, tagless, digest-pinned images and recognized commit tags
- Unknown image schemas and ambiguous `tag` or `version` fields
- Private-source credential management
- GitHub branch/commit refs, duplicate or extra query parameters, encoded or ambiguous
  URLs, non-GitHub remote resources, and local resource paths
- Generated `gotk-*` manifests directly inside a directory named `flux-system`
- Files in hidden directories, cache directories, symlinks, and non-YAML files

Recognizable unresolved targets appear in the Update Plan with stable identities and
reason codes. They do not block independently resolvable updates. Malformed YAML and
runtime or write failures remain errors.

The scanner also reports these declarations without checking or editing them:

- `image.tag` mappings under Helm values without a local image repository
- CloudNativePG `Cluster.spec.imageName`
- Unsupported HTTP(S) and scheme-less GitHub entries in Kustomization `resources`

These entries include their file, document, field, current value, and reason. Digest-pinned
image bindings continue to be reported as unchecked, even when they contain a version tag.
Chart defaults and resource contents are not fetched to resolve these declarations.
Other custom resource fields, arbitrary tag/version keys, non-GitHub scheme-less resource references,
and inherited values without explicit declarations remain outside discovery.

`discovered_count` counts recognized declarations, not every dependency in the repository.
In an update report it equals `checked_count + unchecked_count`; successful checks include
declarations with no available update. Unchecked counts include unsupported declarations
and failed resolution attempts. Counts cover the whole run, including in selected apply mode.

Planned and applied counts count approval groups; a GitHub group may contain several
individually counted declarations. Failed groups report a located skip for each member.

Apply mode prepares and validates every selected transformation before writing, including
checking the scalar, resource identity, chart/source identity, and image/container identity
as applicable. It parses the prepared YAML and rejects unintended semantic changes,
including effects through aliases. Supported block scalars and literal keys containing
punctuation are handled without changing surrounding comments, quoting, document
separators, line endings, or unrelated formatting.

No automatic rollback, deployment, or reconciliation is performed. If an operating-system
write fails after earlier files were written, the error reports partial application and
Git remains the recovery boundary.
