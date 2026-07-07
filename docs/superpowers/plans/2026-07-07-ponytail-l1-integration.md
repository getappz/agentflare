# Ponytail L1 Integration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port ponytail runtime logic (config, state, instructions, switcher, platform output) from Node.js hooks into agentflare Rust. Prompt content stays external — downloaded on demand.

**Architecture:** New `src/ponytail/` module with 6 sub-modules + embedded fallback skill.md. One new top-level CLI command `agentflare ponytail` with subcommands for setup/status/set/default/off/update/hook. Follows existing `Commands` enum + `#[command(subcommand)]` pattern.

**Tech Stack:** Rust, clap (derive), serde_json, dirs, ureq — all already in Cargo.toml.

**Issue:** [#42](https://github.com/getappz/agentflare/issues/42)
**Spec:** `docs/superpowers/specs/2026-07-07-ponytail-l1-integration-design.md`
**Branch:** `feature/ponytail-l1-integration`

## Global Constraints

- Rust edition 2024, rust-version 1.91
- unsafe_code = "warn", clippy all = "warn", pedantic = "warn"
- No new crate dependencies — reuse dirs, serde, serde_json, ureq
- Follow existing clap patterns: `#[command(subcommand)]` for nested commands
- Config/state/cache paths use agentflare paths (not ponytail's original paths)
- Embedded SKILL.md as `include_str!("skill.md")` — fallback only

---

## File Structure

| File | Purpose |
|------|---------|
| `src/ponytail/mod.rs` | Public API re-exports, `PonytailMode` struct |
| `src/ponytail/config.rs` | Mode resolution (env → config.json → "full"), validation, config I/O |
| `src/ponytail/state.rs` | Flag file `.ponytail-active` read/write |
| `src/ponytail/instructions.rs` | SKILL.md loading, intensity filtering, fallback generation |
| `src/ponytail/switcher.rs` | Mode switch detection in user input |
| `src/ponytail/platform.rs` | Agent platform detection, per-platform output formatting |
| `src/ponytail/skill.md` | Embedded default SKILL.md content (compiled into binary) |
| `src/main.rs` | Add `mod ponytail;`, `Ponytail` variant to `Commands`, dispatch |

---

### Task 1: ponytail/config.rs

**Files:**
- Create: `src/ponytail/config.rs`
- Create: `src/ponytail/mod.rs`

**Interfaces:**
- Produces:
  - `pub const DEFAULT_MODE: &str = "full"`
  - `pub const VALID_MODES: &[&str] = &["off", "lite", "full", "ultra", "review"]`
  - `pub const RUNTIME_MODES: &[&str] = &["off", "lite", "full", "ultra"]`
  - `pub fn normalize_mode(mode: &str) -> Option<&'static str>`
  - `pub fn normalize_config_mode(mode: &str) -> Option<&'static str>`
  - `pub fn normalize_persisted_mode(mode: &str) -> Option<&'static str>`
  - `pub fn is_deactivation(text: &str) -> bool`
  - `pub fn default_mode() -> String`
  - `pub fn set_default_mode(mode: &str) -> Result<(), String>`
  - `pub fn config_dir() -> PathBuf`
  - `pub fn config_path() -> PathBuf`

- [ ] **Step 1: Create `src/ponytail/mod.rs` skeleton**

```rust
pub mod config;
pub mod state;
pub mod instructions;
pub mod switcher;
pub mod platform;
```

- [ ] **Step 2: Create `src/ponytail/config.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_MODE: &str = "full";
pub const VALID_MODES: &[&str] = &["off", "lite", "full", "ultra", "review"];
pub const RUNTIME_MODES: &[&str] = &["off", "lite", "full", "ultra"];

pub fn normalize_mode(mode: &str) -> Option<&'static str> {
    let m = mode.trim().to_lowercase();
    RUNTIME_MODES.iter().find(|&&v| v == m).copied()
}

pub fn normalize_config_mode(mode: &str) -> Option<&'static str> {
    let m = mode.trim().to_lowercase();
    VALID_MODES.iter().find(|&&v| v == m).copied()
}

pub fn normalize_persisted_mode(mode: &str) -> Option<&'static str> {
    normalize_mode(mode).or_else(|| normalize_config_mode(mode))
}

pub fn is_deactivation(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let t = t.trim_end_matches(|c: char| c == '.' || c == '!' || c == '?' || c.is_whitespace());
    t == "stop ponytail" || t == "normal mode"
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentflare")
        .join("ponytail")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

#[derive(Serialize, Deserialize, Default)]
struct ConfigFile {
    default_mode: Option<String>,
}

pub fn default_mode() -> String {
    if let Ok(val) = std::env::var("PONYTAIL_DEFAULT_MODE") {
        if let Some(m) = normalize_config_mode(&val) {
            return m.to_string();
        }
    }
    if let Ok(data) = std::fs::read_to_string(config_path()) {
        if let Ok(cfg) = serde_json::from_str::<ConfigFile>(&data) {
            if let Some(mode) = cfg.default_mode {
                if let Some(m) = normalize_config_mode(&mode) {
                    return m.to_string();
                }
            }
        }
    }
    DEFAULT_MODE.to_string()
}

pub fn set_default_mode(mode: &str) -> Result<(), String> {
    let normalized = normalize_config_mode(mode).ok_or_else(|| format!("invalid mode: {mode}"))?;
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cfg: ConfigFile = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default();
    cfg.default_mode = Some(normalized.to_string());
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(config_path(), json).map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 3: Build check**

```bash
cargo check
```

Expected: compiles. `config` type dead-code warnings OK (consumed later).

- [ ] **Step 4: Commit**

```bash
git add src/ponytail/
git commit -m "feat(ponytail): add config module — mode resolution and validation"
```

---

### Task 2: ponytail/state.rs

**Files:**
- Create: `src/ponytail/state.rs`
- Modify: `src/ponytail/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn flag_path() -> PathBuf`
  - `pub fn active_mode() -> Option<String>`
  - `pub fn set_active(mode: &str) -> io::Result<()>`
  - `pub fn clear_active()`

- [ ] **Step 1: Create `src/ponytail/state.rs`**

```rust
use std::io;
use std::path::PathBuf;

pub fn flag_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("agentflare")
        .join("ponytail")
        .join("active")
}

pub fn active_mode() -> Option<String> {
    std::fs::read_to_string(flag_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn set_active(mode: &str) -> io::Result<()> {
    let path = flag_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, mode)
}

pub fn clear_active() {
    let _ = std::fs::remove_file(flag_path());
}
```

- [ ] **Step 2: Build check**

```bash
cargo check
```

- [ ] **Step 3: Commit**

```bash
git add src/ponytail/state.rs
git commit -m "feat(ponytail): add state module — flag file read/write"
```

---

### Task 3: ponytail/skill.md (embedded fallback)

**Files:**
- Create: `src/ponytail/skill.md`

Embed the canonical SKILL.md as a fallback. Copy the content from the cloned ponytail repo at `C:\Users\shiva\workspace\refs\ponytail\skills\ponytail\SKILL.md`.

- [ ] **Step 1: Copy skill.md**

```bash
copy C:\Users\shiva\workspace\refs\ponytail\skills\ponytail\SKILL.md src\ponytail\skill.md
```

- [ ] **Step 2: Commit**

```bash
git add src/ponytail/skill.md
git commit -m "feat(ponytail): embed fallback SKILL.md"
```

---

### Task 4: ponytail/instructions.rs

**Files:**
- Create: `src/ponytail/instructions.rs`

**Interfaces:**
- Consumes: `ponytail::config::normalize_mode`, `ponytail::config::normalize_persisted_mode`, `ponytail::config::DEFAULT_MODE`
- Produces:
  - `pub struct Instructions { pub mode: String, pub body: String }`
  - `pub fn build(mode: &str, skill_path: Option<&std::path::Path>) -> Instructions`
  - `pub fn filter_skill_body(body: &str, mode: &str) -> String`
  - `pub fn fallback_instructions(mode: &str) -> String`

- [ ] **Step 1: Create `src/ponytail/instructions.rs`**

```rust
use crate::ponytail::config;
use std::path::Path;

static EMBEDDED_SKILL: &str = include_str!("skill.md");

pub struct Instructions {
    pub mode: String,
    pub body: String,
}

pub fn build(mode: &str, skill_path: Option<&Path>) -> Instructions {
    let effective = config::normalize_persisted_mode(mode)
        .unwrap_or(config::DEFAULT_MODE);

    let skill_body = if let Some(path) = skill_path {
        std::fs::read_to_string(path).unwrap_or_else(|_| EMBEDDED_SKILL.to_string())
    } else {
        let cache = crate::ponytail::state::flag_path()
            .parent()
            .unwrap_or(Path::new("."))
            .parent()
            .unwrap_or(Path::new("."))
            .parent()
            .unwrap_or(Path::new("."))
            .join("SKILL.md");
        std::fs::read_to_string(&cache).unwrap_or_else(|_| EMBEDDED_SKILL.to_string())
    };

    let filtered = filter_skill_body(&skill_body, effective);

    Instructions {
        mode: effective.to_string(),
        body: filtered,
    }
}

pub fn filter_skill_body(body: &str, mode: &str) -> String {
    let effective = config::normalize_mode(mode).unwrap_or(config::DEFAULT_MODE);
    body.lines()
        .filter(|line| {
            if let Some(cap) = line.trim().strip_prefix("| **") {
                if let Some(end) = cap.find("** |") {
                    let label_mode = config::normalize_mode(&cap[..end]);
                    if label_mode.is_some() {
                        return label_mode.unwrap() == effective;
                    }
                }
            }
            if let Some(rest) = line.trim().strip_prefix("- ") {
                if let Some(colon) = rest.find(':') {
                    let label_mode = config::normalize_mode(rest[..colon].trim());
                    if label_mode.is_some() {
                        return label_mode.unwrap() == effective;
                    }
                }
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn fallback_instructions(mode: &str) -> String {
    let m = config::normalize_mode(mode).unwrap_or(config::DEFAULT_MODE);
    format!(
        "PONYTAIL MODE ACTIVE — level: {m}\n\n\
         You are a lazy senior developer. Lazy means efficient, not careless.\n\n\
         ## The ladder\n\n\
         1. Does this need to exist at all? (YAGNI)\n\
         2. Already in this codebase? Reuse it.\n\
         3. Stdlib does it? Use it.\n\
         4. Native platform feature covers it? Use it.\n\
         5. Already-installed dependency solves it? Use it.\n\
         6. Can it be one line? One line.\n\
         7. Only then: the minimum code that works.\n\n\
         ## Rules\n\n\
         No unrequested abstractions. No boilerplate. Deletion over addition.\n\
         Code first, then at most three lines: what was skipped, when to add it.\n\
         Never simplify away: input validation, error handling, security, accessibility."
    )
}
```

- [ ] **Step 2: Build check**

```bash
cargo check
```

- [ ] **Step 3: Commit**

```bash
git add src/ponytail/instructions.rs
git commit -m "feat(ponytail): add instructions module — skill loading and filtering"
```

---

### Task 5: ponytail/switcher.rs

**Files:**
- Create: `src/ponytail/switcher.rs`

**Interfaces:**
- Consumes: `ponytail::config::normalize_config_mode`
- Produces:
  - `pub enum SwitchAction { SetMode(String), SetDefault(String), Off }`
  - `pub fn detect(input: &str) -> Option<SwitchAction>`

- [ ] **Step 1: Create `src/ponytail/switcher.rs`**

```rust
use crate::ponytail::config;

pub enum SwitchAction {
    SetMode(String),
    SetDefault(String),
    Off,
}

pub fn detect(input: &str) -> Option<SwitchAction> {
    let prompt = input.trim().to_lowercase();

    if config::is_deactivation(&prompt) {
        return Some(SwitchAction::Off);
    }

    let cmd = prompt
        .strip_prefix("/ponytail")
        .or_else(|| prompt.strip_prefix("@ponytail"))
        .or_else(|| prompt.strip_prefix("$ponytail"))?;

    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let sub = parts.first().copied().unwrap_or("");
    let arg = parts.get(1).copied().unwrap_or("");

    if sub.is_empty() || sub == "lite" || sub == "full" || sub == "ultra" {
        let mode = if sub.is_empty() { "full" } else { sub };
        config::normalize_config_mode(mode)?;
        return Some(SwitchAction::SetMode(mode.to_string()));
    }

    match sub {
        "off" => Some(SwitchAction::Off),
        "default" => {
            let dmode = arg;
            if dmode.is_empty() {
                return None;
            }
            config::normalize_config_mode(dmode)?;
            Some(SwitchAction::SetDefault(dmode.to_string()))
        }
        _ => None,
    }
}
```

- [ ] **Step 2: Build check**

```bash
cargo check
```

- [ ] **Step 3: Commit**

```bash
git add src/ponytail/switcher.rs
git commit -m "feat(ponytail): add switcher module — mode switch detection"
```

---

### Task 6: ponytail/platform.rs

**Files:**
- Create: `src/ponytail/platform.rs`

**Interfaces:**
- Produces:
  - `pub enum AgentPlatform { Claude, Codex, Copilot, Fallback }`
  - `pub fn detect() -> AgentPlatform`
  - `pub fn format_hook_output(event: &str, ctx: &str, platform: &AgentPlatform) -> String`

- [ ] **Step 1: Create `src/ponytail/platform.rs`**

```rust
use serde_json::json;

pub enum AgentPlatform {
    Claude,
    Codex,
    Copilot,
    Fallback,
}

pub fn detect() -> AgentPlatform {
    if std::env::var("CLAUDE_CONFIG_DIR").is_ok() {
        AgentPlatform::Claude
    } else if std::env::var("COPILOT_PLUGIN_DATA").is_ok() {
        AgentPlatform::Copilot
    } else if std::env::var("PLUGIN_DATA").is_ok() {
        AgentPlatform::Codex
    } else {
        AgentPlatform::Fallback
    }
}

pub fn format_hook_output(event: &str, ctx: &str, platform: &AgentPlatform) -> String {
    match platform {
        AgentPlatform::Claude => {
            if event == "SessionStart" && !ctx.is_empty() {
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": ctx,
                    }
                })
                .to_string()
            } else {
                let output: serde_json::Value = json!({
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": ctx,
                    }
                });
                output.to_string()
            }
        }
        AgentPlatform::Codex => {
            if event == "SessionStart" {
                json!({
                    "systemMessage": "PONYTAIL:FULL",
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": ctx,
                    }
                })
                .to_string()
            } else {
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": ctx,
                    }
                })
                .to_string()
            }
        }
        AgentPlatform::Copilot => {
            if event == "SessionStart" {
                json!({ "additionalContext": ctx }).to_string()
            } else {
                String::new()
            }
        }
        AgentPlatform::Fallback => ctx.to_string(),
    }
}
```

- [ ] **Step 2: Build check**

```bash
cargo check
```

- [ ] **Step 3: Commit**

```bash
git add src/ponytail/platform.rs
git commit -m "feat(ponytail): add platform module — detection and output formatting"
```

---

### Task 7: ponytail/mod.rs — public API

**Files:**
- Modify: `src/ponytail/mod.rs`

Replace skeleton with full public API.

- [ ] **Step 1: Update `src/ponytail/mod.rs`**

```rust
pub mod config;
pub mod instructions;
pub mod platform;
pub mod state;
pub mod switcher;

