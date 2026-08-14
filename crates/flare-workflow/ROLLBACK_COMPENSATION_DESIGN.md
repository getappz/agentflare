# Design-spec: saga-style rollback/compensation for `flare-workflow` steps

> Item #118. Design doc only — no code change. Follows the item #115
> (`LOOP_DURABILITY_DESIGN.md`) precedent: written recommendation, concrete
> data model, explicit trade-offs, handed back for review before any
> implementation task is scoped.

## Problem

`crates/flare-workflow` has no saga-style compensation mechanism. Cloudflare
Workflows has one: `step.do(name, { rollback: async ({output}) => {...},
rollbackConfig }, fn)` — on workflow failure, every step that registered a
`rollback` handler gets it invoked, in reverse `step-start` order, including
a step that itself failed after registering rollback (its handler runs with
`output: undefined`). This doc designs the `flare-workflow` equivalent.

No prior-art code to copy exists (checked OpenFang, `restate-sdk`, and
crates.io — see item #118's dispatch notes); this is original design work,
same as most of what's already in this crate beyond OpenFang's baseline.

## What already exists that's relevant

- `StepExecutor<D>::on_failure(&self, context, error)` (executor.rs:30) is
  already a "compensation logic" hook per its own doc comment — but it only
  fires for **the step that itself failed**, called once from
  `execute_step_with_retry` right before its terminal failure is journaled
  (engine.rs:1144), and its error is logged and swallowed
  (`tracing::warn!`, doesn't abort the workflow's own terminal transition).
  This is *not* saga rollback (no reverse-order unwind of prior successful
  steps), but its "best-effort, log-and-continue" failure handling is a
  direct precedent this design reuses for Q5.
- The `JournalEntry::StepRun { result: Some(EntryResult::Success(bytes)) }`
  written on every successful step (engine.rs:1063-1072) already carries the
  **full serialized `WorkflowContext<D>`** as of that step's completion —
  not just a status flag. This turns out to be exactly what's needed to give
  a rollback handler the forward step's own captured output (Q1).
- `StepState.started_at` (types.rs:199) is a `DateTime<Utc>` and is durably
  persisted in the SQLite `step_state` projection (`sqlite_store.rs:97`),
  not just held in the in-memory `Instant`-based `StepTracker`. It survives
  a crash and is available for ordering after recovery (Q3).
- The SQLite `journal` table is schema-generic (`run_id, seq, entry_type
  TEXT, payload TEXT` — confirmed in `LOOP_DURABILITY_DESIGN.md`'s
  `LoopIteration` precedent). A new `JournalEntry` variant needs no
  migration, just a new Rust enum arm (Q2).
- `WorkflowDefinition::validate()` already rejects invalid definitions ahead
  of execution (missing deps, cycles) rather than failing at run time — the
  natural place to reject rollback registered on a step mode that can't
  support it.

## Q1 — Data model

**Reuse `StepExecutor<D>` — no new trait.** Add to `StepDefinition<D>`:

```rust
pub rollback: Option<Arc<dyn StepExecutor<D>>>,
/// Retry policy for the rollback handler itself. Falls back to the step's
/// own `retry_policy` (then the workflow default) when unset. The rollback
/// attempt reuses the step's own `timeout` — a separate rollback timeout
/// isn't needed in v1; nothing in this design requires it to diverge.
pub rollback_retry_policy: Option<RetryPolicy>,
```

plus a builder method `StepDefinition::with_rollback(executor, retry_policy:
Option<RetryPolicy>)`.

Cloudflare's handler receives `{output}` — the forward step's own captured
output, not the live/current run state (which has since moved on to later
steps). `flare-workflow` gets this **without a new trait signature**: when
invoking a step's rollback, the engine builds a *fresh* `WorkflowContext<D>`
by deserializing that step's own terminal `StepRun` success entry
(`context_bytes` at engine.rs:1040-1041, already stored) rather than reusing
the run's live context. `context.output` on that reconstructed context is
then exactly what Cloudflare's `{output}` argument represents — and
`context.data`/`variables`/token counts come along for free as a bonus
(a coherent snapshot of everything as of that step's completion), which
Cloudflare's narrower callback signature doesn't offer. `rollback.execute()`
is called with this reconstructed context; its own `context.output` /
`StepResult` after running is **not** chained into `s.input`/`s.output`/
`variables` (the forward pipeline is irrelevant once the workflow has
already failed) — it's only captured into the `Rollback` journal entry for
audit (Q2).

**Which step modes can register rollback.** Only modes whose successful
completion journals a full context snapshot: `Sequential`, `FanOut`, and
`Loop` (its terminal `StepRun` append is untouched by the
`LOOP_DURABILITY_DESIGN.md` per-iteration proposal — still fires). `Collect`
never appends a `StepRun` at all (engine.rs:801-820 updates state directly);
`Sleep`/`WaitEvent` journal only a timer/event marker with no context bytes
(waits.rs). Registering `rollback` on any of these has nothing to
reconstruct a context from. `WorkflowDefinition::validate()` should reject
it explicitly — add `ValidationError::RollbackUnsupportedMode { step:
StepId, mode: &'static str }` — rather than silently no-op-ing at rollback
time.

## Q2 — Durability

**Yes, journal it — reusing the exact CompletableEntry pattern already in
place, at a cost the `LoopIteration` precedent shows is small.**

```rust
JournalEntry::Rollback {
    step_id: StepId,
    attempt: u32,
    result: Option<EntryResult>,
},
```

`is_completed`/`entry_type` get matching arms (~10 LOC, same shape as every
other variant). No schema migration (generic TEXT payload column).

Presence of a completed `Rollback` entry for a `step_id` means "already
compensated, do not re-invoke" — read the same way `StepRun` memoization
already works in the recovery block (engine.rs:483-533). This gives
exactly-once compensation across a crash mid-rollback-phase.

This is admittedly a *stronger* guarantee than Cloudflare's own docs state
(they don't claim rollback survives their own process restart). It's worth
paying for here because: (a) the marginal engineering cost is small — it's
the same append-and-check-before-run pattern the engine already does for
every other durable operation, not new machinery; (b) the alternative
(best-effort, in-memory-only rollback) means a crash mid-unwind either
re-runs an already-executed compensation (double-refund, double-delete) or
silently drops a pending one (leaked side effect) — both worse outcomes for
a mechanism whose entire purpose is correctness cleanup than the "log and
move on" trade-off `on_failure` already accepts for a single best-effort
hook. Recommend v1 includes this from the start rather than as a follow-up.

## Q3 — Ordering

**"Reverse step-start order" = sort by `StepState.started_at` descending,
tie-broken by reverse `StepDefinition` declaration index.**

`started_at` is already set the moment a step attempt begins
(engine.rs:1011, inside `execute_step_with_retry`, before the executor
runs) and is durably persisted per-step — sufficient on its own for the
common case. Concurrent `FanOut` siblings can legitimately share a
millisecond-resolution timestamp; rather than inventing a new monotonic
per-run start-sequence counter (a real option, but unneeded complexity for
v1 — flagged below as a clean additive follow-up if strict ordering within
a fan-out group ever matters), break ties by descending declaration index
in `WorkflowDefinition.steps`. This is deterministic, needs no new field,
and matches the intuitive reading: a fan-out group unwinds together, with
later-declared siblings compensated first.

**Which steps are eligible for a rollback call:**

- Any step with `rollback.is_some()` whose `StepState.status ==
  StepStatus::Succeeded` at the time the unwind starts. (`Skipped` steps —
  whether via `run_if`, `ContinueNextStep`, or upstream-dependency
  blocking — never entered `execute()`, so there's nothing to compensate.)
- Plus, if it registered a rollback, the **specific step whose terminal
  failure caused the workflow to fail** — Cloudflare's documented
  "output: undefined" case, because a step can partially apply side effects
  before its own attempt is judged a failure (e.g. it wrote a record, then
  a post-write validation failed). Since this step has no successful
  `StepRun` snapshot to reconstruct a context from, seed
  `context.output = String::new()` explicitly (the closest Rust equivalent
  of `undefined`) on a context otherwise built from the live
  `WorkflowState.context` at failure time.

## Q4 — Trigger condition

Rollback runs whenever `execute_workflow`'s DAG loop settles the run to
`WorkflowStatus::Failed` — i.e. exactly the two existing settlement points
(the deadlock/deps-blocked early return at engine.rs:687-701, and the
general `failed_step` path at engine.rs:933-946). Both currently write
`status = Failed` + publish `WorkflowFailed` independently; factor them
into one `finish_workflow_failed(...)` helper (see integration section)
that runs the rollback phase first if any registered step qualifies.

- **`FailureAction::ContinueNextStep`**: no rollback. The step's failure is
  absorbed as a skip and the workflow may still reach `Completed`; rollback
  only triggers on outcomes that actually fail the *workflow*, and this one
  by construction doesn't.
- **`FailureAction::RetryIndefinitely`**: excluded automatically, not as a
  special case — `retry::effective_max_attempts` returns `u32::MAX`
  (retry.rs:104-110), so such a step never resolves to a terminal failure
  that reaches either settlement point.
- **Cancellation** (`WorkflowStatus::Cancelled`, engine.rs:537-542) is an
  explicitly separate terminal path that already returns before either
  failure-settlement point is reached. Cloudflare doesn't define
  rollback-on-cancel either. Call this **out of scope** rather than
  half-supporting it — a cancelled run does not run compensation.
- A step `Skipped` via the `deps_blocked_indices` path (upstream dependency
  failed) never executed, so even if it registered a rollback there's
  nothing to invoke — same reasoning as Q3's eligibility list.

## Q5 — Rollback's own retry/failure

Each rollback handler gets its own bounded retry loop, reusing
`retry::Backoff` / `apply_jitter` verbatim with
`rollback_retry_policy.unwrap_or(step.retry_policy.unwrap_or(workflow
default))` and the step's own `timeout` — no new retry code, same machinery
`execute_step_with_retry` already runs.

**If a rollback handler exhausts its retries: log it, journal the failure,
and continue unwinding the remaining earlier steps — do not abort the
rollback phase.** This directly extends the existing `on_failure`-hook
precedent (best-effort, log-and-swallow, engine.rs:1144-1146) rather than
inventing a stricter policy. Aborting the whole unwind because one
compensation failed would leave *earlier*, unrelated successful steps'
side effects uncompensated for no reason connected to them — strictly
worse than finishing the sweep and reporting what didn't get cleaned up.

**Visibility, without inflating `WorkflowStatus`.** The workflow's terminal
status stays `WorkflowStatus::Failed` regardless of whether compensation
fully succeeded — that field answers "did the workflow succeed," and it
didn't, independent of cleanup outcome. Do **not** add a
cross-product status like `FailedRolledBack` / `FailedCompensationFailed`.
Instead:
- Append `Rollback { result: Some(EntryResult::Failure{..}) }` per exhausted
  compensation (so recovery doesn't retry an already-exhausted one forever).
- Add one new event, published once after the unwind finishes:
  `WorkflowEvent::RollbackCompleted { run_id, compensated: Vec<StepId>,
  failed: Vec<StepId> }` (plus `RollbackStepFailed { run_id, step_id, error
  }` per failure, for real-time observability). Consumers that care about
  "was cleanup complete" subscribe to these; consumers that only care about
  pass/fail keep reading `WorkflowStatus` unchanged.
- Fold any rollback failures into the existing `s.error` string alongside
  the original failure reason, so a single `WorkflowState.error` read still
  tells the whole story without a schema change to `WorkflowState`.

## Recommended integration into `execute_workflow` (engine.rs)

New module `src/rollback.rs`, mirroring how `waits.rs` holds
`execute_sleep`/`execute_wait_event` as `impl<D, S> WorkflowEngine<D, S>`
methods (same file-per-concern layout already used in this crate):

```rust
impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    /// Run the saga unwind for `run_id`: every succeeded step with a
    /// registered `rollback` (plus the triggering failed step, if it
    /// registered one), in reverse step-start order. Best-effort per step;
    /// never aborts early. Journal-resumable — already-compensated steps
    /// (a completed `Rollback` entry) are skipped, so a crash mid-unwind
    /// and re-entry via `recover()` picks up where it left off.
    async fn run_rollback_phase(
        &self,
        run_id: WorkflowRunId,
        definition: &WorkflowDefinition<D>,
        failed_step: Option<&StepId>,
    ) -> WorkflowResult<()> { /* ... */ }
}
```

Called from a single new `finish_workflow_failed` helper that both existing
settlement points (engine.rs:687-701, 933-946) call instead of inlining
their own `status = Failed` + `WorkflowFailed` publish:

```rust
async fn finish_workflow_failed(
    &self,
    run_id: WorkflowRunId,
    definition: &WorkflowDefinition<D>,
    failed_step: StepId,
    error_message: String,
) -> WorkflowResult<()> {
    if definition.steps.iter().any(|s| s.rollback.is_some()) {
        self.run_rollback_phase(run_id, definition, Some(&failed_step)).await?;
    }
    self.state_store.update(run_id, |s| {
        s.status = WorkflowStatus::Failed;
        s.error = Some(error_message);
    }).await?;
    self.event_bus.publish(WorkflowEvent::WorkflowFailed { run_id, failed_step, error }).await;
    Ok(())
}
```

Zero behavior change and zero overhead for any workflow that registers no
rollbacks (the `.any(...)` check short-circuits before any journal reads).

**Recovery ordering — no special-case needed at `execute_workflow` entry.**
`recover()` (engine.rs:384-412) only resumes runs from `list_active()`
(`Running`/`Pending`), and `WorkflowStatus` is not flipped to `Failed` until
*after* `run_rollback_phase` returns — so keep status at `Running`
throughout the unwind rather than introducing a new `RollingBack` status.
A crash mid-unwind leaves the run `Running`, so `recover()` naturally
re-invokes `execute_workflow`, whose existing memoization block
(engine.rs:483-533) re-derives `tracker.completed`/`tracker.failed` from
the journal in one pass — all steps are already memoized as terminal, so
the DAG loop exits immediately and falls straight back into
`finish_workflow_failed` → `run_rollback_phase`, which resumes from
whichever `Rollback` journal entries already exist. This reuses the
engine's existing "replay is cheap because everything's memoized" property
instead of adding a new resume path.

Trade-off, stated explicitly rather than solved: while a run is in its
rollback phase, its `WorkflowStatus` still reads `Running` — indistinguishable
from ordinary forward execution to an external dashboard/consumer until it
flips to `Failed`. This is an observability gap, not a correctness one (the
`RollbackStarted`/`RollbackStepFailed`/`RollbackCompleted` events already
carry the detail for anything that needs it in real time). A dedicated
`WorkflowStatus::RollingBack` variant is a clean, additive follow-up if this
turns out to matter in practice — deferred because adding it now means
touching every `WorkflowStatus` match site in `store.rs`/`sqlite_store.rs`
(`list_active`, `cleanup_old_workflows`, `is_cancelled`) for a distinction
nothing currently consumes.

## Explicitly out of scope for v1

- **JSON workflow (`compile_workflow`) support.** `rollback` is a Rust
  builder-API field only. Exposing it to OpenFang-style JSON definitions
  needs a handler-lookup-by-name mechanism analogous to the existing
  `SendMessage` hook, which is a separate, larger surface change — not
  needed to validate the core engine mechanism.
- **`WorkflowStatus::RollingBack`** — see trade-off above.
- **Monotonic per-run `start_seq` on `StepState`** for sub-millisecond
  strict start ordering within a fan-out group — declaration-index
  tie-break is deterministic and sufficient for v1; add only if a concrete
  case needs finer ordering than "the fan-out group unwinds together."
- **Rollback-on-cancel.** Cancellation is a distinct terminal path;
  Cloudflare doesn't define this either.

## Recommendation

Implement as designed above:
`StepDefinition.rollback: Option<Arc<dyn StepExecutor<D>>>` +
`rollback_retry_policy: Option<RetryPolicy>` (reusing the existing trait
and retry types — no new trait, no new backoff code), a new
`JournalEntry::Rollback` variant (no migration), a `ValidationError`
variant rejecting rollback on unsupported step modes, one new
`src/rollback.rs` module following the `waits.rs` layout, and a
`finish_workflow_failed` helper consolidating the two existing failure
settlement points in `engine.rs`. Ordering is `started_at` descending with
declaration-index tie-break; triggering is exactly "workflow settles to
`Failed`"; rollback failures are best-effort and reported via new events
without inflating `WorkflowStatus`. This stays inside the same
"journal a completable operation, resume from the first incomplete one"
pattern the engine already uses everywhere else — no new durability
primitive, no whole-function replay, no schema migration.
