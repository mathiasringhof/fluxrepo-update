# Docs

These docs are organized around how you use the tool:

- [usage.md](usage.md): command guide, safe first-run flow, interactive vs automation modes, and exit codes
- [output.md](output.md): how to interpret `inventory` and `update-helm --json` output
- [coverage.md](coverage.md): what the tool updates, what it only inventories, and what stays out of scope

Suggested reading order for a new user:

1. [README.md](../README.md)
2. [usage.md](usage.md)
3. [coverage.md](coverage.md)

Design references:

- [Domain language](../CONTEXT.md)
- [Manifest-local discovery decision](adr/0001-manifest-local-schema-driven-discovery.md)
- [Helm image value schema research](research/helm-image-value-schemas.md)