pub use config::{
    default_mode, is_deactivation, normalize_config_mode, normalize_mode,
    normalize_persisted_mode, set_default_mode, DEFAULT_MODE, RUNTIME_MODES, VALID_MODES,
};
pub use instructions::{build as build_instructions, fallback_instructions, Instructions};
pub use platform::{detect as detect_platform, format_hook_output, AgentPlatform};
pub use state::{active_mode, clear_active, set_active};
pub use switcher::{detect as detect_switch, SwitchAction};
```

- [ ] **Step 2: Build check**

```bash
cargo check
```

- [ ] **Step 3: Commit**

```bash
git add src/ponytail/mod.rs
git commit -m "feat(ponytail): finalize mod.rs public API"
```

---

### Task 8: CLI integration — Ponytail command

**Files:**
- Modify: `src/main.rs`

Add `mod ponytail;`, `Ponytail` variant to `Commands`, `PonytailAction` enum, and dispatch.

- [ ] **Step 1: Add module declaration to `src/main.rs`**

Add after existing `mod` declarations (after line 28 `mod update;`):

```rust
mod ponytail;
```

- [ ] **Step 2: Add `Ponytail` variant to `Commands` enum**

Add after `Auth` variant:

```rust
    /// Manage Ponytail — lazy senior dev mode for AI agents.
    Ponytail {
        #[command(subcommand)]
        action: PonytailAction,
    },
