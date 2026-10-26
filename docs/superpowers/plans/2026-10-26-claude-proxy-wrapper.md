# Claude Proxy Wrapper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install a reversible Claude shell wrapper with opt-in shared-proxy support.

**Architecture:** Register wrap/unwrap CLI commands, install managed shell wrappers using existing profile infrastructure, and dispatch through an internal launch flag. PowerShell uses a native alias; other shells use functions. Resolve Claude directly; proxy launches run a Rust HTTPS bridge in the agentflare process and reuse default lean-ctx state.

**Tech Stack:** Rust/clap/atomic writer/tempfile, Tokio/Hyper/rustls and rcgen. lean-ctx is the only external proxy runtime dependency.

**Spec:** `docs/superpowers/specs/2026-10-26-claude-proxy-wrapper-design.md`

## Global Constraints

- Document `claude`; accept `claude-code`. Proxy is opt-in with a leading `--with-proxy`.
- Shared optimizer: port 4444, default lean-ctx state; no global endpoint/trust edits.
- Preserve cwd, authentication, native argument boundaries and exit status.
- Do not replace Claude executable or clobber unmanaged functions/profile content.
- Experimental HTTP/1.1 bridge, 2 MB request limit; no paid model tests.

## Review Focus

- Quoted paths and shell metacharacters must remain literal.
- Prompt arguments resembling proxy flags must survive native passthrough.
- Malformed blocks and read failures must never erase profile content.
- Proxy startup must not wait on inherited Windows pipe handles or kill a shared optimizer.
- Unwrap and whole-product uninstall must leave unrelated shell setup intact.

### Task 1: Wrapper dispatch and managed profiles

**Files:** Create `src/wrap.rs`, `src/wrap_tests.rs`, `src/cli/wrap.rs`; modify `src/main.rs`, `src/cli/mod.rs`, `src/uninstall.rs`.

**Interfaces:** `wrap::configure(shell, profile, executable, remove) -> Result<(), String>`; `wrap::launch(args: &[OsString]) -> Result<i32, String>`; `wrap::cleanup_profiles(dry_run: bool)`.

- [x] Write failing tests for leading proxy flag boundaries, idempotent managed install/remove, conflicting definitions, malformed blocks and quoting.
- [x] Run focused tests and confirm failure before implementing.
- [x] Implement managed profile writes and CLI wiring; preserve the native launch contract.
- [x] Run focused Rust tests, real PowerShell/Bash wrapper fixtures, and owned-block removal checks; wire whole-product uninstall cleanup.

### Task 2: Embedded HTTPS transport and release verification

**Files:** `src/wrap_https.rs`, `src/wrap_https_tests.rs`, `src/wrap_interrupt.rs`, `docs/claude-proxy-wrapper.md`; consume `wrap::launch` from Task 1. Remove the earlier Python bridge and its superseded test suite.

**Interfaces:** `Bridge::start(lean_port, previous_ca)` owns a temporary CA directory and loopback listener; `shared_proxy(lean, port)` reuses the default optimizer or starts it detached. The waiting Rust launcher owns the runtime until native Claude exits. Ctrl-C remains Claude's decision; native OS arguments stay lossless.

- [x] Write local-only TLS relay, shared optimizer reuse/lifetime, environment and opaque tunnel tests; verify red.
- [x] Reuse the tested certificate/relay/shared-proxy behavior with production-only helpers, no fixture payloads or credential logging.
- [x] Run native TLS/stream/tunnel tests, focused Rust checks and the real-process fixtures, including a proxy launch with Python absent from PATH.
- [x] Build/install the updated local binary and PowerShell wrapper; document reload/status/savings/removal commands. Global Bash installation declined an existing unmanaged Claude definition; no Bash profile changes made.

## Progress

- User requested removing the Python runtime dependency during PR review. The Rust replacement uses the existing HTTP/TLS dependency tree plus rcgen for certificate generation; no external certificate command or process is required. Native regression tests replace the Python bridge suite.
- Native replacement verified: 12 Rust checks, PowerShell/Git Bash/npm launcher fixtures, raw OS argument forwarding, terminal Ctrl-C survival, and a proxy launch with Python absent from PATH. Tests used fake Claude executables and loopback peers; no model/provider calls. Formatting, whitespace and generated dependency consistency checks passed.

- Approved design captured; implementation method: inline. Working checkout has no tracked changes; preserve unrelated untracked files.
- Branch: `codex/claude-proxy-wrap`, working in the existing checkout; unrelated untracked files preserved.
- RED observed for native managed install/idempotency and proxy argument dispatch. Python session contract RED observed (returned 1 instead of child status 7).
- Python bridge checks pass: TLS/SSE relay, authentication/argument/cwd/exit preservation, two sessions share optimizer state and lifetime, plaintext rejection, opaque other-host tunnel. No provider calls.
- Local shared-cache baseline failed E0514 (cached Rust1.99 vs selected Rust1.98.1). Per-invocation compiler override in untracked `tmp/claude-proxy-build.toml` uses already-installed stable Rust1.99; repository toolchain remains unchanged.
- Ruling: shell wrapper dispatches directly to the real Claude executable for normal calls, preserving ordinary Claude behavior rather than injecting `agentflare run` vault/stage/mise environment changes.
- An independent reviewer inspected the completed source before installation.
- Reviewer found Windows npm resolution, Bash `alias --`, PowerShell `$?` propagation, and Unix signal status issues; regression checks added before fixes. Direct Cargo fallback used after mbx stopped making compiler progress; cache was not cleared.
- Ruling: PowerShell will use a managed native alias to an owned companion executable, because a simple PowerShell function loses `$?` and an advanced function consumes Claude's `-p`. The companion is a hard link/copy of agentflare, never a replacement of the Claude binary. Cost: one extra owned executable path; same user-facing commands.
- Ruling: proxy child launch re-enters Rust's native dispatch, so Windows npm batch launchers use Rust's argument escaping rather than Python's implicit shell handling. Cost: one additional launcher process in proxy mode.
- Final review: all four reviewer findings fixed and covered by regression checks; no deferred findings. 102 focused Rust checks passed (7 wrapper/CLI, 10 shell, 11 alias, 74 agent-registry); real-process Bash/PowerShell/npm fixtures passed without provider calls.
- User-reported TLS EOF reproduced during both HTTP handling and stream close. Narrow client-disconnect handling now closes streams/sockets quietly; unrelated SSL errors remain visible. Seven bridge tests pass, and the installed legacy helper passes both EOF regression checks.
- Local binary installed with version/workflow-store/hash verification and reversible Windows rename; running MCP processes were not stopped. EOF fix deployed to both the legacy launcher and embedded native bridge. Existing sessions need restarting to load it.
- Global Bash profile already defines `claude`; install refused as designed. lean-ctx's protected-path boundary also refused reading that profile; no override or broad home-directory access was attempted.
