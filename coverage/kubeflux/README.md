# Synthetic kubeflux coverage

This opt-in corpus exercises the actual CLI independently of `cargo test` and CI.
Each small repository represents an update-bearing shape found in the
[kubeflux audit](../../docs/kubeflux-coverage.md), including unsupported shapes.
Requires Python 3.10+ (standard library only) and a built CLI. No kubeflux checkout,
credentials, or public registry access is needed. Localhost HTTP listeners must be allowed.

```sh
cargo build --locked
python3 coverage/kubeflux/run.py
python3 coverage/kubeflux/run.py --json > /tmp/kubeflux-coverage.json
python3 coverage/kubeflux/run.py --case helm-values-lists
python3 coverage/kubeflux/run.py --strict
```

The runner copies each example to a temporary directory, substitutes a local HTTP
source, runs the CLI, checks structured output, and compares every file byte-for-byte
against the expected repository. Committed examples are never edited by a run.

- **PASS**: the desired capability or deliberate preservation boundary works.
- **TODO**: a documented unsupported capability or known updater bug still fails its desired checks.
- **XPASS**: a TODO now works; review and promote its expectation to `pass`.
- **FAIL**: a previously working example fails its checks, or any case makes an unexpected edit.
- **ERROR**: invalid case, CLI crash/runtime error, invalid JSON, timeout, or unexpected
  HTTP request. Infrastructure errors are never classified as known TODOs. A specifically
  recorded updater error may be recognized as a TODO as described below.

Default exit status is nonzero for FAIL/ERROR. `--strict` also fails for TODOs.
PASS on a preservation case means safe skipping works, not that an update was checked.
The report retains observed counts, planned/skipped entries, exit codes, and mismatches.

## Case format

Each `cases/<id>/` contains `case.json`, a `repo/` tree, and optionally `expected/`.
Files in `expected/` replace corresponding input files in the expected result; every
other input file must remain unchanged, and unexpected new/deleted files fail.

```json
{
  "id": "example",
  "category": "workloads",
  "description": "A versioned Pod image updates without changing its neighbors.",
  "audit": ["T1"],
  "source": ["apps/production/example/deployment.yaml"],
  "expectation": "pass",
  "todo": "",
  "command": "apply",
  "responses": {
    "/v2/demo/app/tags/list": {"json": {"tags": ["1.0.0", "2.0.0"]}}
  },
  "checks": {
    "exit_code": 20,
    "json": [
      {"at": "/summary/applied_count", "equals": 1},
      {"at": "/planned", "contains": {"yaml_path": "spec.containers[0].image", "latest_version": "2.0.0"}}
    ]
  }
}
```

`command` is `inventory`, `plan`, or `apply`. `expectation` is `pass` or `todo`;
TODO cases require a concrete `todo` explanation. `audit` links to U/T identifiers in
the audit. `source` records the real shape's provenance, without copying private data.
`{{server}}` expands to the local HTTP origin; `{{registry}}` expands to its host:port.
Placeholders work in repositories, expected files, responses, and check values.

Responses use optional `status` (default 200), `headers`, and either `json` or `text`.
JSON checks use RFC 6901 pointers: `equals` compares exactly; `contains` finds an array
element matching the supplied object fields. An optional `exit_code_not` check captures
outcome requirements without choosing a particular future exit code.

Verified updater bugs can declare `known_failure` with `exit_code: 2`,
`error: "runtime_error"`, and a specific `message_contains` string. Only that structured
stderr failure is a TODO; a different failure is an ERROR. Files must stay unchanged.
For a known wrong selection, `known_actual/` records the exact observed wrong bytes,
while `expected/` and JSON checks retain the desired result. Only TODO cases can have
these failure baselines; they never turn a failing desired outcome into PASS.

Repositories and expected overlays contain UTF-8 regular files. Symlinks and special
filesystem entries are rejected, and overlays can only replace existing input files.
Unexpected HTTP requests are errors; external HTTP(S) requests are intercepted by the
local fixture server through proxy environment variables.

Add cases with independently chosen desired values, not snapshots copied from actual
CLI output. A new capability stays TODO until its desired checks pass; do not change
its desired outcome to bless a failure. Keep feature fixes in the normal red/green
regression suite, separate from this exploratory corpus.

The runner's own red/green checks are also opt-in and do not join Cargo tests:

```sh
python3 -m unittest discover -s coverage/kubeflux -p 'test_*.py'
```
