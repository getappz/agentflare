GitHub Coordination Bridge — SDD progress ledger
Plan: artifact uURrhLPHQcgXmeocGrM28 (leanstack-specs)
Local plan: C:/Users/shiva/AppData/Local/Temp/claude/C--Users-shiva-workspace-leanstack/d9d75a39-2c4e-45f1-873f-07a1db6738c6/scratchpad/2026-08-03-github-coordination-bridge-plan.md
Branch: feat/github-bridge (off origin/master)

Pre-flight: 3 findings resolved in plan (two-DB ledger bug, capacity source, re-verify cost).

Task 1: IN PROGRESS (base 732a0fd)
  - agent #1 deadlocked TWICE waiting on run_in_background cargo runs; abandoned.
  - tests written to disk uncommitted; step-2 failure confirmed via compiler diagnostics.
  - agent #2 dispatched to finish from disk state, background execution forbidden.
  - LESSON: every implementer dispatch MUST forbid run_in_background.

Task 1: COMPLETE (commits 732a0fd..6cd7582, spec PASS, quality APPROVED)
  - Important finding was report-methodology only; corrected in task-1-report.md.
  - Reviewer verified verbatim CI gate clean workspace-wide. No pre-existing break.
  - Plan Global Constraints FIXED: clippy needs -A unsafe_code -A clippy::pedantic.

Task 2: COMMITTED 1ca3f62, review pending
  - plan fixed: no [lib] target, use `cargo test --bin agentflare <filter>`
  - flaky dashboard SSE test confirmed pre-existing in BOTH task 1 and 2
Task 2: COMPLETE (6cd7582..1ca3f62, spec PASS, quality APPROVED, no Critical/Important)
  - reviewer probed parse with 8 adversarial inputs; fails closed correctly.
  - Minor/informational only: duplicate marker keys take last-wins (permissive, not a bug).
Task 3: COMMITTED ac0508d, review pending (10/10 claim tests verified by controller)
  NOTE: IDE diagnostics lag ~1 step behind; contradictions with agent reports have been
  stale-diagnostic artifacts in all 3 tasks so far. Verify by running, not by reading diagnostics.
Task 3: COMPLETE (1ca3f62..1568f09, spec PASS; 1 Critical + 2 Important found and FIXED)
  - Critical: self-cede permanently blacklisted owner -> issue unclaimable forever.
  - Important: done from never-claimant killed claimability (was plan-mandated; spec tightened).
  - Important: latest_ts_for i64::MIN overflow.
  - Plan rule text corrected so tasks 6-8 inherit the right semantics.
  - Fix agent crashed (ENOTFOUND) post-verification; controller ran gate + committed.
Task 4: COMMITTED 5fb9ef4 (978 tests), review dispatched in parallel with Task 5 impl (different files, no conflict)
Task 4: REVIEWED — spec PASS, quality APPROVED. 1 Important + 2 Minor, none blocking.
  - Important: max_claims has no floor/doc/test while sibling interval_secs got all three.
    DECISION: 0 is a LEGITIMATE "drain mode" (stop claiming new work, keep re-verifying
    and exporting existing). Document it + add a test. Deferred until Task 5 lands.
  - Minor: from_env instance_id fallback untested; whitespace-only INSTANCE_ID not trimmed.
  - LESSON (controller error): ran Task 4 review concurrently with Task 5 impl on the
    theory that different FILES = independent. FALSE in a single-crate Rust project —
    items.rs mid-edit broke the whole binary, so the reviewer could not run config tests
    and had to take the report on faith. Do NOT overlap review with implementation again;
    Rust compilation is crate-wide, not file-wide.
Task 4 + Task 5 test claims VERIFIED by controller once crate compiled: 33/33 bridge tests
  (marker 8, claim 14, config 5, items 6). Task 5 committed 1b70877.
  NOTE: a SECOND flaky test surfaced (nudge_pace concurrent-writer), distinct from the SSE one.
Task 5: COMPLETE (5fb9ef4..1b70877 + fixes acba740, spec PASS, quality APPROVED)
  - Task 4 + 5 review findings all fixed in one batched commit acba740.
  - Fix 3 APPLIED: state_id_for_group now delegates to backend state::first_in_group
    (verified at state.rs:139) instead of reimplementing a client-side scan.
  - Open observation (not actionable now): no unique index on
    items(project_id, external_source, external_id); list_by_project ignores archived_at.
    Inert today; matters if archival or a 2nd linkage writer lands.
NEXT: Task 6 (tick) base commit acba740.

PLAN DURABILITY: corrected plan copied to .superpowers/sdd/PLAN-corrected-v2.md
  (session scratchpad is session-scoped; artifact uURrhLPHQcgXmeocGrM28 still holds the
   ORIGINAL v1 with 5 known defects. Republish attempt failed on a server error mid-publish.
   If resuming in a new session, USE PLAN-corrected-v2.md, NOT the artifact.)
