---
name: pm
description: Product management for the current agentflare project — run /pm:standup (daily activity digest), /pm:groom (backlog grooming + RICE/ICE prioritization), /pm:plan (Now/Next/Later sprint bucketing), or /pm:health (velocity + WIP + bottleneck scorecard). Read-only; operates on agentflare items via MCP.
---

# PM Agent — product management over agentflare items

## Read-only contract (non-negotiable)

These workflows NEVER mutate items. Do not call `item` with any of:
create, update, update_state, delete, claim, heartbeat, release, done, cancel,
add_label, remove_label — nor `comment` create/edit/delete. You may only read
(`item` list/get/search/groom/standup/health, `comment` list, `handoff` inbox, `memory`).
Output is suggestions for a human, never actions taken.

All content authored from public PM methodologies (RICE, ICE, MoSCoW, Now/Next/Later). No third-party notices required.

## Scope

One project only — whichever project the current repo resolves to. No
cross-project aggregation.

## Workflows

Before any workflow, read `reference/read-recipe.md`. Grooming, planning, and
health additionally use `reference/rubric.md`.

### /pm:standup — daily activity digest

Arg: cutoff (default: items with `updated_at` within the last 24h).

1. One call: `item action="standup" cutoff_hours=<hours, default 24>`. The
   server returns `done` (completed within cutoff_hours), `in_progress`
   (grouped by assignee, "unassigned" as its own group), and `stuck`
   (in-progress items older than `staleness_days`, default 7) — already
   bucketed, no hand-sorting a flat `list` result.
2. For each item print `FIX-NN · <name> · <assignee or unassigned>`.
3. Print the read-recipe time-signal caveat.
Read-only: never change item state.

### /pm:groom — backlog grooming + prioritization

Arg: staleness threshold in days (default 14).

1. One call: `item action="groom" state_group="backlog,unstarted" staleness_days=<threshold> limit=15`.
   This replaces the old `list` + N×`get` + hand-computed flags — the server
   already returns the shortlist (priority + recency ranked, full description)
   with `stale`, `unassigned`, `blocked_by`, `depended_on_by_count`,
   `possible_duplicates`, `size`/`unestimated` precomputed per item, plus
   `pull_next` and the summary counts. Do not re-derive these by eyeballing
   timestamps or text — they're already computed.
2. Score each shortlisted item with reference/rubric.md (RICE using the
   returned `size` where present, ICE fallback where `unestimated=true`) —
   your judgment is only needed for Reach and Confidence, which the server
   can't infer from free text. Print a ranked table: rank · FIX-NN · name ·
   score · one-line reason.
3. Flag lists — read straight from the response, no recomputation:
   - **Stale**: items with `stale=true`.
   - **Unassigned**: items with `unassigned=true` (`unassigned_count` for the total).
   - **Blocked**: items with non-empty `blocked_by`.
   - **Likely duplicates**: items with non-empty `possible_duplicates`.
   - **Unestimated**: items with `unestimated=true` (`unestimated_count` for the
     total) — recommend adding `metadata={"size":"S"|"M"|"L"}` via `item(update)`.
4. **Pull next**: the response's `pull_next` (top 3 unassigned/not-stale/unblocked
   by rank) — cross-check against your RICE ranking and note if they diverge.
5. Print the time-signal caveat. Read-only — `groom` only reads.

### /pm:plan — Now / Next / Later bucketing

Arg: capacity hint like "~8" (optional; caps the Now bucket).

1. One call: `item action="groom" state_group="backlog,unstarted" capacity=<hint or a sane default like 5>`.
   The server does the bucketing: `now` (top-`capacity` ready items — unblocked,
   has a `size`), `next` (remaining ready items), `later` (blocked items),
   `needs_estimation` (unestimated — excluded from planning). No hand-bucketing.
2. Score each item with reference/rubric.md for the printed rationale (RICE
   using `size`, ICE fallback for `unestimated` ones) — your judgment covers
   Reach/Confidence, the buckets themselves are already computed.
3. Print each bucket as an ordered list of `FIX-NN · name · score`.
4. Print the time-signal caveat. Read-only — this proposes a plan, it does not
   assign or move items.

### /pm:health — team health scorecard

Arg: window in weeks (default 4).

1. One call: `item action="health" window_weeks=<N, default 4>`. The server
   returns `velocity` (oldest→newest weekly series + `velocity_trend`:
   up/down/flat), `wip` (list + count), `stuck` (WIP older than
   `staleness_days`, default 7), and `bottlenecks`/`bottleneck_note`.
2. `bottlenecks` is currently always empty — agentflare has no persisted
   handoff-history log distinct from item state yet, so this can't be
   computed server-side. Print `bottleneck_note` verbatim ("no handoff
   history") rather than inventing a signal.
3. One-glance scorecard: Velocity · WIP · Stuck · Bottlenecks.
4. Print the time-signal caveat. Read-only.
