# Design-spec: should `StepMode::Loop` adopt Restate's per-step durability primitive?

> Item #115. Implemented: `JournalEntry::LoopIteration` ships in `types.rs`,
> and `execute_loop` in `engine.rs` resumes from the last journaled iteration
> instead of restarting the counter at 1. Covered by
> `crash_mid_loop_then_recover_resumes_from_last_iteration` in
> `tests/semantics_test.rs`. Left as a design doc for the rationale; the
> "recommendation, no code change" framing below is historical.

Scope: the one gap named in the dispatch — `execute_loop` journals a single
`JournalEntry::StepRun` after the whole loop exits, not per iteration — not a
general "should flare-workflow become Restate" question.

## The gap, precisely

`src/engine.rs::execute_loop` (lines ~1182-1302):

- `for iter in 1..=*max_iterations` runs the step executor once per
  iteration, persisting `ctx.data`/`input`/`output`/`variables` into
  `WorkflowState` via `state_store.update()` after every iteration (this part
  is already durable — it's item #112's basis for putting counters in
  `ctx.data`).
- But `state_store.append_journal(..., JournalEntry::StepRun { .. })` is only
  called **once**, after the `for` loop exits (success) or on failure inside
  the loop. There is no per-iteration journal entry.
- Recovery (`execute_workflow`, lines 483-533) memoizes a step by looking for
  the *last* `JournalEntry::StepRun` with a matching `step_id`. If none
  exists (crash mid-loop, loop never finished), the step is **not** memoized
  and `execute_loop` re-enters from `iter = 1` with a fresh `max_iterations`
  budget on resume — even though N-1 iterations already ran and their
  `ctx.data` side effects already landed in `WorkflowState`.

Net effect: a crash on iteration 7 of a 10-iteration loop doesn't replay
iterations 1-6 for free from the journal — it silently restarts the loop's
iteration *counter* (though whatever `ctx.data` mutations survived stay
persisted, since those went through `state_store.update()`, not the journal).
This is a real but narrow gap: it affects only iteration-count/budget
bookkeeping across a crash, not data loss of `ctx.data` itself.

## How Restate actually solves this (verified against docs.restate.dev)

Restate does **not** special-case loops. Its durable-execution model
(`docs.restate.dev/concepts/durable_execution`, `.../guides/request-lifecycle`)
is: every `ctx.run()` (and `ctx.sleep`/`ctx.call`/awakeable) call is journaled
as an individual entry with a stored result. On failure, **the entire handler
function is replayed from the top**; each time execution reaches a `ctx.*`
call, the SDK checks the journal by call order — if a result is already
recorded, it's returned instantly without re-running the closure; once replay
reaches the first entry with no recorded result, real (live) execution
resumes from there.

Restate's own "Evaluation Feedback Loop" pattern
(`docs.restate.dev/ai/patterns/workflow-evaluator`) is structurally identical
to `StepMode::Loop`: a bounded `for` loop calling `ctx.run()` once per
iteration until a pass condition or `max_iterations`. Their docs state
plainly: *"Each generate and evaluate call is persisted in the journal. If
the process crashes after a successful generation but before evaluation, the
generated code is replayed from the journal without calling the LLM again."*
This is achieved with **zero special loop handling** — it's an emergent
property of the general whole-function-replay engine (every `ctx.run()` call
anywhere in the handler is journaled the same way, loop or not).

So Restate's actual primitive here is the general one: full deterministic
replay of arbitrary handler code, journaling every side-effecting call as it
happens. That general mechanism is precisely item #112's "fork (b)" — the
big, explicitly-out-of-scope rebuild (arbitrary imperative code, deterministic
replay of a whole function/script).

## Answering the three questions

**1. Does Restate have a primitive that closes this gap without requiring
fork (b)?**

Not as a distinct, narrower primitive — Restate's own solution to this exact
shape of problem *is* fork (b), just applied at small scale. There's no
separate "loop checkpoint" API in Restate distinct from `ctx.run()` + replay.

But `flare-workflow` doesn't need to import Restate's *general* mechanism to
close this *specific* gap, because `execute_loop`'s shape is far more
constrained than an arbitrary Restate handler:

- The loop body is always exactly one call: `step.executor.execute()`. There
  is no other branching, no other side-effecting calls interleaved between
  iterations, no arbitrary control flow to rediscover.
- The exit conditions (`until` substring match, `max_iterations`) are static
  data from `StepDefinition`, already known before the loop starts — nothing
  about *why* the loop is shaped this way needs to be re-derived by replaying
  code.
