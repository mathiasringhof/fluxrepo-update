# Conservative repository coverage

We report explicit Version Declarations across eligible repository manifests without
claiming deployment coverage; declarations that cannot be checked retain their location,
current value, and reason, while checking remains separate from write eligibility.
Coverage counts distinguish discovered, checked, and unchecked declarations;
existing best-effort skip behavior and exit codes remain unchanged.

Equivalent Helm Sources must match by name, raw namespace, and complete source specification:
file location and descriptive metadata are ignored, but even harmless specification
differences remain unresolved to avoid field-specific equivalence rules.
Different same-name definitions remain ambiguous, existing namespace checks remain intact,
and all contributing equivalent source definitions must be bound to approval and rechecked
before application.

Kustomize namespace transformations and additional version checks are deferred, leaving
the namespace-transformation portion of [#16](https://github.com/mathiasringhof/fluxrepo-update/issues/16)
open while addressing the visibility boundary in [#17](https://github.com/mathiasringhof/fluxrepo-update/issues/17).
