# Durable message custody for the work-item pipeline

Flare work item: #775 (`VMAo7r1dZb_DkSdZQtbBT`).
Status: approved in Flare by the human; implementation and verification in progress.
Evidence: `.refs/relay-audit/DELIVERY-BRIEF.txt` and `delivery-probe/evidence.json`.

## Intent and scope

Make the pipeline's fallback message drain recoverable across a process restart.
Record whether its persisted workflow context accepted each message, separately
from whether a model processed it. Reuse `agent_messages`, `SqliteStore`,
`patch_run_data` and the existing pipeline outbox.

Existing CLI/hook/MCP take-once behavior stays the default. This first change does
not make stdout delivery reliable, replace coordination infrastructure, or claim
exactly-once provider calls. No push, PR, deployment, external messages or issues.

## Approaches considered

1. Retry old delivered/unread rows: rejected because the audit reproduced duplicate
   output after successful emission. A timer cannot resolve ambiguous delivery.
2. Move delivered_at after output: rejected because it removes the existing
   atomic exclusion among competing consumers without establishing receiver custody.
3. Durable pipeline reservation plus receiver deduplication: recommended. Both
   sides are under AgentFlare's control and already persist state. It permits
   restart reconciliation without inventing a new transport or a global retry policy.

The user authorized proceeding through implementation; choosing the pipeline as
the first receiver is the implementer's proposal. Flare subsequently applied its
independent human-only plan gate and refused the item claim.

## Contract

Within one message-database transaction, reserve eligible messages for a workflow
run and persist a receipt for each. Reservation stamps delivered_at to exclude
existing pickup paths, but the receipt starts `in_doubt`, not `received`.
The immutable message ID identifies the stored request; receipt identity also
binds the target workflow run and original recipient. Do not add a redundant
caller-chosen request ID or accept a receipt for another run/recipient.

The receiver persists a defaulted set of accepted receipt IDs in `WorkItemData`
together with `pending_corrections`. A patch inserts a correction only when its
receipt ID is new. The outbox's existing stale-write protection must preserve
accepted IDs on every patched context, even after the correction text is consumed.
Deduplication prevents creating a NEW outbox entry for an accepted ID; it must not
suppress restoration of an existing unconsumed entry into a stale context that
has the ID but lacks the text. Reserve and render per message using its ID, so
different retry batch composition cannot create partially duplicated text blocks.
Only successful persistence permits the sender receipt to become `received`.
Read/processed state remains distinct.

On restart, reload unresolved receipts for the same run before taking new mail.
If receiver data contains an ID, confirm it without reinserting the text. Otherwise
apply the same ID through the deduplicating patch, then confirm. Rebinding the run
owner must preserve receipts and restore the patch before the resumed engine can
write stale context, including unchanged-owner adoption. Boot recovery currently
calls recover_with directly and can start a run without a polling waiter: both
dashboard boot and retry entry points must use a pipeline recovery wrapper that
reconciles/registers custody preservation before allowing an eligible run to drive.
Use the same pre-drive guard for the wait-loop's orphan takeover as well as boot
and adoption; no resume_run site may bypass it. Failed reconciliation must leave
that run undispatched. Concurrent waiters must
not register conflicting patches.

Workflow state may be evicted after termination. If receiver evidence is missing,
do not infer either absence or success: retain a visibly unknown receipt and do
not replay or confirm it without receiver evidence.

An observed patch error leaves the receipt unresolved: `patch_run_data` registers
its closure before its immediate store update, so an error alone does not prove
the correction can never be applied later. Do not blindly requeue it.

Legacy requeue must not reopen a receipt-owned message. Existing recipient-scoped
mark_read retains its meaning, but cannot manufacture a pipeline custody receipt.
Pruning retains unresolved messages/receipts; resolved receipts follow message
retention. Expose receipt status in a read-only CLI view so unknown deliveries are
visible for reconciliation. An old run cannot acknowledge a reassigned receipt;
this initial version need not automatically reassign unresolved delivery.

## Scope of code changes

- `src/messages.rs` and a focused child module if needed: additive receipt schema,
  transactional reserve/reload/confirm helpers, requeue and retention guards.
- `src/work_item_pipeline/agent_messages.rs`: use reservations, persist receiver
  IDs, preserve outbox behavior, retry unresolved custody for this run.
- `src/work_item_pipeline.rs`: serde-default receiver IDs, fresh initialization,
  and restore preservation on run adoption/recovery before resume.
- `src/dashboard/server.rs` and `src/dashboard/dispatch_boot.rs`: route initial
  and retry recovery through the same pipeline reconciliation wrapper.
- `crates/flare-workflow/src/engine_lease.rs`: an owned-lease pre-resume hook
  keeps custody preparation ahead of execution without racing another driver.
- `src/cli/message.rs`: read-only receipt status and help text; existing watch,
  peek, target selection and output defaults keep their behavior.
- Existing WorkItemData constructor/serialization tests and message/pipeline tests.
- Inspect MCP inbox/piggyback, marker-aware hooks, pending-size/count helpers and
  rerouting; modify only where their behavior can bypass receipt ownership.

No dependencies or global settings are needed. Old persisted runs and message
databases must remain readable. Use mbx for Rust verification.

## Acceptance evidence

- Kill/reopen after reservation and before receiver persistence: same receipt is
  recovered, correction enters persisted run data once.
- Kill/reopen after receiver persistence and before confirmation: reconcile to
  received without adding another correction, including after text consumption.
- Concurrent legacy consumers cannot take reserved mail; stale/wrong-run receipt
  acknowledgment fails; read actions do not imply custody.
- Failed patch stays visible and retryable for its receiver without blind release.
- Stale engine write-back and owner adoption preserve accepted receipt IDs.
- An accepted ID with missing pending text does not suppress restoration of an
  existing unconsumed outbox entry; changed retry batching cannot duplicate text.
- Boot recovery and its retry path restore receipts before starting a run without
  a polling waiter; failed reconciliation cannot advance that run to finalize.
- Missing/evicted workflow state leaves unresolved receipts visible and unknown.
- Grace period, markers, recipient isolation, fanout, normal watch, manual requeue
  for legacy messages, upgrades and cleanup retain existing behavior.
- Tests use actual message/SQLite/workflow paths with temporary state; no live
  agents, models, real messages or external services.

## Limits

This establishes durable workflow custody, not proof of model consumption or
exactly-once external effects. Plain CLI watch retains the audited take-once crash
window. Unresolved receipts for an abandoned run remain explicit and require
reconciliation; silent expiry/reassignment is outside this first implementation.

Implementation notes: pending receipt texts are persisted separately from the
accepted ID set so stale context restoration survives restart. Preparation runs
under the existing executor lease with lease renewal; failure releases the lease
before any executor starts. Terminal repair metadata adoption remains supported
without replaying or confirming unknown messages.
