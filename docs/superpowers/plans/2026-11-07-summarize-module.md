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
