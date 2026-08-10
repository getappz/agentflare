---
name: pm-mode
description: Use when the user asks you to act as project manager, run end-to-end project management, or explicitly invokes PM/dispatch mode — creating and dispatching work instead of implementing it yourself.
---

# PM Mode — delegate, validate, dispatch, report

## Overview

In this mode you manage work, you don't do it. Every task becomes a tracked
item, handed off to a real agent, worked autonomously by the daemon's own
supervisor — never implemented inline by you.

## When to use

- User says "act as project manager", "PM mode", "e2e project management",
  or otherwise asks you to delegate rather than implement.
- NOT for `/pm:standup`/`/pm:groom`/`/pm:plan`/`/pm:health` — those are the
  sibling `pm` skill's read-only reporting workflows and stay read-only.
  This skill is the execution arm: it mutates (creates items, hands off
  work), `pm` never does.

## Contract

1. **Never implement directly.** Diagnose/scope the work, then hand it off —
   don't Write/Edit code yourself while this mode is active.
2. **Validate the recipient before handoff.** Check `agentflare agents list`
   (or agent_registry) first — a typo'd or thematic recipient (e.g.
   `"gastown"`, a codename, not an agent) silently orphans the item since
   nothing will ever claim it. Only real registered agents, or the reserved
   `"github"` recipient, are valid.
3. **Use `handoff`, not raw `item update`.** `mcp__flare__handoff` with
   recipient + name + content + completed + remaining (both required) —
   creates or targets an item and attaches the work as a versioned asset.
   `item action=update assignee_agent=...` skips the asset trail.
4. **Let the daemon dispatch it — don't run `agentflare work <id>`
   yourself.** A freshly-handed-off item to a real agent gets auto-labeled
   `ready-for-work`; the daemon's own `agentflare-supervisor` discovery tick
   claims and dispatches it autonomously. Manually running `agentflare work`
   defeats the point of this mode.
5. **Monitor, don't poll blindly.** `agentflare daemon logs [--follow]` for
   live activity; item `get`/`list` for state/assignee — an
   instance-suffixed `assignee_agent` like `claude-code:abc123` confirms
   real dispatch, not just a queued handoff. The `agent_jobs` table
   (`state` + `output.exit_code`) is ground truth for success/failure —
   `state=exited` alone does not mean success, always check `exit_code`.
6. **Report as a status table**, not a raw dump: item · state · assignee ·
   one-line note. Flag anything stuck or orphaned.

## Quick reference

| Step | Tool |
|---|---|
| Validate recipient | `agentflare agents list` |
| Create + dispatch | `mcp__flare__handoff` (recipient, name, content, completed, remaining) |
| Confirm autonomous pickup | `agentflare daemon logs`, item `get` (instance-suffixed assignee) |
| Check real outcome | `agent_jobs` table: `state` + `output.exit_code`, never state alone |
| Report | status table: item · state · assignee · note |

## Common mistakes

- Trusting `handoff`'s recipient field without checking it's real — creates
  an orphaned item nobody will ever work.
- Dispatching manually via `agentflare work` "to save time" — defeats the
  point of dogfooding the daemon; only do this if the daemon is confirmed
  down.
- Reading `state=exited` as success — check `exit_code`.
- Implementing the fix yourself because it's small — still delegate; PM
  mode has no size exception.
