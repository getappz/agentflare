---
name: flare-code-help
description: >
  Quick-reference card for all flare-code modes, skills, and commands.
  One-shot display, not a persistent mode. Trigger: /flare-code-help,
  "flare-code help", "what flare-code commands", "how do I use flare-code".
---

# Flare Code Help

Display this reference card when invoked. One-shot, do NOT change mode,
write flag files, or persist anything.

## Levels

| Level | Trigger | What change |
|-------|---------|-------------|
| **Lite** | `/flare-code lite` | Build what's asked, name the lazier alternative in one line. |
| **Full** | `/flare-code` | The ladder enforced: YAGNI → stdlib → native → one line → minimum. Default. |
| **Ultra** | `/flare-code ultra` | YAGNI extremist. Deletion before addition. Challenges requirements before building. |

Level sticks until changed or session end.

## Skills

| Skill | Trigger | What it does |
|-------|---------|--------------|
| **flare-code** | `/flare-code` | Lazy mode itself. Simplest solution that works. |
| **flare-code-review** | `/flare-code-review` | Over-engineering review: `L42: yagni: factory, one product. Inline.` |
| **flare-code-audit** | `/flare-code-audit` | Whole-repo over-engineering audit: ranked list of what to delete. |
| **flare-code-debt** | `/flare-code-debt` | Harvest `flare-code:` shortcut comments into a tracked ledger. |
| **flare-code-gain** | `/flare-code-gain` | Measured-impact scoreboard: less code, less cost, more speed. |
| **flare-code-help** | `/flare-code-help` | This card. |
| **flare-code-no-hallucination** | `/flare-code-no-hallucination` | Reality-check layer: blocks invented APIs, deprecated methods, undeclared variables. |

Codex uses `@flare-code`, `@flare-code-review`, and `@flare-code-help`; Claude Code
and OpenCode use the slash-command forms above (OpenCode ships all seven as
slash commands).

## Deactivate

Say "stop flare code" or "normal mode". Resume anytime with `/flare-code`.
`/flare-code off` also works.

## Configure Default Mode

Default mode = `full`, auto-active every session. Change it:

**Environment variable** (highest priority):
```bash
export FLARE_CODE_DEFAULT_MODE=ultra
```

**Config file** (`~/.config/agentflare/flare-code/config.json`, Windows: `%APPDATA%\agentflare\flare-code\config.json`):
```json
{ "default_mode": "lite" }
```

Set `"off"` to disable auto-activation on session start, activate manually
with `/flare-code` when wanted.

Resolution: env var > config file > `full`.

## Update

Enable auto-update once: open `/plugin`, go to Marketplaces, pick flare-code, Enable auto-update. Claude Code then pulls new versions at startup (run `/reload-plugins` when it prompts). Manual refresh: `/plugin marketplace update flare-code` then `/reload-plugins`.

If `/plugin` is not recognized, your Claude Code is out of date. Update it (`npm install -g @anthropic-ai/claude-code@latest`, or `brew upgrade claude-code`) and restart. Other hosts use their own update flow.

## More

Full docs + examples: https://github.com/DietrichGebert/flare-code
