# Read recipe — the current project's items

All PM workflows start here. One project only (resolved from the repo).

## Base list

Call `item` with `action="list"`. Add filters as needed:
- `state_group`: one of `backlog|unstarted|started|completed|cancelled|triage`,
  or a comma-separated set (e.g. `"backlog,unstarted,started"` = all open).
- `assignee_agent`: matches that agent PLUS unassigned items, open-first.

The list projection has ONLY: id, name, state, state_group, priority,
assignee_agent, parent_id, sequence_id, updated_at.

## Detail fetch (only when needed)

`item action="get" id=<id>` returns the full item incl. description, metadata,
labels, timestamps. Grooming/plan fetch detail ONLY for the shortlisted items
(cap at the top 15) to stay bounded.

## Time signals — approximate, state this in output

There is NO created_at and NO transition history in the list. Use `updated_at`
as a proxy for "last activity". Any "changed since / done in window / stale"
claim is therefore approximate; print: "⚠ time signals approximate (updated_at
proxy) — precise in a later release."

## Claims / handoffs

"In progress" = `state_group="started"` (do NOT infer from claim state).
Recent handoffs: `handoff` tool, inbox/thread reads only.
