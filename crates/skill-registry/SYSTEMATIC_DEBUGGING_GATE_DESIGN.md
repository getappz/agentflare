# Design-spec: systematic-debugging enforcement gate

> Item #203. Decision: GO (2026-08-29), from item #168's superpowers-adoption
> gap analysis — greenlit over the sibling brainstorming-gate candidate
> because it's more mechanically enforceable and higher leverage given how
> many backlog items are bugfix-shaped. Written recommendation with concrete
> shape, no code change — per this project's PM-mode design-spec convention
> (design-specs get authored in-session; only the resulting implementation
> gets dispatched). Next: dispatch implementation from this doc.

Scope: extend the completion gate (item #169, `src/hook_redirect.rs`,
`src/hook_completion_gate.rs`, `src/optimize/runtime.rs`, shipped in PR
#581) to also require evidence that root-cause investigation happened
(superpowers' `systematic-debugging` skill, Phase 1) before `item
done`/`check_merge` — but **only** for items whose `metadata.task_type` is
`"bugfix"`, not universally. Sibling item #182 (review-evidence gate) is
deliberately out of scope here — different evidence type, different
trigger condition, tracked separately.

## The gap

The verification-evidence gate (`completion_gate_reason`,
`hook_redirect.rs:239`) already blocks `item done`/`check_merge` unless
this session has fresh, passing `VerificationEvidence` — proof that tests
were *run*. It has no opinion on whether the fix being verified addressed a
confirmed root cause or was a guess that happens to pass today's test run.
`systematic-debugging`'s Iron Law (`NO FIX WITHOUT ROOT-CAUSE
INVESTIGATION FIRST`) has zero mechanical enforcement anywhere in this
codebase — it's a skill an agent can choose to follow or skip, and nothing
notices the difference.

## What stays identical to the shipped verification gate

This is explicitly a mirror, not a new mechanism:

- Same storage: a new optional field on the existing per-session
  `SessionRecord` (`optimize/runtime.rs`), persisted via the same
  `load_runtime`/`save_runtime` JSON file, pruned by the same
  `prune_stale_sessions`.
- Same capture point: the `PostToolUse` success hook
  (`hook_completion_gate.rs::post_tool_use`), which is already the *only*
  place verification evidence is ever recorded — this becomes the only
  place diagnosis evidence is recorded too.
- Same detection style: broad, cheap, lowercased substring markers over a
  Bash-family command, split on statement separators, with
  `strip_quoted_substrings` + `NON_EXECUTING_FIRST_WORDS` reused as-is so a
  command that only *mentions* a marker (a grep pattern, a comment) doesn't
  count — same false-positive-tolerant philosophy as
  `VERIFICATION_COMMAND_MARKERS`: an accidental match makes the gate
  slightly more permissive, never less safe.
- Same gate integration point: `completion_gate_reason` in
  `hook_redirect.rs`, called from `hook.rs::pre_tool_use` exactly where it
  is today — no new call site.

## What's different: detecting "investigation happened" is harder than "tests passed"

A test command's exit code is an unambiguous pass/fail signal. Root-cause
investigation has no equivalent single signal — it's Phases 1-3 of the
skill (read the error, reproduce, check recent changes, find a working
analog, form one falsifiable hypothesis), not one command. Options
considered for what to mechanically check:

**A. Scan comment/PR text for phrases like "root cause" or "hypothesis."**
Rejected. This is exactly the shape of bug that `detect_review_only` had
before #566/#591 — a free-text scan that's both fragile (misses
rephrasing) and trivially gameable (the skill's own "Red flags" section
names "I don't fully understand this but it might work" as the failure
mode this gate exists to catch; a phrase-scanner can't distinguish
performed investigation from typed ritual language claiming it).

**B. Require a failing-then-passing verification transition (Phase 4 step
1/3: write a failing test, confirm it fails, then fix and re-verify).**
Rejected as the sole signal — this is Phase 4 (implementation/verification),
which the *existing* gate already checks (a passing `VerificationEvidence`
is required regardless). Treating red-green as proof of root-cause work
would silently redefine "diagnosis evidence" as "you ran tests twice,"
which proves nothing about Phases 1-3 specifically.

**C. Detect investigative Bash-family commands characteristic of Phase 1
step 3 / Phase 2 step 1 — "check recent changes" (`git log`, `git blame`,
`git diff`, `git show` on the affected paths) and "locate the closest
working analog."** Recommended. This doesn't prove Phases 1-3 were done
*well* (no mechanical check can), but it's the same floor the existing
verification gate sets for Phase 4: "at least the mechanical footprint of
doing this exists," which is exactly the freshness/existence check
`completion_gate_reason`'s own doc comment describes ("nothing previously
stopped an agent from claiming done... without having actually run tests
*now*"). Same shape, same honest limitation, applied one phase earlier.

## Concrete shape

**1. `optimize/runtime.rs`** — additive, no schema migration (same
`#[serde(default)]` pattern the file already uses everywhere):

```rust
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct DiagnosisEvidence {
    pub command: String,
    pub ts: u64,
}
```

Add `pub last_diagnosis: Option<DiagnosisEvidence>` to `SessionRecord`
(mirrors `last_verification`).

```rust
const DIAGNOSIS_COMMAND_MARKERS: &[&str] = &[
    "git log", "git blame", "git diff", "git show", "git bisect",
];
pub fn is_diagnosis_command(command: &str) -> bool { /* identical shape to is_verification_command, same statement-splitting + quote-stripping + NON_EXECUTING_FIRST_WORDS reuse, different marker list */ }
```

Freshness: unlike `VerificationEvidence`, diagnosis evidence is **not**
invalidated by a later mutating tool call. Verification has to be
invalidated on edit because a passing test run stops proving anything once
the tree changes underneath it; "did you look at recent history/a working
analog before fixing" isn't undone by the edit that follows it. So
`has_fresh_diagnosis_evidence` only needs a same-session existence check
(reuse `VERIFICATION_FRESHNESS_SECS`'s window, or simply "recorded this
session at all" — deferred to implementation, not a load-bearing decision
here) — and `hook_completion_gate.rs`'s existing
`invalidate_verification` call on `MUTATING_TOOLS` must **not** also clear
`last_diagnosis`.

**2. `hook_completion_gate.rs::post_tool_use`** — after the existing
verification-recording block, add a parallel check: if
`is_diagnosis_command(command)`, record `DiagnosisEvidence { command:
command.clone(), ts: now }` into `record.last_diagnosis`. Both checks can
fire off the same parsed command (a session might run `git log` and
`cargo test` as separate calls, or — less commonly — a compound command
matching both marker sets in one call); no mutual exclusion needed.

**3. `hook_redirect.rs::completion_gate_reason`** — needs to know the
target item's `task_type` before deciding whether to require diagnosis
evidence, which the function doesn't have today (it only sees the raw
`tool_input`, i.e. the `id` field on a `done`/`check_merge` call — a UUID
or bare `sequence_id`, not yet resolved). This needs a backend DB lookup
that doesn't exist on this call path today. Precedent: `hook.rs`'s own
`session_start_message` (`hook.rs:124`) already opens the backend DB
synchronously and unconditionally via
`agentflare_backend::db::open_db`/`item::list_by_assignee_agent` — a hook
reading the backend DB directly is established practice here, not new
architecture. Concretely:

```rust
pub(crate) fn completion_gate_reason(
    tool_name: &str,
    tool_input: Option<&Value>,
    has_fresh_verification: bool,
    has_fresh_diagnosis: bool,   // new: only meaningful when the item is task_type=bugfix
) -> Option<String> {
    // ...existing verification check unchanged...
    // then: if the item's metadata.task_type == "bugfix" and !has_fresh_diagnosis,
    // return a message in the same style, naming git log/git diff/git blame as
    // what would satisfy it.
}
```

The `task_type` lookup itself (resolve `id` via
`agentflare_backend::item::resolve_id(conn, None, id_or_seq)` then
`item::get` and parse `metadata.task_type`) happens in `hook.rs::pre_tool_use`
right alongside the existing `load_runtime()`/session lookup, not inside
`completion_gate_reason` — keeps that function a pure decision core
(matches its existing testability: the doc comment on
`branch_guard_reason_for` calls out the same pattern, "no git process
spawned here, so it's unit-testable"). `completion_gate_reason` takes the
already-resolved `bool`, same as `has_fresh_verification` today.

A lookup failure (bad id, DB error, item not found) must fail **open**
(don't add a diagnosis requirement) — same fail-open posture the module
header already documents for IO-needing rules, and the existing
`has_fresh_passing_verification`/`item_action_succeeded` "unknown, not
failed" convention used throughout this file. This lookup is a single
indexed SQLite row read on the same machine (not network IO); no timeout
wrapper needed beyond what already exists for the file, consistent with
`session_start_message`'s precedent above rather than routing through
`redirect_decision`'s `decide_with_timeout`/`GATING_TIMEOUT` machinery
(that wrapper exists specifically for the PreToolUse *redirect* classifier
path, a separate call site).

**4. Tests** — mirror the existing suite's shape exactly (same file,
`hook_completion_gate.rs`'s `#[cfg(test)] mod tests`, and
`hook_redirect.rs`'s `completion_gate_*` tests):
- `is_diagnosis_command` marker/quote/non-executing-word cases, copy-adapted
  from `is_verification_command`'s existing tests.
- `completion_gate_reason` blocks `done` on a `task_type=bugfix` item with
  no diagnosis evidence even when verification evidence is fresh; allows it
  once both are fresh; ignores the diagnosis requirement entirely for a
  non-bugfix (or untagged) item.
- `post_tool_use` records `last_diagnosis` on a `git log`/`git diff` Bash
  call; confirms a later mutating-tool call does **not** clear it (the one
  behavioral difference from `last_verification`).

## What's explicitly out of scope

- **Detecting Phases 2-3 (pattern analysis, hypothesis) mechanically.**
  No tool-call signal maps cleanly to "read a working analog in full" or
  "stated one falsifiable hypothesis." Same limitation the skill itself
  flags as unenforceable outside a real code-review pass — a human/review
  concern (arguably folds into #182's review-evidence gate later), not
  this one.
- **`ctx_shell`/MCP-gateway-wrapped commands.** Both the existing
  verification gate and this new diagnosis gate only see a Bash-family
  tool's top-level `command`/`cmd`/`script` field
  (`hook_completion_gate.rs::parse_post_tool_use`). A command run via this
  project's own `ctx_shell` gateway
  (`mcp__flare__tool(action="execute", server="leanctx", tool="ctx_shell",
  args={command: "..."})`) nests the actual command under `args`, which
  neither parser unwraps — a pre-existing blind spot, not introduced or
  fixed here. Worth its own follow-up if it turns out to matter in
  practice (this project's own hook policy pushes agents toward
  `ctx_shell` over native `Bash`, which makes the gap more likely to bite
  than it looks).
- **Non-`bugfix` task types.** Deliberately scoped narrow per the item's
  own instruction — a `research`/`design-spec`/`implementation` item never
  requires diagnosis evidence, regardless of whether it happens to touch a
  bug.
- **Retroactive scan of history for "did investigation happen at some
  point earlier."** Same session-scoped, forward-looking-only model the
  verification gate already uses; not revisited here.

## Recommendation

Ship the `DiagnosisEvidence` gate as designed above, additive and
same-shape as the shipped verification-evidence gate: new marker set +
`SessionRecord` field in `optimize/runtime.rs`, capture in
`hook_completion_gate.rs::post_tool_use`, and a `task_type`-gated check in
`hook_redirect.rs::completion_gate_reason` fed by a synchronous backend-DB
lookup in `hook.rs::pre_tool_use` (precedented, fail-open on lookup
failure). Roughly 80-120 LOC plus tests, no schema migration, no new call
sites, no change to the existing verification gate's behavior for
non-bugfix items.
