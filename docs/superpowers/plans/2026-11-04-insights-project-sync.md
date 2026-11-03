# Project-scoped Insights Sync Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans in this session. Steps use checkbox syntax.

**Goal:** Automatically scope agent-run syncs to their project while manual CLI sync remains global.

**Architecture:** Carry an optional normalized project path in InsightsConfig. Filter each source before parsing bodies and namespace persisted cursors by project. Reuse the agent detector and Git-root discovery in the CLI.

**Tech Stack:** Existing Rust, rusqlite, serde_json, walkdir, clap and flare_process.

**Spec:** `docs/superpowers/specs/2026-11-04-insights-project-sync-design.md` (approved by user).

## Global Constraints

- Manual CLI sync remains global.
- No new dependencies or separate project databases.
- Unknown project metadata is excluded only in scoped syncs.
- Preserve force and pruning behavior.

## Review Focus

- Project A must not consume project B's cache state.
- Nested paths match; similarly named sibling projects do not.
- Older eligible OpenCode sessions must not disappear behind a global LIMIT.
- Cursor bubbles must not be loaded for excluded composers.
- Source/read storage failures must remain retryable.

### Task 1: Project filtering and cursor namespaces

**Files:** `crates/flare-insights/src/config.rs`, `src/ingest/{common,mod,claude,codex,opencode,cursor,antigravity,watcher}.rs`, `src/store.rs`.

**Interfaces:** Add `InsightsConfig.project: Option<PathBuf>`, `matches_project(Option<&str>) -> bool`, `cursor_source(&str) -> String`. Add a lightweight JSONL project check reused by file adapters. Preserve Adapter::scan and scan_all signatures.

- [x] Add a default-none project field and update explicit config initializers.
- [x] Write tests for scoped source filtering, nested/sibling paths and project/global cache isolation; run to confirm behavior fails.
- [x] Implement path matching and project-specific cursor source names.
- [x] Filter OpenCode session metadata before messages and remove the global cap for scoped discovery.
- [x] Filter Cursor composer workspace metadata before bubble reads.
- [x] Prefilter JSONL sources using cwd/workspace metadata and Antigravity native summaries.
- [x] Persist cursors under the selected namespace only after successful data writes.
- [x] Run `mbx test -p agentflare-flare-insights --lib --test source_paths`; expect all passing.

### Task 2: Agent-run CLI behavior and documentation

**Files:** `src/cli/insights.rs`, `crates/flare-insights/README.md`.

**Interfaces:** CLI sync resolves project root only when flare_process::agent_name() detects an agent; non-agent runs leave config.project unset.

- [x] Add CLI scope-selection tests for agent versus manual execution and Git/non-Git directories; run before implementing behavior.
- [x] Resolve Git worktree root with existing Git helpers and current directory fallback; report scope in sync output.
- [x] Document automatic agent scoping and global manual sync.
- [x] Run insights tests, CLI tests/check and `git diff --check`.
- [x] Request one read-only review and resolve material findings with regressions.

Deliver as a pull request, as requested by the user; preserve pre-existing untracked files.

## Implementation notes

- Ruling: Antigravity's native workspace mapping is authoritative for project
  association; transcript hints are a fallback. Otherwise proving that a
  transcript omits hints requires reading its entire body before filtering it.
  A stale native mapping can temporarily misclassify a conversation until the
  source updates; mapping changes invalidate the adapter cursors.
- Review findings reproduced: Cursor URI-only workspaces, globally capped
  OpenCode subagents, and reads of unrelated native Antigravity transcripts.
  Each has a regression that failed before its fix.
- Verification: `mbx test -p agentflare-flare-insights` passed 32 tests, including
  the real-data import; `mbx test -p agentflare --bin agentflare
  cli::insights::evidence_tests` passed both CLI regressions. Existing unsafe-code
  warnings remain in the binary test build; no new dependencies were added.
