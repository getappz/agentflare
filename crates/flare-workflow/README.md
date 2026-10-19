# flare-workflow

Embedded **durable workflow engine** for agent orchestration — typed DAG
steps with journaled execution, durable waits, and human-in-the-loop events.
No external binary, no sidecar runtime: durability lives in agentflare's own
SQLite (via `agentflare-db-kit`).

This is the Cloudflare/Vercel-Workflows-style spine beneath the coordination
layer (items / claims / handoffs).

## Design lineage

| Source | Adopted | License |
|---|---|---|
| SMG `wfaas` | typed DAG engine, retry/backoff, `StateStore` trait, event bus | Apache-2.0 |
| OpenFang | StepMode semantics, `{{input}}`/`{{var}}` templating, ErrorMode, JSON schema | MIT / Apache-2.0 |
| Restate | journal durability (CompletableEntry), durable timers/promises | BSL — design only, no code |
| DBOS | step outputs checkpointed with user state ⇒ exactly-once | MIT |
| Google Research Harness² | configuration identity checked before resume | Apache-2.0 — design adapted |

## Usage

```rust
use flare_workflow::{WorkflowDefinition, StepDefinition, WorkflowEngine, InMemoryStore};
use flare_workflow::executor::FunctionStep;

let wf = WorkflowDefinition::new("wf", "wf")
    .add_step(StepDefinition::new("a", "a", Arc::new(FunctionStep::new(|ctx| {
        ctx.output = "hello".into();
        Box::pin(async { Ok(StepResult::Success) })
    }))))
    .add_step(StepDefinition::new("b", "b", /* ... */).depends_on(&["a"]));

let engine = WorkflowEngine::<Ctx, InMemoryStore<Ctx>>::new();
engine.register_workflow(wf)?;
let run = engine.start_workflow(WorkflowId::new("wf"), ctx, "input".into()).await?;
engine.wait_for_completion(run, "wf", Duration::from_secs(300)).await?;
```

### Durable execution

- **Journal**: every terminal step result is appended (`StepRun` / `Sleep` /
  `WaitEvent`); a step with a completed entry is **never re-executed**.
- **Recovery**: `engine.recover()` resumes `Running` runs from the SQLite
  journal after a crash, skipping completed steps (exactly-once).
- **Durable waits**: `StepMode::Sleep { duration_secs }` (timer) and
  `StepMode::WaitEvent { name, timeout_secs }` (promise) survive restart;
  `engine.complete_event(run_id, name, result)` resolves them from anywhere
  (journaled pre-delivery closes the notify-before-wait race).
- **Retries**: per-step `RetryPolicy` (fixed/exponential/linear backoff +
  jitter), `is_retryable`, `RetryIndefinitely`, per-attempt timeout.

### JSON workflows (agent pipelines)

OpenFang-style JSON definitions route prompts to agents via a `SendMessage`
hook:

```rust
let json: JsonWorkflow = serde_json::from_str(WORKFLOW_JSON)?;
let wf = compile_workflow(&json, send_message)?;
engine.register_workflow(wf)?;
```

Step modes: `sequential` / `fan_out` / `collect` / `conditional` / `loop` /
`sleep` / `wait_event`; error modes `fail` / `skip` / `retry`; `{{input}}` and
`{{var}}` templating; per-step token accounting.

Each step also accepts `model`, `args` (extra CLI flags), `hard_cap_secs`, and
`idle_timeout_secs` — all optional, all forwarded to the `SendMessage` hook
via `StepInvocation` so a workflow author can tune agent/model/flags/timeouts
per step as plain JSON, with no crate rebuild.

JSON workflows persist their typed definition and absolute execution directory as
a resume identity in the existing workflow state. `resume_run`, `resume_workflow`
and boot recovery refuse mismatched identities before replay or agent calls.
Recovery skips incompatible runs and continues recovering compatible ones; saved
state and journals remain available for inspection. JSON formatting and key order
do not change identity. An implicit working directory is captured at compilation.

Legacy JSON runs have no identity and cannot be resumed by a newly compiled JSON
definition; start a new run. Rust-defined workflows retain their existing behavior
unless they opt in with `.with_resume_identity("my-workflow:v1")`. Change that
revision when the executor's replay contract changes. Neither mode fingerprints
repository contents, executables, inherited environment, agent defaults, or persona
file contents; callers must include external configuration in an explicit identity
when it affects compatibility. No additional completion markers or store are used.

A project can commit its own workflows to `<repo_root>/.agentflare/workflows/
<name>.json` (JSONC — comments and trailing commas allowed) and run them by
name instead of pasting the full definition: `agentflare workflow run <name>`
or `mcp__flare__workflow(action="run", workflow_name="<name>")`;
`action="list_definitions"` / `agentflare workflow list-definitions` lists
what's available. See `agentflare::workflow::resolve_named_definition`.

