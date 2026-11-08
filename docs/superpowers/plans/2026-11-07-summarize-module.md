# Summarize Module Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task by task.

**Goal:** Extract complete timestamped YouTube transcripts through a separate summarize module and summarize them through a bundled agent skill.

**Architecture:** `flare-summarize` owns validation, transcript types, direct retrieval, browser fallback, and rendering. The CLI supplies agentflare's existing browser runner. The skill instructs the calling agent to summarize, avoiding a second model provider.

**Tech Stack:** Rust, youtube-transcript-api, Tokio, serde, existing flare-browser integration.

**Spec:** Approved fetch-first → browser fallback → agent-written summary workflow, with a separate summarize module.

## Global Constraints

- Prefer direct captions; no audio downloads or speech recognition.
- Preserve timestamps and complete coverage; never substitute the video description.
- Use an isolated browser session and bounded chunk reads.
- Respect language preference and support fetch-only operation.

## Review Focus

- Reject non-YouTube URLs before network actions.
- Empty or malformed captions trigger fallback or a clear error.
- Read transcripts beyond the browser output cap without losing segments.
- Never silently substitute another requested language.
- Preserve both retrieval errors when fallback also fails.

### Task 1: Transcript module

- [x] Write and run failing tests for URL validation, transcript validation, fetch-first behavior, disabled fallback, chunk completeness, and fallback errors.
- [x] Implement `crates/flare-summarize`, using youtube-transcript-api for HTTP extraction.
- [x] Run the crate test suite and clippy.

### Task 2: CLI and skill

- [x] Add `agentflare summarize <URL> [--language CODE] [--json] [--fetch-only]` and internal raw browser execution access.
- [x] Embed the summarize skill and test registry materialization.
- [x] Document usage and attribution; run formatting, registry tests, CLI checks, and the build.

## Verification

- 10 summarize unit tests, 84 registry unit tests, 1 registry golden test,
  3 CLI integration tests, and 5 browser JavaScript fixture tests passed.
- `mbx clippy -p flare-summarize --all-targets -- -D warnings` passed.
- Cargo/rustfmt formatting, skill validation, and `git diff --check` passed.
- Live fetch-only smoke test for `dQw4w9WgXcQ` returned 61 English segments
  with `source=fetch`. Browser fallback is fixture-tested; its live path was
  not exercised because the sidecar is not installed in this environment.
- Final implementation is in the claimed worktree `.worktrees/task/457`.

## Resumed validation (2026-11-08)

- Direct results now use the same case-insensitive language/variant validation
  as browser results. A wrong-language direct result triggers fallback, or an
  explicit error in fetch-only mode. The regression test failed before the fix.
- Shared mbx targets are read-only in the dispatched sandbox; verification uses
  absolute-path Cargo with `/tmp/agentflare-457-target` instead.
- Fresh verification passed: 12 transcript unit tests, 9 summary integration
  tests, browser JavaScript fixtures, crate clippy with `-D warnings`,
  `cargo fmt --check`, and `git diff --check`. Independent review: no findings.
- `gh pr checks 1044` could not connect to api.github.com. Live retrieval and
  remote CI status cannot be verified in this sandbox.

## CI dependency integration follow-up (2026-11-08)

- PR #1044 is reachable in this run. Its `hakari` job fails because the new
  crate is absent from workspace-hack management and generated dependency
  features are stale. Reproduced `cargo hakari generate --diff` failure with
  CI's pinned cargo-hakari 0.9.38 before changing the generated configuration.
- Added the managed workspace-hack dependency to `flare-summarize`, regenerated
  the feature manifest, and updated the lockfile without changing dependency
  versions. All three exact CI checks now pass: `cargo hakari generate --diff`,
  `cargo hakari manage-deps --dry-run`, and `cargo hakari verify`.
- Fresh `cargo fmt --check`, six browser JavaScript fixtures, and
  `git diff --check` pass. Remote CI still describes the previously pushed
  revision until this commit is pushed and checks rerun.
- Fresh Rust verification passes: the `flare-summarize` suite, crate clippy
  with `--all-targets -- -D warnings`, and `agentflare-skill-registry` tests
  (84 unit tests and the golden-query integration test, including builtin
  summarize skill materialization). Registry compilation reports existing
  `agentflare-config` warnings.
- All six `cargo test --test summarize_cli --locked` integration tests pass,
  including CLI options, early validation, and serving the builtin skill.
  The full CLI build also reports existing unsafe-code warnings.
