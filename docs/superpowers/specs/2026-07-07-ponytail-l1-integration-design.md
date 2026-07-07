# Ponytail L1 Integration — Design

**Issue:** [#42](https://github.com/getappz/agentflare/issues/42)
**Date:** 2026-07-07
**Branch:** `feature/ponytail-l1-integration`

## Goal

Port ponytail's runtime logic (config management, state tracking, instructions builder, mode switcher, platform output formatting) from Node.js hooks into agentflare's Rust binary. Prompt content (SKILL.md) stays external — fetched on demand from the ponytail repo.

agentflare becomes the hook provider that every AI agent platform calls. The ponytail npm plugin becomes a thin manifest pointing at `agentflare ponytail hook`.

## Architecture

```
┌─ CLI surface ──────────────────────────────────────────┐
│ agentflare ponytail setup          download SKILL.md    │
│ agentflare ponytail status         show active mode     │
│ agentflare ponytail set <MODE>     session-scoped mode  │
│ agentflare ponytail default <MODE> persist default mode │
│ agentflare ponytail off            shortcut off         │
│ agentflare ponytail update         re-download skill    │
│ agentflare ponytail hook <EVENT>   hook entrypoint      │
└─────────────────────────────────────────────────────────┘
         │
┌─ Core lib (src/ponytail/) ─────────────────────────────┐
│ mod.rs           — pub API, re-exports                 │
│ config.rs        — Config struct, mode resolution      │
│ state.rs         — flag file r/w (.ponytail-active)     │
│ instructions.rs  — SkillDoc, filter_skill, fallback    │
│ switcher.rs      — SwitchAction, detect_switch         │
│ platform.rs      — AgentPlatform, format_output        │
│ skill.md         — embedded default (fallback)          │
└─────────────────────────────────────────────────────────┘
         │
┌─ Storage ──────────────────────────────────────────────┐
│ ~/.config/agentflare/ponytail/config.json               │
│ ~/.local/state/agentflare/ponytail/active               │
│ ~/.cache/agentflare/ponytail/SKILL.md  (downloaded)      │
└─────────────────────────────────────────────────────────┘
```

State paths are agentflare-owned to avoid collision with existing ponytail plugin installs.

## Module Details

### config.rs

```rust
const DEFAULT_MODE: &str = "full";
const VALID_MODES: [&str; 5] = ["off", "lite", "full", "ultra", "review"];
const RUNTIME_MODES: [&str; 4] = ["off", "lite", "full", "ultra"];

struct Config {
    default_mode: String,
}
impl Config {
    fn load() -> Self;                              // env -> config.json -> "full"
    fn set_default(&mut self, mode: &str) -> bool;  // persist to config.json
    fn save(&self);
}
fn normalize_mode(mode: &str) -> Option<&str>;       // validate against RUNTIME_MODES
fn normalize_config_mode(mode: &str) -> Option<&str>;// validate against VALID_MODES
fn is_deactivation(text: &str) -> bool;              // "stop ponytail" / "normal mode"
```

Resolution order: `PONYTAIL_DEFAULT_MODE` env → `config.json` → `"full"`.
Config path: `~/.config/agentflare/ponytail/config.json`.

### state.rs

```rust
fn flag_path() -> PathBuf;                     // ~/.local/state/agentflare/ponytail/active
fn active_mode() -> Option<String>;
fn set_active(mode: &str) -> io::Result<()>;
fn clear_active();
```

Simple file-based flag. Write "full", "lite", etc. Delete on "off". Used by statusline and session-start to know active mode without re-parsing config.

### instructions.rs

```rust
struct Instructions {
    mode: String,
    body: String,     // filtered SKILL.md content
}

fn build(mode: &str, skill_path: Option<&Path>) -> Instructions;
fn filter_skill(body: &str, mode: &str) -> String;
fn fallback(mode: &str) -> String;
```

SKILL.md loading:
1. `skill_path` arg → custom path
2. `~/.cache/agentflare/ponytail/SKILL.md` → downloaded copy
3. Embedded `skill.md` → compiled-in fallback

Filtering: intensity-specific rows in the table and example lines are kept only for the active mode. All other rules pass through unchanged.

### switcher.rs

```rust
enum SwitchAction {
    SetMode(String),       // session-scoped (off|lite|full|ultra)
    SetDefault(String),    // persist to config (off|lite|full|ultra)
    Off,                   // shortcut for SetMode("off")
}

fn detect(input: &str) -> Option<SwitchAction>;
```

Matches `/ponytail` command patterns in user prompt input. Used by the `prompt-submit` hook event.

### platform.rs

```rust
enum AgentPlatform { Claude, Codex, Copilot, Fallback }

fn detect() -> AgentPlatform;
fn format(event: &str, ctx: &str, platform: AgentPlatform) -> String;
```

Platform detection via env vars:
- `CLAUDE_CONFIG_DIR` → Claude
- `PLUGIN_DATA` + not `COPILOT_PLUGIN_DATA` → Codex
- `COPILOT_PLUGIN_DATA` → Copilot
- none → Fallback (raw text)

Output formats (exactly matching pony's current behavior):
- **Claude:** `{"hookSpecificOutput":{"hookEventName":"...","additionalContext":"..."}}`
- **Codex:** `{"systemMessage":"PONYTAIL:FULL","hookSpecificOutput":{"hookEventName":"...","additionalContext":"..."}}`
- **Copilot:** `{"additionalContext":"..."}` (SessionStart only, empty otherwise)
- **Fallback:** raw rules text on stdout

## Hook Command

```
agentflare ponytail hook <EVENT>
```

Events:

| Event | Action |
|-------|--------|
| `session-start` | Write flag file, emit rules as hook context |
| `subagent-start` | Emit rules for subagent context (no flag write) |
| `prompt-submit` | Parse input for mode switch, update flag if found |
| `statusline` | Output mode badge (ANSI colored) |

Exit 0 on success, non-zero on error. Hook author handles failure gracefully (never blocks session start).

## CLI Commands

```
agentflare ponytail setup           download SKILL.md to cache, print per-platform hook configs
agentflare ponytail status          print active mode (reads flag + config)
agentflare ponytail set <MODE>      write flag, session-scoped (off|lite|full|ultra)
agentflare ponytail default <MODE>  persist to config.json, write flag
agentflare ponytail off             shortcut: ponytail set off
agentflare ponytail update          re-download SKILL.md from ponytail repo
```

## Dependencies

No new crate dependencies. Existing deps cover everything:
- `dirs` — config/state/cache paths
- `serde` / `serde_json` — config serialization, hook JSON output
- `ureq` — HTTP download of SKILL.md

## Testing

Unit tests per module:
- `config`: mode resolution order, validation, config r/w roundtrip
- `state`: flag file lifecycle, concurrent reads
- `instructions`: filter removes correct intensity rows, fallback generates
- `switcher`: detects all switch patterns, ignores false positives
- `platform`: detection from env vars, output format per platform

Integration test:
- `agentflare ponytail hook session-start` → writes flag, emits Claude-format JSON

## Out of Scope

- Porting SKILL.md prompt content into Rust (stays external)
- Multi-platform plugin manifest files (`.claude-plugin/`, `.codex-plugin/`, etc.)
- Statusline scripts (`.ps1`, `.sh`) — these just call `agentflare ponytail hook statusline`
- ponytail-review, ponytail-audit, ponytail-debt, ponytail-gain skills (separate features)
- Benchmark suite
