---
name: pm
description: Product management for any project — read-only reporting (/pm:standup, /pm:groom, /pm:plan, /pm:health, /pm:portfolio) plus PM mode, an explicitly-activated execution arm that creates and dispatches work via handoff instead of implementing it directly. Embedded in the agentflare binary, so it's available regardless of which project's .claude/skills is on disk.
---

# PM Agent — product management over agentflare items

## Two arms, one contract boundary

- **Reporting** (default, Part 1 below): `/pm:standup`, `/pm:groom`, `/pm:plan`,
  `/pm:health`, `/pm:portfolio` — always read-only, never mutate.
- **PM mode** (Part 2 below — explicit activation only: "act as project
  manager", "PM mode", "e2e project management", or `/pm` with no args /
  `/pm mode on`): the execution arm. Creates items, hands work off to real
  agents, never implements inline. Typing the literal `/pm` (bare) or `/pm
  mode on`/`/pm mode off` also flips a session-scoped flag in agentflare's
  UserPromptSubmit hook (mirrors flare-code's own session-mode flag) — once
  set, every subsequent turn in this session gets a "PM MODE ACTIVE" reminder
  injected automatically, so the mode survives context compaction instead of
  depending on the model remembering a skill instruction. Only `/pm mode off`
  clears it; it does not expire on its own.

Never treat a reporting workflow's inputs as license to mutate, and never
slip into PM mode's mutating behavior without one of the explicit triggers
above.

## Scope

Default: one project — whichever project the current repo resolves to.
`/pm:portfolio` is the one reporting exception: it loops the read-only
reports across every project in the workspace via the `project` override
param (still read-only, still one workspace). PM mode's dispatch actions
likewise default to the current repo's linked project unless told otherwise.

---

## Part 1 — Reporting workflows (read-only, non-negotiable)

Never call `item` with any of: create, update, update_state, delete, claim,
heartbeat, release, done, cancel, add_label, remove_label — nor `comment`
create/edit/delete. Only read (`item` list/get/search/groom/standup/health,
`comment` list, `handoff` inbox, `memory`). Output is suggestions for a
human, never actions taken.

All content authored from public PM methodologies (RICE, ICE, WSJF,
Value-Effort, MoSCoW, Now/Next/Later). No third-party notices required.

Before any workflow, read `reference/read-recipe.md`. Grooming, planning,
and health additionally use `reference/rubric.md`.

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

Args: staleness threshold in days (default 14); framework (`rice` default,
`wsjf`, `value-effort` — see reference/rubric.md's Framework selection).

1. One call: `item action="groom" state_group="backlog,unstarted" staleness_days=<threshold> limit=15`.
   This replaces the old `list` + N×`get` + hand-computed flags — the server
   already returns the shortlist (priority + recency ranked, full description)
   with `stale`, `unassigned`, `blocked_by`, `depended_on_by_count`,
   `possible_duplicates`, `size`/`unestimated` precomputed per item, plus
   `pull_next` and the summary counts. Do not re-derive these by eyeballing
   timestamps or text — they're already computed.
2. Score each shortlisted item with reference/rubric.md using the requested
   framework (RICE using the returned `size` where present, ICE fallback
   where `unestimated=true`; `wsjf`/`value-effort` when explicitly requested,
   marking `?` per item instead of ICE-falling-back when `unestimated=true`)
   — your judgment is only needed for Reach/Confidence (RICE) or the
   equivalent qualitative factors the other frameworks reuse, none of which
   the server can infer from free text. Print a ranked table: rank · FIX-NN ·
   name · score · one-line reason.
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

Args: capacity hint like "~8" (optional; caps the Now bucket); framework
(`rice` default, `wsjf`, `value-effort` — see reference/rubric.md's
Framework selection).

1. One call: `item action="groom" state_group="backlog,unstarted" capacity=<hint or a sane default like 5>`.
   The server does the bucketing: `now` (top-`capacity` ready items — unblocked,
   has a `size`), `next` (remaining ready items), `later` (blocked items),
   `needs_estimation` (unestimated — excluded from planning). No hand-bucketing.
2. Score each item with reference/rubric.md for the printed rationale, using
   the requested framework (RICE using `size`, ICE fallback for `unestimated`
   ones; `wsjf`/`value-effort` when explicitly requested) — your judgment
   covers the qualitative factors, the buckets themselves are already
   computed.
3. Print each bucket as an ordered list of `FIX-NN · name · score`.
4. Print the time-signal caveat. Read-only — this proposes a plan, it does not
   assign or move items.

### /pm:health — team health scorecard

Arg: window in weeks (default 4).

1. One call: `item action="health" window_weeks=<N, default 4>`. The server
   returns `velocity` (oldest→newest weekly series + `velocity_trend`:
   up/down/flat), `wip` (list + count), `stuck` (WIP older than
   `staleness_days`, default 7), and `bottlenecks`/`bottleneck_note`.
2. `bottlenecks` lists items handed between different agents ≥2× inside the
   window, computed server-side from the persisted assignment log (written on
   every claim/reassignment). Print each entry as returned (`#N name — K
   handoffs (owner chain)`), plus `bottleneck_note` — it carries the one
   caveat that matters: history starts at the assignment-log migration, so
   older transitions are invisible.
3. One-glance scorecard: Velocity · WIP · Stuck · Bottlenecks.
4. Print the time-signal caveat. Read-only.

### /pm:portfolio — cross-project roll-up

Args: which report (`health` default, or `standup`); the report's own args
pass through (window weeks / cutoff hours).

1. One call: `project action="list"` — every project in the linked workspace.
2. For each project, one call: `item action="<health|standup>"
   project=<project name>` — the `project` override is honored only by the
   read-only reporting actions, so this stays mutation-free by construction.
3. Print one roll-up table, one row per project:
   - health: project · velocity trend · WIP · stuck · bottleneck count.
   - standup: project · done · in-progress · stuck counts.
   Follow with a short "needs attention" list: any project with stuck items,
   a `down` velocity trend, or non-empty bottlenecks, and why.
4. Print the time-signal caveat once (it applies to every row). Read-only.

---

## Part 2 — PM mode (mutating — explicit activation only)

In this mode you manage work, you don't do it. Every task becomes a tracked
item, handed off to a real agent, worked autonomously by the daemon's own
supervisor — never implemented inline by you.

### When to use

- User says "act as project manager", "PM mode", "e2e project management",
  or otherwise asks you to delegate rather than implement.
- NOT for `/pm:standup`/`/pm:groom`/`/pm:plan`/`/pm:health`/`/pm:portfolio` —
  those are Part 1's read-only reporting workflows and stay read-only even
  while PM mode is active for other tasks. PM mode is the execution arm: it
  mutates (creates items, hands off work), Part 1 never does.

### Contract

1. **Never implement directly.** Diagnose/scope the work, then hand it off —
   don't Write/Edit code yourself while this mode is active.
2. **Validate the recipient before handoff.** Check `agentflare agents list`
   (or agent_registry) first — a typo'd or thematic recipient (e.g.
   `"gastown"`, a codename, not an agent) silently orphans the item since
   nothing will ever claim it. Only real registered agents, or the reserved
   `"github"` recipient, are valid.
3. **Classify the task type, then shape the handoff content for it.**
   Diagnosis already tells you what kind of work this is — decide the
   framing before writing the handoff `content`, using the table below.
   This is not about picking a different recipient (only `claude-code` and
   `opencode` are real registered agents today) — it's about how the
   dispatched session should read the prompt once it claims the item. A
   diff dumped with no ask gets worked generically; a framed request gets
   the deliverable you actually want.
4. **Use `handoff`, not raw `item update`.** `mcp__flare__handoff` with
   recipient + name + content + completed + remaining (both required) —
   creates or targets an item and attaches the work as a versioned asset.
   `item action=update assignee_agent=...` skips the asset trail.
5. **Let the daemon dispatch it — don't run `agentflare work <id>`
   yourself.** A freshly-handed-off item to a real agent gets auto-labeled
   `ready-for-work`; the daemon's own `agentflare-supervisor` discovery tick
   claims and dispatches it autonomously. Manually running `agentflare work`
   defeats the point of this mode.
6. **Monitor, don't poll blindly.** `agentflare daemon logs [--follow]` for
   live activity; item `get`/`list` for state/assignee — an
   instance-suffixed `assignee_agent` like `claude-code:abc123` confirms
   real dispatch, not just a queued handoff. The `agent_jobs` table
   (`state` + `output.exit_code`) is ground truth for success/failure —
   `state=exited` alone does not mean success, always check `exit_code`.
7. **Report as a status table**, not a raw dump: item · state · assignee ·
   one-line note. Flag anything stuck or orphaned.

### Task-type routing heuristic

Before calling `handoff`, match the diagnosed work to a shape and frame the
`content` accordingly. The recipient stays whichever real agent is being
used — this only changes how the prompt inside the handoff is written.

| Task type | Signal from diagnosis | Frame the handoff content as |
|---|---|---|
| Review | "review this PR/diff", find bugs or style issues, no code should change | State it's review-only up front + link/diff + what to check for + "report findings, don't fix" |
| Research | "investigate", "find out", "what's the state of X" | The question + scope boundaries + expected output shape (summary, comparison, recommendation) |
| Bugfix | Reproducible failure, stack trace, failing test | Repro steps + expected vs. actual + suspected area — not just "fix this" |
| Design-spec | "design", "propose an approach", pre-implementation | Constraints + goals + explicitly ask for a written plan/spec, not code |
| Implementation | Already scoped/designed change ready to build | Spec or acceptance criteria + relevant files — enough to build without re-deriving intent |

A handoff that just pastes a diff or a one-line title forces the dispatched
session to guess the task type from content alone — it'll often default to
"implement", which is wrong for review/research/design work.

### Quick reference

| Step | Tool |
|---|---|
| Validate recipient | `agentflare agents list` |
| Create + dispatch | `mcp__flare__handoff` (recipient, name, content, completed, remaining) |
| Confirm autonomous pickup | `agentflare daemon logs`, item `get` (instance-suffixed assignee) |
| Check real outcome | `agent_jobs` table: `state` + `output.exit_code`, never state alone |
| Report | status table: item · state · assignee · note |

### Common mistakes

- Trusting `handoff`'s recipient field without checking it's real — creates
  an orphaned item nobody will ever work.
- Dispatching manually via `agentflare work` "to save time" — defeats the
  point of dogfooding the daemon; only do this if the daemon is confirmed
  down.
- Reading `state=exited` as success — check `exit_code`.
- Implementing the fix yourself because it's small — still delegate; PM
  mode has no size exception.
- Dumping a diff or a bare title into `content` without task-type framing —
  the dispatched session guesses "implement" by default and does the wrong
  thing for review/research/design work.
