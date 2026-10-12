# Adopted Jev research patterns

These are independently implemented patterns from the reference checkouts in
`~/refs`; no upstream commits were cherry-picked and no dependencies were added.

| Reference | Pinned revision | Adopted pattern |
| --- | --- | --- |
| [devagrawal09/jev-code](https://github.com/devagrawal09/jev-code/tree/85f39e71db0f615b8ce6161a6672a2bf955fa7fd) | `85f39e7` | Per-run request/input budgets and typed response validation |
| [devagrawal09/jev-review](https://github.com/devagrawal09/jev-review/tree/31f89602797fb7bea007f8a480bf368bf564954e) | `31f8960` | Screen a file, select cited diff evidence, then score severity |
| [browser-use/jev-ultrafast](https://github.com/browser-use/jev-ultrafast/tree/1231850a0bf1a0c0341fe408ef1668dbbfdfac46) | `1231850` | Observed element references and freshness checks before actions |
| [gargpratyush/jev-router](https://github.com/gargpratyush/jev-router/tree/38da6b84ea01241bfc41fbddc0928d0f40a703f0) | `38da6b8` | Explicit pins, native candidate pools and stable session choices |
| [0xNatoshi/jev-codex-router](https://github.com/0xNatoshi/jev-codex-router/tree/8701ef788aa8cb0948f299538747fb01029d32b8) | `8701ef7` | Compact bounded decision state separate from full executor context |
| [Twister915/typesafe-ai](https://github.com/Twister915/typesafe-ai/tree/d4455efb1d061ae6aac47b40c42ef390182201b1) | `d4455ef` | Disable HTTP redirects (adapted to existing ureq 2 client) |
| [gilljon/typesafe-ai-rs](https://github.com/gilljon/typesafe-ai-rs/tree/06f52208c22f63326226446926859174c538e296) | `06f5220` | Explicit response read errors and credential-safe error display |

Additional inspected Rust leads: `AbdelStark/typesafe-rs` (`8e8b7a2`) and
`AbdelStark/s1-rs` (`b916897`). Their SDK/derive frameworks were unnecessary for
the existing typed client. The routing proxy `xinyao27/jevonian` (`3bb7816`, AGPL-3.0)
was inspected; no code was copied. The implementations here independently adapt
patterns; the existing transport remains dependency-compatible.

## Agent-aware model selection

Fresh agentflare-managed headless sessions (including SDD roles) can select a
configured native model for `claude-code`, `codex`, `cursor`, or `opencode`.
Enable `AGENTFLARE_ROUTER=jev` and `AGENTFLARE_JEV=1`, then add candidates to
`~/.agentflare/config.toml`:

```toml
[[model_routing]]
agent = "codex"
model = "YOUR_AVAILABLE_CODEX_MODEL_ID"
effort = "low"
description = "Lower-cost model suitable for mechanical coding edits"
roles = ["implementer", "task"]

[[model_routing]]
agent = "codex"
model = "YOUR_AVAILABLE_STRONG_MODEL_ID"
effort = "high"
description = "Stronger model for complex changes and evidence-based reviews"
roles = ["implementer", "reviewer", "judge", "analyst", "task"]
exhausted = false
```

Use model IDs supported by that installed CLI/account; OpenCode IDs must be
`provider/model`. Roles are optional (empty means all roles). Mark unavailable
or quota-exhausted models `exhausted=true`; this does not poll provider quotas.
Optional `effort` is validated against low/medium/high/xhigh/max, then passed using
Codex `-c model_reasoning_effort=...`, Claude `--effort`, or OpenCode `--variant`.
Only configure efforts supported by that model/provider; Cursor effort is encoded
in its native model ID. Explicit effort arguments also bypass automatic routing.
Roles do not impose a difficulty floor: simple reviews can select cheaper models
and low effort. Jev sees eligible descriptions, the host/role, original prompt
byte count, truncation status, and at most 2000 redacted task characters. It does
not inspect the native session's hidden context. It can select only supplied
model/effort pairs, with confidence at least 0.80,
one request, 40000 input bytes and a 1500 ms timeout. Empty/oversized pools,
invalid responses, low confidence and transport errors retain native defaults.

Explicit model arguments and native resume/continue flags bypass automatic
selection, preserving the model for the session's tool chain. This applies to
agentflare-managed launches; it does not hot-switch an already running IDE or
desktop chat. Existing prompt-hook nudges remain advisory. Model descriptions
and availability are operator supplied, so reproduce quality/cost on your tasks.
MCP `get_routing_suggestion` accepts an optional `agent` to return a configured
native `model` choice; callers decide when to apply it. Omitting `agent` preserves
the existing advisory nudge response.

## Rust transport safeguards

The shared client follows no redirects, reads at most 512000 response bytes,
reports truncated/invalid bodies, and displays generic HTTP errors rather than
server snippets that may echo credentials or source text. It makes no automatic
retries; failed attempts retain their budget reservation.

## Advisory diff screening

```sh
agentflare review scan
agentflare review scan --base origin/main --head HEAD --max-requests 12
agentflare review scan --max-input-bytes 100000
```

The default diff includes tracked staged and unstaged changes relative to HEAD.
With a head, the comparison is `base...head`; untracked files are excluded.
MCP: `review` with `action="scan"`, optional `base`, `head`, `max_requests`,
and `max_input_bytes`. Results are JSON and do not enter the review ledger.

The existing `AGENTFLARE_JEV=1` opt-in and configured provider credentials are
required. Selected diff text is sent to that provider after existing credential
pattern redaction. Redaction is heuristic; inspect the diff before opting in.
Environment/credential/secret paths and private-key files are excluded.

The run permits at most 48 requests, 512000 cumulative serialized state/question
bytes, and 64000 bytes per request. Flags can lower these limits. Failed requests
retain their reservation. These are input limits, not a dollar-cost guarantee.
Provider-reported input tokens appear separately in the report.

The scan handles at most 24 files, 32 hunks per file, 48000 patch bytes per file,
and a 2000000-byte total diff. Unsupported or excluded files are `unjudged`.
Screen probabilities below 0.70 receive no follow-up. Follow-ups require at least
0.55 location/severity confidence and produce at most eight cited signals.
Missing confidence, provider failures, and exhausted limits remain `unjudged`;
an empty signals list is not a correctness or security guarantee.

Shared response validation rejects missing/extra answers, mismatched types,
unknown choices, invalid probability/confidence values, and out-of-rubric scores.
Existing callers keep their deterministic fallback on Jev errors.

## Observed browser decisions

```sh
agentflare browser plan click @e1
agentflare browser act <returned-id>
agentflare browser plan fill @e2 --text "search terms"
```

MCP: `browser` with `action="plan"`, `operation="click"`, `target="@e1"`;
then `action="act"`, `decision="<returned-id>"`, using the same session.
Fill/type/select additionally use `text`. Planning supports click, hover, check,
uncheck, fill, type, and select on observed refs. It executes no page mutation.

A decision stores a hash of the URL and full accessibility snapshot, expires
after five minutes, and is scoped to its browser session. Execution atomically
consumes the id before checking freshness or performing the action; failed or
uncertain executions cannot be retried with that id. Plan a fresh decision after
navigation, a changed snapshot, expiry, or a consumed id.

Records are local under `~/.agentflare/browser-decisions` (or the configured
agentflare home), with entered text stored until consumption. Unix files use
mode 0600 and session directories 0700; Windows inherits the home directory ACL.
Unconsumed records remain on disk after expiry. No Jev call is needed for this
guarded execution path; a caller may choose the bounded operation itself.

Freshness is best-effort: the backend performs snapshot and action as separate
calls, so a page can change between them. The hash covers observable URL/tree
state, not every hidden DOM property. This is not an atomic browser transaction
or a permission boundary. Existing direct browser commands remain available.
