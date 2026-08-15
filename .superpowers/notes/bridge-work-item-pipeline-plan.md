# Bridge Work-Item Pipeline (flare-workflow) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `execute_work`'s single opaque `run_headless` call with a real `flare-workflow`-backed pipeline (`coder` → `review_or_fix` loop → `finalize`), so a claimed item's execution is durable across a daemon crash without redoing already-completed agent turns.

**Architecture:** A new `src/work_item_pipeline.rs` builds a hand-authored `flare_workflow::WorkflowDefinition<WorkItemData>` (Rust builder API, not the JSON/OpenFang schema) with three steps. `coder` and the branching `review_or_fix` step wrap `agent_launch::run_headless` via a reusable async closure (same pattern `agent_send_hook()` already uses in `src/workflow.rs`); `finalize` is a `FunctionStep` running today's `item_done`/`item_release`/comment/notify logic, with its own `RetryPolicy`. The run's `run_id` is persisted on the item's `metadata` JSON field (read-merge-write, since `item_update` replaces metadata wholesale). A long-lived engine registered once at daemon boot (`src/dashboard/server.rs::run`, before `WorkerPool::start`) calls `engine.recover()` exactly once to resume any run left non-terminal by a crash — `recover()` is a whole-store sweep with no per-run targeting, so it must never be called per-dispatch (that would risk double-executing another live process's in-flight run against the same shared `~/.agentflare/workflows.db`).

**Tech Stack:** Rust, `flare-workflow` (already a workspace crate, `crates/flare-workflow`), existing `agentflare_jobs`/`agent_launch`/`mcp_server` infrastructure. No new dependencies.

**Spec:** attached to item #110 (`bridge-work-item-workflow-design.md`, asset id `xWwOol_hf_MX8vbbhdxUw`) — this plan implements it, corrected against `flare-workflow`'s real API in two places noted below.

## Corrections vs. the spec (found during API research, before writing this plan)

1. **`StepMode::Loop` re-invokes ONE executor repeatedly**, chaining `output → input` between iterations (confirmed `crates/flare-workflow/src/engine.rs:1179-1260`) — it does NOT alternate between two different steps. The spec's "reviewer step ⇄ fixer step" picture is implemented as a single `review_or_fix` `StepDefinition` whose closure branches on `ctx.input` (starts with `REVIEW_ISSUES:` → act as fixer; otherwise → act as reviewer), looped via `StepMode::Loop { max_iterations: 2 * MAX_REVIEW_CYCLES, until: "APPROVED".into() }`.
2. **`engine.recover()` is a whole-store sweep** (`crates/flare-workflow/src/engine.rs:243-275`), not per-run-id. It resumes every non-terminal run in the store whose `workflow_id` is currently registered — called once at daemon boot only. Per-dispatch, `execute_work` does NOT call `recover()`; it inspects the stored run's status via `get_status` and either awaits an already-resumed run or starts a fresh one (Task 6).

## Global Constraints

- No feature flag — this replaces `execute_work`'s internals for all daemon-dispatched items and the `agentflare work` CLI path (both call `execute_work`).
- Do not touch: `src/github/bridge/tick.rs` (claim/heartbeat/cede), `src/supervisor.rs::run_review_sweep` (post-PR CI self-repair), `crate::quota::decide`, `supervisor::dispatch_item`/`enqueue_work_job`.
- `reviewer`/`fixer` reuse the same `assignee_agent` as `coder` — no new per-role agent config.
- Metadata writes must read-merge-write (`item_update` replaces the whole `metadata` field — confirmed `mcp_server/tests/item_tests.rs:724-738`).

---

### Task 1: Widen `item_update` visibility so `execute_work` can persist `workflow_run_id`

**Files:**
- Modify: `src/mcp_server/item.rs` (the `item_update` function, currently `pub(super)`)
- Test: `src/mcp_server/item.rs` (existing `#[cfg(test)]` module, or wherever `item_update_sets_metadata`-style tests live per the research — add alongside it)

**Interfaces:**
- Produces: `pub(crate) fn item_update(&self, req: ItemRequest) -> Result<String, ErrorData>` on `AgentflareMcp`, callable from `src/cli/work.rs` (matches `item_claim`/`item_release`'s existing `pub(crate)` visibility, called the same way from `execute_work`).

- [ ] **Step 1: Change visibility**

In `src/mcp_server/item.rs`, change:
```rust
pub(super) fn item_update(&self, req: ItemRequest) -> Result<String, ErrorData> {
```
to:
```rust
pub(crate) fn item_update(&self, req: ItemRequest) -> Result<String, ErrorData> {
```

- [ ] **Step 2: Write a regression test proving cross-module callability**

Add near the existing metadata tests in this file's `#[cfg(test)]` module (or the crate's existing `mcp_server` test harness, matching whatever pattern `item_update_sets_metadata` already uses):

```rust
#[test]
fn item_update_is_reachable_from_outside_mcp_server() {
    // Compile-time proof, not a runtime assertion: if `item_update` were
    // still `pub(super)`, this file (outside `mcp_server`) would fail to
    // build. Mirrors how `item_claim`/`item_release` are already exercised
    // cross-module from `cli::work`.
    let _ = crate::mcp_server::AgentflareMcp::item_update;
}
```

Place this specific test in `src/cli/work.rs`'s own `#[cfg(test)]` module instead (it needs to reference the symbol from outside `mcp_server` to actually prove the point) — adjust the `use` path to whatever `src/cli/work.rs`'s existing tests already import (`AgentflareMcp` is already imported there per its top-of-file `use` block).

- [ ] **Step 3: Run it**

```bash
cargo test --lib item_update_is_reachable_from_outside_mcp_server
```
Expected: PASS (this is really a compile check — if it compiles, it passes).

- [ ] **Step 4: Commit**

```bash
git add src/mcp_server/item.rs src/cli/work.rs
git commit -m "fix(mcp): widen item_update visibility to pub(crate) for cross-module metadata writes"
```

---

### Task 2: `WorkItemData` context type and shared constants

**Files:**
- Create: `src/work_item_pipeline.rs`
- Modify: `src/main.rs` (add `mod work_item_pipeline;` near the existing `mod workflow;` at line 69)
- Test: `src/work_item_pipeline.rs` (inline `#[cfg(test)]` module)

