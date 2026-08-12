---
description: Backlog grooming — ranked shortlist + stale/unassigned/blocked/duplicate flags (read-only)
argument-hint: [staleness-days] [rice|wsjf|value-effort]
---

Load the `pm` skill (Skill tool, name "pm") and run its `/pm:groom` workflow
exactly as written there. Arguments: "$ARGUMENTS" — optional staleness threshold
in days (default 14) and optional scoring framework (`rice` default, `wsjf`,
`value-effort`). Read-only — never mutate items.