```

- [ ] **Step 3: Add `PonytailAction` subcommand enum**

Add after `AuthAction` enum definition:

```rust
#[derive(Subcommand)]
enum PonytailAction {
    /// Download SKILL.md and print per-platform hook config snippets.
    Setup,
    /// Show active ponytail mode (reads flag file + config default).
    Status,
    /// Set session-scoped mode (off|lite|full|ultra). Writes flag file.
    Set {
        mode: String,
    },
    /// Persist default mode to config. Survives session restarts.
    Default {
        mode: String,
    },
    /// Turn ponytail off for this session.
    Off,
    /// Re-download SKILL.md from ponytail repo to cache.
    Update,
    /// Hook entry point — called by agent hook systems. Not for manual use.
    Hook {
        #[command(subcommand)]
        event: PonytailHookEvent,
    },
}

#[derive(Subcommand)]
enum PonytailHookEvent {
    /// Session start — emit rules as hook context, write flag file.
    SessionStart,
    /// Subagent start — emit rules for subagent context only.
    SubagentStart,
    /// Prompt submit — parse input for mode switch, update flag if found.
    PromptSubmit,
    /// Output ANSI mode badge for terminal statusline.
    Statusline,
}
```

- [ ] **Step 4: Add dispatch in `main()` function**

Add before the last closing brace of `main()`:

```rust
        Commands::Ponytail { action } => match action {
            PonytailAction::Setup => {
                println!("download SKILL.md to cache, print per-platform hook configs");
            }
            PonytailAction::Status => {
                let mode = ponytail::active_mode().unwrap_or_else(ponytail::default_mode);
                println!("{mode}");
            }
            PonytailAction::Set { mode } => {
                let normalized = ponytail::normalize_config_mode(&mode)
                    .unwrap_or("full");
                ponytail::set_active(normalized).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                println!("{normalized}");
            }
            PonytailAction::Default { mode } => {
                ponytail::set_default_mode(&mode).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                });
                ponytail::set_active(&mode).ok();
                println!("default: {mode}");
            }
            PonytailAction::Off => {
                ponytail::clear_active();
                println!("off");
            }
            PonytailAction::Update => {
                println!("re-download SKILL.md from ponytail repo");
            }
            PonytailAction::Hook { event } => match event {
                PonytailHookEvent::SessionStart => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" {
                        ponytail::state::clear_active();
                        println!("OK");
                        return;
                    }
                    ponytail::set_active(&mode).ok();
                    let instructions = ponytail::build_instructions(&mode, None);
                    let platform = ponytail::detect_platform();
                    let output = ponytail::format_hook_output(
                        "SessionStart",
                        &instructions.body,
                        &platform,
                    );
                    println!("{output}");
                }
                PonytailHookEvent::SubagentStart => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" {
                        println!("OK");
                        return;
                    }
                    let instructions = ponytail::build_instructions(&mode, None);
                    let platform = ponytail::detect_platform();
                    let output = ponytail::format_hook_output(
                        "SubagentStart",
                        &instructions.body,
                        &platform,
                    );
                    println!("{output}");
                }
                PonytailHookEvent::PromptSubmit => {
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).ok();
                    if let Some(action) = ponytail::detect_switch(&input) {
                        match action {
                            ponytail::SwitchAction::SetMode(m) => {
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::SetDefault(m) => {
                                ponytail::set_default_mode(&m).ok();
                                ponytail::set_active(&m).ok();
                            }
                            ponytail::SwitchAction::Off => {
                                ponytail::clear_active();
                            }
                        }
                    }
                    println!("OK");
                }
                PonytailHookEvent::Statusline => {
                    let mode = ponytail::active_mode()
                        .unwrap_or_else(ponytail::default_mode);
                    if mode == "off" || mode.is_empty() {
                        return; // no output = no badge
                    }
                    if mode == "full" {
                        print!("\x1b[38;5;108m[PONYTAIL]\x1b[0m");
                    } else {
                        let upper = mode.to_uppercase();
                        print!("\x1b[38;5;108m[PONYTAIL:{upper}]\x1b[0m");
                    }
                }
            },
        }
