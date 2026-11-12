# Message Receipts Implementation Plan

> For agentic workers: use superpowers:executing-plans for inline execution with test-driven-development; perform a fresh final review.

**Goal:** Recover pipeline message custody across restart using durable receipts and receiver deduplication.

**Architecture:** Reserve in the existing message database before handoff. Persist receipt IDs with pipeline corrections in workflow data; reconcile the same receipt after restart. Preserve legacy consumer exclusion using the existing delivered_at boundary.

**Tech stack:** Existing Rust, rusqlite, serde and flare-workflow; mbx builds.

**Spec:** `docs/superpowers/specs/2026-10-25-message-receipts-design.md`.

## Global constraints

- Work item #775's human plan approval and claim were confirmed before product edits.
- Use the Flare item claim's worktree; preserve existing root changes.
- Keep work local. Do not use item done if it would push/open a PR.
- No new dependencies, live agents, messages or external services in tests.
- Preserve default CLI/hook/MCP behavior and old persisted data compatibility.
- Receiver custody and model processing remain separate; never blindly replay ambiguous output.
- Exact new schema/API names below are implementer choices, not claims about existing interfaces.

## Review focus

1. Crash between databases: same receipt must be reconciled, not blindly released.
2. Stale engine context after consumption: accepted IDs must survive write-back and owner adoption.
3. Legacy requeue/read/cleanup: none may reopen or erase an unresolved reserved message.
4. Concurrent waiter or wrong run: must not confirm custody for a different receipt owner.
5. Old schema and old WorkItemData: missing receipt fields must migrate/default safely.

## Task 1: Message store receipts and status

**Files:** `src/messages.rs`, optional `src/messages/receipts.rs`, `src/cli/message.rs`, focused tests beside these modules.

**Interfaces:** Internal receipt record contains immutable message ID, receipt ID, run ID, original recipient, status and timestamps. Provide transactionally reserving eligible messages, loading unresolved receipts for a run, confirming an exact receipt, and reading status. These are internal APIs; bind queries to the run/recipient and use parameters.

- [x] Write migration/reservation/reopen/concurrency tests before implementation. Assert atomic pickup and receipt publication, unchanged legacy pickup, and wrong-run confirmation rejection.
- [x] Run `mbx test -p agentflare --offline messages:: -- --test-threads=1`; capture the expected new-test failure before implementing.
- [x] Implement additive schema and transactionally reserve messages plus receipts. Reuse stored IDs; same receipt resumes, never invent a second delivery for an unresolved crossing.
- [x] Guard requeue/prune and any other mutation that can release/delete a reserved row. Preserve legacy behavior for rows without receipts.
- [x] Add a read-only `message receipts` status view using existing mailbox observation conventions. Verify parsing, empty result, recipient filtering, and unresolved visibility.
- [x] Run focused message/CLI tests; inspect full diff for any consumer filter bypass.

## Task 2: Pipeline durable custody and restart reconciliation

**Files:** `src/work_item_pipeline.rs`, `src/work_item_pipeline/agent_messages.rs`, `src/dashboard/server.rs`, `src/dashboard/dispatch_boot.rs`, WorkItemData constructors/serialization tests, adoption and recovery tests.

**Interfaces:** Consume Task 1 receipts. Add a serde-default accepted-receipt ID set to WorkItemData. Use one patch to insert new correction text and persist its ID; make acceptance idempotent and preserve IDs through stale writes/consumption.

- [x] Write file-backed restart tests for reservation-before-custody and custody-before-confirmation, plus stale-write and owner-adoption tests. Assert one persisted correction, one accepted ID and final received status.
- [x] Test a stale context that contains the accepted ID but lacks pending text: preserve restoration of an existing unconsumed outbox entry. Test differently composed retry batches and missing/evicted workflow state (unknown, no false confirmation or blind replay).
- [x] Run the new tests before implementation and record expected behavioral failure. Use actual temporary SQLite/workflow stores, with no live agent.
- [x] Replace pipeline take/requeue with reserve/reconcile/confirm. Unresolved receipts are retried for the same run even when no fresh undelivered messages exist.
- [x] Reserve/render per message ID. Use accepted IDs to prevent new duplicate outbox entries while preserving existing unconsumed-entry restoration; never confirm/replay unresolved custody when the receiving workflow evidence has disappeared.
- [x] Rehydrate durable receipt preservation before engine resume/adoption. Keep owner rebind and correction patch in one compatible registration, and prevent conflicting concurrent waiter patches.
- [x] Route dashboard initial/retry recovery through a pipeline wrapper that reconciles and registers the patch before recover_with may spawn an eligible run, including same-owner restarts without a waiter. Test that failure leaves that run undispatched and cannot finalize it.
- [x] Apply the same pre-drive reconciliation guard to the wait-loop's orphan takeover (`resume_run`), and test that it cannot bypass custody restoration.
- [x] Add a file-backed process-termination regression harness at the two handoff boundaries if unit reopen tests cannot exercise the real persisted integration. Keep barriers test-only.
- [x] Run message, hook, MCP-message, pipeline, workflow recovery/adoption and serialization tests. Confirm existing grace/marker/fanout/read behavior.
- [x] Run formatting, appropriate clippy/build checks and the project test command. Name any baseline failure explicitly; do not claim a full green suite from selected tests.
- [x] Perform final independent review, fix material findings with failing regressions, and record results/limits on #775. Leave the implementation locally reviewable; no push, merge or PR.

## Execution record

2026-10-25: Traced source and prepared plan. Flare claim returned blocked_by_plan.
Product edits and worktree provisioning have not started. The user's instruction
authorizes implementation, but the tool's human-only approval route must unlock
the pending plan before the item can be claimed.

Independent builder check identified boot recovery as an additional entry point:
dashboard/server.rs and dashboard/dispatch_boot.rs directly recover runs without
the waiter/adoption path. Both are now explicitly covered before implementation.
It also identified the distinction between ID deduplication and restoring pending
text, retry batch stability, and disappearance of workflow evidence. All three
are explicit design constraints and regression cases above.

Execution completed locally on 2026-10-26 after human approval and Flare claim.
The implementation uses an owned-lease pre-resume hook to close the
preparation/start race. Independent review found two important issues; both have
observed failing regressions followed by passing final tests. Final receipt
tests passed 16/16; full flare-workflow tests passed 150/150; all six root
integration targets passed 23/23; root library passed 8/8; formatting and clippy
passed. The root inventory union accounts for 2,840 passed and 10 ignored cases.
The original combined command timed out at one hour with 15 fixture failures;
all 15 passed in isolated reruns. Four require an external echo executable in
the test PATH; a representative supervisor order-dependent failure also
reproduces in the saved pre-pipeline build. No full uninterrupted green-suite
claim is made. Detailed evidence and limits: `.refs/item-775/IMPLEMENTATION.txt`
and `.refs/item-775/progress.md`. Changes remain staged, local and reviewable.
