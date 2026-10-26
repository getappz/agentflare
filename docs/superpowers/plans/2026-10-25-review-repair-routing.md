# Review repair routing implementation plan

> **For agentic workers:** Use superpowers:executing-plans inline. Accounting investigation and final review are independent read-only agent tasks.

**Goal:** Prevent item #724's wrong-item repairs and prevent fully reported no-op repairs from exhausting the repair cap.

**Architecture:** Validate live PR identity before review-side effects using one shared predicate also used by discovery. Reuse GitHub's changed-file list to annotate foreign findings. Preserve gross retry counting and apply durable, idempotent no-op credits tied to the reporting dispatch.

**Tech Stack:** Rust, existing serde_json metadata, SQLite metadata merge helpers, GitHub mock server, mbx.

**Spec:** Agentflare item #724, acceptance criteria in its current description.

## Mapping investigation

Discovery selected `item_owning_branch` through `item_owns_branch_here`, which accepts a matching numeric task prefix when the branch exists locally; it ignores the slug and item title. PR #870's live body contains `Fixes #308` but no UUID or origin tag. That allows an unrelated workstation-local item #308 to be attached to the failover branch before discovery stamps it. The original database mutation was not recovered, so this identifies the reproducible collision path rather than proving which discovery tick performed it.

## Global constraints

- Work only in `.worktrees/task/724`; preserve the main checkout and other worktrees.
- Reuse current PR origin/identity tags, task-branch naming, changed-file API and atomic metadata merges.
- No new dependencies, database migrations, tool names or request fields.
- Preserve item #766's settled out-of-scope handling and conservative accounting for lost/unreported jobs.
- Open PRs with `agentflare pr`.

## Review focus

- A colliding numeric task prefix must not override a different task slug/title or explicit item identity.
- Strong identity permits legitimate title/branch renames; metadata pointing at a different PR does not.
- A failed identity/file lookup must not produce actionable findings.
- Renamed/deleted files must remain in scope; foreign files must receive explicit out-of-scope instructions.
- Mixed outcomes, duplicate reports, result corrections and stale dispatch owners must not over-credit attempts.

### Task 1: Reject mismatched review ownership

Files: `src/worktree/mod.rs`, `src/worktree/discovery.rs`, new `src/worktree/review_identity.rs`, `src/supervisor/review_bots.rs`, focused supervisor tests.

Interface: `pr_item_mismatch(item: &Item, pr: &PullRequest) -> Option<String>` shared by discovery and live review validation.

- [x] Add regressions for the historical same-number/different-task collision, explicit other PR/item identity, and legitimate stamped renames.
- [x] Run the regressions and observe the mismatch accepted before the fix.
- [x] Implement the shared predicate; gate mismatches with a durable needs-human reason and return unknown review state before replies or dispatch.
- [x] Verify mismatch state prevents dispatch through the existing merge/repair path.

### Task 2: Make review scope explicit

Files: `src/supervisor/review_findings.rs`, `src/supervisor/review_bots.rs`, focused tests.

Interface: annotate `BotFinding` with `outside_diff: bool`; use existing `PullRequestFile` paths and rename predecessors.

- [x] Add a mixed current/foreign/renamed-file regression and inspect the generated repair brief.
- [x] Observe the missing foreign-file instruction in the original renderer before implementation.
- [x] Fetch changed files for actionable live reviews; fail closed on lookup errors. Render foreign findings with instructions to report `out_of_scope` without editing them.
- [x] Preserve existing mandatory per-thread outcome instructions and escaping.

### Task 3: Credit fully reported no-op attempts

Files: new `src/supervisor/review_attempts.rs`, `src/mcp_server/item_review.rs`, with hooks in `src/supervisor.rs` and `src/supervisor/review_bots.rs`.

Interfaces: `begin_attempt(meta, job_id, findings)`, `record_outcome(meta, owner, thread_id, round, outcome)`, `charged_attempts(meta, gross)`.

- [x] Add result-recording regressions for complete no-op, partial, mixed-fixed, duplicate/corrected outcomes and a stale owner.
- [x] Observe missing credit against the current implementation before writing accounting logic.
- [x] Persist current dispatch identity/thread rounds, outcomes and one-time credit in existing metadata. Match the reporting claim owner to the dispatch job.
- [x] Keep marker/silent counts as gross attempts; subtract durable credits with saturation. Lost jobs and legacy attempts remain charged. Explicit parking records missing per-thread outcomes before settling the attempt.
- [x] Verify all-no-op batches do not consume the cap and lost runs remain bounded.

### Final validation

- [x] Run focused identity, discovery, review scope, review-result and repair-cap tests.
- [x] Run rustfmt, git diff checks and an independent review of the full patch.
- [x] Commit, push and open the stamped PR for item #724; report verification limits (PR #965).

## Review and compatibility notes

