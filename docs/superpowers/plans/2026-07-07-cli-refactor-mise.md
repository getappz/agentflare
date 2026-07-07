# CLI Architecture Refactor — Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development.

**Goal:** Refactor agentflare's 500-line inline `main.rs` into mise-style modular CLI. One file per subcommand, typed Args structs, global flags on Cli, clean delegation.

**Architecture:** `src/cli/mod.rs` holds `Cli` struct (global flags) + `Commands` enum (delegates to subcommand structs). Each variant is `VariantName(cmd::Args)`, dispatches via `cmd.run()`. Global flags like `-y`/`--yes`, `-q`/`--quiet` sit on `Cli`.

**Issue:** [#44](https://github.com/getappz/agentflare/issues/44)
**Branch:** `feature/cli-refactor-mise`

## File Structure

| File | From | To |
|------|------|-----|
| `src/main.rs` | 500 lines | ~30 lines (thin entrypoint) |
| `src/cli/mod.rs` | — | Cli struct, Commands enum, dispatch |
| `src/cli/init.rs` | main.rs inline | InitArgs struct + run() |
| `src/cli/hook.rs` | main.rs inline | HookArgs struct + run() |
| `src/cli/cost.rs` | main.rs inline | CostArgs struct + run() |
| `src/cli/coaching.rs` | main.rs inline | CoachingArgs struct + run() |
| `src/cli/agents.rs` | main.rs inline | AgentsArgs struct + run() |
| `src/cli/alias.rs` | main.rs inline | AliasArgs struct + run() |
| `src/cli/update.rs` | main.rs inline | UpdateArgs + run() |
| `src/cli/uninstall.rs` | main.rs inline | UninstallArgs + run() |
| `src/cli/auth.rs` | main.rs inline | AuthArgs + run() |
| `src/cli/ponytail.rs` | main.rs inline | PonytailArgs + run() |
| `src/cli/mcp.rs` | main.rs inline | McpArgs + run() |

## Pattern (each subcommand file)

```rust
// src/cli/cost.rs
use clap::Args;

#[derive(Args)]
pub struct CostArgs {
    #[arg(long)]
    pub days: Option<u32>,
    #[arg(long)]
    pub by_project: bool,
}

impl CostArgs {
    pub fn run(self) {
        crate::cost::run(self.days, self.by_project);
    }
}
```

## Global flags (on Cli in mod.rs)

```rust
#[derive(Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,
}
```

---

### Task 1: Create `src/cli/` skeleton + move `Cli` and `Commands`

**Files:** Create `src/cli/mod.rs`, modify `src/main.rs`

Extract Cli struct and Commands enum from main.rs into mod.rs. main.rs becomes thin entrypoint calling `Cli::parse().command.run()`.

### Task 2: Extract Init subcommand

**Files:** Create `src/cli/init.rs`, modify `src/cli/mod.rs`

Move Init variant to InitArgs struct. Wire `init::run(agent.as_str(), yes)` into `InitArgs::run()`.

### Task 3: Extract Hook subcommand

**Files:** Create `src/cli/hook.rs`, modify `src/cli/mod.rs`

### Task 4: Extract Cost subcommand

**Files:** Create `src/cli/cost.rs`, modify `src/cli/mod.rs`

### Task 5: Extract Coaching subcommand

**Files:** Create `src/cli/coaching.rs`, modify `src/cli/mod.rs`

### Task 6: Extract Agents subcommand

**Files:** Create `src/cli/agents.rs`, modify `src/cli/mod.rs`

### Task 7: Extract Alias subcommand

**Files:** Create `src/cli/alias.rs`, modify `src/cli/mod.rs`

### Task 8: Extract Update subcommand

**Files:** Create `src/cli/update.rs`, modify `src/cli/mod.rs`

### Task 9: Extract Uninstall subcommand

**Files:** Create `src/cli/uninstall.rs`, modify `src/cli/mod.rs`

### Task 10: Extract Auth subcommand

**Files:** Create `src/cli/auth.rs`, modify `src/cli/mod.rs`

### Task 11: Extract Ponytail subcommand

**Files:** Create `src/cli/ponytail.rs`, modify `src/cli/mod.rs`

### Task 12: Build, test, cleanup

Verify all tests pass, clippy clean, binary works.
