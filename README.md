<div align="center">

# leanstack

**A single Rust binary. No Node, no runtime dependencies. lean-ctx + engram
powered token-saving stack across Claude Code, Codex, Cursor, Windsurf,
VS Code, Cline, and Continue.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>

---

## What this is

Two non-overlapping layers, plus two Claude-Code-only companions:

| Layer | What it compresses | Tool |
|---|---|---|
| **lean-ctx** | tool I/O *within* a session — reads, shell output, search, up to 99% | [yvgude/lean-ctx](https://github.com/yvgude/lean-ctx) |
| **engram** | knowledge *across* sessions — decisions, facts, preferences that survive a session ending | [Gentleman-Programming/engram](https://github.com/Gentleman-Programming/engram) |
| **Caveman** (Claude Code only) | conversation verbosity, ~65% | companion plugin |
| **Ponytail** (Claude Code only) | code-writing over-engineering | companion plugin |

lean-ctx and engram aren't substitutes for each other — one saves tokens inside a
session, the other saves the re-explaining tax across sessions.

**Why Rust, not Node:** Claude Code doesn't bundle or require Node.js — it's a
standalone compiled binary. A plugin whose hooks shell out to `node` breaks on
any machine that installed Claude Code without separately installing Node.
leanstack is a single static binary; the only runtime dependency is leanstack
itself.

**No plugin marketplace for Claude Code or Cursor** — `leanstack init --agent X`
writes the hook config directly into the target's own settings file (Claude
Code's `~/.claude/settings.json`, Cursor's `.cursor/hooks.json`). Codex is the
one exception: its hook system only activates through its plugin loader, so
that wiring ships as a small `.codex-plugin/` manifest instead.

## Metrics

Numbers below are each project's own published, reproducible benchmarks — attributed,
not blended into a fake combined total, and not accepted on faith. Where a claim had no
supporting evidence in its own repo, it's flagged instead of repeated.

| Tool | Published claim | Methodology | Confidence |
|---|---|---|---|
| lean-ctx | 98.1% compression (`map` mode), 96.7% (`signatures`), ~99.99% cached re-read | CI-gated, reproducible via `lean-ctx benchmark report .`, measured on a 50-file repo with the GPT-4o tokenizer | High — real, reproducible, methodology named |
| Caveman | 65% avg output-token reduction (range 22–87%, 10 prompts) | Committed in `benchmarks/`/`evals/` — and its own docs flag the failure mode: ~1–1.5k input-token overhead per turn can make it net-negative on already-terse workloads (`docs/HONEST-NUMBERS.md`) | High — reproducible, unusually transparent about limits |
| Ponytail | ~54% less code (94% ceiling on best task), ~20% cheaper, ~27% faster, 100% safe | 12 real feature tasks on a FastAPI+React repo, Haiku 4.5, n=4 — self-corrected an earlier overgeneralized single-shot figure | High — reproducible, self-corrected once already |
| engram | "95.8% accuracy on LongMemEval-S — #1 globally" | **Not citing this.** No benchmark directory, results file, or harness exists anywhere in the repo, despite being listed as a shipped feature. | Unverifiable — flagged, not repeated |

### Real usage, one live project

Not a demo — pulled live from the maintainer's own project while building this repo,
for a sense of scale. Not a controlled benchmark; one data point, your mileage varies.

```
lean-ctx   34.2M tokens saved   92% compression   $88.45 saved   (lifetime; lean-ctx gain)
caveman    1.16M tokens saved (~65%)                              (single session; caveman-stats hook)
ponytail   23 `ponytail:` shortcut markers logged, no token figure (ponytail doesn't measure per-repo savings)
engram     2 sessions, 11 observations tracked, across 2 projects  (engram stats)
```

Check your own: `lean-ctx gain` · `/caveman-stats` (Claude Code) · `ponytail-debt` skill ·
`engram stats`. Don't trust this table blindly either — re-run those commands yourself.

---

## Install the CLI

**Linux/macOS** (downloads a prebuilt binary, checksum-verified; builds from
source instead if run from inside a clone):
```bash
curl -fsSL https://raw.githubusercontent.com/getappz/leanstack/main/install.sh | sh
```

**Homebrew:**
```bash
brew tap getappz/leanstack
brew install leanstack
```

**Windows, build from source** (no unsigned prebuilt binary to trip an AV
heuristic):
```powershell
git clone https://github.com/getappz/leanstack
cd leanstack
.\install.ps1
```

**Windows, Scoop** (prebuilt binary — unsigned, like most small Rust CLIs;
Defender/SmartScreen false-positives are possible, report an issue if hit):
```powershell
scoop bucket add leanstack https://github.com/getappz/leanstack
scoop install leanstack
```

**Windows, Winget:**
```powershell
winget install getappz.leanstack
```

**Any platform with Rust, no clone needed:**
```bash
cargo install --git https://github.com/getappz/leanstack
```

**Uninstall:**
```bash
curl -fsSL https://raw.githubusercontent.com/getappz/leanstack/main/install.sh | sh -s -- --uninstall
```

## Set up an agent

One command per tool, run once. Running it is the consent — installs happen
immediately, no separate confirm step.

```bash
leanstack init --agent claude-code    # writes ~/.claude/settings.json hooks directly, no marketplace
leanstack init --agent cursor         # writes .cursor/hooks.json directly, no marketplace
leanstack init --agent windsurf
leanstack init --agent vscode-copilot
leanstack init --agent cline
leanstack init --agent continue
```

**Codex** is the one exception — its hook system only activates through its own
plugin loader:
```
codex plugin marketplace add getappz/leanstack
codex plugin install leanstack
```
then `leanstack init --agent codex` for the rules/lean-ctx/engram setup (Codex's
hook wiring itself comes from the plugin manifest, not `init`).

Each run: writes rule files (if absent), installs lean-ctx (`npm install -g
lean-ctx-bin && lean-ctx onboard`) and engram (`go install`/`brew`, never the
prebuilt Windows binary — see below) if missing, wires hooks/MCP where the host
supports it. Detection-first — already-satisfied components are skipped, nothing
gets clobbered.

**engram's install safety**: the project's own docs say prebuilt Windows binaries
get flagged as AV false positives, and explicitly recommend `go install` or
Homebrew instead. `leanstack init` only auto-installs engram through one of those
two paths; if neither is available, it prints the exact command instead of
downloading something that might trip your AV.

## Docs-only fallback (Aider, other AGENTS.md readers)

```bash
curl -sL https://raw.githubusercontent.com/getappz/leanstack/main/AGENTS.md > AGENTS.md
```

---

## Architecture

```
src/
├── paths.rs             # home-dir resolution (LEANSTACK_HOME_OVERRIDE for tests —
│                         # dirs::home_dir() ignores HOME/USERPROFILE overrides on
│                         # Windows, learned the hard way)
├── state.rs              # ~/.leanstack/state.json — on/off flag for the hooks
├── rule_text.rs           # shared rule copy (Exa, git, lean-ctx, engram usage)
├── engram_install.rs      # engram's safe-install logic (go install/brew only)
├── components.rs          # registry: each entry checks + fixes itself, host-aware
├── init.rs                # `leanstack init --agent X` — runs every component,
│                           # wires hooks directly for claude-code/cursor
├── hook.rs                # `leanstack hook session-start|prompt-submit --agent X`
└── main.rs                 # clap CLI, dispatch

.codex-plugin/              # Codex only — its hooks require the plugin loader
install.sh, install.ps1      # installers (checksum-verified download / local build)
.github/workflows/          # ci.yml (build+test), release.yml (cross-compile on tag)
```

Adding a new managed component means adding one entry to `components.rs` — neither
`init` nor `hook` hardcodes per-tool logic.

---

## What Gets Created

**Claude Code**: `~/.claude/rules/{exa,git,lean-ctx,engram}.md`, `~/.claude/settings.json` hooks section, `~/.config/{caveman,ponytail}/config.json`, `~/.leanstack/`.

**Codex**: project-local `AGENTS.md` (only if absent), `~/.leanstack/`.

**Cursor**: project-local `.cursor/rules/leanstack.mdc`, `.cursor/hooks.json`, `~/.cursor/mcp.json`, `~/.leanstack/`.

**Windsurf/VS Code/Cline**: project-local rules file (see table above), MCP config for lean-ctx/engram.

**Continue**: `.continue/mcpServers/{leanstack,engram}.json`.

Nothing is created if it already exists.

---

## Uninstall

Remove the binary (see Install section above), then remove whatever `init`
wrote for the hosts you set up — see "What Gets Created" above. Ponytail/
Caveman/engram plugins themselves stay installed (uninstall separately if
wanted).

---

<div align="center">

MIT License

</div>