- Independent review found and verified fixes for stale retry briefs after scope changes, bare numeric branches for non-Latin task names, and explicit PR adoption retaining the source UUID.
- Scope fingerprints include a digest before truncation, covering long bot comments and large batches.
- Existing adoptions without the new source-identity record may need `agentflare pr adopt` again. Historical dispatches without an attempt snapshot remain charged; no speculative refunds.
- The expanded supervisor suite passed all 208 tests. The affected-module run passed 221 checks and exposed one discovery owner-seam regression, which was corrected; all 31 discovery/routing checks then passed on the final code.
- Rustfmt and `git diff --check` passed. The full workspace suite and live cross-instance adoption were not run.
- Follow-up verification: all 10 `item724` regression tests, `cargo fmt --check`, `git diff --check`, `cargo deny check`, and the exact CI workspace/all-targets/all-features clippy command passed. Removed the needless borrow that failed PR #965's clippy check; GitHub checks for #870 and #965 were inspected.

## Reviewer fix round

- P1: explicit holds and holds for unread corrections now record missing current-job/current-round findings as `skipped` with the hold reason, in the same metadata transaction as attempt accounting. Existing results are preserved; any reported fix keeps the attempt charged. Repeated finalization cannot duplicate a credit, and a superseded job cannot report for the new attempt through this path.
- P2: discovery flags a local identity collision with `agentflare:needs-human` and a PR comment containing the mapping reason. Repeated sweeps reuse the same reason; foreign-origin PRs remain untouched and the wrong item is never attached.
- The new parked-finalization regression and strengthened discovery regression failed before the fixes. All 11 `item724` tests passed afterward, including mixed fixed/parked outcomes, stale owners and finalization retries.
- Independent read-only review found no blockers in these changes. It set aside a pre-existing race where a late direct `review_result` can overwrite a newer result: a blanket owner rejection would also block supported manual reporting, so that needs a separate distinction between dispatched and manual reporters.
- Final verification passed: 11 item #724 regressions; 24 discovery, 9 finalization, 5 review-result and 1 brief-rendering tests; `cargo fmt --check`; `git diff --check`; `cargo deny check`; and `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -A unsafe_code -A clippy::pedantic`. All commands finished before commit. GitHub checks on #870 and #965 were inspected; the full workspace test suite was not run.

## Title-rename review round

- PR #965's new finding reproduces for unstamped PRs created by discovery or explicitly adopted: a later title change loses the only task-identity evidence. Persist validation only at creation, explicit adoption, or after discovery's shared mismatch check passes; legacy number-only records still need validation. Explicit conflicting tags and PR numbers keep precedence over stored validation.
- Updated the task branch with `origin/master`. Reproduced the CI compilation failure with `cargo nextest run --workspace --verbose`: the repair-cap regression lacked the new `head_sha` argument. Added `None` to that test call.
- Before the provenance fix, `cargo test --bin agentflare item724 -- --test-threads=1` passed the existing 11 regressions and failed exactly the 3 new title-rename regressions at the identity assertion. The fix records `pr.identity_validated` at the trusted mapping boundaries; legacy mappings are not silently grandfathered in.
- Independent read-only review of the full branch and marker follow-up found no actionable issues. Explicit PR-number, instance, item-tag and origin checks still precede stored validation; partial or fixed repairs remain charged.
- The green focused run passed all 14 item #724 regressions. `cargo fmt --check`, `git diff --check`, and `cargo deny check` also passed (dependency-policy warnings only).
- Verification environment: mbx's 45 GiB free-space floor repeatedly evicted its cache on this host (6–9 GiB free). The documented absolute-Cargo fallback exposed mixed 1.98.1/1.99 artifacts in the ambient shared target. Further workspace verification pins Rust 1.98.1 and uses this worktree's private `target`, with debug symbols disabled and two build jobs; no shared target was cleaned.
- The exact CI lint command passed on the private target: `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -A unsafe_code -A clippy::pedantic`.
- `cargo test --doc --workspace --verbose` passed. `gh pr checks` was inspected for both #870 and #965; #965's remote head still reports the compilation failure fixed by this round's missing-argument update.
- Full `cargo nextest run --workspace --verbose` first stopped on the concurrent-dispatch test's 60-second timeout (1,621 passes). That test passed alone in 25.657 seconds and in the four-worker full rerun in 31.329 seconds.
- The four-worker full rerun stopped after 2,206 passes on scheduler fixtures `t15_offline_manual_survives_and_executes` and `t37_repos_stay_isolated_and_concurrent_starts_share_def` (2,432 tests left unrun). Their shared `echo_wf` helper launches `echo` as an executable, absent from this Windows PATH. Adding the existing `C:/Program Files/Git/usr/bin` to the test-only PATH made both tests and the cancellation-time `t49b_ticker_runs_independently` failure pass (3/3). No scheduler source or global environment was changed; the full suite was not repeated after that environment correction.