```

- [ ] **Step 5: Build check**

```bash
cargo check
```

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(ponytail): add CLI commands — setup, status, set, hook"
```

---

### Task 9: Unit tests

**Files:**
- Create: `src/ponytail/config.rs` (append tests)
- Create: `src/ponytail/state.rs` (append tests)
- Create: `src/ponytail/instructions.rs` (append tests)
- Create: `src/ponytail/switcher.rs` (append tests)

- [ ] **Step 1: Add config tests to `src/ponytail/config.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_valid_modes() {
        assert_eq!(normalize_mode("full"), Some("full"));
        assert_eq!(normalize_mode("off"), Some("off"));
        assert_eq!(normalize_mode("ULTRA"), Some("ultra"));
    }

    #[test]
    fn rejects_invalid_modes() {
        assert_eq!(normalize_mode("extreme"), None);
        assert_eq!(normalize_mode(""), None);
        assert_eq!(normalize_config_mode("review"), Some("review"));
        assert_eq!(normalize_mode("review"), None); // review not a runtime mode
    }

    #[test]
    fn detects_deactivation() {
        assert!(is_deactivation("stop ponytail"));
        assert!(is_deactivation("normal mode"));
        assert!(is_deactivation("Normal Mode."));
        assert!(!is_deactivation("add a normal mode toggle"));
    }

    #[test]
    fn defaults_to_full() {
        std::env::remove_var("PONYTAIL_DEFAULT_MODE");
        assert_eq!(default_mode(), "full");
    }

    #[test]
    fn reads_env_var() {
        std::env::set_var("PONYTAIL_DEFAULT_MODE", "lite");
        assert_eq!(default_mode(), "lite");
        std::env::remove_var("PONYTAIL_DEFAULT_MODE");
    }
}
```

