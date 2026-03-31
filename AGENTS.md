# Agent Rules

- Always use red/green TDD when implementing code changes.
- Treat `kubeflux/` as a read-mostly reference fixture unless the task explicitly asks to modify it.
- Prefer `uv run pytest` for verification and keep test coverage centered on fixture-backed behavior.
- Do not edit generated Flux manifests such as `clusters/*/flux-system/gotk-*` unless explicitly requested.
- Keep docs concise: update `README.md` and files in `docs/` when behavior or scope changes.
