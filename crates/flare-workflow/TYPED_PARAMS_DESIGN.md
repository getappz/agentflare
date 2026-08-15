# Design-spec: typed/structured params for flare-workflow triggers

> Item #125. Written recommendation, no code change — the gap identified
> here is real and cheap to close, but nothing today depends on it. Recorded
> so the concrete shape and cost estimate exist for whoever picks it up.

Scope: how a run's *initial* payload is passed in and exposed to steps —
not the `WaitEvent`/`complete_event` durable-wait mechanism, which already
matches Cloudflare's `waitForEvent`/`sendEvent` closely (including
documented event-buffering semantics) and is out of scope here.

## The gap, precisely

Source doc: Cloudflare Workflows, "Events and parameters"
(`developers.cloudflare.com/workflows/build/events-and-parameters/`,
fetched verbatim via `rivalsearch.content_operations` retrieve).

Cloudflare gives every triggered instance a `WorkflowEvent<Params>`:

```ts
export type WorkflowEvent<Params = unknown> = {
  payload: Readonly<Params>;   // caller-supplied, typed via a TS generic
  timestamp: Date;             // when the instance was triggered
  instanceId: string;
  workflowName: string;
  schedule?: WorkflowCronSchedule;  // set when a cron binding created this run
};
```

`payload` is populated from any of three call sites — Worker binding
`env.MY_WORKFLOW.create({ params })`, `wrangler workflows trigger
<name> '<json>'`, or (mid-run) `step.waitForEvent` — and is required to be
JSON-serializable, but is otherwise an arbitrary structured object (`{
userEmail, createdTimestamp, metadata }}`-shaped, in their own example).

`flare-workflow`'s equivalent entrypoint:

```rust
// engine.rs:317
pub async fn start_workflow(&self, definition_id: WorkflowId, data: D, input: String)
    -> WorkflowResult<WorkflowRunId>
```

`data: D` is genuinely typed (`D: WorkflowData`), but it's supplied by the
*Rust caller that owns the step closures* — it's how the crate's own
consumers (e.g. an agentflare feature with bespoke `D`) carry typed state.
It is not reachable from the one boundary that accepts data from *outside*
the process: the MCP `workflow` tool's `run` action, whose JSON/OpenFang
pipeline path (`json.rs`) pins `D = PipelineData`, a zero-field unit
struct. The MCP boundary's `input` field is the only thing that actually
crosses that boundary, and it's a single opaque `String`.

Concretely, at the MCP entrypoint today:

- No structured/typed payload — only one string. A caller wanting several
  named fields (`userId`, `orderId`, `action`) has to pre-serialize JSON
  into `input` and re-parse it inside step logic by hand; there's no
  `{{params.userId}}`-style access.
- No auto-populated trigger metadata. `WorkflowContext<D>` (`types.rs:325`)
  carries `run_id` (a UUIDv7, so a creation timestamp is *recoverable* from
  it, but not surfaced as a field) and nothing else — no echoed workflow
  name, no "how was this run created" metadata analogous to `schedule`.

## Options considered

**A. Reuse `input` as a JSON-in-a-string convention, document it, ship no
code.** Zero cost, but doesn't actually close the gap — it just pushes
JSON parsing into every step's prompt/executor by hand, with no
`{{params.x}}` templating support and no schema/type signal at the
boundary. Rejected: this is the status quo dressed as a decision.

**B. Give `PipelineData` real fields (`struct PipelineData { params: Value
}`), matching Rust-caller ergonomics.** Doesn't help — the MCP boundary
compiles JSON workflows generically; there's no per-workflow Rust type to
special-case fields on, so `PipelineData` would still end up being "one
untyped `serde_json::Value` field," identical in shape to option C below
but wrapped in a struct for no benefit. Rejected as strictly more code for
the same expressiveness.

**C. Add a parallel `params: serde_json::Value` channel alongside the
existing `input: String` pipeline, plus auto-populated trigger metadata.**
Keeps `input`/`{{input}}` untouched (no breaking change to the existing
OpenFang string-pipeline model or its tests) and adds a second channel
purpose-built for structured data, mirroring Cloudflare's `payload` +
`timestamp`/`workflowName` split. Recommended — detailed below.

## Concrete shape and cost estimate (option C)

