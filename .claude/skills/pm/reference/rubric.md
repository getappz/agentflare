# Scoring rubric (LLM-applied; hardens to Rust later)

## RICE = (Reach × Impact × Confidence) ÷ Effort

Map each factor to a fixed 1–5 from readable signals. Show the reason inline.
- Reach   — how many users/areas the item touches (item text). 1 niche … 5 broad.
- Impact  — value if shipped. Bump from `priority` field and labels like
  `customer`, `revenue`, `priority:high|urgent`. 1 trivial … 5 critical.
- Confidence — how well-specified the item is (has a clear description/acceptance).
  1 vague … 5 crisp.
- Effort  — size. From `groom`'s `size` field (parsed server-side from
  `metadata.size`, set via `item(update)` with `metadata={"size":"S"|"M"|"L"}`).
  1 = large/expensive … 5 = tiny. `groom` sets `unestimated=true` when
  `size` is absent — treat that as UNESTIMATED (see below), don't guess a
  size from description prose.

Print each score as: `RICE 9.6 — R4 I5 C3 / E? (UNESTIMATED)` with one-line why.

## ICE fallback

When items lack any effort/size signal, use ICE = Impact × Confidence × Ease
(1–5 each) and label the table "ICE (no effort estimates present)".

## Sizing an unestimated item (mutating — outside the read-only workflows)

`groom`/`plan` never call `item(update)` — sizing is a deliberate exception,
done only when a human directly asks you to size specific items, not as an
automatic step in any workflow.

When asked, don't guess uniformly:
- **Self-contained description** (states its own size, or is a trivially
  small single fix) — size directly from the text.
- **Judgment call** (the estimate depends on how much of this already exists,
  how tangled the current code is, or how much is genuinely new) — verify
  against the actual codebase first (`ctx_compose`/`ctx_search`/`ctx_read`)
  before committing a size via `item(update) metadata={"size":...}`. Trusting
  an item's own scope claims without checking is how a real L gets shipped as
  M — found live in this project (#100 claimed reusable ledgers that don't
  exist anywhere in the codebase; verifying caught it, the description alone
  would not have).

## Unestimated handling

Never fail. Score what you can, mark the missing factor `?`, and list all
UNESTIMATED items separately so the team can add `size:*` labels.

## Velocity (health)

Count items currently in `state_group="completed"` whose `updated_at` falls in
each trailing 7-day window, over N windows (default 4). This approximates
"completed per week" (updated_at proxy — state it).