Task 6: COMMITTED 2038033, review found 2 CRITICAL (both reproduced), fix pass dispatched.
  C1: ceded item re-ceded EVERY tick forever -> duplicate GitHub comments indefinitely.
  C2: held_count counts cancelled items (cancelling never sets completed_at) ->
      every cede permanently shrinks effective max_claims until instance starves to 0.
  Root cause (shared): nothing distinguishes "actively held" from "locally cancelled".
  VERIFIED GOOD: reviewer deliberately broke the two-DB ledger test (swapped ctx.ledger->conn);
    it FAILED as designed, then reverted. That regression test has real teeth.
  imported-field removal reasoning CONFIRMED sound end-to-end by reviewer.
  DEFERRED to Task 7: is_soft excludes GitHubError::Transport, so a network blip returns a
    hard Err from run_once. Task 7 runner MUST treat a hard Err as log-and-continue, never
    as fatal, or a transient network error kills the poll loop.
Task 6: COMPLETE (acba740..be9d78f). Both CRITICALs fixed via shared helper items::is_active
  (checks state group, not completed_at). imported field removed. 10/10 tick tests, 996 workspace.
  NOTE: SSE flake now fails CONSISTENTLY (3/3 runs, two failure modes) — no longer intermittent.
  Still out of scope, but worth filing separately.
NEXT: Task 7 (daemon wiring) base be9d78f.
Task 7: COMMITTED 56f225b. Task 8: COMMITTED 51eebb6.
FINAL WHOLE-BRANCH REVIEW: *** DO NOT SHIP *** — 2 Critical, 5 Important, 6 Minor.
  ROOT CAUSE: feature has NO HEARTBEAT. Spec promised one; plan never implemented it.
  Only export_if_dirty emits markers, and only on hash change -> a steady-state instance
  watches its own marker go stale, cedes its own live work, cancels the item, and can
  never reclaim it. Pure wall-clock timer, no adversary needed.
  Task 8 spec verdict FAIL: tests no two instances at all, duplicates of claim.rs units,
  and the a^b assertion is TAUTOLOGICAL (resolve_holder returns Option -> at most one).
  All 5 pass while C1/C2/I1/I2/I3 are live.
FULL HANDOFF: .superpowers/sdd/HANDOFF.md  <-- START HERE next session

=== FIX PASS (2026-08-03, agentflare epic #410) — all 13 findings cleared ===
Commits 51eebb6..485a99a on feat/github-bridge. CI gate clean: fmt, clippy
(-D warnings -A unsafe_code -A clippy::pedantic), cargo test --workspace
= 1023 passed / 0 failed / 1 ignored (the ignored one is a pre-existing
manual benchmark). Bridge tests 53 -> 72.

  #412 C2 (2d72b9d): instance id persisted to ~/.agentflare/bridge-instance-id,
       created with create_new so racing processes converge on one. Done FIRST
       because C1's fix is worthless without it.
  #411 C1 (6c48a9f): heartbeat. DESIGN DECISION — the remote refresh EDITS the
       comment already carrying our claim marker (new issues::update_comment)
       rather than posting a new one every half-TTL, so the comment id
       resolve_holder orders by is unchanged and the issue does not collect a
       bookkeeping comment every 15 min. Ledger lease refreshed every tick we
       hold. Second half: a cede is no longer terminal — step 3 re-adopts a
       cancelled linked item (never a completed one, which still owes a `done`).
  #413 I1-I3 (8a24adc): headroom counts successes not attempts; step 2 guards
       on is_ceded (NOT !is_active); our own orphan claim is adopted.
       Found while fixing: step 3 would re-probe an issue ceded earlier in the
       SAME tick — now skipped until the next one.
  #414 I4-I5 (31edd39): latches written BEFORE the GitHub write and the write
       gated on them, with rollback if the remote call fails; startup failures
       all log; AGENTFLARE_BRIDGE_REPO overrides cwd-derived repo resolution
       (the daemon plists set no working directory); soft errors surfaced on
       TickReport and logged by the runner on CHANGE, not every tick.
  #415 (920a220): two_instance.rs rewritten — two backends, two ledgers, two
       owner ids, one mock GitHub with real mutable comment/label/closed state,
       all driving run_once. MockServer gained start_with(handler); the fixed
       queue now 500s on over-request instead of hanging the client.
       *** NEW BUG THIS TEST CAUGHT ***: tick::cede built its marker with
       hash: "", which renders as `hash=`, and Marker::parse fails closed on an
       empty value — so EVERY cede the bridge ever posted was unparseable and
       invisible to other instances. All 14 claim.rs unit tests missed it
       because their fixtures used a non-empty hash the code never wrote.
       render() now substitutes "-" at that single choke point.
  #416 M1-M6 (485a99a): stale allows removed (exposing dead
       issue_body_with_marker, deleted); claim marker rewritten with the real
       item id instead of item=pending; claimed: labels removed on cede and on
       done (new issues::remove_label); debug_assert out of the poll loop;
       startup work moved onto the spawned thread; bridge opens its own backend
       connection instead of holding the shared mutex across network I/O.

STILL OPEN: the manual verification checklist in PLAN-corrected-v2.md — needs a
human and a scratch GitHub repo. The one thing no test can confirm is that the
marker renders INVISIBLY in GitHub's web UI.
NOTE: the dashboard SSE test the earlier review called consistently-failing
passed in this session's full workspace run. Not investigated; not touched.
