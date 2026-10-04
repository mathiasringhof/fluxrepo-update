# Synthetic kubeflux coverage

This opt-in corpus exercises the CLI independently of `cargo test` and CI. The
[audit](../../docs/kubeflux-coverage.md) records real source shapes;
[STATUS.md](STATUS.md) records results. Requires Python 3.10+, a built CLI, and localhost
HTTP listeners. No kubeflux checkout, credentials, or public registry access is needed.

```sh
cargo build --locked
python3 coverage/kubeflux/run.py
python3 coverage/kubeflux/run.py --json > /tmp/kubeflux-coverage.json
python3 coverage/kubeflux/run.py --case helm-values-lists
python3 coverage/kubeflux/run.py --strict
python3 -m unittest discover -s coverage/kubeflux -p 'test_*.py'
```

Each run copies examples to temporary repositories, serves local HTTP responses,
validates CLI reports, and compares every file byte-for-byte with the expected result.

- **PASS**: the desired capability or deliberate preservation boundary works.
- **TODO**: a documented unsupported capability or known updater bug still fails its desired checks.
- **XPASS**: a TODO now works; review and promote its expectation to `pass`.
- **FAIL**: a previously working check or any invariant fails, or a case makes an unexpected edit.
- **ERROR**: invalid case, CLI crash/runtime error, malformed report, timeout, or unexpected
  HTTP request. Infrastructure errors are never classified as known TODOs. A specifically
  recorded updater error may be recognized as a TODO as described below.

Default exit status is nonzero for FAIL/ERROR; `--strict` also fails for TODOs.
Reports retain observed counts, planned/skipped entries, exit codes, and mismatches.

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
TODO checks describe proposed behavior, including report fields or policies the current
CLI does not support; [output documentation](../../docs/output.md) defines the current API.
`{{server}}` expands to the local HTTP origin; `{{registry}}` expands to its host:port.
Placeholders work in repositories, expected files, responses, and check values.

GitHub cases retain public `github.com` URLs. A local TLS proxy serves configured
`api.github.com` release responses using the fixture-only CA and key under `tls/`;
only the CLI subprocess receives that CA trust. Unexpected hosts, paths, and methods
remain fixture errors, and no external network request is forwarded.

Responses use optional `status` (default 200), `headers`, and either `json` or `text`.
JSON checks use RFC 6901 pointers: `equals` compares exactly; `contains` finds an array
element matching the supplied object fields. `checks.invariants` uses the same format
for requirements that must hold even while a desired capability remains TODO, such as
preserving an ordinary adjacent declaration. An invariant mismatch is FAIL, never TODO.
`json` may be empty when invariants cover the report and only the exit outcome remains TODO.
`exit_code_not` captures requirements without choosing a particular future exit code.

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

Choose desired values independently of CLI output. Develop examples red-first and keep
new capabilities TODO until their checks pass; feature fixes belong in the normal
red/green regression suite. Runner checks remain opt-in as shown above.