**Interfaces:**
- Produces:
  - `pub(crate) struct WorkItemData { pub reply_text: String, pub session_id: Option<String>, pub cost_usd: Option<f64>, pub hold_reason: Option<String>, pub review_issues: Option<String>, pub pr_url: Option<String> }` — implements `Default`, `Clone`, `serde::Serialize`, `serde::Deserialize`, and `flare_workflow::WorkflowData`.
  - `pub(crate) const MAX_REVIEW_CYCLES: u32 = 3;` (mirrors `quota::decide::SELF_REPAIR_CAP`'s existing cap-constant style).
  - `pub(crate) const WORKFLOW_ID: &str = "agentflare-work-item";`

- [ ] **Step 1: Write the failing test**

```rust
// src/work_item_pipeline.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_item_data_round_trips_through_json() {
        let data = WorkItemData {
            reply_text: "did the thing".into(),
            session_id: Some("sess-1".into()),
            cost_usd: Some(0.42),
            hold_reason: None,
            review_issues: Some("- fix the thing".into()),
            pr_url: Some("https://github.com/x/y/pull/1".into()),
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: WorkItemData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reply_text, "did the thing");
        assert_eq!(back.pr_url.as_deref(), Some("https://github.com/x/y/pull/1"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails (file/module doesn't exist yet)**

```bash
cargo test --lib work_item_data_round_trips_through_json
```
Expected: FAIL — `work_item_pipeline` module not found.

- [ ] **Step 3: Write the module and register it**

`src/work_item_pipeline.rs`:
```rust
//! Builds and runs the per-work-item `flare-workflow` pipeline: `coder` →
//! a bounded `review_or_fix` loop → `finalize`. See
//! `docs` item #110 for the design; corrected against the crate's real
//! Rust builder API (not the JSON/OpenFang schema — `finalize` runs real
//! Rust logic, not an agent prompt).

/// Cap on review/fix cycles before an item is gated for a human instead of
/// looping forever on an agent that can't converge. Mirrors
/// `quota::decide::SELF_REPAIR_CAP`'s existing cap-constant pattern.
pub(crate) const MAX_REVIEW_CYCLES: u32 = 3;

/// `flare_workflow::WorkflowId` name for this pipeline definition —
/// registered once at daemon boot (see `src/dashboard/server.rs`) and
/// referenced by every dispatched item's run.
pub(crate) const WORKFLOW_ID: &str = "agentflare-work-item";

/// Per-run state threaded through `coder` → `review_or_fix` → `finalize`.
/// `flare_workflow::WorkflowContext::data` persists and mutates across
/// steps within a run — this is where step results live, not the
/// `input`/`output` string channel (which only carries the loop's own
/// phase signal, see `build_review_or_fix_step`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkItemData {
    pub reply_text: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    /// Set when `coder` detects an `AGENTFLARE_HOLD:` signal — short-circuits
    /// the rest of the pipeline straight to `item_release` (see Task 4).
    pub hold_reason: Option<String>,
    /// Latest unresolved reviewer findings, if any — read by `finalize`'s
    /// cap-exceeded path to post a useful gate comment.
    pub review_issues: Option<String>,
    pub pr_url: Option<String>,
}

impl flare_workflow::WorkflowData for WorkItemData {
    fn workflow_type() -> &'static str {
        WORKFLOW_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_item_data_round_trips_through_json() {
        let data = WorkItemData {
            reply_text: "did the thing".into(),
            session_id: Some("sess-1".into()),
            cost_usd: Some(0.42),
            hold_reason: None,
            review_issues: Some("- fix the thing".into()),
            pr_url: Some("https://github.com/x/y/pull/1".into()),
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: WorkItemData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reply_text, "did the thing");
        assert_eq!(
            back.pr_url.as_deref(),
            Some("https://github.com/x/y/pull/1")
        );
    }
}
```

In `src/main.rs`, add next to the existing `mod workflow;` (line 69):
```rust
mod work_item_pipeline;
```

If `cargo build` reports `flare_workflow::WorkflowData` isn't re-exported at the crate root, change the `impl` line to whatever path the compiler suggests (e.g. `flare_workflow::types::WorkflowData`) — `src/workflow.rs` already imports several `flare_workflow::*` root re-exports directly (`StateStore`, `StepStatus`), so root-level is the expected path, but confirm against the compiler since this wasn't independently verified.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test --lib work_item_data_round_trips_through_json
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/work_item_pipeline.rs src/main.rs
git commit -m "feat(work): add WorkItemData context type for the work-item flare-workflow pipeline"
```

---

### Task 3: `coder` step

**Files:**
- Modify: `src/work_item_pipeline.rs`
- Test: same file, `#[cfg(test)]`

**Interfaces:**
- Consumes: `flare_workflow::executor::FunctionStep`, `flare_workflow::{StepDefinition, StepResult, WorkflowContext, WorkflowResult}`, `crate::workflow::agent_send_hook` (existing, reused as-is — it already wraps `agent_launch::run_headless` via `spawn_blocking`, see `src/workflow.rs:38-63`).
- Produces: `pub(crate) fn build_coder_step(agent: agent_registry::Agent, prompt: String) -> flare_workflow::StepDefinition<WorkItemData>`.

- [ ] **Step 1: Write the failing test**

```rust
// appended to src/work_item_pipeline.rs's #[cfg(test)] mod tests
use flare_workflow::{StepStatus, WorkflowEngine, StateStore};
use flare_workflow::store::InMemoryStore;
use flare_workflow::{WorkflowDefinition, WorkflowId};
use std::sync::Arc;

fn mock_send_ok(reply: &'static str) -> flare_workflow::json::SendMessage {
    Arc::new(move |_agent: String, _prompt: String| {
        Box::pin(async move { Ok((reply.to_string(), 10u64, 0u64)) })
    })
}

#[tokio::test]
async fn coder_step_populates_reply_text_and_no_hold_reason() {
    let step = build_coder_step_with_sender(
        "Work item #1 — do the thing\n".to_string(),
        mock_send_ok("implemented it"),
    );
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item")
        .add_step(step);

    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            assert_eq!(state.data.reply_text, "implemented it");
            assert!(state.data.hold_reason.is_none());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("coder step did not complete");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib coder_step_populates_reply_text_and_no_hold_reason
```
Expected: FAIL — `build_coder_step_with_sender` not defined.

- [ ] **Step 3: Implement the step**

Append to `src/work_item_pipeline.rs` (above the test module):

