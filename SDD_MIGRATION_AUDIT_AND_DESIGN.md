# SDD Skill Migration — Completeness Audit + Model-Tier & Agent-Router Design

Scope: design-spec (no code). Audit of `src/work_item_pipeline.rs`'s `sdd_loop`
against Anthropic's canonical **subagent-driven-development** skill, plus two
composable designs (model tier, agent-backend routing) concrete enough for a
follow-up implementation task.

Source of truth: `~/.claude/plugins/cache/claude-plugins-official/superpowers/6.3.0/`
(skills/subagent-driven-development/`SKILL.md`, `implementer-prompt.md`,
`task-reviewer-prompt.md`, `re-review-prompt.md`, `scripts/{sdd-workspace,task-brief,review-package}`).
6.3.0 is the newest cached version (6.2.0 also present) — audited against 6.3.0.

---

## Part A — Completeness audit

Legend: ✅ implemented · ◐ partial · ✗ missing.

| # | Skill requirement (SKILL.md) | Status | Evidence (file:line) | Risk if unaddressed |
|---|---|---|---|---|
| 1 | **Fix loop, 5 rounds; rounds 1-3 resume implementer, 4-5 fresh implementer on ≥1-tier-higher model** | ◐ | Cap exists: `MAX_FIX_ROUNDS=5` `work_item_pipeline.rs:201`, trip at `:343`. But all rounds dispatch identically: role branch `:251-280` keys only on `review_issues`/`last_report`, never on `fix_round`; always the same `agent_name` (`:258/:266/:274/:279`). `JudgeAction::Escalate` `:347-349` only bumps the counter — no agent/model change. Every dispatch is a fresh headless run (`real_agent_send_hook` `:545-575`), so "resume same implementer" is inherently the skill's *allowed fallback* (fresh + report/prompt state) — but the round 4-5 capability bump is absent. `Escalate` also bypasses the cap check, so a judge alternating `fix_round`/`escalate` can exceed 5. | A stuck task runs 5 identical rounds against the same model, then hard-fails the run. The skill's actual convergence lever (fresh eyes + ≥1 tier) never fires. |
| 2 | **Breaker (round 5, open findings): three-way — park (wrong/contestable) · park-as-real-but-deferred · rule-and-continue (load-bearing)** | ✗ | `JudgeAction::{ParkFinding,RuleAndContinue}` declared `:85-86` but the apply arm is a no-op `:372-374`; there is no third "real-but-deferred" variant. At cap the loop returns `StepResult::Failure` `:343-344` → `execute_loop` propagates Failure (`crates/flare-workflow/src/loops.rs:63-77`) → dependent `finalize` is blocked/skipped (engine.rs `deps_blocked`) → `run_or_resume` returns `Err` → `execute_work_impl` posts a generic failure + releases the claim (`src/cli/work.rs:850-861`). No adjudication, no `Task N: parked — … — Ruling: …` lines, no `Task N: complete (K parked)`. | The breaker — the skill's way of *ending a loop productively* — is replaced by a hard failure that re-queues the item. Contestable/non-load-bearing findings get no rulings and are never surfaced (no final review either, see #7). `run_or_resume` seeds a fresh run with an empty ledger (`:722-725`), so even the in-run ledger is lost on re-dispatch. |
| 3 | **Task reviewer returns BOTH spec-compliance ✅/❌ AND code-quality approved/not** | ✗ | `build_task_reviewer_prompt` `:890-895` returns a single aggregate `REVIEW_APPROVED` / `REVIEW_ISSUES:` verdict. `parse_judge_decision` `:168-190` requires only `action/rationale/ledger_line/task_model_tier`. No implementer self-review is demanded either (`build_implementer_prompt` `:874-886` is a 3-line status contract, no report file). | The "both verdicts required" gate is unenforced; spec gaps and quality regressions are conflated into one pass/fail, and a reviewer can pass on one dimension while the other is weak. |
| 4 | **Severity classification: Critical/Important → loop; Minor → deferred to ledger, never loop** | ✗ | Findings are an opaque string in `review_issues` (`:293-295`); nothing parses Critical/Important/Minor. Every finding funnels into the loop (`:251-269`). No `Task N: minor (deferred): <one-liner>` path, no deferred list the final review can triage. | Minor findings burn full fix-round cycles (cost), and the skill's "roll-up nobody reads is a silent discard" safeguard is absent. |
| 5 | **Ledger discipline: exact line formats incl. `Task N: complete (commits a..b, review clean)`, `fix round R/5 (...)`, `Ruling:`; Finish collects all `Ruling:` lines** | ◐ | `WorkItemData.ledger: Vec<String>` exists `:67`; judge's free-text `ledger_line` pushed verbatim `:339`, schema is unconstrained (`:932`). No commit ranges anywhere — the loop records no BASE/HEAD (`real_agent_send_hook` `:545-575` discards git info). `finalize` `:419-508` never scans the ledger; the Finish-section "Rulings I made" report has no counterpart. | The ledger is the skill's post-compaction recovery map; lines without commit ranges and rulings make it unrecoverable, and the "no secret decisions" guarantee (every ruling reaches the human) is unenforceable. |
| 6 | **Pre-flight conflict scan before Task 1: cross-task table + self-contradiction check + rulings, written to ledger** | ✗ | `run_or_resume_with_sender` computes tasks (`:693`) and starts the run immediately (`:708`). `load_or_synthesize_tasks` `:819-835` / `parse_task_headings` `:837-869` only parse headings. No conflict scan, no table, no upfront rulings. | Cross-task conflicts surface mid-run as review findings or as a load-bearing breaker case with no prior ruling; "rule before you start" is skipped. |
| 7 | **Final whole-branch review: separate dispatch, most-capable model, pointed at deferred-minor/parked ledger lines, ONE fix wave + one scoped re-review** | ✗ | Pipeline has exactly two steps `sdd_loop`→`finalize` (`:598-600`). `build_finalize_step` `:419-508` = hold branch / `review_issues` human-gate / `item_done`+comment+notify. No final broad-review dispatch, no deferred/parked triage, no fix wave. | The "does the whole branch hang together" pass never happens; parked/deferred findings are never triaged; cross-task integration defects reach merge unreviewed. |
| 8 | **Four stop conditions: irreversible/destructive · security-sensitive · external side effect (merge/push/publish) · plan-broken-beyond-guessing, via `AGENTFLARE_HOLD:`** | ✗ | `hold_reason: Option<String>` `:49` + finalize on-hold branch `:430-447` exist, but **nothing sets it** — `AGENTFLARE_HOLD:` detection lived in the removed `coder` step (doc refs `:47`, `:401-402`); `sdd_loop`'s reply handling `:293-300` has no hold detection. The single `Option<String>` also doesn't classify the four classes. (External-side-effect protection exists only one layer down — git-shim classification, claim/worktree setup — not in-loop.) | An implementer hitting a real stop condition has no in-loop signal path; the pipeline plows on (would attempt a push/publish it should ask about first). |
| 9 | **Never dispatch multiple implementers in parallel** | ✅ | Structural: a single `StepMode::Loop` step (`:383-387`), one role `send` per iteration (`:282`, `:309`) — at most one agent in flight per run. (The engine's DAG executor could parallelize steps, but this pipeline has one loop step, serializing all role dispatches.) | none. |
| 10 | **Batch small same-shape work into one dispatch** | — | Not implemented. One implementer per parsed task heading (`:251-280`, per `current_task_index`). | **Note as deliberate non-goal.** An automated engine has no cheap "same shape" signal without a plan-text analysis pass; the skill's batching text targets human controllers' context cost, which this pipeline's file-based handoff already mitigates. Minor cost overhead only, not a correctness gap. |

### Cross-cutting observations (beyond the 10 rows)

- **Premise correction for B2:** `agent_registry::route()` **does** have a production
  caller today — `resolve_agent` (`src/cli/work.rs:320`, `route()` call `:353`),
  driven by `execute_work_impl` (`:765`, detection/config at `:750-764`). It is
  **item-level** (one backend for the whole item) via `TaskContext{labels,kind,size,assigned_agent}`
  (`router.rs:14-23`, precedence `router.rs:195-221`). The real B2 gap is
  **per-role routing inside the SDD loop**, where `build_work_item_pipeline_with_sender`
  passes one `agent_name` for implementer/reviewer *and* judge (`:595`).
- **`TaskModelTier` is dead in production** (premise confirmed): set only by
  `InsertTask` consuming the judge's `task_model_tier` (`:365`) and in the
  roundtrip test (`:1217`); `load_or_synthesize_tasks`/`parse_task_headings`
  always emit `model_tier: None` (`:833/:851/:865`). Never read by any prompt
  builder or the dispatch path.
- **Stale/dead finalize gate:** the `review_issues` human-gate (`:449-462`) is
  unreachable from the current DAG — a loop Failure *skips* `finalize`, it never
  runs it. Its message also cites `MAX_REVIEW_CYCLES=3` (`:456`), a different
  (old-pipeline) cap than the loop's `MAX_FIX_ROUNDS=5`.
- **Cap-failure restarts from scratch:** `run_or_resume` treats a `Failed` run as
  re-dispatchable and seeds the new run with `tasks` only, empty ledger (`:722-725`,
  `:745-746`); combined with #2, a task that trips the cap loses all its ledger
  history and re-runs from Task 0.

---

## Part B1 — Model tier: read the tier, produce a Claude model

### Plumbing facts

- Today one model per run: `WorkArgs.model` (`--model`) → `build_extra_args`
  (`work.rs:239-275`, emits `--model <name>` at `:270-273`) → `extra_args` →
  `real_agent_send_hook` (`:545-575`) → `run_headless(…, extra_args)` for **every**
  role dispatch, unchanged. There is no per-role/per-task model.
- `SddTask.model_tier: Option<TaskModelTier>` (`:25`) is never set by task
  sourcing and never read by dispatch.
- The judge's JSON schema already carries `task_model_tier` (`:932`), consumed
  only by `InsertTask` (`:365`).

### 1. Tier → model mapping

Add on `TaskModelTier` a `resolve_claude_model()` (or a config-overridable table)
with the skill's semantics — names below are defaults; the *resolution site* is
the design, exact names belong in config:

| Tier | SKILL.md rule | Default model |
|---|---|---|
| `Mechanical` | cheapest | `claude-sonnet-4-5` |
| `Integration` | standard (also the **floor** for implementers from prose, and for reviewers: "turn count beats token price") | `claude-sonnet-4-5` |
| `Architecture` | most capable; final whole-branch review is always this | `claude-opus-4-1` |

Escalation step for fix rounds 4-5: `Mechanical → Integration → Architecture`
(clamp at `Architecture`). Because agentflare is not Claude-only, resolve against
the *routed backend* (see B2): a per-agent model-name table (`claude-code` gets
Claude names; other backends fall back to the run's `--model` or no `--model`).

### 2. Who sets `SddTask.model_tier`

- **Deterministic classifier in task sourcing** (recommended primary): in
  `parse_task_headings` (`:837`) and `load_or_synthesize_tasks` (`:819`), classify
  each task body by keyword signal — mechanical-only ("1-2 files", "isolated
  function", "single constant/flag", complete code included) → `Mechanical`;
  integration ("multi-file", "coordinate", "refactor across", "debug") →
  `Integration`; architecture ("design", "architecture", "restructure",
  "cross-cutting", "interface/API shape") → `Architecture`. No signal → default
  `Integration` (skill's mid-tier floor for prose implementers). Testable, free,
  no dispatch.
- **Keep `InsertTask` judge-classified** (`:365`) as-is; a judge-inserted task
  already carries a tier.
- Optional secondary: a single cheap pre-pass judge dispatch to classify all
  tasks up front. Rejected for now — the deterministic classifier plus the
  review loop's fix-round re-review already covers the "wrong tier" case without
  a new dispatch surface. Note this in the impl ticket as a possible follow-up.

### 3. Where the tier is read → model parameter

- Add `model: Option<String>` to `flare_workflow::json::StepInvocation` (the
  clean lever: B1 and B2 both ride on the dispatch envelope, and the test seam
  `mock_send` `:1461-1480` can assert on it). `StepInvocation::simple` gains a
  model parameter or a builder.
- In `real_agent_send_hook` (`:545-575`), append `--model <name>` to `extra_args`
  (mirroring `build_extra_args` `work.rs:270-273`) when the invocation carries a
  model; otherwise leave the run's `--model` as the fallback.
- In `build_sdd_loop_step`, the role-selection block (`:251-280`) computes the
  model for the turn:

```
role_model(task, ctx.data.fix_round, role, run_base_model) -> Option<String>
  implementer, fresh / fix round ≤ 3   = task.model_tier→model (tier None → run_base_model)
  implementer, fix round ≥ 4           = escalate(task.model_tier)   // ≥1 tier above the stuck implementer
  task-reviewer                        = Integration floor            // scale to diff size/risk when a metric exists
  re-reviewer                          = Mechanical (small fix diffs) / Integration
  judge                                = Integration floor
  final whole-branch review (once #7)  = Architecture always
```

The **fix-round branch must key off `ctx.data.fix_round`** — today it doesn't
read it at all (only `review_issues`/`last_report`). `JudgeAction::Escalate`
(`:347-349`) becomes the round-4 trigger in the apply arm.

### 4. Interaction with B2

Route picks the backend; B1 picks the `--model` inside it. If a role's routed
agent has no per-agent model mapping, drop `--model` and let that backend's
default run — never pass a Claude-only model name to a non-Claude backend.

---

## Part B2 — Agent-backend routing per SDD role

### Premise correction

`route()` is already production-wired **at the item level** (`resolve_agent`,
`work.rs:320-360`, called `:765`). What is missing is **per-role routing inside
the SDD loop**: `build_work_item_pipeline_with_sender` passes one `agent_name`
for implementer, reviewer, and judge (`:595`).

### 1. Roles → `kind`

Use `TaskContext.kind` (`router.rs:16`, case-insensitive match `:54-63`):

| Role | `kind` | Suggested rule |
|---|---|---|
| implementer (fresh + fix rounds) | `implement` | `[opencode, claude-code]` preference |
| task-reviewer + re-reviewer | `review` | `claude-code` only |
| judge | `judge` | `claude-code` only (JSON contract; deterministic print mode) |

### 2. Where to route

- In `build_sdd_loop_step` (`:251-280`), after the role is selected, build a
  `TaskContext { labels, kind: Some(role_kind), size, repo, assigned_agent }`
  and call `agent_registry::route(&task, &router_config, &installed)`.
- **Load `[router]` config + installed set once per run**, not per iteration:
  reuse the exact pattern in `execute_work_impl` (`work.rs:750-764`). `load_router_config`
  (`work.rs:284-309`) is private to `cli::work` — promote to `pub(crate)` (or move
  into agent-registry) so the pipeline shares it. Thread the config/installed
  (or a small `RouteEnv` struct) into `build_work_item_pipeline_with_sender`
  (`:585`) → `build_sdd_loop_step` (`:227`).

### 3. Once per role per task, or once per task?

**Compute per role per task, cache per task.** Three `route()` calls (implement /
review / judge) with `kind` folded in — a task's implementation legitimately
lands on a different backend than its review, so one call with a composite kind
cannot express it. But the loop interleaves roles across iterations, so:

- On task entry (`AdvanceTask`/`SkipTask`, or when `current_task_index` changes),
  compute the three decisions and store them in durable `WorkItemData` (e.g.
  `role_agents: RoleAgents` or a `[Option<Agent>; 3]`). Each loop iteration reads
  the cached decision for its role — no re-route per iteration, and decisions
  survive crash-resume because `ctx.data` is the durable state (`loops.rs` persists
  context per iteration).

### 4. Item-level explicit override flows down and pins everything

- `item.assignee_agent` is already folded into `TaskContext.assigned_agent`
  (`work.rs:340-351`), and `route()` returns it unconditionally — "explicit
  wins" (`router.rs:195-202`). Feed the *resolved item-level agent* into each
  per-role `TaskContext` as `assigned_agent: Some(item_agent)`, so per-role rules
  never fire and the **entire task stays pinned to one backend** — matching both
  the router's precedence and the current single-agent behavior. Only when the
  item has no assignment / no `--agent` do per-role rules fire.
- `--agent` explicit flag: same path — it already short-circuits `resolve_agent`
  (`work.rs:327-331`) before the pipeline runs, so treat "the resolved agent" as
  the override and pin it.

### 5. Fallback and failure behavior

- `route()` returns `None` when no rule/default is installed (`router.rs:223-224`).
  Degrade to the item-level agent (today's behavior) — never hard-fail routing.
- The judge's backend must support the JSON reply contract (`parse_judge_decision`
  `:168-190`); if routing puts the judge on a backend without a headless print
  mode, `run_headless` already errors clearly (`agent_launch.rs:377-381`) — surface
  that as a config error, not a silent fallback.

---

## Follow-up implementation scope (items this document enables)

1. Fix-round escalation: branch on `ctx.data.fix_round`; round ≥4 dispatch with
   escalated tier model; make `Escalate` respect the cap; remove the no-op
   `ParkFinding`/`RuleAndContinue` ambiguity (implement the three-way or drop them).
2. Dual-verdict reviewer output + severity classification (Critical/Important →
   loop, Minor → `Task N: minor (deferred)` ledger lines).
3. Commit-range ledger lines (`Task N: complete (commits a..b, …)`): record
   BASE/HEAD in the loop, or drop commit ranges explicitly and document why.
4. Pre-flight conflict scan step (before `sdd_loop`; writes table + rulings to
   ledger) and final whole-branch review step (after `sdd_loop`, before
   `finalize`, Architecture model, deferred/parked triage, one fix wave + one
   scoped re-review).
5. `AGENTFLARE_HOLD:` detection in the role-reply path (restore the removed
   `coder`-step behavior) with the four stop classes surfaced in the hold reason.
6. B1: `StepInvocation.model`, tier classifier in task sourcing, per-role model
   in the dispatch block.
7. B2: expose `load_router_config`, `RouteEnv` threading, per-role `route()`
   with `assigned_agent` pinning, cached `role_agents` in `WorkItemData`.