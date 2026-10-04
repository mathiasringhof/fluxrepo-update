# Domain guidance

This repository has one domain context. Before changing architecture, terminology,
or behavior:

1. Read the root [CONTEXT.md](../../CONTEXT.md) and use its terms in code, tests, and proposals.
2. Read relevant [architecture decisions](../adr/). Discovery, source equivalence,
   and coverage changes must account for [ADR 0001](../adr/0001-conservative-repository-coverage.md).
3. Surface any conflict with an existing decision before replacing it. Record resolved
   terminology in `CONTEXT.md` and durable design decisions in `docs/adr/`.

Keep user-facing behavior in [usage](../usage.md), [output](../output.md), or
[coverage](../coverage.md); the glossary names concepts rather than repeating those guides.
