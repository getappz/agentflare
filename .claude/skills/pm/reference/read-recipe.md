# Read recipe — the current project's items

All PM workflows start here. One project only (resolved from the repo).

## Base list

Call `item` with `action="list"`. Add filters as needed:
- `state_group`: one of `backlog|unstarted|started|completed|cancelled|triage`,
  or a comma-separated set (e.g. `"backlog,unstarted,started"` = all open).
- `assignee_agent`: matches that agent PLUS unassigned items, open-first.

The list projection has ONLY: id, name, state, state_group, priority,
assignee_agent, parent_id, sequence_id, updated_at.

## Standup: one call, not list+bucket

`item action="standup"` (optional `cutoff_hours` default 24, `staleness_days`
default 7 for the stuck threshold) returns `done`/`in_progress` (grouped by
assignee)/`stuck` pre-bucketed server-side, plus counts. Use this instead of
`list` + hand-sorting for standup.

## Grooming/plan: one call, not list+N×get

`item action="groom"` (optional `state_group`, `staleness_days` default 14,
`limit` default 15) returns the priority+recency-ranked shortlist with full
description AND precomputed `stale`/`unassigned`/`blocked_by`/
`depended_on_by_count`/`possible_duplicates`/`size`/`unestimated`, plus
`pull_next` and summary counts — computed server-side in one round trip.
Do not fall back to `list` + per-item `get` for grooming/planning; that was
the old N+1 path this action replaces.

## Detail fetch (only when needed)

`item action="get" id=<id>` returns one full item incl. description, metadata,
timestamps — for a single ad-hoc lookup outside grooming, not for building a
shortlist (use `groom` for that). Labels are a separate join, not part of
this response.

## Time signals — approximate, state this in output

There is NO created_at and NO transition history in the list. Use `updated_at`
as a proxy for "last activity". Any "changed since / done in window / stale"
claim is therefore approximate; print: "⚠ time signals approximate (updated_at
proxy) — precise in a later release."

## Claims / handoffs

"In progress" = `state_group="started"` (do NOT infer from claim state).
Recent handoffs: `handoff` tool, inbox/thread reads only.
