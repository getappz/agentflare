---
name: pm
description: Product management for the current agentflare project — run /pm:standup (daily activity digest), /pm:groom (backlog grooming + RICE/ICE prioritization), /pm:plan (Now/Next/Later sprint bucketing), or /pm:health (velocity + WIP + bottleneck scorecard). Read-only; operates on agentflare items via MCP.
---

# PM Agent — product management over agentflare items

## Read-only contract (non-negotiable)

These workflows NEVER mutate items. Do not call `item` with any of:
create, update, update_state, delete, claim, heartbeat, release, done, cancel,
add_label, remove_label — nor `comment` create/edit/delete. You may only read
(`item` list/get/search, `comment` list, `handoff` inbox, `memory`). Output is
suggestions for a human, never actions taken.

## Scope

One project only — whichever project the current repo resolves to. No
cross-project aggregation.

## Workflows

Before any workflow, read `reference/read-recipe.md`. Grooming, planning, and
health additionally use `reference/rubric.md`.

<!-- Task 4: /pm:standup -->
<!-- Task 5: /pm:groom -->
<!-- Task 6: /pm:plan -->
<!-- Task 7: /pm:health -->