- [ ] **Step 2: Run config tests**

```bash
cargo test ponytail::config
```

Expected: 5 PASS

- [ ] **Step 3: Add state tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_active_mode() {
        clear_active();
        assert_eq!(active_mode(), None);
        set_active("full").unwrap();
        assert_eq!(active_mode(), Some("full".to_string()));
        clear_active();
        assert_eq!(active_mode(), None);
    }

    #[test]
    fn clear_nonexistent_is_noop() {
        clear_active(); // should not panic
    }
}
```

- [ ] **Step 4: Run state tests**

```bash
cargo test ponytail::state
```

Expected: 2 PASS

- [ ] **Step 5: Add instructions tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_generates_for_mode() {
        let f = fallback_instructions("full");
        assert!(f.contains("PONYTAIL MODE ACTIVE"));
        assert!(f.contains("The ladder"));
    }

    #[test]
    fn build_uses_embedded_skill() {
        let ins = build("full", None);
        assert!(!ins.body.is_empty());
        assert_eq!(ins.mode, "full");
    }

    #[test]
    fn filter_keeps_non_mode_lines() {
        let input = "some rule\n| **lite** | lite only |\n| **full** | full only |\nother rule";
        let filtered = filter_skill_body(input, "full");
        assert!(filtered.contains("some rule"));
        assert!(filtered.contains("full only"));
        assert!(!filtered.contains("lite only"));
        assert!(filtered.contains("other rule"));
    }
}
```

