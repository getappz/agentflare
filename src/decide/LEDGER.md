# Decision ledger

`shadow.jsonl` is the canonical decision envelope for every enabled Jev site.
It also holds later `record_type: outcome` events and dropped-write counts.
`dataset.jsonl` remains the opt-in input supplement; `decision_id` joins it to
the envelope. No third store or remote telemetry is introduced. Old rows without
an ID, model, or usage still load; their missing metadata remains unknown.

New consumers call `tracking::ask_with(site, baseline, applied, state, questions,
ask)` and retain the returned `Tracked.decision_id` alongside their own work.
The result is unchanged, including failures. Disabled calls create no record or
writer. Consumers with a semantic decision made from multiple answers (router,
skill ranking, judge) use `tracking::finish` and supply their actual comparison
label and whether they applied it. Batch billing is apportioned once across sites.
Batch `elapsed_ms` is the full shared request duration at each participating
site, suitable for per-site latency percentiles. It is not additive across sites:
report and bench consumers must not sum those durations into a cross-site total.
Question IDs must be program-generated identifiers, never prompt text.

Generic comparison labels are JSON objects mapping question IDs to choice keys,
numeric scores, or booleans (noul >= 0.5). Confidence and probabilities stay in
the typed answer rather than affecting agreement. Existing sites retain their
labels (router target, skill ordering, judge action). Use that same domain for
outcomes; `native-default`, `unjudged`, and ping's `none` explicitly denote the
absence of a baseline model selection, review decision, or answer.
Those three site-specific placeholders do not enter agreement or disagreement
metrics. `compared` is the agreement denominator; `answered` still counts all
answers. Outcomes still calibrate Jev without a baseline, while
`baseline_labeled` counts only outcomes with an available baseline.

Append ground truth later with `tracking::outcome_add`, or:

```text
agentflare decide outcome add --decision-id <id> --kind human_override --answer <correct-label>
agentflare decide report --json
agentflare decide report --site router
agentflare decide clear
```

Outcome kinds are `human_override`, `pr_review_result`, `item_final_state`, and
`incident_confirmed`. The last appended outcome is authoritative; any human
override counts once per decision even if superseded. Calibration uses only
answered decisions with an outcome; it never substitutes baseline agreement for
ground truth. Reports expose per-site, per-model, and UTC-day cohorts, failures,
disagreements, overrides, usage, latency, storage, and drops for the bench harness.
`decisions` includes breaker-open rows so every decision stays accounted for.
`breaker_open` and the status histogram show those short circuits separately;
they do not enter failure, agreement, override, or latency denominators. Failure
rate is `failed / (decisions - breaker_open)` and is unknown with no attempted calls.

Questions/instructions and input features are written only when
`AGENTFLARE_DECIDE_CAPTURE=1`, using the existing redaction and size bounds.
The normal envelope contains input hash/length, typed question kinds, answers,
and metadata. It contains no state or instruction text. Reports include both
retained segments. `decide clear` deletes decisions, outcomes, drops, and capture;
`decide dataset clear` retains its existing capture-only scope.

Normal writes use a bounded background thread: 256 queued records plus at most
256 in its current batch, each limited to 64 KiB when encoded. File I/O never
holds the queue bookkeeping lock. Batches reuse one nonblocking cross-process
file lock and retain two-segment rotation (5 MiB ledger, 10 MiB capture per
segment). Appends repair an interrupted trailing JSON fragment and preserve
complete historical rows without a final newline.

Normal return calls `decide::shutdown`; decision-producing direct exits use
`decide::exit`. Both give the writer a 50 ms grace period. When work remains or
drops need saving, a one-shot recovery process receives a bounded 30 KiB
environment payload. The caller performs no filesystem I/O during shutdown and
does not wait for recovery startup or disk completion; process creation and
payload preparation are additional to the 50 ms grace period. Recovery is not
started on the normal recording path. Its marker prevents writer re-entry.

Recovery replays pending rows that fit its payload, deduplicating decision IDs
under the file lock. It persists counts and IDs for all pending writes, including
those too large to replay. A failed replay becomes a counted drop; ledger
accounting is retried independently for up to five seconds, with a watchdog.
Reports count only identified writes still missing from the retained decision
and capture segments, so a late or recovered row is not also counted as dropped.
Historical dropped records without ID lists retain their original count.
Startup failures and queue backpressure are counted even without a live writer.

Consumers needing a read-after-write barrier can call `decide::flush`; this
explicit operation can perform synchronous drop-accounting I/O. Recovery after
exit is eventually visible to other processes. Failed recovery spawning, forced
termination, host restrictions on child lifetimes, or a ledger that remains
unwritable can still lose data or accounting. This is a best-effort local ledger.

Every configured request retains `requested_model` on the calling thread until
its consumer records the decision. `reported_model` is the provider's optional
response model; `source_model` uses that when present, otherwise the requested
model, including timeout/402/breaker/fail-open paths. Configuration failures
before a model is selected remain unknown. Billing is never invented on failures.

Benchmarks cover warm recording (ID/hash, sanitization and enqueue), cold writer
construction plus first enqueue, and first recording in fresh processes. The
first two assert p95 < 2 ms. Fresh-process envelope creation includes first-use
hashing and redaction and is reported separately rather than hidden by warm-up.
Storage asserts < 1 MB/1k without captured inputs; rotation bounds growth with
capture enabled too.

Measured on Windows x86_64, 2026-10-29, with unoptimized tests (root debuginfo
disabled to fit local disk), using
`cargo test -p agentflare --bin agentflare --config profile.test.package.agentflare.debug=0 decide:: -- --nocapture`:

| Measurement | p50 | p95 |
| --- | ---: | ---: |
| Warm recording helper | — | 0.504 ms |
| Cold writer construction + enqueue | 0.205 ms | 0.361 ms |
| Fresh-process envelope creation + record | 24.857 ms | 50.093 ms |
| Fresh-process shutdown | 3.594 ms | 15.198 ms |

Storage was **445,984 bytes per 1,000 decisions**. All 113 focused tests passed,
including real-process exit with 8 decisions and 8 joined capture rows,
failed startup, in-flight file-lock delay, oversized recovery payloads,
independently failed capture recovery, and interrupted JSON tails.