## Store backends

- `InMemoryStore<D>` — default, tests/dev.
- `SqliteStore<D>` — durable, on `agentflare-db-kit` (WAL, migrations);
  tables `workflow_runs` (authoritative JSON state), `journal` (append-only),
  `step_state`, `run_vars` (queryable projections).

## Execution evidence reports

Use `agentflare workflow report <run-id>` (or the workflow MCP tool with
`action="report", run_id="..."`) for a JSON evidence report. It reads a consistent
snapshot of existing workflow state and journal, then joins explicitly recorded
provider session IDs to the existing insights index. It does not launch agents,
run checks, publish artifacts, or modify either database. `--db-path` works as it
does for `workflow status`; the MCP database remains fixed in production.

Reports preserve journal attempt failures, observed verification commands and
their known exit outcomes, time-scoped conversation/tool/file activity, recorded
PR links and agent claims. Replaced provider session IDs are retained for new
work-item runs. Missing/ambiguous sessions, unrecorded timestamps and unknown
outcomes remain explicit; unrelated worktree sessions are excluded. Text uses the
existing handoff redactor and 4,000-character field cap, with a truncation count.
Use source session/tool IDs to inspect the indexed originals.

Session costs may be estimates covering an entire resumed session; they are not
reported as total run spending. Acceptance stays `unknown`: successful execution
does not prove every required check passed against the final source. File activity
records tool requests, not an immutable Git/artifact diff. Legacy runs may lack
session history and failed-call usage. Claude/Codex importers now retain returned
tool output; refresh previously indexed files with `agentflare insights sync
--force` (read-only for source logs). Existing `insights show` and artifact export
tools remain available for deeper inspection and publication.

## Shared workflow budgets

Workflows can opt into one durable allowance shared by agent dispatches:

```json
"budget": {
  "max_cost_usd": 5.0,
  "reserve_per_call_usd": 0.5,
  "max_invocations": 12,
  "max_elapsed_secs": 900
}
```

The dollar fields must appear together; invocation and elapsed limits can be used
independently. Native work-item runs accept the same object in item metadata as
`workflow_budget`. Rust definitions can use `with_budget`; per-run typed data can
provide `WorkflowData::budget_limits`. Existing definitions without a budget keep
their current behavior. A policy is captured at run creation and cannot change on
resume; changing it requires a new run.
Budgeted runs also carry a versioned resume identity, so older executors cannot
silently recover them without budget enforcement.
For budgeted work items, automatic job retries cannot replace failed/cancelled or
missing runs with a fresh allowance. Explicitly clear the item's `workflow_run_id`
when authorizing a new run; ordinary resume keeps the existing ledger.

Before each send, including fan-out branches, retries, loop roles and stale-session
fallbacks, reserve an invocation and up to `reserve_per_call_usd` from the remaining
allowance. Reservations and reported usage share the existing workflow SQLite
database in a separate transactional ledger, so context replay cannot refill it.
Known unused cost is released after reconciliation; missing, failed or interrupted
usage retains its reservation. Counts are dispatch reservations, not proof that a
provider started. Original step/agent IDs and logical attempts remain recorded.
Claude resumed-session totals include earlier calls, so those invocations retain
their reservations rather than treating the cumulative estimate as per-call usage.
Zero-cost error results are also treated as unknown. Cost fields are client-side
estimates; see [Claude's cost tracking documentation](https://code.claude.com/docs/en/agent-sdk/cost-tracking).

Dollar-limited headless launches currently require Claude Code's existing
`--max-budget-usd` support. The allocated cap cannot override a stricter caller
cap; unsupported providers and positional argument delimiters are rejected.
Provider-reported overruns block subsequent sends. Invocation/time limits work
without a dollar limit. Custom Rust send hooks must honor `StepInvocation::allocation`
and record available cost through its usage receipt; the engine cannot enforce a
third-party provider's billing behavior. Command steps and tools are not separately
priced or counted as provider sends.
Optional fire-and-forget judge shadow calls are skipped for budgeted work items
because that API has no enforceable per-call allowance.

The elapsed limit is wall-clock time from run creation, including waits and pauses.
It clamps executor/subprocess timeouts and cancels the run when exhausted; resumed
runs keep the original deadline. Existing subprocess cleanup applies. A dropped
future keeps its reservation, including when a blocking launch completes later.

`workflow status` and `workflow report` expose limits, invocation reservations,
reported cost, held cost, unknown usage and overruns. These are budget records,
not a verified bill. The report remains read-only. No paid agent is launched to
inspect a budget.

## License

Apache-2.0. Restate ideas are adopted as design only (BSL 1.1); no Restate
code is copied.
