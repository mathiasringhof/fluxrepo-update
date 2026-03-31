# Output

This page explains the main fields returned by `inventory --json` and
`update-helm --json`.

## `inventory --json`

Top-level summary fields:

- `repo_root`: absolute path to the scanned repository
- `repository_count`: number of `HelmRepository` resources found
- `chart_target_count`: number of `HelmRelease` resources that can be updated directly
- `deployment_target_count`: number of `Deployment` image fields that can be updated directly
- `helmreleases_without_chart_version_count`: number of `HelmRelease` resources that do
  not expose `spec.chart.spec.version`
- `unresolved_chart_target_count`: number of releases that carry a version but still lack
  enough metadata to update
- `image_reference_count`: number of discovered image references
- `skipped_paths`: YAML files intentionally skipped, such as generated `gotk-*` manifests

### `chart_targets`

Each item describes a `HelmRelease` that can be updated:

- `path`: relative file path
- `document_index`: document position inside a multi-document YAML file
- `name`: `metadata.name`
- `namespace`: `metadata.namespace`
- `chart_name`: `spec.chart.spec.chart`
- `repo_name`: `spec.chart.spec.sourceRef.name`
- `current_version`: current `spec.chart.spec.version`
- `source_path`: manifest that supplied chart metadata
- `source_is_inherited`: whether chart metadata came from another matching `HelmRelease`

If `source_is_inherited` is `true`, the file being updated is usually an overlay patch that
contains the version field but not the full chart source metadata.

### `deployment_targets`

Each item describes a `Deployment` image field that can be updated directly:

- `path`: relative file path
- `document_index`: document position inside a multi-document YAML file
- `name`: `metadata.name`
- `namespace`: `metadata.namespace`
- `yaml_path`: exact image field inside the YAML document
- `image`: current image reference

### `helmreleases_without_chart_version`

These are `HelmRelease` resources that do not expose `spec.chart.spec.version`. In this
repository they are usually values-only overlays such as:

- `apps/production/audiobookshelf/release-patch.yaml`
- `apps/production/uptimekuma-values.yaml`

This category can also include manifests that still name a chart and repository but omit
the chart version, such as `apps/production/minecraft-bedrock/release.yaml`.

They are inventoried because they matter for understanding the repo, but they are not
edited by `update-helm`.

### `unresolved_chart_targets`

These are `HelmRelease` resources that contain a version field but still cannot be updated
because chart or repository metadata could not be resolved, even after inheritance.

## `update-helm --json`

Top-level fields:

- `mode`: `plan` or `apply`
- `strict`: whether `--strict` was enabled
- `non_interactive`: whether prompts were disabled
- `summary`: counts for planned, applied, skipped, and changed files
- `planned`: planned or applied chart updates
- `skipped`: targets that could not be resolved

### `summary`

- `planned_count`: number of updates that were available
- `applied_count`: number of updates actually written
- `skipped_count`: number of targets skipped during version resolution
- `changed_file_count`: number of files written during apply mode

### `planned`

Each item includes:

- `path`
- `document_index`
- `target_kind`
- `target_name`
- `yaml_path`
- `current_version`
- `latest_version`
- `inherited_source`

`HelmRelease` items also include `chart_name` and `repo_name`.

`Deployment` items also include `current_image` and `latest_image`.

`inherited_source` means the update target inherited chart metadata from another manifest,
typically a base `HelmRelease`. For `Deployment` items it is always `false`.

### `skipped`

Each item includes:

- `path`
- `reason`

Typical reasons:

- missing `HelmRepository`
- unsupported repository type
- chart missing from repository index
- incompatible version scheme change with no comparable upgrade path
- network failure while fetching remote metadata
- mutable image tag such as `latest` or `main`
- image reference missing an explicit tag or pinned by digest
- registry failure while listing image tags

## Exit Codes

- `0`: no updates applied, no updates found, or no updates approved
- `2`: invalid arguments or strict failure
- `10`: planning mode found updates
- `20`: updates were applied
