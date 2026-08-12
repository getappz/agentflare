---
description: Product management over agentflare items — standup, groom, plan, health, portfolio (read-only), or PM dispatch mode
argument-hint: <standup|groom|plan|health|portfolio|mode> [args]
---

Parse "$ARGUMENTS": the first word is the subcommand; everything after it is
that subcommand's arguments. Dispatch:

- **standup** `[cutoff-hours]` — load the `pm` skill (Skill tool, name "pm")
  and run its `/pm:standup` workflow. Cutoff in hours, default 24.
- **groom** `[staleness-days] [rice|wsjf|value-effort]` — load the `pm` skill
  and run its `/pm:groom` workflow. Staleness default 14 days; framework
  default `rice`.
- **plan** `[~capacity] [rice|wsjf|value-effort]` — load the `pm` skill and
  run its `/pm:plan` workflow. Capacity hint like `~8` caps the Now bucket.
- **health** `[window-weeks]` — load the `pm` skill and run its `/pm:health`
  workflow. Window in weeks, default 4.
- **portfolio** `[standup|health] [per-subcommand args]` — load the `pm`
  skill and run its `/pm:portfolio` workflow: the chosen report (default
  `health`) rolled up across every project in the workspace.
- **mode on** — load the `pm-mode` skill and act as project manager (create
  and dispatch work instead of implementing it yourself) until turned off.
- **mode off** — leave PM mode: stop following the `pm-mode` skill and return
  to normal implementation behavior. Confirm in one line.

The `pm` skill's workflows are read-only over items — never mutate item state
from any subcommand except what `pm-mode` itself explicitly directs while mode
is on. No subcommand or empty arguments → print a one-line usage summary of
the subcommands above and stop.
