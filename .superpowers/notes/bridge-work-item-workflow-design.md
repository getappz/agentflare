# Bridge work-item pipeline on `flare-workflow`

Status: draft, pending user review
Author: session with shiva, 2026-08-13

## Context

The github bridge (`src/github/bridge/`) claims GitHub issues, tracks them as
items, and — once a claimed item's assignee is a confirmed-autonomous agent —
labels it `ready-for-work`. `src/supervisor.rs::run_discovery_tick` polls for
that label, gates dispatch through `crate::quota::decide` (capacity/cooldown/
host-pressure), and enqueues an in-process `agentflare-work` job onto
`agentflare_jobs::Queue` (`WorkItemExecutor`, `src/cli/work.rs`).

`WorkItemExecutor::execute` calls `execute_work` (`src/cli/work.rs:690-1008`),
which today does everything as one continuous, inline sequence:

1. Resolve/route the agent, build the prompt (`build_prompt`), chdir into the
   item's worktree.
2. One `agent_launch::run_headless` call — a single agent turn that is
   expected to implement, build/test, and commit, all in one continuous
   context (the prompt explicitly tells the agent this is "a one-shot
   headless run... no mechanism to resume").
3. Inline post-processing on the reply: an `AGENTFLARE_HOLD:` signal routes
   to `item_release`; otherwise `mcp.item_done(...)` (push + open PR),
   post a result comment, optionally notify.

Separately, `crates/flare-workflow` is a fully-built durable DAG workflow
engine (SQLite-journaled, per-step retry, `WaitEvent`, OpenFang-style JSON
pipelines) exposed via `mcp__flare__workflow` / `agentflare workflow`, but
wired into nothing else in the codebase — it's a standalone, user-triggered
feature today.

**Goal of this change:** give a claimed item a real multi-agent pipeline
(coder → reviewer/fixer loop → finalize) instead of one opaque agent turn,
built on `flare-workflow` so progress is durable and a crash doesn't force
redoing already-completed steps.

## Non-goals

- Not touching the bridge's claim/heartbeat/cede/TTL logic in
  `src/github/bridge/tick.rs`. That system already solves cross-process,
  multi-instance-safe ownership arbitration for "who may work this issue
  right now," and nothing about this change requires replacing it.
- Not touching `run_review_sweep`'s post-PR CI self-repair loop
  (`src/supervisor.rs`) — that operates on a different trigger (CI status on
  an already-open PR) and stays exactly as-is. This pipeline covers only the
  pre-PR portion of dispatch.
- Not touching `quota::decide`, `agentflare_jobs::Queue`'s job-level retry,
  or `supervisor::dispatch_item`/`enqueue_work_job` — the dispatch entry
  point into `execute_work` is unchanged.
- No new per-role agent configuration. `reviewer`/`fixer` steps reuse the
  same `assignee_agent` as `coder`. A distinct reviewer-agent config is a
  future addition, not part of this change.

## Why `flare-workflow` is the right tool here (and wasn't, for the narrower version)

An earlier, narrower version of this idea (wrap the single `run_headless`
call plus its inline finalize logic in a 2-step workflow, purely for
per-step retry) was rejected: `execute_work`'s `claim_guard` (an RAII guard
released only once `item_done`/`item_release` decides the claim's fate) has
to stay armed for the item's entire execution in one live process, so a
crash loses everything regardless of whether the work inside was split into
steps — `flare-workflow`'s actual advantage (resuming past a process crash)
couldn't be exploited.

That calculus changes with a real multi-step, multi-agent pipeline with a
bounded review/fix loop: there is now real sunk cost worth protecting (a
coder turn, potentially several review/fix turns) and a materially longer
total runtime, so resuming a mid-pipeline crash without redoing completed
agent turns is a real, valuable property — not just journal-for-its-own-sake.

## Architecture

```
claim (existing bridge mechanism, unchanged)
  │
  ▼
[coder]       agent step — build_prompt + run_headless, unchanged behavior
  │
  ▼
┌─[reviewer]   agent step — reviews `git diff` of the item's branch;
│   │           replies REVIEW_APPROVED or REVIEW_ISSUES:\n- ...
│   ▼           (marker convention, same style as AGENTFLARE_HOLD:/
│  conditional   detect_hold_signal)
│   │
│   ├─ approved ──────────────────────────────► [finalize]
│   │
│   └─ issues, cycles remain
│        │
│        ▼
│      [fixer]  agent step — prompt = reviewer's issues + diff, fixes, commits
│        │
└────────┘ loop back to [reviewer], capped at MAX_REVIEW_CYCLES
             cap hit with issues still open → do NOT finalize; fall through
             to the existing ask_item/needs-human-gate path
  │
  ▼
[finalize]    FunctionStep, retryable — today's item_done + comment + notify
```

Built with `flare-workflow`'s Rust API (`WorkflowDefinition`/`StepDefinition`,
not the JSON/OpenFang schema — the JSON schema is agent-prompt-only via
`SendMessage`, and `finalize` needs to run real Rust logic, not an agent
turn).

## Components

### New: pipeline builder (`src/workflow.rs`, alongside existing JSON-pipeline code)

A new function, e.g. `build_work_item_pipeline(...)`, constructs the
`WorkflowDefinition` above:

