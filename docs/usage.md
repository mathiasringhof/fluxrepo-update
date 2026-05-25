# Usage

## Mental Model

Use `inventory` to answer "what does this repository contain?".

Use `update-helm` to answer "what version bumps are available?" and, if desired, apply
those bumps.

`update-helm` always scans the repository first, then resolves the latest chart versions
and Deployment image tags from remote sources.

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
- `Deployment targets`
- `HelmReleases without chart version`
- `Unresolved chart targets`
- `Image references`
- `Skipped generated files`

Use `--json` when you need the actual item lists instead of summary counts.

### `update-helm`

Plans or applies updates for updateable `HelmRelease` resources and versioned
`Deployment` image fields.

```bash
cargo run -- update-helm /path/to/flux-repo
cargo run -- update-helm /path/to/flux-repo --non-interactive
cargo run -- update-helm /path/to/flux-repo --json --non-interactive
cargo run -- update-helm /path/to/flux-repo --write
cargo run -- update-helm /path/to/flux-repo --write --non-interactive
cargo run -- update-helm /path/to/flux-repo --write --non-interactive --apply-id '<id-from-plan>'
```

Options:

- `--json`: emit machine-readable output
- `--write`: apply all planned updates without prompts; requires `--non-interactive`
- `--apply-id <ID>`: apply one planned item by JSON plan ID; repeat for multiple items
- `--strict`: fail if any target is skipped during version resolution
- `--best-effort`: keep planning even if some targets are skipped
- `--non-interactive`: disable prompts

`Deployment` updates are limited to direct `containers[*].image` and
`initContainers[*].image` fields. Mutable tags such as `latest` and `main`, digest-pinned
images, and bare image references without an explicit tag are reported as skipped.

## Interactive Vs Automation Behavior

Default behavior is for humans:

- `cargo run -- update-helm /path/to/flux-repo`
  prompts for each planned update and applies the ones you approve

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

## Error Cases

The CLI rejects these combinations:

- `--write` without `--non-interactive`
- `--apply-id` without `--write`
- unknown or stale `--apply-id` values

`--strict` changes skipped resolutions from a warning into a failing exit code. A skip can
happen because:

- the referenced `HelmRepository` is missing from the scanned repo
- the chart could not be found in the repository index
- the remote repository could not be reached
- the repository changed to an incompatible version scheme, so no comparable upgrade path could be derived
- the image tag is mutable or otherwise not comparable
- the container registry could not list tags for the image
- the repository type is unsupported, such as generic OCI

## Exit Codes

- `0`: no updates applied, no updates found, or no updates approved
- `2`: invalid arguments or `--strict` encountered skipped targets
- `10`: planning mode found updates
- `20`: updates were applied
