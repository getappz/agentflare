# Design-spec: workflow observability (instance & step metrics)

Scope: the ask — "add observability similar to Cloudflare Workflows'
metrics-analytics" (`cloudflare-docs/.../workflows/observability/metrics-analytics.mdx`)
— for `flare-workflow`. Recommendation only; no code changed by this doc.

## What Cloudflare's feature actually is

Two access paths over the same underlying data: the Cloudflare dashboard
(charts, per-Workflow and account-wide) and the GraphQL Analytics API
(`workflowsMetricsAdaptiveGroups`-style dataset, filtered/grouped by workflow
name, status, time bucket). Metrics exposed: **instance counts by status**
(queued/running/errored/terminated/complete/paused), **CPU time** and
**wall-clock duration** per instance, **step-level** duration/CPU/outcome,
and error rates.

The important structural fact: this is a **read-only aggregate query layer
over data the execution engine already records for every run** — not a
bolt-on instrumentation layer. Cloudflare isn't adding new tracing to
Workflows to build this; they're exposing what the durable-execution engine
already durably writes (state transitions, timing, step outcomes) via
structured queries.

## Current state in `flare-workflow` (verified directly against source)

- `crates/flare-workflow` is a standalone library crate — checked the
  workspace `Cargo.toml` dependency graph and no other crate currently
  depends on it (only worktree copies of itself do). There is no existing
  CLI/API/MCP surface exposing workflow runs today; whatever this spec
  proposes ships as a library capability first.
- Per-run/per-step telemetry **is already recorded**, matching Cloudflare's
  "expose what's already there" framing:
  - `WorkflowState` (`types.rs:411-426`): `status`, `created_at`/`updated_at`,
    `step_states: HashMap<StepId, StepState>`.
  - `StepState` (`types.rs:198-211`): `status`, `attempt`,
    `started_at`/`completed_at`, `input_tokens`, `output_tokens`,
    `duration_ms` — this is already the Cloudflare "CPU time / duration /
    step outcome" shape, per step, per attempt.
  - `JournalEntry` (`types.rs:257-308`, append-only, `journal` table in
    `sqlite_store.rs`): typed entries (`StepRun`, `Sleep`, `WaitEvent`,
    `Rollback`, `LoopIteration`, `Output`). No timestamp column on the
    journal table itself (`run_id, seq, entry_type, payload` only) — it's
    ordered by `seq`, not wall-clock, so it's not a time-series source on
    its own. `StepState`'s own timestamps are the right source for
    duration-style metrics, not the journal.
- **No aggregate query capability exists on the store actually used in
  production.** `count_by_status`/`count` (`store.rs:82-93`) are **inherent
  methods on `InMemoryStore` only** — verified by reading `SqliteStore`'s
  full `StateStore` impl in `sqlite_store.rs`: it has no such methods. The
  `StateStore` trait itself declares no aggregate method. `list_all()`/
  `list_active()` return fully deserialized `WorkflowState` structs; today,
  "count by status" against SQLite means loading and deserializing every
  run's full `state_json` blob client-side, not a SQL `GROUP BY`.
- **Real schema gap for step-level metrics:** the SQLite `step_state`
  projection table (`sqlite_store.rs` migrations) has columns `run_id,
  step_id, status, attempt, last_error, started_at, completed_at` — it does
  **not** have `duration_ms`, `input_tokens`, or `output_tokens` as columns,
  even though those fields exist on the `StepState` struct and are written
  into the `workflow_runs.state_json` blob. So step-level duration/token
  aggregation can't be done in SQL today without a migration; it would
  require deserializing every run's JSON.
- No metrics/tracing/analytics/GraphQL/dashboard infrastructure exists
  anywhere else in agentflare to reuse — repo-wide search for
  `metrics|analytics|tracing|instrumentation|prometheus|otel|graphql|
  grafana` returned zero hits outside this investigation. This is new
  infra, not a wire-up of something that already exists elsewhere.

## Recommended shape

Cloudflare's feature reduces to "SQL-level aggregate queries over
already-durable execution data." The equivalent here:

1. **Migration**: add `duration_ms INTEGER`, `input_tokens INTEGER`,
   `output_tokens INTEGER` columns to `step_state` (new `M::up(...)` entry —
   the crate already uses `rusqlite_migration` incrementally for its 4
   existing migrations, no new dependency). `write_state`'s existing
   `step_state` UPSERT gets the 3 extra columns.
2. **New `StateStore` trait method**: `async fn workflow_metrics(&self,
   filter: MetricsFilter) -> WorkflowResult<WorkflowMetrics>` (`store.rs`).
   `MetricsFilter`: optional `workflow_id`, optional status, optional time
   range. `WorkflowMetrics`: counts-by-status, `avg`/`sum` of
   `duration_ms` and tokens, and a per-`step_id` breakdown (status counts +
   avg duration) — directly mirrors Cloudflare's instance-count /
   CPU-time / step-outcome shape.
3. **`SqliteStore` impl**: one indexed `GROUP BY status` query against
   `workflow_runs` for instance counts, one `GROUP BY step_id, status`
   query against `step_state` for step-level aggregates. Both tables
   already have the needed indexed columns after the migration — no
   full-row JSON deserialization needed for the aggregate path.
4. **`InMemoryStore` impl**: mirror with the same signature, iterating the
   in-memory map (trivial, same pattern as its existing `count_by_status`).
5. **`WorkflowEngine::metrics(filter)`**: thin passthrough to the store.
   This is where the spec deliberately stops — no dashboard, GraphQL API,
   or CLI command. No consumer crate wires `flare-workflow` in yet (see
   above), so building a query surface beyond the library boundary would
   be speculative UI ahead of an actual integration point.
6. **Test**: seed N runs with mixed statuses/step outcomes in a temp
   `SqliteStore`, assert `workflow_metrics()` aggregate counts match,
   alongside existing coverage in `tests/engine_test.rs`.

## Cost estimate

- Migration + `write_state` column additions: ~20 LOC.
- `MetricsFilter`/`WorkflowMetrics` types (new `metrics.rs` or appended to
  `types.rs`): ~30-40 LOC.
- Trait method + `SqliteStore` aggregate SQL + `InMemoryStore` mirror:
  ~70-90 LOC.
- `WorkflowEngine::metrics()` passthrough: ~10 LOC.
- Test: ~40-60 LOC.

Total: roughly 170-210 LOC plus one migration, no new external
dependencies. No schema migration risk beyond the usual additive
`ALTER TABLE ADD COLUMN` (existing rows get `NULL`, aggregates already
need to tolerate that via SQL `COALESCE`/Rust `Option`).

## Recommendation

Worth doing, scoped as above: it's a genuine SQL-level aggregate layer over
data the engine already durably records, which is what Cloudflare's feature
actually is once you look past the dashboard chrome. The one real gap to
close is schema-level (`step_state` missing duration/token columns), not
architectural — no new instrumentation, tracing, or execution-path changes
are needed. Recommend explicitly **not** building a dashboard, GraphQL API,
or CLI surface in this pass: `flare-workflow` has no consumer crate today,
and a query surface with no caller would be built ahead of need. Land the
library-level `workflow_metrics()` capability now; revisit an exposed query
surface (CLI subcommand, MCP tool, or HTTP endpoint) once a consumer
actually wires the engine in and needs one.