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

All content authored from public PM methodologies (RICE, ICE, MoSCoW, Now/Next/Later). No third-party notices required.

## Scope

One project only — whichever project the current repo resolves to. No
cross-project aggregation.

## Workflows

Before any workflow, read `reference/read-recipe.md`. Grooming, planning, and
health additionally use `reference/rubric.md`.

### /pm:standup — daily activity digest

Arg: cutoff (default: items with `updated_at` within the last 24h).

1. Read items: `item action="list" state_group="started,completed"`.
2. Bucket:
   - **Done** — state_group=completed, updated_at ≥ cutoff.
   - **In progress** — state_group=started (all), grouped by assignee_agent.
   - **Stuck** — in-progress items whose updated_at is older than 7 days.
3. For each item print `FIX-NN · <name> · <assignee or unassigned>`.
4. Print the read-recipe time-signal caveat.
Read-only: never change item state.

### /pm:groom — backlog grooming + prioritization

Arg: staleness threshold in days (default 14).

1. Read open items: `item action="list" state_group="backlog,unstarted"`.
2. Shortlist the top candidates by `priority` (urgent>high>medium>low>none),
   cap 15, and `item action="get"` each for description/labels.
3. Score each shortlisted item with reference/rubric.md (RICE, ICE fallback).
   Print a ranked table: rank · FIX-NN · name · score · one-line reason.
4. Flag lists (from the full open list, no get needed):
   - **Stale**: updated_at older than &lt;threshold&gt; days.
   - **Unassigned**: assignee_agent is null.
   - **Likely duplicates**: items whose names are near-identical (same key tokens).
   - **Unestimated**: no size/effort signal (from the shortlist gets).
5. **Pull next**: top 3 ranked items that are unassigned and not stale.
6. Print the time-signal caveat. Read-only.

### /pm:plan — Now / Next / Later bucketing

Arg: capacity hint like "~8" (optional; caps the Now bucket).

1. Reuse the groom ranking (steps 1–3 of /pm:groom).
2. Bucket by rank and readiness:
   - **Now**: highest-ranked items that are ready (have an estimate, not blocked
     by an open dependency). Cap to the capacity hint if provided.
   - **Next**: next tier by rank.
   - **Later**: the tail + anything low-confidence.
3. Separately list **Needs estimation** (unestimated items) — cannot be planned.
4. Print each bucket as an ordered list of `FIX-NN · name · score`.
5. Print the time-signal caveat. Read-only — this proposes a plan, it does not
   assign or move items.

### /pm:health — team health scorecard

Arg: window in weeks (default 4).

1. Velocity: `item action="list" state_group="completed"`; per rubric.md, count
   items whose `updated_at` falls in each trailing 7-day window; show the series
   and the trend arrow.
2. WIP: `item action="list" state_group="started"`; report the count and list.
3. Stuck: WIP items with `updated_at` older than 7 days.
4. Bottlenecks: read `handoff` history (read-only) for items handed off
   repeatedly; if none available, print "no handoff history".
5. One-glance scorecard: Velocity · WIP · Stuck · Bottlenecks.
6. Print the time-signal caveat. Read-only.
