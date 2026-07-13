
# Known Issues and Technical Debt

## Table of Contents

- [Known Bugs](#known-bugs)
- [Technical Debt](#technical-debt)
- [Deprecated Dependencies](#deprecated-dependencies)
- [Performance Concerns](#performance-concerns)
- [Security Concerns](#security-concerns)
- [Missing Tests](#missing-tests)
- [Architecture Concerns](#architecture-concerns)

---

## Known Bugs

### Hung child process on version probe timeout

`src/agent_detect.rs:206-212` — `run_version_command` spawns a child process on a helper thread and waits up to `VERSION_TIMEOUT` (5s). On timeout, the helper thread is abandoned and the hung child is never killed. This leaks OS process handles. The code acknowledges this limitation with a comment ("killing cross-platform requires platform-specific process-group handling, which is overkill"), but repeated timeouts during `agentflare agents list` could accumulate orphaned/running child processes.

**Impact: Medium** | **Likelihood: Low** (version probes are rare and fast)

### `auth login` reports "not found" then auto-creates the profile

`src/auth.rs:565-570` — `auth_login` checks if the isolated profile exists, prints "not found" error, then immediately calls `isolate_add` to create it. The error-to-follow is confusing UX. The control flow should explicitly branch on "create or not-found" rather than print an error then proceed.

**Impact: Low** | **Likelihood: Low**

### rpassword failure silently sets empty passphrase

`src/auth_crypt.rs:24` — `rpassword::prompt_password("vault passphrase: ").unwrap_or_default()` returns an empty `String` on failure, which is trimmed and treated as "no passphrase set". If the terminal is non-interactive and `AGENTFLARE_VAULT_PASSPHRASE` is unset, encrypted vault files are silently skipped with a `stderr` warning — but the user may never see that warning if the agent's hook output is suppressed.

**Impact: Medium** (data loss risk: encrypted backups become inaccessible) | **Likelihood: Low**

### `daemon_running` uses `pgrep`/`tasklist` which may fail cross-platform

`src/auth_runner.rs:108-119` — `daemon_running` calls `pgrep` on non-Windows and `tasklist` on Windows. If neither is available (e.g., minimal containers, Alpine without `procps`), daemon detection silently returns `false`, and the restart warning is never shown.

**Impact: Low** | **Likelihood: Low**

---

## Technical Debt

### Monolithic CLI dispatch in `main.rs`

`src/main.rs:200-292` — The `main` function is a ~90-line `match` statement that routes every CLI command to its handler. As new subcommands are added, this grows linearly. A command-registry pattern (trait-based, with each command registering itself) would keep `main` bounded and make adding commands self-contained.

**Impact: Medium** | **Effort: Medium** | Pattern: refactor to `Command` trait with `fn run(&self) -> Result<()>`

### Large `auth.rs` module (1198 lines)

`src/auth.rs` handles vault backup, restore, profile rotation, cooldowns, project associations, aliases, isolations, login, and detection — all in one file. This was flagged as a complexity hotspot during analysis (`activate_with` w=35, `isolate_ls` w=22). Sub-modules like `auth_vault.rs`, `auth_rotation.rs`, `auth_isolate.rs` would improve navigability.

**Impact: Medium** | **Effort: Medium**

### Complexity hotspots in `uninstall.rs`

`src/uninstall.rs:35-87` (`clean_claude_code` w=37) and `clean_opencode` (w=21) contain deeply nested JSON manipulation that surgically removes agentflare hooks from shared config files. The logic is correct but hard to modify or extend to new hosts. Extracting per-host uninstall strategies (similar to `components.rs`'s per-host wiring) would reduce nesting.

**Impact: Low** | **Effort: Low**

### Complexity hotspot in `alias.rs`

`src/alias.rs:42-163` (`run` function w=48) handles alias resolution, shell detection, profile management, managed-block I/O, and JSON/terminal output all in one function. The control flow mixes pure logic with I/O and formatting. Separating concern: `resolve` → `write` → `report` would make each phase testable independently.

**Impact: Low** | **Effort: Medium**

### Duplicated database open/rebuild pattern

Both `src/auth_db.rs:70-77` and `src/rollup.rs:118-135` implement `open_or_rebuild()` with nearly identical logic: create parent dirs, try open, on failure delete and retry, fall back to in-memory. A shared utility would eliminate the duplication and ensure both databases get the same corruption-recovery behavior.

**Impact: Low** | **Effort: Low**

### `unsafe` env var manipulation in test helpers

`src/paths.rs:29-34` — Test support functions `with_temp_home` and `with_temp_cwd` use `unsafe { std::env::set_var(...) }` because `AGENTFLARE_HOME_OVERRIDE` and `set_current_dir` are process-global in Rust. The comment acknowledges this and documents the serialization via `GLOBAL_STATE_LOCK`. This is pragmatic but fragile — a `#[serial_test::serial]` attribute or per-test tempdir strategy with explicit path injection would eliminate the `unsafe` block entirely.

**Impact: Low** | **Effort: Low**

### Inconsistent error handling patterns

Some functions use `thiserror` typed errors (`AuthError`, `AgentDetectError`, etc.), while others use ad-hoc `String` errors or `Box<dyn Error>`. The `agent_install.rs` module uses a `String`-based `Outcome` enum rather than a typed error. Standardizing on `thiserror` throughout would give callers structured error matching.

**Impact: Low** | **Effort: Medium**

---

## Deprecated Dependencies

### Dependency freshness assessment

All dependencies in `Cargo.toml` use current, maintained versions:

| Crate | Version | Status |
|-------|---------|--------|
| `clap` | 4 | Current major version |
| `serde` / `serde_json` | 1 | Stable |
| `dirs` | 6 | Current |
| `chrono` | 0.4 | Stable (note: `chrono` has known soundness issues with `localtime_r`, but agentflare's usage is safe — no `LocalResult` unwrapping on ambiguous times) |
| `rmcp` | 1.8.0 | Pinned — no known deprecation |
| `rusqlite` | 0.40 | Current |
| `ureq` | 2 | Current, minimal HTTP client |
| `aes-gcm` | 0.10 | Current |
| `pbkdf2` | 0.12 | Current |
| `thiserror` | 2 | Current |
| `eyre` / `color-eyre` | 0.6 | Current |

**Overall: No deprecated dependencies detected.** Minimum supported Rust version set at `rust-version = "1.91"` (audited 2025-07) with `edition = "2024"` — both recent and well-supported.

---

## Performance Concerns

### Full cache sync on every `agentflare cost` invocation

`src/cost.rs:248-251` — `run()` calls `crate::rollup::sync()` which walks the entire `~/.claude/projects/` directory and reindexes any changed session files before querying. For users with many projects and long session histories, this is O(n×m) where n = project directories and m = lines per session file. The SQLite cache mitigates this by skipping unchanged files (mtime+size fingerprint check), so subsequent runs are fast. However, the first run after a pricing change (which invalidates the entire cache) will re-parse every session file.

**Impact: Low** (cache avoids full re-scan on 99% of runs) | **Mitigation exists**

### Session file read loads entire JSONL into memory

`src/rollup.rs:157` — `reindex_file` calls `std::fs::read_to_string(path)` to load the entire session file into memory before line-by-line parsing. Session files for long-running Claude Code sessions can reach 10+ MB. This is acceptable for a CLI tool but worth noting: a buffered line reader would reduce peak memory usage.

**Impact: Low** | **Effort: Low** (swap `read_to_string` with `BufReader::lines()`)

### `find_binary` walks PATH directories without caching

`src/agent_detect.rs:28-45` — `find_binary` linearly scans every directory on PATH for each binary name. Called once per `agentflare agents list` invocation (17 registry entries × PATH directories). This is fast in practice (filesystem metadata ops, <1s) but could be optimized with a once-per-run directory cache.

**Impact: Negligible**

### No known N+1 query patterns

The SQLite queries in `auth_db.rs` and `rollup.rs` are well-structured single-query aggregations. No looped queries detected.

---

## Security Concerns

### Error messages may leak file paths to stderr

Several `eprintln!("error: ...")` calls across the codebase include full file paths (e.g., `src/auth.rs:185`, `src/update.rs:108`). In non-interactive CLI contexts (CI/CD, agent hooks), these leak local filesystem structure. This is typical for CLI tools but worth enumerating.

**Impact: Low** | **Mitigation: None needed for a local CLI tool**

### `record_error` stores raw error messages in SQLite

`src/auth_db.rs:86-103` — `record_error` stores raw agent stderr output (including HTTP error bodies) in the `profile_health` table's `last_error_time` column. Error messages from rate-limited API calls could theoretically contain tokens or request fragments. The stored data is local-only (`~/.local/share/agentflare/auth.db`) and never transmitted.

**Impact: Low** | **Mitigation: Truncate error messages to first ~200 chars**

### Shell command execution in `init`

**Impact: Low** | **Mitigation: Explicit consent required**

### Test helpers use `unsafe` for env var mutation

`src/paths.rs:29-34` — As noted in Technical Debt, test helpers use `unsafe` to set process-global env vars. While this is test-only code, it's technically a soundness concern under Rust's safety guarantees if two tests race.

**Impact: Low** (serialized by `Mutex`) | **Effort: Low** (use `serial_test` crate)

### `rpassword` handles sensitive input — no audit of dependency

`Cargo.toml:41` — `rpassword = "7"` is used to read the vault passphrase from the terminal. This crate disables echo on the terminal for the duration of input. No security audit has been performed on this dependency's supply chain. Pin to a known-good commit hash if this is a concern.

**Impact: Low** | **Mitigation: Standard crate, widely used**

---

## Missing Tests

### Integration test suite is empty

`tests/auth_integration.rs:1` — Contains only a comment: `// Integration tests for Auth Vault Phase 2`. No integration tests exist for the auth vault workflow (backup → activate → detect active → rotate). Unit tests in `src/auth.rs` cover individual functions, but the end-to-end flow across multiple commands is untested.

**Impact: Medium** | **Priority: High**

### No tests for `agent_install.rs`

The `src/agent_install.rs` module (package manager integration: npm, pip, etc.) has no unit tests. The `run_install`, `run_update`, and `run_uninstall` functions spawn real child processes and are only testable via the CLI.

**Impact: Medium** | **Priority: Medium**

### No tests for `agent_launch.rs`

`src/agent_launch.rs` (agent binary launching with model/mode flags) has no tests. Like `agent_install`, this spawns real processes.

**Impact: Low** | **Priority: Low**

### `agentflare://sessions` MCP resource is deliberately untested

`src/mcp_server.rs:233-240` — The test suite notes that `read_resource_sync("agentflare://sessions")` is untestable because it reads shared on-disk runtime state whose path is not injectable. This means any regression in session resource rendering would only be caught by manual MCP client testing.

**Impact: Low** | **Priority: Low**

### Test coverage is uneven across modules

| Module | Test Coverage | Notes |
|--------|---------------|-------|
| `auth.rs` | Good | Roundtrip, rotation, cooldown, alias, isolate |
| `coaching.rs` | Good | CRUD, validation, edge cases |
| `rollup.rs` | Excellent | Schema, sync, dedup, query, invalidation, readonly |
| `cost.rs` | Good | Parse, aggregate, tiered pricing |
| `shell.rs` | Good | Detection, parsing, managed blocks |
| `alias.rs` | Good | Resolution, managed blocks, fallback chain |
| `agent_detect.rs` | Good | PATH search, version resolution, cache |
| `optimize.rs` | Good | Nudges, routing, batching, hygiene |
| `state.rs` | Good | Load/save roundtrip, corruption recovery |
| `auth_db.rs` | Good | CRUD, penalty decay, project cascade |
| `auth_crypt.rs` | Good | Encrypt/decrypt roundtrip, legacy compat |
| `agents.rs` | Good | JSON output, table rendering |
| `agent_registry.rs` | Good | Registry consistency |
| `mcp_server.rs` | Moderate | Routing, health; sessions resource skipped |
| `hook.rs` | Moderate | Prompt extraction, pre-tool-use parsing |
| `init.rs` | Moderate | Wiring idempotency, settings preservation |
| `components.rs` | Moderate | Host coverage, rule targets |
| `uninstall.rs` | None | No tests |
| `update.rs` | Minimal | Only `clear_v` and `asset_name` tested |
| `agent_install.rs` | None | No tests |
| `agent_launch.rs` | None | No tests |
| `pricing.rs` | Good | Load, lookup, tiered cost, aliases |
| `paths.rs` | None (test helpers only) | No production-logic tests |

---

## Architecture Concerns

### No dependency injection — paths are globally hardcoded

All modules call `crate::paths::home()` and `crate::state::state_dir()` directly, making it impossible to test with alternate filesystem roots except via the `AGENTFLARE_HOME_OVERRIDE` env var hack. A `Config` struct with injectable paths (passed through the call stack or via a context object) would eliminate the need for `unsafe` env var manipulation in tests and make the codebase more testable.

**Impact: Medium** | **Effort: Large** | Pattern: Introduce `AppContext` struct with `home_dir`, `state_dir`, `data_dir` fields

### State management is split across JSON and SQLite

The codebase uses four separate state stores:
1. `~/.agentflare/state.json` — global on/off toggle + version cache (JSON)
2. `~/.agentflare/runtime-state.json` — session tracking (JSON)
3. `~/.local/share/agentflare/auth.db` — auth vault (SQLite)
4. `~/.agentflare/analytics.db` — cost rollup cache (SQLite)

Two JSON files for transient state and two SQLite databases for persistent storage. Consolidating the two SQLite databases into one with attached schemas, or merging all state into SQLite, would simplify backup and migration.

**Impact: Low** | **Effort: Medium** | Benefit: simpler backup, single migration path

### Tight coupling between `components.rs` and host-specific wiring

`src/components.rs:172-282` — The `get_components` function returns a `Vec<Component>` where each component's `check` and `apply` closures capture host-specific knowledge (plugin marketplace commands, MCP registration formats, config file paths). Adding a new host requires touching this single large function. A trait-based host adapter pattern would allow each host to declare its own component compatibility.

**Impact: Low** | **Effort: Medium**

### Unsafe code exists only in tests (acknowledged)

`src/paths.rs:29-34` and `src/agent_detect.rs` test helpers use `unsafe` for `set_var`. The `Cargo.toml` configures `unsafe_code = "warn"` at the crate level, and no `unsafe` exists in production code. The test `unsafe` blocks are documented and serialized with `Mutex`.

**Status: Acceptable** — no production unsafe code.

### Large struct passed by value in `catalog_for` lookup

`src/auth.rs:17-21` — `AuthCatalog` derives `Clone` and `Copy` could be derived instead for the `&'static`-only struct. This is minor but affects API ergonomics — callers must clone the catalog when they want to own it.

**Impact: Negligible**

---

## Summary

| Category | Count | Highest Priority |
|----------|-------|-----------------|
| Known Bugs | 4 | Hung child process on version timeout (Medium) |
| Technical Debt | 7 | Monolithic `main.rs` dispatch and large `auth.rs` (Medium) |
| Deprecated Dependencies | 0 | None |
| Performance Concerns | 3 | Full cache sync on cost (Low — mitigated by cache) |
| Security Concerns | 5 | rpassword silent failure (Medium) |
| Missing Tests | 10 | Empty integration test suite (High), `agent_install.rs` untested (Medium) |
| Architecture Concerns | 5 | No dependency injection (Medium) |

**Overall health: Good.** The codebase is well-structured for a CLI tool of its size. The most impactful improvements would be: (1) adding integration tests for the auth vault workflow, (2) breaking up `auth.rs` into sub-modules, and (3) introducing a dependency-injection pattern for testability. No critical bugs or security vulnerabilities were identified.