```rust
use flare_workflow::executor::FunctionStep;
use flare_workflow::{StepDefinition, StepResult, WorkflowContext, WorkflowResult};

/// Grep a headless reply for `AGENTFLARE_HOLD: <reason>`, same convention
/// `cli::work::detect_hold_signal` already uses — duplicated rather than
/// imported because `cli::work`'s version is a private `fn` and this crate
/// module is lower-level than `cli`; keep them in sync by hand if either
/// changes (both grep the same literal prefix agents are told to use).
fn detect_hold_signal(reply: &str) -> Option<&str> {
    reply.lines().find_map(|line| {
        let reason = line.trim().strip_prefix("AGENTFLARE_HOLD:")?.trim();
        (!reason.is_empty()).then_some(reason)
    })
}

/// Real entry point: dispatch to `crate::workflow::agent_send_hook()`.
pub(crate) fn build_coder_step(
    agent: agent_registry::Agent,
    prompt: String,
) -> StepDefinition<WorkItemData> {
    build_coder_step_with_sender(prompt, crate::workflow::agent_send_hook())
        .with_agent(agent) // see note below if this builder method doesn't exist
}

/// Test seam: same step, an injected `SendMessage` instead of the real
/// headless agent hook (mirrors `src/workflow.rs`'s own
/// `run_workflow_json_with_sender` test seam).
fn build_coder_step_with_sender(
    prompt: String,
    send: flare_workflow::json::SendMessage,
) -> StepDefinition<WorkItemData> {
    let agent_name = "agent".to_string(); // placeholder identity for the send hook
    StepDefinition::new(
        "coder",
        "coder",
        std::sync::Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<WorkItemData>| {
            let send = send.clone();
            let agent_name = agent_name.clone();
            let prompt = prompt.clone();
            Box::pin(async move {
                let (reply, in_tok, out_tok) = send(agent_name, prompt)
                    .await
                    .map_err(flare_workflow::WorkflowError::Journal)?;
                ctx.input_tokens += in_tok;
                ctx.output_tokens += out_tok;
                if let Some(reason) = detect_hold_signal(&reply) {
                    ctx.data.hold_reason = Some(reason.to_string());
                } else {
                    ctx.data.reply_text = reply;
                }
                ctx.output = ctx.data.reply_text.clone();
                Ok(StepResult::Success)
            })
        })),
    )
}
```

`.with_agent(agent)` does not exist on `StepDefinition` (not found in the API research) — remove that line; the real agent name must be threaded into `build_coder_step_with_sender`'s `agent_name` instead of the `"agent"` placeholder. Fix before this step is considered done:

```rust
pub(crate) fn build_coder_step(
    agent: agent_registry::Agent,
    prompt: String,
) -> StepDefinition<WorkItemData> {
    build_coder_step_with_sender(agent.as_str().to_string(), prompt, crate::workflow::agent_send_hook())
}

fn build_coder_step_with_sender(
    agent_name: String,
    prompt: String,
    send: flare_workflow::json::SendMessage,
) -> StepDefinition<WorkItemData> {
    StepDefinition::new(
        "coder",
        "coder",
        std::sync::Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<WorkItemData>| {
            let send = send.clone();
            let agent_name = agent_name.clone();
            let prompt = prompt.clone();
            Box::pin(async move {
                let (reply, in_tok, out_tok) = send(agent_name, prompt)
                    .await
                    .map_err(flare_workflow::WorkflowError::Journal)?;
                ctx.input_tokens += in_tok;
                ctx.output_tokens += out_tok;
                if let Some(reason) = detect_hold_signal(&reply) {
                    ctx.data.hold_reason = Some(reason.to_string());
                } else {
                    ctx.data.reply_text = reply;
                }
                ctx.output = ctx.data.reply_text.clone();
                Ok(StepResult::Success)
            })
        })),
    )
}
```

Update the test to call `build_coder_step_with_sender("agent".to_string(), prompt, send)` (three args) to match.

`WorkflowError::Journal(String)` is the closest existing variant for "the send hook returned a plain `String` error" per the enum found in research (`NotFound`, `DefinitionNotFound`, `StepFailed{..}`, `StepTimeout{..}`, `Cancelled`, `InvalidStateTransition`, `ShuttingDown`, `Journal(String)`, `Store(String)`) — if compilation shows a better-fitting variant (e.g. a dedicated `StepFailed`-shaped constructor expected by the engine's retry logic), switch to it; verify against `crates/flare-workflow/src/types.rs`'s `WorkflowError` definition directly before finalizing.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test --lib coder_step_populates_reply_text_and_no_hold_reason
```
Expected: PASS

- [ ] **Step 5: Add the hold-signal test**

```rust
#[tokio::test]
async fn coder_step_sets_hold_reason_and_leaves_reply_text_empty() {
    let step = build_coder_step_with_sender(
        "agent".to_string(),
        "prompt".to_string(),
        mock_send_ok("looked into it\nAGENTFLARE_HOLD: waiting on PR #1"),
    );
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            assert_eq!(state.data.hold_reason.as_deref(), Some("waiting on PR #1"));
            assert!(state.data.reply_text.is_empty());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("coder step did not complete");
}
```

Run: `cargo test --lib coder_step_sets_hold_reason_and_leaves_reply_text_empty` → PASS.

- [ ] **Step 6: Commit**

```bash
git add src/work_item_pipeline.rs
git commit -m "feat(work): add coder step to the work-item pipeline"
```

---

### Task 4: `review_or_fix` looped step

**Files:**
- Modify: `src/work_item_pipeline.rs`

**Interfaces:**
- Consumes: `WorkItemData` (Task 2), `StepMode::Loop` (confirmed `crates/flare-workflow/src/types.rs:96-116`).
- Produces: `pub(crate) fn build_review_or_fix_step(agent_name: String, worktree_path: std::path::PathBuf, item_summary: String) -> StepDefinition<WorkItemData>` (real version) and a `_with_sender` test seam mirroring Task 3's pattern.

**Design note (from the API research):** `StepMode::Loop` re-invokes the SAME executor, chaining `output → input` between iterations. The closure decides its role each call from `ctx.input`: empty or previous output was `FIXED:...` → act as reviewer (re-diff the live worktree — the diff is the real state, `ctx.input`/`ctx.output` only carry the phase signal, never the diff itself); starts with `REVIEW_ISSUES:` → act as fixer. Loop config: `max_iterations: 2 * MAX_REVIEW_CYCLES`, `until: "APPROVED"`.

- [ ] **Step 1: Write the failing test — approved on first pass**

```rust
#[tokio::test]
async fn review_or_fix_step_stops_immediately_when_approved() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls2 = calls.clone();
    let send: flare_workflow::json::SendMessage = std::sync::Arc::new(move |_a, _p| {
        calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(("REVIEW_APPROVED".to_string(), 1u64, 0u64)) })
    });
    let step = build_review_or_fix_step_with_sender(
        "agent".to_string(),
        "dummy diff prompt".to_string(),
        send,
    );
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(state.data.review_issues.is_none());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("review_or_fix step did not complete");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib review_or_fix_step_stops_immediately_when_approved
```
Expected: FAIL — `build_review_or_fix_step_with_sender` not defined.

- [ ] **Step 3: Implement**

```rust
const REVIEW_APPROVED_MARKER: &str = "REVIEW_APPROVED";
const REVIEW_ISSUES_MARKER: &str = "REVIEW_ISSUES:";

fn build_review_or_fix_step(
    agent_name: String,
    diff_prompt_prefix: String,
) -> StepDefinition<WorkItemData> {
    build_review_or_fix_step_with_sender(
        agent_name,
        diff_prompt_prefix,
        crate::workflow::agent_send_hook(),
    )
}

