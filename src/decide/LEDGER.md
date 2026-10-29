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
Question IDs must be program-generated identifiers, never prompt text.

Generic comparison labels are JSON objects mapping question IDs to choice keys,
numeric scores, or booleans (noul >= 0.5). Confidence and probabilities stay in
the typed answer rather than affecting agreement. Existing sites retain their
labels (router target, skill ordering, judge action). Use that same domain for
outcomes; `native-default`, `unjudged`, and ping's `none` explicitly denote the
absence of a baseline model selection, review decision, or answer.

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

Questions/instructions and input features are written only when
`AGENTFLARE_DECIDE_CAPTURE=1`, using the existing redaction and size bounds.
The normal envelope contains input hash/length, typed question kinds, answers,
and metadata. It contains no state or instruction text. Reports include both
retained segments. `decide clear` deletes decisions, outcomes, drops, and capture;
`decide dataset clear` retains its existing capture-only scope.

Hook writes use nonblocking, authenticated loopback IPC to a local helper
process, prepared before the backend request. A reader independent of disk owns
received frames and queues at most 256 for writing. Shutdown confirms receipt
with a bounded (20 ms maximum) IPC handshake, then closes IPC without waiting
for disk. Socket buffers, that queue, and one partial frame bound memory.
A separate control
stream reports sender drops even when the record stream is full; unfinished
record frames are counted by the helper at EOF. Barrier acknowledgements have
their own stream so a timed-out reader cannot reset pending record delivery.
The helper uses nonblocking cross-process file locks and the existing two-segment
rotation (5 MiB ledger, 10 MiB capture per segment; 64 KiB per record maximum).
Backpressure and write failures are counted and persisted in the ledger when
storage recovers. Consumers needing a read-after-write barrier can explicitly
call `decide::flush`; normal exit uses `decide::shutdown` instead. A crashed or
forcibly killed helper, failed IPC startup, or permanently unwritable storage
can still lose records: this is a best-effort local ledger, not a transaction log.

Every configured request retains `requested_model` on the calling thread until
its consumer records the decision. `reported_model` is the provider's optional
response model; `source_model` uses that when present, otherwise the requested
model, including timeout/402/breaker/fail-open paths. Configuration failures
before a model is selected remain unknown. Billing is never invented on failures.

`storage_tests::ledger_enqueue_p95_and_growth_per_thousand` measures the full
recording helper after startup (including ID/hash, sanitization, enqueue and
the helper's receipt acknowledgement) and bytes for
1,000 representative decisions. It asserts p95 < 2 ms and < 1 MB/1k without
captured inputs. Rotation bounds disk growth even when capture is enabled.