- [ ] **Step 6: Run instructions tests**

```bash
cargo test ponytail::instructions
```

Expected: 3 PASS

- [ ] **Step 7: Add switcher tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mode_switch() {
        assert!(matches!(detect("/ponytail lite"), Some(SwitchAction::SetMode(m)) if m == "lite"));
        assert!(matches!(detect("/ponytail full"), Some(SwitchAction::SetMode(m)) if m == "full"));
    }

    #[test]
    fn detects_off() {
        assert!(matches!(detect("/ponytail off"), Some(SwitchAction::Off)));
    }

    #[test]
    fn detects_deactivation_phrase() {
        assert!(matches!(detect("stop ponytail"), Some(SwitchAction::Off)));
    }

    #[test]
    fn detects_default() {
        assert!(matches!(detect("/ponytail default ultra"), Some(SwitchAction::SetDefault(m)) if m == "ultra"));
    }

    #[test]
    fn ignores_false_positives() {
        assert!(detect("let's talk about ponytail").is_none());
        assert!(detect("").is_none());
    }
}
```

- [ ] **Step 8: Run switcher tests**

```bash
cargo test ponytail::switcher
```

Expected: 5 PASS

- [ ] **Step 9: Commit**

```bash
git add src/ponytail/config.rs src/ponytail/state.rs src/ponytail/instructions.rs src/ponytail/switcher.rs
git commit -m "test(ponytail): add unit tests for config, state, instructions, switcher"
```

---

### Task 10: Build and lint

- [ ] **Step 1: Full build**

```bash
cargo build
```

- [ ] **Step 2: Run all ponytail tests**

```bash
cargo test ponytail
```

- [ ] **Step 3: Clippy**

```bash
cargo clippy -- -D warnings
```

- [ ] **Step 4: Check for unsafe**

```bash
cargo check
```

- [ ] **Step 5: Commit any lint fixes**

```bash
git add -u
git commit -m "chore(ponytail): fix clippy warnings"
```