- Custom step-output/context type (not the JSON schema's `PipelineData`)
  carrying what downstream steps and `finalize` need: reply text, session_id,
  cost_usd, hold reason (if any), review verdict, accumulated diff/commit
  info, PR url once known.
- `coder`, `reviewer`, `fixer` steps: agent steps that call
  `agent_launch::run_headless` the same way `agent_send_hook` in
  `src/workflow.rs` already does for the JSON pipeline — reuse that wrapping
  pattern rather than writing a new one.
- `finalize` step: a `FunctionStep` wrapping the existing inline logic from
  `execute_work` (hold-signal → `item_release`; else `item_done` + comment +
  notify), with a `RetryPolicy` (a few attempts, backoff) so a transient
  GitHub API failure retries without re-running any agent step.
- Loop: `StepMode::loop` per the crate's existing step-mode support, bounded
  by a `MAX_REVIEW_CYCLES` constant (mirrors `quota::decide::SELF_REPAIR_CAP`'s
  existing cap-constant pattern).

### Changed: `src/cli/work.rs`

- `execute_work` restructured to build/run this pipeline instead of a single
  `run_headless` call plus inline post-processing. The pre-dispatch parts
  (agent resolution/routing, claim, worktree chdir, `claim_guard`) are
  unchanged — the pipeline runs inside that same guarded scope.
- New prompt builders alongside the existing `build_prompt`:
  `build_review_prompt` (item diff + coder's summary), `build_fixer_prompt`
  (item diff + reviewer's issues).
- New `detect_review_verdict`, structured the same way as the existing
  `detect_hold_signal` (grep the reply for a marker line).

### Resumability

The run's `run_id` is stored in the item's existing `metadata` JSON field —
the same convention `item_model_override` (`src/supervisor.rs`) already uses
for `metadata.model`. On entry, `execute_work` checks `item.metadata` for a
`workflow_run_id`:

- None → start a fresh run, then persist the new `run_id` onto the item.
- Present, and the run's status is non-terminal → call the engine's recovery
  path and resume; already-journaled completed steps (e.g. `coder` already
  committed) are not re-executed.
- Present, and the run's status is terminal (`Completed`/`Failed`) → this is
  a genuine re-dispatch (e.g. self-repair after CI), start a fresh run and
  overwrite the stored `run_id`.

Store path: reuse `crate::workflow::default_db_path()`
(`~/.agentflare/workflows.db`) — the same store the standalone
`mcp__flare__workflow`/`agentflare workflow` feature already uses. Runs from
both surfaces coexist in the same table; they're distinguished by
`workflow_id`/`run_id`, not by a separate database.

## Data flow

1. Supervisor dispatches (unchanged) → `WorkItemExecutor::execute` →
   `execute_work`.
2. `execute_work` resolves the agent, claims/chdir (unchanged), then looks up
   `metadata.workflow_run_id` on the item.
3. Builds or resumes the pipeline; the engine runs steps in-process on
   `WORKFLOW_RT` (existing shared runtime from `src/workflow.rs`), called
   synchronously via `block_on` from this sync context — matching the
   existing CLI-side sync-wrapper pattern in `src/workflow.rs`, since
   `InProcessExecutor::execute` (`crates/agentflare-jobs/src/executor.rs`) is
   itself a synchronous trait method.
4. `finalize` step performs `item_done`/`item_release` + comment + notify,
   exactly as `execute_work` does today.
5. `execute_work` returns `WorkOutcome` as today — job-level retry
   (`agentflare_jobs::Queue`, `retry_after_secs`) is unchanged and still
   applies if the whole job fails outright (e.g. `finalize` exhausts its own
   retries).

## Error handling

- Agent step (`coder`/`reviewer`/`fixer`) failure/timeout: not silently
  retried by the workflow engine itself (an agent turn is expensive; blind
  retry duplicates `agentflare_jobs`' own job-level retry). Surfaces as a
  failed run; `execute_work` maps it to `WorkOutcome { exit_code: 1, .. }`
  the same way today's `HeadlessOutcome` failure arm does, including
  `classify_and_cooldown`.
- `finalize` step failure: retried in-place per its `RetryPolicy` (a few
  attempts, backoff) before surfacing as a job failure — this is the
  concrete improvement over today's behavior, where any `item_done` failure
  fails the whole job and forces a full agent re-run on the next attempt.
- Review/fix loop exceeding `MAX_REVIEW_CYCLES` with issues still open: do
  not finalize. Reuse the existing `ask_item`/`NEEDS_HUMAN_GATE_LABEL`
  pattern from `src/supervisor.rs` so this reads the same way an existing
  gated item does today, rather than inventing a new label/state.
- Hold signal from `coder` (unchanged: `detect_hold_signal`): skips review
  entirely, routes straight to `item_release`, same as today.

## Testing

- Unit tests for `build_review_prompt`/`build_fixer_prompt`/
  `detect_review_verdict`, mirroring the existing tests' style for
  `build_prompt`/`detect_hold_signal`.
- A `flare-workflow`-level test analogous to the existing
  `coder_reviewer_pr_pipeline_runs_real_git_flow` in `src/workflow.rs` —
  real git operations in a temp repo, injected `SendMessage` mock standing
  in for the agent calls, driving the actual coder → reviewer → fixer loop →
  finalize sequence including a cycle that needs one fix round.
- A resume test: start a run, simulate a crash after `coder` completes
  (drop the engine/process), reopen the store, resume, and assert `coder`'s
  step is not re-invoked (mirrors the crate's own `recovery_test.rs`
  patterns).
- A cap test: reviewer keeps returning `REVIEW_ISSUES` past
  `MAX_REVIEW_CYCLES`; assert the run ends in the gated (not finalized)
  state and no PR is opened.

## Rollout

No feature flag proposed — this replaces `execute_work`'s internals directly
for all daemon-dispatched items once merged (the CLI entry point,
`agentflare work`, goes through the same `execute_work`, so it changes too).
