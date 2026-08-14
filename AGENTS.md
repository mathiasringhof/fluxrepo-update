# Agent Rules

- Always use red/green TDD when implementing code changes.
- Before committing, always run the same checks as CI: `cargo fmt --all --check`,
  `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --locked`.
- Do not pin or assert exact CLI help text in tests; cover behavior instead.
- Do not edit generated Flux manifests such as `clusters/*/flux-system/gotk-*` unless explicitly requested.
- Keep docs concise: update `README.md` and files in `docs/` when behavior or scope changes.

## Agent skills

### Issue tracker

Issues are tracked in this repository's GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

Triage uses the five default canonical labels. See `docs/agents/triage-labels.md`.

### Domain docs

Domain documentation uses a single-context layout. See `docs/agents/domain.md`.
