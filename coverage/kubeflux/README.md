# Synthetic kubeflux coverage

This corpus supplies both normal library tests and an opt-in CLI audit. The
[audit](../../docs/kubeflux-coverage.md) records real source shapes;
[STATUS.md](STATUS.md) records CLI results. No kubeflux checkout, credentials, or public
registry access is needed.

All cases marked `pass` run in `cargo test` and CI without launching the CLI. They call
`scan_repo` and `UpdateRun::execute` with real resolvers against local HTTP fixtures,
check the domain values specified by the case assertions, and compare every file
byte-for-byte. Exit expectations map to `UpdateRunStatus`; CLI serialization and actual
exit codes remain covered by CLI tests and the Python audit. The fixture client routes
all HTTP(S) through a local proxy that rejects external destinations and unexpected
requests. No global proxy environment variables are changed by the Rust tests.

```sh
cargo test --locked --test rust_corpus
cargo test --locked --test rust_corpus charts_http
```

The full CLI audit includes TODO cases and requires Python 3.10+, a built CLI, and
localhost HTTP listeners:

```sh
cargo build --locked
python3 coverage/kubeflux/run.py
python3 coverage/kubeflux/run.py --json > /tmp/kubeflux-coverage.json
python3 coverage/kubeflux/run.py --case helm-values-lists
python3 coverage/kubeflux/run.py --strict
python3 -m unittest discover -s coverage/kubeflux -p 'test_*.py'
```

Each CLI audit run copies examples to temporary repositories, serves local HTTP responses,
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
Unexpected HTTP requests are errors; the Python runner intercepts external HTTP(S)
requests through proxy environment variables scoped to the CLI subprocess.

Choose desired values independently of CLI output. Develop examples red-first and keep
new capabilities TODO until their checks pass; feature fixes belong in the normal
red/green regression suite. When promoting a case, register its name (hyphens become
underscores) in [tests/rust_corpus.rs](../../tests/rust_corpus.rs). A registration test
ensures the Rust suite covers every passing case. Unsupported assertion forms fail
explicitly and need a corresponding harness extension. Python runner checks remain
opt-in as shown above.
