# Usage

## Mental Model

Use `inventory` to answer "what does this repository contain?".

Use `update-helm` to answer "what version bumps are available?" and, if desired, apply
those bumps.

`update-helm` always scans the repository first, then resolves the latest chart versions
and image-binding versions from public remote sources. It works on explicit values in
individual manifests, so a base-file update may be overridden by a Kustomize overlay.
Review upgrades for compatibility; latest stable can include a new major version.

## Safe First Run

Start with read-only commands:

```bash
cargo run -- inventory /path/to/flux-repo --json
cargo run -- update-helm /path/to/flux-repo --json --non-interactive
```

That gives you:

- the list of repositories and update targets the scanner found
- the planned version bumps
- any skipped targets and the reason they could not be resolved

## Commands

### `inventory`

Summarizes what the scanner found in a repository.

```bash
cargo run -- inventory /path/to/flux-repo
cargo run -- inventory /path/to/flux-repo --json
```

Human-readable output includes counts for:

- `Repositories`
- `Chart targets`
- `Image bindings`
- `HelmReleases without chart version`
- `Unresolved chart targets`
- `Image references`
- `Skipped generated files`

Use `--json` when you need the actual item lists instead of summary counts.
`Repositories` counts every discovered source, including copies. JSON lists equivalent
copies under `equivalent_repositories` and conflicting definitions under
`ambiguous_repositories`; charts using an ambiguous name are skipped.

### `update-helm`

Plans or applies explicit chart versions and image bindings.

```bash
cargo run -- update-helm /path/to/flux-repo
cargo run -- update-helm /path/to/flux-repo --non-interactive
cargo run -- update-helm /path/to/flux-repo --json --non-interactive
cargo run -- update-helm /path/to/flux-repo --write --non-interactive
cargo run -- update-helm /path/to/flux-repo --write --non-interactive --apply-id '<id-from-plan>'
```

Options:

- `--json`: emit machine-readable output
- `--write`: apply all planned updates without prompts; requires `--non-interactive`
- `--apply-id <ID>`: apply one planned item by JSON plan ID; repeat for multiple items
- `--non-interactive`: disable prompts

Image bindings include standard workload `containers`/`initContainers` scalars and
recursive scalar or `repository`/`tag` mappings under `HelmRelease.spec.values`.
Mutable, templated, digest-pinned, tagless, blank, and unknown schemas remain unchanged.
Apply mode edits the targeted YAML scalar in place so unrelated formatting, comments,
quote and block-scalar styles, and multi-document separators stay intact. Literal mapping
keys containing dots or brackets are addressed separately from nested paths.

## Interactive Vs Automation Behavior

Default behavior is for humans:

- `cargo run -- update-helm /path/to/flux-repo`
  prompts for each planned update; press `y` or `n` to approve or skip

Each interactive approval identifies the file, document, resource, and field. Chart
prompts include the source and chart; image prompts show the full old and new image.

Agent mode is explicit:

- `cargo run -- update-helm /path/to/flux-repo --non-interactive`
  prints the plan and never modifies files
- `cargo run -- update-helm /path/to/flux-repo --write --non-interactive`
  applies all planned updates without prompts
- `cargo run -- update-helm /path/to/flux-repo --write --non-interactive --apply-id '<id-from-plan>'`
  applies only selected planned updates without prompts

Recommended patterns:

- Manual review with prompts:
  - `cargo run -- update-helm /path/to/flux-repo`
- Automation preview:
  - `cargo run -- update-helm /path/to/flux-repo --json --non-interactive`
- Automation apply-all:
  - `cargo run -- update-helm /path/to/flux-repo --write --non-interactive`
- Automation apply-selected:
  - inspect `planned[].id` from `--json --non-interactive`
  - pass each chosen ID as `--apply-id` with `--write --non-interactive`

Treat IDs as opaque. Current planned IDs use `v2:` and include resource, chart source,
and image/container identity as applicable. Older `v1:` planned IDs and IDs whose
reviewed target or versions changed are rejected; generate and review a fresh plan.

Before writing, apply rechecks the reviewed scalar and its identity, then parses the
prepared YAML to verify that only approved values changed. All selected files must pass
this preflight. Operating-system write failures can still leave earlier files changed;
inspect the working tree and use Git for recovery.

## Error Cases

The CLI rejects these combinations:

- `--write` without `--non-interactive`
- `--apply-id` without `--write`
- unknown or stale `--apply-id` values

With `--json`, failures after argument parsing are reported as JSON on stderr with
`error`, `message`, and `exit_code` fields.

Unresolved targets are normal best-effort plan outcomes and do not block other updates.
A skip can happen because:

- the referenced `HelmRepository` is missing or has a known namespace mismatch
  (`missing_helm_repository`)
- conflicting `HelmRepository` definitions share the referenced name
- the chart could not be found in the repository index
- the remote repository could not be reached
- the version scheme is unfamiliar or incomparable
- the image tag is mutable or otherwise not comparable
- the container registry could not list tags for the image
- the image is templated, mutable, tagless, or digest-pinned
- a tag-only override, CloudNativePG image, or remote Kustomize URL is recognized but not checked

When every target is skipped, the output reports skipped targets rather than declaring
the repository up to date. Exit code `0` does not mean every version was resolved;
inspect `skipped` and its reason codes before treating a check as complete.

Both commands identify their scope as repository manifests, including inactive bases and
untracked YAML files. Update coverage counts distinguish discovered, checked, and unchecked
declarations without changing exit codes. Equivalent source copies are accepted only when
their name, raw namespace, and full specification match; namespace transformations are not evaluated.

## Exit Codes

- `0`: no updates applied, no updates found, or no updates approved
- `2`: invalid arguments or a runtime/write failure
- `10`: planning mode found updates
- `20`: updates were applied
