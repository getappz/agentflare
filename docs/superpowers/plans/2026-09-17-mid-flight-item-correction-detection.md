# Dispatched Agents Miss Mid-Flight Item Corrections — Investigation & Proposal

**Item:** #269. **Status:** investigation + proposed mechanism, no code changes in this doc.

## Problem

A correction posted as an item comment (or a handoff onto an already-claimed
item) after a dispatch has claimed the item and started work is never read by
that dispatch before it opens or merges its PR. Reproduced twice in one
session:

- **Item #266** (`zhFrBZbaNILetTcvI42kk`): dispatched at `1789626898`
  (comment: "supervisor — dispatched", job `vBFqDD2Yr7...`). A correction
  comment landed 118s later at `1789627016`: *"Correction to the approach in
  this item's original handoff: use `mise env --json <spec>`, not `mise which
  -t`... Please disregard the `mise which -t` framing in the original handoff
  content."* That job then failed (`orphaned by daemon restart`,
  `1789627240`), a second attempt failed (`No conversation found with session
  ID`, `1789628103`), and a **third** job (`lHY93TzO1OfuqV4DfgAv7`) completed
  at `1789628280` and opened PR #753 — using `mise which -t`, the exact
  approach the correction said to disregard. Three separate dispatches across
  ~23 minutes, and none of them read the comment.
- **Item #265** (`GjkuiHXwOQsg7yD8ev5mU`): a human/agent posted a second
  comment on the item (`1789569350`) proposing an alternative approach
  (`mise env --json`) *after* the PR (#751) implementing the first approach
  (`mise where` + PATH-skip) was already posted as complete
  (`1789568917`) — the follow-up was filed as a brand-new item (#266) instead,
  because there was no mechanism to feed it back into the still-open #265
  work.
- A human eventually noticed #266's miss and posted a **PR-comment** hold
  request directly on #753 (`syP55hJzzA9a9gyTOsI-m`, `1789628530`) — *"that
  channel is more likely to be seen before merge than another item
  comment"* — which is itself evidence the item-comment channel is known to
  be unreliable for stopping in-flight work.

## Why it happens — the pipeline has no read path for post-claim comments

Traced through `src/work_item_pipeline.rs`, `src/work_item_pipeline/task_sourcing.rs`,
`src/work_item_pipeline/prompt_builders.rs`, and `src/mcp_server/{comment,handoff,item}.rs`.

1. **Tasks are synthesized once, at dispatch, from a description snapshot.**
   `load_or_synthesize_tasks(item_description: &str, ...)`
   (`task_sourcing.rs:88`) parses (or falls back to synthesizing a single task
   from) the item's `description` field into `WorkItemData::tasks`. This runs
   once, when the run starts (`fresh_data()` in `run_or_resume_with_sender`,
   `work_item_pipeline.rs:1150`). Nothing re-reads the item afterward.

2. **Every role prompt is built only from that frozen task + prior reviewer
   findings.** `build_implementer_prompt`, `build_review_analyst_prompt`, etc.
   (`prompt_builders.rs:15` on) take `task: &SddTask` and an optional
   `fix_context: Option<&str>` — `fix_context` is always a **prior
   review-cycle's own findings** (`ctx.data.review_issues`), never an item
   comment. `build_sdd_loop_step` (`work_item_pipeline.rs:332`) re-runs once
   per task/fix-round turn and rebuilds the prompt from `ctx.data` each time —
   but `ctx.data` itself is never refreshed from the item's comment/handoff
   history, only from the workflow's own internal state.

3. **`comment` has a `list` action, but nothing in the pipeline calls it.**
   `src/mcp_server/comment.rs:149` (`action: "list"`) exists and is reachable
   by any agent that chooses to call the `comment` MCP tool itself — but no
   prompt tells the dispatched agent to check, and the automated
   coder/reviewer/judge/finalize steps never call it. `grep` for
   `comment_impl` in `work_item_pipeline.rs` shows every call site is a
   *write* (`action: "create"`, posting hold/findings/success comments), never
   a `list` read.

4. **`item_done` (`src/mcp_server/item.rs:1106`), which opens the PR
   (`pr_url` computed at `item.rs:1221`), is called from `finalize`
   (`work_item_pipeline.rs:794`) with no gate for unread comments.** `finalize`
   already branches on several stop conditions read from `ctx.data` — a hold
   signal, review-only mode, exceeding `MAX_REVIEW_CYCLES` — but none of those
   branches ever queries the item's live comment/handoff state. It goes
   straight from "loop says done" to `item_done`.

5. **`handoff` targeting an existing `item_id` doesn't touch the comment
   table either.** `src/mcp_server/handoff.rs:228` on: a handoff onto an
   already-claimed item updates `assignee_agent` and `metadata` (bumping
   `handoff_depth`) and clears redispatch-blocking labels, but the handoff's
   own `content`/`description`/`facts` fields aren't merged into the item's
   `description` or otherwise surfaced to a run that's already mid-flight —
   they land as separate persisted state (asset/queue entry) a *new* dispatch
   would pick up, not the live one.

6. **The only thing that already polls the item repeatedly during a run is
   the claim heartbeat**, in `run_or_resume_with_sender`'s outer loop
   (`work_item_pipeline.rs:1169-1192`): every 200ms it checks workflow status,
   and every 300s (`HEARTBEAT_INTERVAL`) it calls `mcp.item_heartbeat(...)`
   purely to keep the claim lease alive. It already has `mcp` and `item.id` in
   scope for the full lifetime of the run — and is the one place in the
   pipeline that isn't step-scoped, so it doesn't get respawned/lost across
   the crash-restarts that item #266 hit.

The net effect: the described gap isn't a bug in one function, it's a
structural absence — there is no point anywhere between claim and PR-open
where the pipeline re-reads what's been said about the item since it started.
A correction is exactly as invisible to the running agent as a comment posted
on a completely unrelated item.

## Proposed mechanism

Two complementary insertion points, because they catch different failure
windows:

### 1. Heartbeat-tick comment/handoff watch (catches long-running turns)

Extend the existing heartbeat loop (`work_item_pipeline.rs:1169-1192`) to also
poll `comment(action="list", item_id)` at the same cadence it already visits
the item for the heartbeat call (no new timer). Track a `last_seen_comment_at`
cursor (new field on `WorkItemData`, seeded to the run's start time on
`fresh_data()`). Any comment newer than the cursor and not authored by the
running agent itself gets appended to a new `WorkItemData::pending_corrections:
Vec<String>` and the cursor advances.

This is a pure background observer — it doesn't interrupt the in-flight agent
turn (which may be a single long non-streaming call, per the session-resume
memory's finding that cursor-agent's plain `-p` mode buffers until completion)
but it means the *next* checkpoint the workflow reaches already has the
correction in hand.

### 2. Two checkpoints that consume `pending_corrections`

- **Per-task-turn, in `build_sdd_loop_step`** (`work_item_pipeline.rs:332`,
  right before building `role_prompt`): if `ctx.data.pending_corrections` is
  non-empty, prepend it to the implementer/analyst prompt as a labeled
  "Note — posted after this task started" block and clear it. This is the
  cheap, common case: most items go through 2+ role turns (implement → review
  → maybe fix), so a correction posted mid-run has a real chance to land
  before the *next* turn even without reaching finalize.

- **In `finalize`, before the `item_done` call** (`work_item_pipeline.rs:794`,
  ahead of the squash-commits block at `:783`): if `pending_corrections` is
  still non-empty at this point — i.e. the loop reached its last task turn in
  the same tick the correction arrived, or the correction landed after the
  final task turn already ran — do **not** call `item_done` directly. Instead
  treat it the same way `finalize` already treats `hold_reason` (`:723`):
  release the claim, post an "on hold — unread correction, needs a fresh
  pass" comment quoting the correction, and stop short of opening the PR. This
  is a deliberate fail-safe, not a full course-correction: a stale-comment gate
  at the very last moment guarantees "never silently ignored," at the cost of
  needing a redispatch to actually act on it — cheaper to build and reason
  about than trying to make `finalize` itself re-enter the SDD loop.

This directly addresses item #266's failure mode: the correction (`1789627016`)
predates every one of the three dispatch attempts' actual work, so either
checkpoint — a heartbeat tick during the ~23-minute span, or the finalize gate
on the third attempt — would have caught it well before PR #753 opened.

### Why not simpler alternatives

- **Re-synthesize tasks from the live item description on every turn**
  (extending `load_or_synthesize_tasks` to run per-turn instead of once)
  doesn't help here: the correction in both reproductions was posted as a
  *comment*, not an edit to the item's `description` field. Re-reading the
  description changes nothing unless correction-posters are also required to
  edit the description, which isn't how the comment/handoff tools are
  actually used today (confirmed by both #265 and #266's own comment
  history).
- **Telling the agent in its prompt to call `comment(action="list")` itself**
  relies on the dispatched agent remembering to do it, every turn, without
  being told anything changed — the same class of instruction-following gap
  that produced item #170/#216's `detect_review_only`/`design-spec` bugs
  referenced elsewhere in this pipeline. A structural, code-driven check is
  more reliable than an added prompt instruction competing with the rest of
  the role prompt for attention.
- **A dedicated poller thread/process separate from the heartbeat** would
  duplicate the exact lifecycle problem the heartbeat already solves (surviving
  step-boundary respawns and crash-restarts within `run_or_resume_with_sender`)
  for no added benefit — piggybacking on the existing loop is strictly
  simpler.

## Open questions for whoever picks this up

- Whether `pending_corrections` should also flow into `judge_decision.rs`'s
  approval logic (e.g. treat a fresh correction as an automatic
  `review_issues` re-open even if the judge already approved) — this doc
  proposes the minimal "prepend to next prompt + gate finalize" version and
  leaves loop-reopening as a follow-up if the minimal version proves
  insufficient in practice.
- Whether a handoff onto an already-claimed item (`handoff.rs:228` branch)
  should itself be surfaced through the same `pending_corrections` channel —
  today it silently updates `assignee_agent`/`metadata` on the live item with
  no comment side-effect, so the heartbeat-tick comment poll proposed above
  won't see it. Either handoff-onto-claimed should also write a comment (small,
  reuses this mechanism for free), or the heartbeat poll needs a second,
  handoff-specific check.
- Cadence: reusing the 300s `HEARTBEAT_INTERVAL` is free but means up to 5
  minutes of latency on long single turns. If that's too slow in practice, it
  should get its own shorter timer rather than shortening the heartbeat itself
  (which exists specifically to stay "negligible DB load" against a 200ms
  status-poll loop, per its own doc comment).