fn build_review_or_fix_step_with_sender(
    agent_name: String,
    diff_prompt_prefix: String,
    send: flare_workflow::json::SendMessage,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let send = send.clone();
            let agent_name = agent_name.clone();
            let diff_prompt_prefix = diff_prompt_prefix.clone();
            let is_fix_round = ctx.input.starts_with(REVIEW_ISSUES_MARKER);
            Box::pin(async move {
                let prompt = if is_fix_round {
                    format!(
                        "{diff_prompt_prefix}\n\nAddress this reviewer feedback, commit the \
                         fix, then reply with a one-line summary:\n{}",
                        ctx.input
                    )
                } else {
                    format!(
                        "{diff_prompt_prefix}\n\nReview the diff above. Reply with exactly \
                         `{REVIEW_APPROVED_MARKER}` if it's correct and ready, or \
                         `{REVIEW_ISSUES_MARKER}` followed by a bullet list of concrete \
                         issues to fix."
                    )
                };
                let (reply, in_tok, out_tok) = send(agent_name, prompt)
                    .await
                    .map_err(flare_workflow::WorkflowError::Journal)?;
                ctx.input_tokens += in_tok;
                ctx.output_tokens += out_tok;

                if is_fix_round {
                    ctx.data.review_issues = None;
                    ctx.output = format!("FIXED: {reply}");
                } else if reply.trim_start().starts_with(REVIEW_APPROVED_MARKER) {
                    ctx.data.review_issues = None;
                    ctx.output = REVIEW_APPROVED_MARKER.to_string();
                } else {
                    ctx.data.review_issues = Some(reply.clone());
                    ctx.output = reply;
                }
                Ok(StepResult::Success)
            })
        },
    ));

    StepDefinition::new("review_or_fix", "review_or_fix", executor).with_mode(
        flare_workflow::StepMode::Loop {
            max_iterations: 2 * MAX_REVIEW_CYCLES,
            until: REVIEW_APPROVED_MARKER.to_string(),
        },
    )
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test --lib review_or_fix_step_stops_immediately_when_approved
```
Expected: PASS

- [ ] **Step 5: Write and run the one-fix-cycle test**

```rust
#[tokio::test]
async fn review_or_fix_step_fixes_once_then_approves() {
    let call_n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_n2 = call_n.clone();
    let send: flare_workflow::json::SendMessage = std::sync::Arc::new(move |_a, _p| {
        let n = call_n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if n == 0 {
                Ok(("REVIEW_ISSUES:\n- fix the typo".to_string(), 1, 0))
            } else {
                Ok(("REVIEW_APPROVED".to_string(), 1, 0))
            }
        })
    });
    let step = build_review_or_fix_step_with_sender("agent".to_string(), "diff".to_string(), send);
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            assert_eq!(call_n.load(std::sync::atomic::Ordering::SeqCst), 3);
            assert!(state.data.review_issues.is_none());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("review_or_fix step did not complete");
}
```

Note: this expects 3 calls (review→issues, fix, review→approved) — if the loop's actual iteration semantics diverge from this expectation once run against the real engine (e.g. `until` matching happens before vs. after the fix round differently than modeled here), adjust the assertion to match observed behavior and add a one-line comment explaining the real sequencing; do not weaken the test to `>= 2`.

Run: `cargo test --lib review_or_fix_step_fixes_once_then_approves` → PASS (or fix the model above to match reality first).

- [ ] **Step 6: Write and run the cap-exceeded test**

```rust
#[tokio::test]
async fn review_or_fix_step_stops_at_cap_with_issues_still_open() {
    let send: flare_workflow::json::SendMessage = std::sync::Arc::new(move |_a, _p| {
        Box::pin(async { Ok(("REVIEW_ISSUES:\n- still broken".to_string(), 1, 0)) })
    });
    let step = build_review_or_fix_step_with_sender("agent".to_string(), "diff".to_string(), send);
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            assert!(state.data.review_issues.is_some());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("review_or_fix step did not complete");
}
```

Run: `cargo test --lib review_or_fix_step_stops_at_cap_with_issues_still_open` → PASS.

- [ ] **Step 7: Commit**

```bash
git add src/work_item_pipeline.rs
git commit -m "feat(work): add bounded review_or_fix loop step to the work-item pipeline"
```

---

### Task 5: `finalize` step

**Files:**
- Modify: `src/work_item_pipeline.rs`
- Reference (read, do not modify in this task): `src/cli/work.rs:850-950`ish (the existing hold/`item_done`/comment/notify logic being ported)

**Interfaces:**
- Consumes: `WorkItemData.hold_reason`, `.review_issues`, `.reply_text`, `.session_id`, `.cost_usd` (Task 2/3/4); `mcp_server::AgentflareMcp::{item_release, item_done, comment_impl}` (existing, now `pub(crate)`/already-`pub(crate)`); `src/cli/work.rs`'s existing `detect_hold_signal`-adjacent helpers `cap_reply_for_comment`, `format_success_comment`, `notify` — these stay `pub(crate)` in `cli::work` and are called from here (widen their visibility from private `fn` to `pub(crate) fn` if the compiler reports them unreachable — same one-line fix pattern as Task 1).
- Produces: `pub(crate) fn build_finalize_step(mcp: std::sync::Arc<AgentflareMcp>, item_id: String, notify_recipient: Option<String>) -> StepDefinition<WorkItemData>`.

- [ ] **Step 1: Widen the three helper functions' visibility in `src/cli/work.rs`**

Change:
```rust
fn cap_reply_for_comment(mcp: &AgentflareMcp, item_id: &str, reply: &str) -> String {
fn format_success_comment(reply: &str, session_id: Option<&str>, cost_usd: Option<f64>, pr_url: Option<&str>) -> String {
fn notify(recipient: &str, body: &str, item_id: &str) {
```
to `pub(crate) fn` each (same rationale as Task 1 — these need to be callable from `work_item_pipeline`, a sibling module of `cli::work`, not a descendant, so plain `fn`'s crate-private-but-not-really default won't reach it. Actually: bare `fn` on an item inside `pub mod work` (or however `cli::work` is declared) is private to the `work` module by default in Rust — always requires at least `pub(crate)` for any cross-module caller, matching what Task 1 already established for `item_update`.)

- [ ] **Step 2: Write the failing test — success path**

```rust
#[tokio::test]
async fn finalize_step_calls_item_done_on_success() {
    // Uses the same in-memory/test AgentflareMcp construction pattern
    // src/mcp_server's own test module already uses elsewhere in this
    // crate (an in-memory sqlite-backed AgentflareMcp with a project +
    // item pre-created) — reuse that harness rather than inventing a new
    // one; import path/helper name TBD against that existing harness.
    let (mcp, item_id) = crate::mcp_server::test_support::mcp_with_item(); // adjust to the real helper
    let mcp = std::sync::Arc::new(mcp);

    let mut data = WorkItemData {
        reply_text: "implemented the thing".into(),
        ..Default::default()
    };
    let step = build_finalize_step(mcp.clone(), item_id.clone(), None);
    let mut ctx = WorkflowContext {
        run_id: flare_workflow::WorkflowRunId::new(),
        data: data.clone(),
        input: String::new(),
        output: String::new(),
        variables: Default::default(),
        input_tokens: 0,
        output_tokens: 0,
    };
    // Directly exercise the step's executor (StepDefinition doesn't expose
    // a public single-step runner in the researched API — call through the
    // engine instead, same pattern as the other step tests in this file,
    // if a direct call isn't possible).
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            let item = mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok()).unwrap().unwrap();
            assert!(item.state_id != ""); // replace with an actual "in_review or completed" assertion once the real test harness's state ids are known
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("finalize step did not complete");
}
```

This test's exact harness (`mcp_with_item`) needs to be located in the existing `mcp_server` test support code before this step is considered done — search for how `mcp_server/tests/item_tests.rs` (referenced in Task 1's research) constructs its `AgentflareMcp` + seeded project/item, and reuse that helper (exposing it as `pub(crate)` from a `test_support` module if it isn't already shared).

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo test --lib finalize_step_calls_item_done_on_success
```
Expected: FAIL — `build_finalize_step` not defined.

- [ ] **Step 4: Implement**

```rust
use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::{CommentRequest, ItemRequest};

fn build_finalize_step(
    mcp: std::sync::Arc<AgentflareMcp>,
    item_id: String,
    notify_recipient: Option<String>,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let mcp = mcp.clone();
            let item_id = item_id.clone();
            let notify_recipient = notify_recipient.clone();
            Box::pin(async move {
                if let Some(reason) = ctx.data.hold_reason.clone() {
                    let _ = mcp.item_release(ItemRequest {
                        action: "release".into(),
                        id: Some(item_id.clone()),
                        ..Default::default()
                    });
                    let body = format!("## agentflare work — on hold\n\n{reason}");
                    let _ = mcp.comment_impl(CommentRequest {
                        action: "create".into(),
                        item_id: Some(item_id.clone()),
                        body: Some(body.clone()),
                        ..Default::default()
                    });
                    if let Some(recipient) = notify_recipient.as_deref() {
                        crate::cli::work::notify(recipient, &body, &item_id);
                    }
                    return Ok(StepResult::Success);
                }

                if ctx.data.review_issues.is_some() {
                    // Cap exceeded with issues still open (Task 4's loop
                    // stopped at max_iterations, not on approval) — gate
                    // for a human instead of opening a PR on unreviewed
                    // code. Mirrors `supervisor::ask_item`'s
                    // needs-human-gate pattern (comment + relabel), done
                    // here via a plain comment since this step has no
                    // access to `supervisor`'s label-id lookups — the
                    // label relabel itself stays the supervisor's job on
                    // its next discovery tick.
                    let issues = ctx.data.review_issues.clone().unwrap_or_default();
                    let _ = mcp.comment_impl(CommentRequest {
                        action: "create".into(),
                        item_id: Some(item_id.clone()),
                        body: Some(format!(
                            "## agentflare work — needs human review\n\n\
                             Automated review/fix did not converge after {MAX_REVIEW_CYCLES} \
                             cycles. Latest outstanding issues:\n\n{issues}"
                        )),
                        ..Default::default()
                    });
                    return Ok(StepResult::Success);
                }

                let done_resp = mcp
                    .item_done(ItemRequest {
                        action: "done".into(),
                        id: Some(item_id.clone()),
                        summary: Some(ctx.data.reply_text.clone()),
                        ..Default::default()
                    })
                    .map_err(|e| flare_workflow::WorkflowError::Journal(e.message.to_string()))?;
                let done_val: serde_json::Value =
                    serde_json::from_str(&done_resp).unwrap_or(serde_json::Value::Null);
                ctx.data.pr_url = done_val["pr_url"].as_str().map(str::to_string);

                let comment_reply =
                    crate::cli::work::cap_reply_for_comment(&mcp, &item_id, &ctx.data.reply_text);
                let comment_body = crate::cli::work::format_success_comment(
                    &comment_reply,
                    ctx.data.session_id.as_deref(),
                    ctx.data.cost_usd,
                    ctx.data.pr_url.as_deref(),
                );
                let _ = mcp.comment_impl(CommentRequest {
                    action: "create".into(),
                    item_id: Some(item_id.clone()),
                    body: Some(comment_body.clone()),
                    ..Default::default()
                });
                if let Some(recipient) = notify_recipient.as_deref() {
                    crate::cli::work::notify(recipient, &comment_body, &item_id);
                }
                Ok(StepResult::Success)
            })
        },
    ));

    StepDefinition::new("finalize", "finalize", executor).with_retry(flare_workflow::RetryPolicy {
        max_attempts: 3,
        backoff: flare_workflow::BackoffStrategy::Exponential {
            base: std::time::Duration::from_secs(1),
            max: std::time::Duration::from_secs(30),
        },
    })
}
```

Fix the exact `test_support`/harness reference from Step 2 once located; this step's production code does not depend on that harness.

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test --lib finalize_step_calls_item_done_on_success
```
Expected: PASS (after fixing the test harness reference and the `item.state_id` assertion to check the project's real "in_review"/"completed" state id, matching how other tests in this codebase assert item state post-`item_done` — search `mcp_server/tests` for an existing `assert_eq!(item.state_id, ...)`-style check on a post-`done` item and mirror it).

- [ ] **Step 6: Commit**

```bash
git add src/work_item_pipeline.rs src/cli/work.rs
git commit -m "feat(work): add retryable finalize step (item_done/release/comment) to the work-item pipeline"
```

---

### Task 6: Pipeline assembly + resumable run entrypoint

**Files:**
- Modify: `src/work_item_pipeline.rs`

**Interfaces:**
- Consumes: `build_coder_step`, `build_review_or_fix_step`, `build_finalize_step` (Tasks 3-5); `crate::workflow::default_db_path()` (existing, `src/workflow.rs`); `flare_workflow::sqlite_store::SqliteStore::open_file` (confirmed `crates/flare-workflow/src/sqlite_store.rs:47`).
- Produces:
  - `pub(crate) fn build_work_item_pipeline(agent: agent_registry::Agent, coder_prompt: String, review_prompt_prefix: String, mcp: std::sync::Arc<AgentflareMcp>, item_id: String, notify_recipient: Option<String>) -> flare_workflow::WorkflowDefinition<WorkItemData>`
  - `pub(crate) fn engine() -> &'static flare_workflow::WorkflowEngine<WorkItemData, flare_workflow::sqlite_store::SqliteStore<WorkItemData>>` — a `std::sync::LazyLock`-backed shared engine (mirrors `src/workflow.rs`'s existing `WORKFLOW_RT`/engine-per-call pattern, but this one must be a *single shared instance* across the process, not built fresh per call, so that a run resumed via `recover()` at boot and a later `execute_work` call see the same registered definition and in-memory bookkeeping).
  - `pub(crate) fn run_or_resume(mcp: std::sync::Arc<AgentflareMcp>, item: &agentflare_backend::item::Item, agent: agent_registry::Agent, coder_prompt: String, review_prompt_prefix: String, notify_recipient: Option<String>) -> Result<(), String>` — the function `execute_work` calls (Task 7). Blocks synchronously (via `WORKFLOW_RT.block_on`, reusing `src/workflow.rs`'s existing shared runtime) until the run reaches a terminal state.

- [ ] **Step 1: Write the failing test — fresh run persists a run_id onto the item, resume skips the coder step**

```rust
#[tokio::test]
async fn run_or_resume_persists_run_id_and_resume_skips_completed_coder_step() {
    let (mcp, item_id) = crate::mcp_server::test_support::mcp_with_item(); // same harness as Task 5
    let mcp = std::sync::Arc::new(mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
        .unwrap()
        .unwrap();

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // First run: everything succeeds normally.
    let result = run_or_resume(
        mcp.clone(),
        &item,
        agent_registry::Agent::ClaudeCode,
        "implement it".to_string(),
        "diff prefix".to_string(),
        None,
    );
    assert!(result.is_ok());

    let updated = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
        .unwrap()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
    assert!(metadata["workflow_run_id"].as_str().is_some());
}
```

This test exercises the REAL `agent_send_hook()` path (no mock), which requires a headless agent binary on `PATH` — mark it `#[ignore]` with a comment (`// requires a real headless agent binary; run manually / in an environment with one installed`) rather than have it fail in ordinary `cargo test` runs, consistent with how `src/workflow.rs`'s own tests inject a mock sender for everything except the one real-git-flow test that also needs a real environment. Add a second, non-ignored version using an injected sender (add `_with_sender` variants to `build_work_item_pipeline`/`run_or_resume`, following the same seam pattern as every other step in this file) for the part of this test that can run unconditionally in CI: the metadata-persistence assertion.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib run_or_resume_persists_run_id_and_resume_skips_completed_coder_step
```
Expected: FAIL — `run_or_resume`/`build_work_item_pipeline` not defined.

- [ ] **Step 3: Implement**

```rust
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::{WorkflowEngine, WorkflowId, WorkflowStatus};

pub(crate) fn build_work_item_pipeline(
    agent: agent_registry::Agent,
    coder_prompt: String,
    review_prompt_prefix: String,
    mcp: std::sync::Arc<AgentflareMcp>,
    item_id: String,
    notify_recipient: Option<String>,
) -> flare_workflow::WorkflowDefinition<WorkItemData> {
    flare_workflow::WorkflowDefinition::new(WORKFLOW_ID, "work item")
        .add_step(build_coder_step(agent, coder_prompt))
        .add_step(
            build_review_or_fix_step(agent.as_str().to_string(), review_prompt_prefix)
                .depends_on(&["coder"]),
        )
        .add_step(
            build_finalize_step(mcp, item_id, notify_recipient).depends_on(&["review_or_fix"]),
        )
}

/// Process-lifetime shared engine — built once, reused by every dispatch
/// AND by the boot-time `recover()` sweep (Task 8), so a run resumed at
/// startup and a later `run_or_resume` call for a DIFFERENT item share the
/// same registered `WorkflowDefinition`/in-memory bookkeeping. A fresh
/// engine per call (the pattern `src/workflow.rs`'s JSON pipeline uses)
/// would work for isolated JSON runs but would defeat `recover()`'s
/// "definition must already be registered on this engine" requirement here.
pub(crate) fn engine()
-> &'static WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>> {
    static ENGINE: std::sync::LazyLock<WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>> =
        std::sync::LazyLock::new(|| {
            let store = SqliteStore::open_file(&crate::workflow::default_db_path())
                .expect("open workflow store for work-item pipeline");
            WorkflowEngine::<WorkItemData, _>::with_store(store)
                .with_runtime_handle(crate::workflow::blocking_runtime_handle())
        });
    &ENGINE
}

pub(crate) fn run_or_resume(
    mcp: std::sync::Arc<AgentflareMcp>,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    coder_prompt: String,
    review_prompt_prefix: String,
    notify_recipient: Option<String>,
) -> Result<(), String> {
    let existing_metadata: serde_json::Value =
        serde_json::from_str(&item.metadata).unwrap_or(serde_json::Value::Object(Default::default()));
    let existing_run_id = existing_metadata["workflow_run_id"]
        .as_str()
        .and_then(|s| flare_workflow::WorkflowRunId::from_str(s).ok());

    let eng = engine();
    let definition = build_work_item_pipeline(
        agent,
        coder_prompt,
        review_prompt_prefix,
        mcp.clone(),
        item.id.clone(),
        notify_recipient,
    );
    eng.register_workflow(definition).map_err(|e| e.to_string())?;

    crate::workflow::blocking_runtime().block_on(async move {
        let run_id = match existing_run_id {
            Some(run_id) => {
                let state = eng.get_status(run_id).await.map_err(|e| e.to_string())?;
                if matches!(state.status, WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled) {
                    // Terminal — this is a genuine re-dispatch (e.g. a
                    // fresh self-repair pass), not a crash resume. Start
                    // over with a new run.
                    let new_run_id = eng
                        .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
                        .await
                        .map_err(|e| e.to_string())?;
                    persist_run_id(&mcp, &item.id, &existing_metadata, new_run_id)?;
                    new_run_id
                } else {
                    // Non-terminal: either already resumed by the
                    // boot-time `recover()` sweep (Task 8) or genuinely
                    // still running in this same live process. Either
                    // way, do NOT start a second run against it — just
                    // await this one.
                    run_id
                }
            }
            None => {
                let new_run_id = eng
                    .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
                    .await
                    .map_err(|e| e.to_string())?;
                persist_run_id(&mcp, &item.id, &existing_metadata, new_run_id)?;
                new_run_id
            }
        };

        loop {
            let state = eng.get_status(run_id).await.map_err(|e| e.to_string())?;
            match state.status {
                WorkflowStatus::Completed => return Ok(()),
                WorkflowStatus::Failed | WorkflowStatus::Cancelled => {
                    return Err(state.error.unwrap_or_else(|| "workflow run failed".to_string()));
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
            }
        }
    })
}

fn persist_run_id(
    mcp: &AgentflareMcp,
    item_id: &str,
    existing_metadata: &serde_json::Value,
    run_id: flare_workflow::WorkflowRunId,
) -> Result<(), String> {
    let mut merged = existing_metadata.clone();
    merged["workflow_run_id"] = serde_json::Value::String(run_id.to_string());
    mcp.item_update(crate::mcp_server::types::ItemRequest {
        action: "update".into(),
        id: Some(item_id.to_string()),
        metadata: Some(merged),
        ..Default::default()
    })
    .map(|_| ())
    .map_err(|e| e.message.to_string())
}
```

This step references `crate::workflow::blocking_runtime()` and `crate::workflow::blocking_runtime_handle()` — the first already exists as `#[cfg(test)] fn blocking_runtime()` in `src/workflow.rs`; widen it to a non-test `pub(crate) fn blocking_runtime() -> &'static tokio::runtime::Runtime { &WORKFLOW_RT }` (drop the `#[cfg(test)]`, since this module now needs it outside tests too), and add `pub(crate) fn blocking_runtime_handle() -> tokio::runtime::Handle { WORKFLOW_RT.handle().clone() }` alongside it.

`WorkflowRunId::from_str`/`.to_string()` — confirmed used this way already in `src/workflow.rs`'s `parse_run_id`; reuse the same pattern.

- [ ] **Step 4: Widen `blocking_runtime` in `src/workflow.rs`**

Change:
```rust
#[cfg(test)]
fn blocking_runtime() -> &'static tokio::runtime::Runtime {
    &WORKFLOW_RT
}
```
to:
```rust
pub(crate) fn blocking_runtime() -> &'static tokio::runtime::Runtime {
    &WORKFLOW_RT
}

pub(crate) fn blocking_runtime_handle() -> tokio::runtime::Handle {
    WORKFLOW_RT.handle().clone()
}
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test --lib run_or_resume_persists_run_id_and_resume_skips_completed_coder_step
```
Expected: PASS (the non-ignored, mock-sender variant per Step 1's note).

- [ ] **Step 6: Commit**

```bash
git add src/work_item_pipeline.rs src/workflow.rs
git commit -m "feat(work): assemble the work-item pipeline and add a resumable run_or_resume entrypoint"
```

---

### Task 7: Rewire `execute_work` to use the pipeline

**Files:**
- Modify: `src/cli/work.rs` (`execute_work`, `src/cli/work.rs:690-1008`)

**Interfaces:**
- Consumes: `work_item_pipeline::run_or_resume` (Task 6), all of `execute_work`'s existing pre-pipeline setup (claim, worktree chdir, agent resolution, `build_prompt`) — unchanged.
- Produces: `execute_work` keeps its existing `pub(crate) fn execute_work(args: WorkArgs, log: &mut dyn std::io::Write) -> WorkOutcome` signature — callers (`WorkItemExecutor::execute`, the CLI) are unaffected.

- [ ] **Step 1: Write the failing integration test**

Add to `src/cli/work.rs`'s existing `#[cfg(test)]` module (reusing whatever fixture the file's current tests already use for a full `execute_work` run — locate it before writing this test; it likely stands up a temp repo + temp `AgentflareMcp` + a claimable item, similar to the harness referenced in Task 5):

```rust
#[test]
fn execute_work_runs_through_the_pipeline_and_reports_pr_url() {
    // Reuse this file's existing execute_work test fixture (locate the
    // current test that exercises the pre-pipeline run_headless path and
    // mirror its setup) — inject a mock agent binary/sender exactly the
    // way that existing test already does, extended to also answer the
    // review_or_fix step with REVIEW_APPROVED, and finalize with a
    // successful item_done. Assert `WorkOutcome.exit_code == 0` and that
    // the item's final comment mentions a PR url, matching this file's
    // existing assertion style for the pre-change single-call flow.
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib execute_work_runs_through_the_pipeline_and_reports_pr_url
```
Expected: FAIL (test body is a stub pending Step 1's real fixture — fill it in using the located fixture, then this becomes a real failing test against the still-unchanged `execute_work`).

- [ ] **Step 3: Replace `execute_work`'s post-setup body**

In `src/cli/work.rs`, keep everything through the worktree chdir and `build_prompt`/`comments`/`latest_handoff` construction (lines ~690-812ish) unchanged. Replace the block from `let outcome = agent_launch::run_headless(...)` (`:123` in the earlier archive excerpt, i.e. `src/cli/work.rs` around line 852 in the full file) through the end of the `match outcome { ... }` block (through line ~1007) with:

```rust
    let review_prompt_prefix = format!(
        "Work item #{} — {}\n\nCurrent diff:\n{}",
        item_detail.sequence_id,
        item_detail.name,
        crate::work_item_pipeline::worktree_diff(wpath).unwrap_or_default(),
    );

    let mcp = std::sync::Arc::new(mcp);
    let result = crate::work_item_pipeline::run_or_resume(
        mcp.clone(),
        &item_detail,
        agent_enum,
        prompt,
        review_prompt_prefix,
        args.notify.clone(),
    );

    // Restore cwd regardless of outcome.
    if let Some(d) = original_dir {
        let _ = std::env::set_current_dir(d);
    }

    match result {
        Ok(()) => {
            claim_guard.disarm();
            let _ = writeln!(log, "done: {item_id}");
            0.into()
        }
        Err(msg) => {
            crate::ui::error(&msg);
            let _ = writeln!(log, "failed: {msg}");
            let retry_after_secs = classify_and_cooldown(agent_enum.as_str(), &msg);
            WorkOutcome {
                exit_code: 1,
                retry_after_secs,
            }
        }
    }
```

Note the cwd-restore now happens BEFORE calling `run_or_resume`'s result is matched but AFTER the pipeline itself ran — since the pipeline's `review_or_fix` step needs to read the live worktree diff via `wpath`, the chdir must stay in effect for the whole `run_or_resume` call, not just the old single `run_headless` call. Move the `std::env::set_current_dir(wpath)` call (already present earlier in the function, unchanged) and the restore so they bracket the `run_or_resume` call rather than the old `run_headless` call.

- [ ] **Step 4: Add `worktree_diff` helper**

In `src/work_item_pipeline.rs`:
```rust
/// `git diff` of the worktree's current branch against its merge-base with
/// the default branch — the same "everything this item has changed so far"
/// scope a human PR reviewer works from. Best-effort: an error collapses to
/// an empty diff (the reviewer step still runs, just with less context)
/// rather than failing the whole pipeline over a git plumbing hiccup.
pub(crate) fn worktree_diff(worktree_path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["diff", "HEAD@{upstream}...HEAD"])
        .current_dir(worktree_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}
```

If `HEAD@{upstream}` isn't set for a freshly created worktree branch (likely, since `item::claim` creates a new branch that may not track a remote), replace with the actual base-branch-resolution approach this codebase already uses elsewhere for worktree diffs — search for an existing "diff against base branch" helper (e.g. near `flare-git-core`'s worktree code, `crates/flare-git-core/src/worktree.rs`) before finalizing; reuse it rather than inventing a second base-branch-resolution strategy (ladder rule #2).

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test --lib execute_work_runs_through_the_pipeline_and_reports_pr_url
```
Expected: PASS

- [ ] **Step 6: Run the full existing `cli::work` test suite to confirm no regressions**

```bash
cargo test --lib cli::work::
```
Expected: all PASS — in particular, any existing hold-signal/failure-path tests must still pass with the same externally-visible behavior, since `WorkOutcome`'s shape and `execute_work`'s signature are unchanged.

- [ ] **Step 7: Commit**

```bash
git add src/cli/work.rs src/work_item_pipeline.rs
git commit -m "refactor(work): route execute_work through the flare-workflow pipeline"
```

---

### Task 8: Boot-time engine registration + `recover()`

**Files:**
- Modify: `src/dashboard/server.rs` (the `run` function, near `:704-729` where `WorkItemExecutor` is registered on the `WorkerPool`)

**Interfaces:**
- Consumes: `work_item_pipeline::engine()` (Task 6).

- [ ] **Step 1: Write the failing test**

Add near `src/dashboard/server.rs`'s existing daemon-startup-ordering tests (the file already documents and presumably tests the `reconcile_orphaned_jobs`-before-`WorkerPool::start` ordering per its doc comments — locate that test and mirror its style):

```rust
#[test]
fn recover_work_item_runs_happens_before_worker_pool_start() {
    // Mirrors this file's existing ordering test for
    // `reconcile_orphaned_jobs` vs `WorkerPool::start` (locate it and
    // follow the same assertion style — e.g. a call-order recorder, or
    // whatever mechanism that existing test already uses).
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib recover_work_item_runs_happens_before_worker_pool_start
```
Expected: FAIL (stub pending the real ordering-check mechanism located in Step 1).

- [ ] **Step 3: Add the boot hook**

In `src/dashboard/server.rs`'s `run` function, before the `.with_executor(std::sync::Arc::new(crate::cli::work::WorkItemExecutor))`/`WorkerPool::start` sequence (`:729`ish), add:

```rust
// Register the work-item pipeline definition and resume any run left
// non-terminal by a previous crash, BEFORE the WorkerPool can dispatch a
// new job for the same item — same ordering rationale as
// `reconcile_orphaned_jobs` running before `WorkerPool::start` above.
// `recover()` is a whole-store sweep (resumes every non-terminal run
// whose workflow_id is registered here) — it is only ever called here,
// once, never per-dispatch (see `work_item_pipeline`'s module doc for
// why: no per-run targeting or ownership lease, so calling it from
// `run_or_resume` would risk double-executing a run another live process
// still owns).
{
    let dummy_definition = crate::work_item_pipeline::build_work_item_pipeline(
        agent_registry::Agent::ClaudeCode, // placeholder agent; only the
        // workflow_id/step topology matters for registration+recover — the
        // actual agent/prompts used per-run come from run_or_resume's own
        // registration call each dispatch, which re-registers with the
        // real prompt. `register_workflow` on an already-registered
        // `workflow_id` must overwrite, not error — confirm this against
        // `crates/flare-workflow/src/engine.rs`'s `register_workflow`
        // before relying on it; if it errors on duplicate ids instead,
        // skip this dummy pre-registration and instead have `recover()`
        // called lazily on `work_item_pipeline::engine()`'s first real use
        // via a `std::sync::Once` guard inside `run_or_resume`.
        String::new(),
        String::new(),
        std::sync::Arc::new(mcp.clone()), // adjust to however `mcp`/an
        // AgentflareMcp handle is already available in this function's
        // scope at this point — check the surrounding code for the
        // existing `mcp` binding used by `.with_executor(...)` nearby.
        String::new(),
        None,
    );
    let _ = crate::work_item_pipeline::engine().register_workflow(dummy_definition);
    let _ = crate::workflow::blocking_runtime().block_on(crate::work_item_pipeline::engine().recover());
}
```

This step has two explicit open questions flagged inline in the code comments above (duplicate-registration behavior, and the exact `mcp` binding available in scope) — resolve both by reading `crates/flare-workflow/src/engine.rs`'s `register_workflow` body and the surrounding code in `src/dashboard/server.rs::run` before finalizing; this is a real gap in the research, not glossed over.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test --lib recover_work_item_runs_happens_before_worker_pool_start
```
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/dashboard/server.rs
git commit -m "feat(work): resume in-flight work-item pipeline runs at daemon boot"
```

---

### Task 9: Real end-to-end pipeline test (git flow)

**Files:**
- Modify: `src/work_item_pipeline.rs`

**Interfaces:**
- Consumes: everything from Tasks 2-6.

- [ ] **Step 1: Write the test**

Mirror `src/workflow.rs`'s existing `coder_reviewer_pr_pipeline_runs_real_git_flow` test — real `git init`/commit in a temp repo, an injected `SendMessage` whose behavior branches on the agent-role prompt content (coder: create branch + write file + commit; review_or_fix first call: read the real `git diff` and return `REVIEW_ISSUES:` naming a specific real problem; fixer call: fix it for real in the worktree, commit; second review call: `REVIEW_APPROVED`) — driving `build_work_item_pipeline` end to end and asserting the final git history has both the original and the fix commit, and `WorkItemData.review_issues` is `None` at completion.

```rust
#[tokio::test]
async fn full_pipeline_runs_real_git_flow_with_one_fix_cycle() {
    // Full test body: adapt `src/workflow.rs`'s
    // `coder_reviewer_pr_pipeline_runs_real_git_flow` git-repo setup
    // helper (the `git()` fn defined in that test module) verbatim, then
    // drive `build_work_item_pipeline` with a role-branching `SendMessage`
    // as described above. Write the concrete branching logic and
    // assertions once Task 4's real iteration-count behavior (confirmed
    // in Task 4 Step 5) is known, since this test's call-count
    // expectations depend on it.
}
```

- [ ] **Step 2: Run test to verify it fails, then passes after any fixes surfaced**

```bash
cargo test --lib full_pipeline_runs_real_git_flow_with_one_fix_cycle
```

- [ ] **Step 3: Commit**

```bash
git add src/work_item_pipeline.rs
git commit -m "test(work): add end-to-end real-git-flow test for the work-item pipeline"
```

---

## Self-Review Notes (from writing this plan)

- **Spec coverage:** all sections of the item #110 design doc are covered — `coder`/`review_or_fix`/`finalize` steps (Tasks 3-5), resumability via item metadata (Task 6), boot-time `recover()` (Task 8), the cap-exceeded gate reusing the existing ask/needs-human-gate convention (Task 5), and a real-git-flow test (Task 9) matching the spec's testing section.
- **Known open items, not glossed over:** Task 3's `WorkflowError` variant choice, Task 2's `WorkflowData` trait import path, Task 8's duplicate-registration behavior and `mcp` binding — each is called out inline as a specific, narrow thing to confirm against the compiler or one more source read, not a vague "handle appropriately." Task 5 and Task 7's exact test-harness/base-branch-diff helper names need to be located from existing code before those steps compile — also called out specifically, not left vague.
- **Type consistency check:** `WorkItemData` (Task 2) fields (`reply_text`, `session_id`, `cost_usd`, `hold_reason`, `review_issues`, `pr_url`) are used identically by name across Tasks 3, 4, 5, and 9. `run_or_resume`'s signature (Task 6) matches its call site in Task 7 exactly (same argument order/types).
