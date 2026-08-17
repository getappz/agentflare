---
description: Act as the project's PM — bare /pm enables PM mode and runs the daily kickoff; subcommands give targeted reports (standup, groom, plan, health, portfolio)
argument-hint: [standup|groom|plan|health|portfolio|mode] [args]
---

Parse "$ARGUMENTS": the first word (if any) is the subcommand; everything after
it is that subcommand's arguments.

## Bare `/pm` (no arguments) — start the PM day

1. Load the `pm` skill (covers both reporting and PM mode) and enter PM
   mode — Part 2 of the skill: create and dispatch work instead of
   implementing it yourself — until `/pm mode off`.
2. Run the daily kickoff, in this order:
   a. **Standup** — `/pm:standup` workflow (last 24h): what shipped, what's in
      flight per assignee, what's stuck.
   b. **Intake triage** — `/pm:groom` workflow's flag lists only: new/unassigned
      items, blocked items, likely duplicates, unestimated items.
   c. **Blockers first** — for each stuck or blocked item, say what unblocks it
      and who should act.
   d. **Pull next** — the groom `pull_next` shortlist, cross-checked against
      priorities.
3. Close with a **morning briefing**: ≤10 lines — Done / In flight / Stuck /
   Recommended dispatches — then propose the concrete dispatch actions PM mode
   allows (assignments, handoffs, item creation) and wait for approval before
   executing any of them.

## Subcommands (targeted, read-only reports — no PM mode change)

- **standup** `[cutoff-hours]` — `pm` skill `/pm:standup`; cutoff default 24.
- **groom** `[staleness-days] [rice|wsjf|value-effort]` — `/pm:groom`;
  staleness default 14, framework default `rice`.
- **plan** `[~capacity] [rice|wsjf|value-effort]` — `/pm:plan`; capacity hint
  like `~8` caps the Now bucket.
- **health** `[window-weeks]` — `/pm:health`; window default 4.
- **portfolio** `[standup|health] [args]` — `/pm:portfolio`: the chosen report
  (default `health`) rolled up across every project in the workspace.
- **mode on** — enable PM mode (Part 2 of the `pm` skill) without the daily
  kickoff.
- **mode off** — leave PM mode: stop following Part 2, return to normal
  implementation behavior, confirm in one line.

Typing the literal `/pm`, `/pm mode on`, or `/pm mode off` also sets/clears a
session-scoped flag in agentflare's own hook — the running session gets a "PM
MODE ACTIVE" reminder on every subsequent turn until `/pm mode off`, so the
mode holds even across context compaction, not just while the model happens
to remember it.

The `pm` skill's Part 1 report workflows are read-only over items — item
mutations happen only through Part 2's PM-mode dispatch actions the user has
approved. Unknown subcommand → one-line usage summary, then stop.
