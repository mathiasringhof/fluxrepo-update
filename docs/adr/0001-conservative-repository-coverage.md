# Conservative repository coverage

We report explicit Version Declarations across eligible repository manifests without
claiming deployment coverage; declarations that cannot be checked retain their location,
current value, and reason, while checking remains separate from write eligibility.
Coverage counts distinguish discovered, checked, and unchecked declarations;
existing best-effort skip behavior and exit codes remain unchanged.

Equivalent Helm Sources must match by name, unambiguous effective namespace, and complete source specification:
file location and descriptive metadata are ignored, but even harmless specification
differences remain unresolved to avoid field-specific equivalence rules.
Different same-name definitions remain ambiguous; no source is selected from conflicting definitions,
and all contributing equivalent source definitions must be bound to approval and rechecked
before application.

For [#16](https://github.com/mathiasringhof/fluxrepo-update/issues/16), derive effective namespaces
only through local `resources` file references and recursive directory references to
`kustomization.yaml`/`kustomization.yml`. Accept Kustomize configuration, including omitted
kind/API version; exclude Flux Kustomization objects. On each complete inclusion chain,
the outermost nonempty `namespace` wins. Without a transformer, retain the raw namespace.
Disagreeing chains or cycles remain unresolved as `ambiguous_kustomize_namespace`.

Use effective namespaces for source equivalence and sourceRef matching; an explicit
`sourceRef.namespace` stays authoritative, otherwise use the release's effective namespace.
JSON retains raw namespaces and adds effective namespaces and governing file paths.
Bind every contributing inclusion-chain file, including intermediate files without a
namespace, to selection identity and recheck its complete YAML snapshot before writes.
Namespace discovery never edits these files or changes scalar-edit validation.

Remote resources, `bases`, components, patches, generators, Flux `targetNamespace`, and
general Kustomize evaluation remain outside namespace discovery. Additional version
checks remain separate from the visibility boundary in [#17](https://github.com/mathiasringhof/fluxrepo-update/issues/17).
