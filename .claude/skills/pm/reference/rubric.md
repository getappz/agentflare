# Scoring rubric (LLM-applied; hardens to Rust later)

## Framework selection

Default is RICE, with an automatic ICE fallback when an item has no `size`
(RICE and ICE are two sizes of the same tool, not a user choice). WSJF and
Value-Effort are alternative framing on the same signals — selected via an
explicit `framework` arg to `/pm:groom`/`/pm:plan` (`rice` default, `wsjf`,
`value-effort`), not auto-picked, because unlike ICE they still need a job-size
signal and so aren't a fallback for missing data. When selected, they use the
same UNESTIMATED handling as RICE (see below).

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

## WSJF = Cost of Delay ÷ Job Size

Cost of Delay = Business Value + Time Criticality + RR/OE, each mapped 1–5
from the same readable signals RICE already uses — no new data required.
- Business Value — same signal as RICE Impact: `priority` field and labels
  like `customer`, `revenue`, `priority:high|urgent`. 1 trivial … 5 critical.
- Time Criticality — urgency signal: `priority:urgent`/`urgent` labels, or
  description language naming a deadline. 1 no urgency … 5 time-boxed.
- RR/OE (Risk Reduction / Opportunity Enablement) — how much shipping this
  unblocks other work, from `groom`'s `depended_on_by_count`. 1 isolated …
  5 unlocks many.
- Job Size — same as RICE Effort: `groom`'s `size` field. 1 large/expensive
  … 5 tiny. Same UNESTIMATED handling as RICE.

Print each score as: `WSJF 2.7 — BV4 TC2 RR3 / JS3` with one-line why.

## Value-Effort (2×2 quadrant)

No arithmetic score — plot Value against Effort into a quadrant, with V−E as
a tiebreaker for ranking within a quadrant.
- Value — same signal as RICE Impact: `priority` field and labels like
  `customer`, `revenue`, `priority:high|urgent`. 1 low … 5 high.
- Effort — `groom`'s `size` field, natural direction (opposite of RICE's
  Effort scale): 1 tiny … 5 large. Same UNESTIMATED handling as RICE.

Quadrants: Quick Win (V≥4, E≤2) · Big Bet (V≥4, E≥3) · Fill-in (V≤3, E≤2) ·
Money Pit (V≤3, E≥3).

Print each score as: `Value-Effort: Quick Win — V4/E2` with one-line why.

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
UNESTIMATED items separately so the team can add `size:*` labels. Applies to
any framework keyed on `size` — RICE, WSJF's Job Size, Value-Effort's Effort.
RICE alone has the ICE fallback since Ease doesn't need `size` at all; WSJF
and Value-Effort just mark `?` and move on.

## Velocity (health)

Count items currently in `state_group="completed"` whose `updated_at` falls in
each trailing 7-day window, over N windows (default 4). This approximates
"completed per week" (updated_at proxy — state it).