- Because of that, `flare-workflow` can get the *same resume behavior*
  Restate gets from full replay-and-skip, but without an interpreter/replay
  step at all: just read the journal once at loop entry, compute how many
  iterations are already durably recorded, and start the Rust `for` loop's
  counter from `resume_from` instead of `1`. This is the CompletableEntry
  idea (`JournalEntry`'s own doc comment already invokes it) applied one
  level deeper — into a single step's internal iteration count — rather than
  Restate's general per-call, whole-function version of it.

So: yes, a *scoped-down instance* of Restate's underlying idea (per-operation
journaling, resume-from-first-incomplete-entry) closes this cleanly. What
doesn't transfer is Restate's actual mechanism (whole-function replay) —
that part is legitimately fork (b) and stays out of scope.

**2. Concrete shape of the change, and cost estimate**

Cheap, bounded addition — not fork (b) in disguise. Confirmed against the
actual schema: the SQLite `journal` table (`sqlite_store.rs`) is already
generic (`run_id, seq, entry_type TEXT, payload TEXT`) — a new `JournalEntry`
variant needs **no migration**, just a new Rust enum arm.

- `types.rs`: add `JournalEntry::LoopIteration { step_id: StepId, iteration:
  u32, output: Vec<u8> }` as a **new, separate** variant — not reusing
  `StepRun`. Keeping it separate matters: `execute_workflow`'s DAG-level
  memoization (lines 500-514) pattern-matches only on `StepRun`/`Sleep` to
  decide a step is complete; a `LoopIteration` entry must never be mistaken
  for that terminal signal, or a mid-loop crash would wrongly memoize the
  step as done. Add matching `is_completed`/`entry_type` arms (~10 LOC).
- `execute_loop`: at the top, read the run's journal, filter
  `LoopIteration` entries for this `step.id`, take the max `iteration` found
  (default 0), seed `current_output`/`executed` from it, and change the loop
  bound from `for iter in 1..=*max_iterations` to
  `for iter in (resume_from + 1)..=*max_iterations`. After each successful
  iteration's existing `state_store.update()` call, add one
  `state_store.append_journal(run_id, JournalEntry::LoopIteration { .. })`
  call. Leave the failure path and the final terminal `StepRun` append (which
  still marks the step complete for DAG purposes) untouched. ~30-40 LOC.
- No `StateStore` trait changes — `append_journal`/`journal` already exist
  and are exactly what's needed.
- New test: crash-mid-loop resume — run N iterations, drop the run before
  the terminal `StepRun` is written (or simulate via directly appending
  `LoopIteration` entries then re-invoking `execute_loop`), assert it resumes
  from `N+1` and doesn't re-run 1..N. Natural home: alongside the existing
  loop coverage in `tests/semantics_test.rs`.

Total: roughly 50-70 LOC plus one test. No schema migration, no new trait
methods, no changes outside `flare-workflow`.

Known trade-off, same class of risk the engine already accepts elsewhere:
if a crash lands between the per-iteration `state_store.update()` and the
new `append_journal` call, that one iteration's step executor may re-run on
resume (its `ctx.data` effects already landed, but the checkpoint didn't).
This is the identical window `execute_step_with_retry` already has today
between its own `state_store.update()` and `append_journal` calls (lines
1033-1072) — not a new risk class, just the existing single-step guarantee
applied per-iteration instead of per-step.

**3. If no clean primitive existed** — moot; #2 gives a concrete, cheap path.
The domain-level workaround from #112 (counters in `ctx.data`) remains
correct and doesn't need to change — this closes the engine-level gap
*underneath* it, it doesn't replace it. Domains that already rely on
`ctx.data` counters keep working unchanged; they'd simply also start
resuming their iteration budget correctly across a crash instead of
restarting it.

## Recommendation

Worth doing as a small, self-contained follow-up (not urgent, not blocking
anything): add `JournalEntry::LoopIteration` and journal each successful
iteration in `execute_loop`, resuming the iteration counter from the journal
on re-entry. This directly closes the per-iteration durability gap, costs
~50-70 LOC with no schema migration and no trait changes, and stays entirely
within the existing "journal a completable operation, resume from the first
incomplete one" pattern the engine already uses for `StepRun`/`Sleep`/
`WaitEvent` — it does not require adopting Restate's general whole-function
deterministic-replay engine (fork (b)), because `execute_loop`'s shape is
static and known ahead of time rather than something that needs rediscovery
via replay.
