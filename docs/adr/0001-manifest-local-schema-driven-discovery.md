# Use manifest-local, schema-driven discovery

Version discovery uses explicit values and supported schemas within each manifest rather than evaluating Kustomize composition or inferring values across files. This deliberately trades some coverage for predictable, safe updates and a simpler extension model: unknown schemas remain unchanged until concrete examples justify a built-in adapter.