- `types.rs`: add two fields to `WorkflowContext<D>` —
  `pub params: serde_json::Value` (default `Value::Null` via
  `#[serde(default)]`) and `pub triggered_at: DateTime<Utc>`. Both are
  cheap, serializable, and require no new trait bounds (`D` already forces
  `Serialize`/`DeserializeOwned` on the context as a whole).
  `workflow_name` doesn't need its own context field — `WorkflowState`
  already carries `workflow_id`, and `WorkflowDefinition::name` is
  available wherever a run is dispatched; only add a field if a step
  executor genuinely can't reach it otherwise (it can, via the definition
  handle already threaded through `execute_workflow`).
- `engine.rs::start_workflow`: add a `params: serde_json::Value` parameter
  (existing Rust callers pass `Value::Null` — or, better, keep the current
  signature and add `start_workflow_with_params(..., params)` as the one
  new public entrypoint, so no existing caller/test needs to change).
  Journal it inside the **existing** `JournalEntry::Input { value: Vec<u8>
  }` entry rather than adding a new `JournalEntry` variant: wrap `{input,
  params}` in one small serde struct and serialize that as `value`. This
  is a schema-compatible change (`journal` table's `payload` column is
  already opaque `TEXT`/blob) — no SQLite migration, matching how the
  `LOOP_DURABILITY_DESIGN.md` cost estimate treated the same table.
- `variables.rs::expand_variables`: extend the `{{...}}` resolver to
  recognize a `params.` prefix and do a dotted-path lookup into the
  `serde_json::Value` (`params.userId`, `params.metadata.foo`), falling
  back to the literal `{{...}}` text unresolved (matching existing
  `{{var}}` miss behavior) when the path doesn't exist. This is the one
  piece of real logic in the change — a small recursive `Value` walk keyed
  on `.`-split segments, no external dependency needed (`serde_json::Value`
  already supports `.get(key)` and array-index-by-string-as-usize is not
  needed for the common object-payload case Cloudflare's docs show).
- `mcp__flare__workflow` tool (`run` action): add an optional `params`
  string field (JSON text, parsed with `serde_json::from_str`, empty/absent
  → `Value::Null`) alongside the existing `input` field. Existing callers
  that only pass `input` are unaffected.
- Tests: one round-trip test (`params` flows from `run` through
  `{{params.x}}` expansion into a step's prompt) plus one recovery test
  (crash after `Input` is journaled, confirm `params` survives replay) —
  natural home alongside the existing input/recovery coverage in
  `tests/recovery_test.rs` and `tests/examples_test.rs`.

Total: roughly 60-90 LOC plus two tests. No schema migration, no new
`JournalEntry` variant, no changes to the `WaitEvent`/`complete_event`
machinery, and the existing `{{input}}`/`{{var}}` pipeline is untouched —
`params` is strictly additive.

## What's explicitly out of scope

- **Per-payload TypeScript-style generics.** Cloudflare's `Params` type
  parameter is a compile-time-only convenience on top of the same runtime
  JSON object `flare-workflow` would carry as `serde_json::Value`; Rust
  callers who want a real static type already have `D: WorkflowData` for
  that. `params: Value` at the MCP boundary is deliberately dynamic,
  matching the boundary it crosses (untyped MCP tool input).
- **`schedule` / cron-trigger metadata.** Cloudflare surfaces this because
  Workflows is a serverless platform where a binding can itself be a cron
  trigger. `flare-workflow` is an embedded library invoked by its caller;
  "why was this run created" (cron, webhook, manual) is the caller's
  concern, not something the engine needs to model — if agentflare's own
  scheduler ever needs to tag runs this way, that's a caller-side
  `params.trigger_source` convention, not an engine feature.
  `StepDefinition::scheduled_at`/`delay` already cover per-*step*
  scheduling, which is a different concept.
- **Size limits / validation on `params`.** The source doc doesn't document
  an explicit payload size cap either (only that it must be
  JSON-serializable); no cap is proposed here beyond what SQLite/storage
  already imposes in practice.

## Recommendation

Ship option C: a parallel `params: serde_json::Value` channel on
`WorkflowContext`/`start_workflow`, journaled inside the existing `Input`
entry (no migration), with `{{params.x}}` dotted-path template support
added to `variables.rs`, and a new optional `params` field on the MCP
`workflow` tool's `run` action. This closes the actual gap — structured,
typed-shaped initial payloads reachable from outside the process — without
touching the existing `input`/`{{input}}` string pipeline, the
`WaitEvent`/`complete_event` mechanism (already a close match to
Cloudflare's model), or requiring any storage migration.
