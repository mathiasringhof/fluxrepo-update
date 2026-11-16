# Fail a run when every Version Check fails

For [#18](https://github.com/mathiasringhof/fluxrepo-update/issues/18), a run with zero successful
Version Checks and at least one Failed Version Check returns exit `2`, retaining the
full report on stdout. This narrows the unchanged-exit policy in
[ADR 0001](0001-conservative-repository-coverage.md): mixed runs keep their partial plans,
failure details, and existing exits, without a strict-mode option.

Unsupported forms/version schemes and missing or ambiguous manifest-local source
identities remain unsupported. Unsupported-only and empty inventories exit `0` and
explicitly report that nothing was checked. Failed requests (regardless of retryability),
missing charts/versions, malformed or inconsistent metadata, and unclassified resolution
failures count as failed checks. Classification uses existing skip reasons without
changing resolver behavior.

`UpdatePlan` exposes completeness and counts for the full check, independent of review
and application. Existing coverage counts retain their meanings: unchecked equals all
skips. New counts split checked into unchanged/update-available and unchecked into
unsupported/failed. GitHub groups count each declaration, as required by
[ADR 0002](0002-github-resource-approval-groups.md); planned/applied counts still count groups.

See [Output](../output.md#check_completeness) for the four completeness states and JSON contract.
