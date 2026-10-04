# Usage

Use `inventory` to inspect discovered declarations without network access. Use
`update-helm` to resolve available versions and optionally apply them. The latter
updates chart versions, container images, and GitHub Kustomize resource pins despite
its historical command name.

Examples use the installed binary. From a source checkout, replace `fluxrepo-update`
with `cargo run --`.

## Inspect and preview

```sh
fluxrepo-update inventory /path/to/flux-repo
fluxrepo-update inventory /path/to/flux-repo --json
fluxrepo-update update-helm /path/to/flux-repo --json --non-interactive
```

Inventory text summarizes counts; JSON includes sources, targets, and unchecked
declarations. A preview returns the available updates and skips without changing files.
Planning requires access to public Helm indexes, OCI/container registries, and GitHub
release metadata.

Each manifest is interpreted independently. A base version can be updated even when
an overlay overrides it. Latest stable selection can cross major versions, so review
chart values, application migrations, and cluster compatibility before applying.

## Choose a mode

Add these flags to `fluxrepo-update update-helm /path/to/flux-repo`:

| Flags | Behavior |
| --- | --- |
| none | Prompt for each update and write approved changes; default answer is No. |
| `--non-interactive` | Print the plan without prompts or writes. |
| `--write --non-interactive` | Apply every planned update without prompts. |
| `--write --non-interactive --apply-id '<id>'` | Apply only selected updates; repeat `--apply-id` for multiple items. |

`--json` controls report formatting; it does not disable interactive approval.
Use `--non-interactive` in scripts and agents. `--write` requires `--non-interactive`,
and `--apply-id` requires `--write`.

Interactive approval shows the relative file, document, resource, and exact field.
Chart prompts include source and chart identity; image prompts show the full old and
new image. Press `y` to approve or `n` to skip.

GitHub resource references in one file with the same project and current pin form one
update, including across YAML documents. Approval shows every URL change and applies
the entire group. The chosen release must contain every required release asset;
subdirectories, asset names, and HTTPS or scheme-less spelling are retained.

## Apply selected updates

First inspect `planned[]` in a JSON preview. Pass each chosen `planned[].id` unchanged:

```sh
fluxrepo-update update-helm /path/to/flux-repo --json --non-interactive
fluxrepo-update update-helm /path/to/flux-repo --write --non-interactive --apply-id '<id-from-plan>'
```

IDs are opaque and bind the reviewed resource, source, image identity, and versions.
Application plans again; unknown or stale IDs reject the entire selection without
writing. Generate and review a fresh plan when the target or available version changes.
See [selection IDs](output.md#planned) for the current format and migration rules.

## Skips, errors, and recovery

Unresolved targets do not block independent updates. Inspect the report's `skipped`
entries and coverage counts even when the command exits `0`. Missing or conflicting
sources, failed requests, unsupported version schemes, and declarations that cannot be
checked all leave update availability unknown. See [coverage](coverage.md) for supported
forms and [output](output.md#skipped) for stable reason codes.

Apply prepares and validates all selected edits before writing. It rechecks target
and source identities and rejects unintended YAML changes. Approved scalars are edited
in place, preserving surrounding formatting and comments. An operating-system write
failure can still leave earlier files changed; inspect the working tree and use Git
for recovery. There is no automatic rollback, deployment, or reconciliation.

With `--json`, runtime and mode-validation failures produce a structured error on
stderr. Argument parsing keeps its normal text errors. Scripts must handle the
[exit codes](output.md#exit-codes): `10` is a successful preview with updates and `20`
is a successful apply, rather than a failure.
