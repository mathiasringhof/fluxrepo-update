# Domain Glossary

## Flux Repository Inventory

A snapshot of the update-relevant declarations discovered in one Flux repository, including version targets and the sources used to resolve them.

## Planned Update

A proposed change from a target's current version to a resolved candidate version.

## Approved Update

A planned update authorized for application during an Update Run.

## Update Run

A single attempt to plan updates from a Flux Repository Inventory, select or approve them according to its mode, and optionally apply them.

## Update Run Mode

The declared behavior of an Update Run: **Plan Only**, **Review and Apply**, **Apply All**, or **Apply Selected**.

## Update Plan

The complete set of Planned Updates and skipped update attempts produced before approval or application.

## Update Selection Identity

A stable identity for one Planned Update within a Flux repository, used to select it without reproducing its internal details.

## Update Review

The point in an Update Run at which planned updates are presented and approved selection identities are returned.

## Update Run Status

The canonical state reached by an Update Run: no updates, updates planned, no updates approved, updates applied, or run rejected.

## Update Run Outcome

The authoritative result of an Update Run. It retains the complete plan, skipped updates, approved subset, applied subset, changed files, and any expected rejection, regardless of which facts a particular presentation displays.

## Update Run Rejection

An expected refusal discovered during a valid Update Run, such as an unknown update selection, represented as part of its outcome rather than as an operational failure.
