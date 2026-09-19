# Flux Repository Updates

This context describes how version changes are discovered, reviewed, and applied to Flux repository manifests.

## Language

**Interactive approval**:
The manual review step in which a person decides, one proposal at a time, which planned Flux manifest version changes may be applied.
_Avoid_: Prompt loop

**Flux Repository Inventory**:
The discovered manifest-local targets, source identities, original document snapshots,
and exclusions from one repository scan.

**Planned Update**:
One resolved version change whose target, source identity, current value, and proposed
value are fixed before review.

**Update Plan**:
The complete ordered collection of Planned Updates and unresolved attempts. It remains
intact when some updates are declined or a selection is rejected.

**Update Selection Identity**:
The opaque identity binding a Planned Update to its target, source context, and versions.
It is stable for equivalent inputs and invalidates an approval when those facts change.

**Update Review**:
Immutable presentation facts used by Interactive approval to choose Update Selection
Identities without changing the underlying Planned Updates.

**Approved Update**:
A Planned Update selected by its Update Selection Identity for application.

**Update Run**:
One attempt to plan, select or review, and optionally apply updates from a Flux Repository
Inventory. The public interface owns workflow decisions and hides concurrency and editing.

**Update Run Mode**:
One valid operation: Plan Only, Review and Apply, Apply All, or Apply Selected.

**Update Run Outcome**:
The complete Update Plan, approved and applied identities, ordered unique relative changed
paths, status, and any selection rejection. Operational failures return an error instead.

**Update Run Status**:
No Updates, Updates Planned, No Updates Approved, Updates Applied, or Run Rejected.
No Updates describes the plan and does not imply that unresolved targets are current.

**Update Run Rejection**:
A selection containing unknown identities. The outcome retains the plan and all unknown
identities; nothing is approved or written.
