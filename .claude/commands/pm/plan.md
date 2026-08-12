---
description: Sprint bucketing — Now / Next / Later + needs-estimation over the backlog (read-only)
argument-hint: [~capacity] [rice|wsjf|value-effort]
---

Load the `pm` skill (Skill tool, name "pm") and run its `/pm:plan` workflow
exactly as written there. Arguments: "$ARGUMENTS" — optional capacity hint like
"~8" (caps the Now bucket) and optional scoring framework (`rice` default,
`wsjf`, `value-effort`). Read-only — this proposes a plan, it never assigns or
moves items.
