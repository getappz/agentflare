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

## License

Apache-2.0. Restate ideas are adopted as design only (BSL 1.1); no Restate
code is copied.
