# Multi-Crate Workspace + CLI Refactor — Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development.

**Goal:** Refactor agentflare into mise-style multi-crate workspace. Independent modules become sub-crates under `crates/`. CLI split into `src/cli/` one file per subcommand with typed Args structs.

**Reference:** https://github.com/jdx/mise — `crates/` workspace + `src/cli/` modular CLI

**Issue:** [#44](https://github.com/getappz/agentflare/issues/44)
**Branch:** `feature/cli-refactor-mise`

---

## Target workspace structure

```
agentflare/                    (root crate — binary + CLI layer)
  Cargo.toml                   [workspace] + [package] agentflare
  src/
    main.rs                    thin entrypoint (~30 lines)
    cli/
      mod.rs                   Cli struct, Commands enum, dispatch
      init.rs                  InitArgs + run()
      hook.rs                  HookArgs + run()
      cost.rs                  CostArgs + run()
      coaching.rs              CoachingArgs + run()
      agents.rs                AgentsArgs + run()
      alias.rs                 AliasArgs + run()
      update.rs                UpdateArgs + run()
      uninstall.rs             UninstallArgs + run()
      auth.rs                  AuthArgs + run()
      ponytail.rs              PonytailArgs + run()
      mcp.rs                   McpArgs + run()
    (remaining modules stay in root: auth*, coaching, cost, init, ...)

crates/
  ponytail/                    (sub-crate — standalone skill engine)
    Cargo.toml                 [package] ponytail
    src/
      lib.rs                   pub mods, re-exports
      config.rs                mode resolution
      state.rs                 flag file r/w
      instructions.rs          skill loading + filtering
      switcher.rs              mode switch detection
      platform.rs              agent + output formatting
      sub_skills.rs            embedded sub-skill content
      detect.rs                process-tree detection
      skill*.md                embedded skill files

  agent-registry/              (sub-crate — agent definitions)
    Cargo.toml                 [package] agent-registry
    src/
      lib.rs                   pub mods
      registry.rs              Agent enum, AgentSpec, Tier
      detect.rs                find_binary, extract_version, version cache
```

---

## Phase 1: CLI modularization (file-level)

### Task 1: Foundation — `src/cli/mod.rs` + thin `main.rs`

- Create `src/cli/` directory
- Move `Cli` struct, `Commands` enum, `AGENTFLARE_VERSION` to `mod.rs`
- Move all subcommand enums to their respective files
- Thin `main.rs` to: `Cli::parse().command.run(cli.yes)`
- Global `-y`/`--yes` and `-q`/`--quiet` flags on `Cli`

### Tasks 2-12: Extract each subcommand

One file per subcommand. Pattern:

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

Each task extracts one subcommand. All 11 follow identical mechanical pattern.

---

## Phase 2: Workspace extraction (crate-level)

### Task 13: Extract `crates/ponytail/`

- Move `src/ponytail/` → `crates/ponytail/src/`
- Create `crates/ponytail/Cargo.toml` with deps (serde, serde_json, dirs, ureq, sysinfo-optional)
- Root Cargo.toml: add `ponytail = { path = "crates/ponytail" }` to workspace + deps
- Update imports: `crate::ponytail` → `ponytail` in root

### Task 14: Extract `crates/agent-registry/`

- Move `src/agent_registry.rs` + `src/agent_detect.rs` → `crates/agent-registry/src/`
- Create `crates/agent-registry/Cargo.toml` (clap, serde, dirs deps)
- Root: add to workspace + deps
- Update imports

### Task 15: Build, test, clippy
