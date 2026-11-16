# Output

This page explains the main fields returned by `inventory --json` and
`update-helm --json`, plus the human output conventions for terminal use.

## Human Output

`inventory` prints plain summary count lines to stdout.
Both commands explicitly report repository-manifest scope, without claiming deployment coverage.

`update-helm` prints human status, skipped targets, planned updates, prompts, and final
summaries to stderr. Update lines include the relative file path, target kind, resource
name, update field, and version change:

```text
apps/base/demo/release.yaml: HelmRelease demo demo-chart 1.0.0 -> 1.2.0
apps/base/sonarr/deployment.yaml: ImageBinding sonarr spec.template.spec.containers[0].image 4.0.0 -> 4.1.0
```

When stderr is a terminal, `update-helm` colors paths cyan, current versions yellow, and
latest versions green, and shows a compact resolving progress indicator. `--json` disables
human status, color, and progress output. Interactive approval still prompts unless
`--non-interactive` is set; see [modes](usage.md#choose-a-mode).

Interactive approval includes the relative file, document number, resource, exact field,
and chart/source or full image change. GitHub groups show every member URL, document,
and field. Summaries state check completeness, discovered/checked/unchecked counts,
and counts for unchanged, update-available, unsupported, and failed declarations.
Total failure explicitly reports failed checks; unsupported-only and empty runs say
that nothing was checked. Skips include the field and current value when available.

## `inventory --json`

Top-level summary fields:

- `repo_root`: absolute path to the scanned repository
- `scope`: `repository_manifests`, including eligible untracked and inactive manifests
- `discovered_count`: chart targets, unresolved chart targets, image bindings, remote resource targets, and unsupported declarations
- `repository_count`: total discovered `HelmRepository` resources, including ambiguous sources
- `chart_target_count`: releases with enough local chart identity to attempt resolution
- `image_binding_count`: number of explicit workload or Helm-values image bindings
- `remote_resource_target_count`: number of supported GitHub Kustomize resource pins
- `helmreleases_without_chart_version_count`: number of `HelmRelease` resources that do
  not expose `spec.chart.spec.version`
- `unresolved_chart_target_count`: number of releases that carry a version but still lack
  enough metadata to update
- `image_reference_count`: number of discovered image references
- `skipped_paths`: YAML files intentionally skipped, such as generated `gotk-*` manifests

### `repositories`

Each item describes a unique `HelmRepository` or one representative of equivalent copies:

- `path`: relative file path
- `document_index`: document position inside a multi-document YAML file
- `name`: `metadata.name`
- `namespace`: `metadata.namespace`, or `null` when omitted
- `effective_namespace`: the namespace used for source matching, or `null` when absent or ambiguous
- `namespace_kustomizations`: sorted relative paths of outermost namespace setters across inclusion chains; empty without transforms
- `namespace_ambiguous`: whether inclusion chains disagree or contain a cycle
- `url`: `spec.url`
- `repo_type`: `spec.type`, or `default` when omitted

### `ambiguous_repositories`

An array of groups with `name` and `sources`. Each source has the same fields as a
`repositories` item. Conflicting same-name definitions appear here, including across
namespaces; none is chosen. Affected charts produce `ambiguous_helm_repository` skips.

### `equivalent_repositories`

Groups with `name` and all `sources` whose name, unambiguous effective namespace, and complete `spec` match.
Paths and descriptive metadata can differ. The representative also appears in `repositories`;
`repository_count` counts each physical source once, not every occurrence in these arrays.

### `unchecked_version_declarations`

Recognized declarations without supported checking: tag-only Helm `image.tag` overrides,
CloudNativePG `spec.imageName`, and unsupported remote Kustomize `resources` URLs. Each entry contains
`path`, zero-based `document_index`, `yaml_path`, `current_value`, and `reason`.
These declarations also appear in update `skipped` output and cannot be applied.
Inventory does not perform remote checks; further targets can become skips during planning.

### `chart_targets`

Each item describes a `HelmRelease` with enough manifest-local identity to attempt resolution:

- `path`: relative file path
- `document_index`: document position inside a multi-document YAML file
- `name`: `metadata.name`
- `namespace`: `metadata.namespace`
- `chart_name`: `spec.chart.spec.chart`
- `repo_name`: `spec.chart.spec.sourceRef.name`
- `current_version`: current `spec.chart.spec.version`
- `source_path`: the same manifest containing the explicit chart identity
- `source_is_inherited`: always `false`; cross-manifest inference is not performed

Chart targets, unresolved chart targets, and releases without chart versions also include
`effective_namespace`, `namespace_kustomizations`, and `namespace_ambiguous` as above.
`namespace` keeps its raw meaning. Effective namespaces fall back to raw namespaces when
no local transformer applies; ambiguous namespaces are never used to choose a source.

### `image_bindings`

Each item describes an explicit workload or Helm-values image binding:

- `path`: relative file path
- `document_index`: document position inside a multi-document YAML file
- `name`: `metadata.name`
- `namespace`: `metadata.namespace`
- `yaml_path`: exact image field inside the YAML document
- `image`: current image reference

Paths quote literal mapping keys when needed: `spec.values["a.b"].image` selects the
key `a.b`, while `spec.values.a.b.image` selects nested keys. `[0]` denotes a sequence item.

### `remote_resource_targets`

Each supported GitHub pin includes `path`, zero-based `document_index`, `yaml_path`,
`current_value`, `owner`, `repository`, `current_version`, and `required_asset` (or `null`
for a Git ref). Inventory reports each declaration independently without network access.

### `helmreleases_without_chart_version`

These are `HelmRelease` resources that do not expose `spec.chart.spec.version`, including
values-only overlays and releases that name a chart and repository but omit the version.

Their chart versions are not edited. Supported explicit image bindings under their
`spec.values` can still appear in the update plan.

### `unresolved_chart_targets`

These are `HelmRelease` resources that contain a version field but still cannot be updated
because its own manifest lacks chart or repository identity.

## `update-helm --json`

Top-level fields:

- `mode`: `plan` or `apply`
- `non_interactive`: whether prompts were disabled
- `scope`: `repository_manifests`
- `check_completeness`: `complete`, `incomplete`, `failed`, or `not_checked` (defined below)
- `summary`: full-check counts plus planned, applied, skipped, and changed-file counts
- `planned`: planned or applied chart, image, and remote resource updates
- `skipped`: targets that could not be resolved

### `summary`

- `planned_count`: number of items in `planned`
- `applied_count`: number of updates actually written
- `skipped_count`: number of targets skipped during version resolution
- `changed_file_count`: number of files written during apply mode
- `discovered_count`: number of recognized declarations in the full run
- `checked_count`: successfully resolved declarations, including those with no update
- `unchecked_count`: unsupported declarations and failed checks; equals `skipped_count`
- `checked_unchanged_count`: successfully checked declarations with no update available
- `update_available_count`: successfully checked declarations with an update available
- `unsupported_count`: unsupported forms/version schemes or missing/ambiguous manifest-local source identity
- `failed_count`: failed requests, missing charts/versions, malformed or inconsistent source
  metadata, and unclassified resolution failures; includes non-retryable failures

`discovered_count = checked_count + unchecked_count`,
`checked_count = checked_unchanged_count + update_available_count`, and
`unchecked_count = unsupported_count + failed_count = skipped_count`.
These counts describe the full check even when updates are declined or only some are
applied. They do not measure deployment coverage or include declarations outside the
scanner's recognized shapes. Inventory `unchecked_version_declarations` contribute to
`unsupported_count`.

GitHub resources grouped by file, project, and current pin count as one planned/applied
update. All full-check counts still count each resource declaration, including every
member of a successful or failed group.

Plan mode includes all proposed updates. Apply mode retains the existing output
contract: `planned` and `planned_count` describe the applied subset, so unapproved
updates are omitted. Skipped resolution targets are reported in both modes.

### `check_completeness`

| Value | Meaning | Exit behavior |
| --- | --- | --- |
| `complete` | At least one successful check; none unchecked | Existing `0`/`10`/`20` behavior |
| `incomplete` | At least one successful check; some unsupported or failed | Existing `0`/`10`/`20` behavior; partial plans retained |
| `failed` | Zero successful checks; at least one failed check | `2`, with the full report on stdout |
| `not_checked` | Zero successful and zero failed checks; unsupported-only or empty inventory | `0` |

Completeness is independent of approval and application. `complete` with zero
`update_available_count` means all discovered declarations were checked and need no
update. Exit `0` alone does not establish that. Classification uses the existing skip
reasons without changing resolution behavior; request retryability does not determine
completeness. Invalid arguments and runtime/write failures retain the error contract below.

### `planned`

Each item includes:

- `id`
- `path`
- `document_index`
- `target_kind`
- `target_name`
- `yaml_path`
- `current_version`
- `latest_version`
- `inherited_source`

`HelmRelease` items also include `chart_name`, `repo_name`, raw `namespace`,
`effective_namespace`, `namespace_kustomizations`, and `namespace_ambiguous`.
Each `sources` entry includes `path`, zero-based `document_index`, and the same four
namespace fields for every source copy used. Inventory provides source URLs.

`ImageBinding` items also include `current_image` and `latest_image`.

`RemoteResource` items name the GitHub project in `target_name` and include
`resource_changes`, with each member's `document_index`, `yaml_path`, `current_resource`,
and `latest_resource`. The top-level document and field locate the first member.
One ID approves every member; all resource lists in the file bind that identity.

`inherited_source` is retained for compatibility and is always `false`.

Use `planned[].id` with repeated `--apply-id` flags to apply selected updates in
`--write --non-interactive` mode. Treat these IDs as opaque. Current planned IDs start
with `v2:` and bind the current/latest versions, resource kind/name/namespace, and the
relevant chart source, image mapping, or workload container identity. Older `v1:` planned
IDs must be replaced by rerunning the preview. IDs for `skipped` targets are separate.

Apply rechecks the targeted scalar and reviewed identity, including every equivalent
chart source copy and contributing Kustomization inclusion-chain file. It validates all selected files before writing and rejects
prepared YAML whose meaning differs beyond the approved values.

### `skipped`

Each item includes:

- `path`
- `id`: stable unresolved-target identity
- `yaml_path`: exact explicit version field when known
- `document_index`: zero-based document position when known
- `current_value`: current scalar value when available, including tags, images, and remote URLs
- `reason`: human-readable explanation, intended for display
- `reason_code`: stable snake_case code for automation
- `retryable`: whether another attempt may succeed; network failures, HTTP 408/429,
  and server errors are retryable, while permanent HTTP 4xx errors and pagination
  cycles are not
- `source_url`: failing metadata URL when known, otherwise `null`

`reason_code` and `retryable` are intended for agents and scripts; use `reason` for
human-facing logs.

Chart skips include the same namespace fields and `sources` context as chart plans;
conflicts list every candidate source. `ambiguous_kustomize_namespace` is non-retryable.

Current reason codes include:

- `missing_helm_repository`
- `ambiguous_helm_repository`
- `ambiguous_kustomize_namespace`
- `missing_chart_identity`
- `unsupported_repository_type`
- `chart_not_found`
- `incompatible_version_scheme`
- `current_version_not_found`
- `current_version_newer_than_source`
- `chart_request_failed`
- `registry_request_failed`
- `github_request_failed`
- `github_release_assets_missing`
- `github_release_metadata_invalid`
- `github_release_version_unavailable`
- `mutable_image_tag`
- `image_reference_missing_tag`
- `image_reference_pinned_by_digest`
- `templated_image_reference`
- `unparseable_image_reference`
- `unsupported_image_schema`
- `unsupported_version_declaration`
- `unclassified`

An empty `planned` array does not establish that declarations are current. Inspect
`check_completeness`, full-check counts, and skips even when the exit code is `0`.

## JSON Errors

If a command includes `--json` and encounters a mode-validation or runtime/write failure
after argument parsing, the CLI writes a JSON error object to stderr and exits with the
same code the text error would use:

```json
{
  "error": "runtime_error",
  "message": "failed to resolve /missing/path: No such file or directory (os error 2)",
  "exit_code": 2
}
```

Clap parse and help errors keep their normal text output.
`error` is a stable machine label such as `runtime_error` or `invalid_arguments`.
Total version-check failure instead preserves the normal report on stdout with
`check_completeness: "failed"` and exit `2`; it emits no separate stderr JSON error.

## Exit Codes

- `0`: successful inventory, or no updates applied, found, or approved (including `not_checked`)
- `2`: invalid arguments, a runtime/write failure, or zero successful checks with at least one failed check
- `10`: planning mode found updates
- `20`: updates were applied
