# Scoring rubric (LLM-applied; hardens to Rust later)

## RICE = (Reach × Impact × Confidence) ÷ Effort

Map each factor to a fixed 1–5 from readable signals. Show the reason inline.
- Reach   — how many users/areas the item touches (item text). 1 niche … 5 broad.
- Impact  — value if shipped. Bump from `priority` field and labels like
  `customer`, `revenue`, `priority:high|urgent`. 1 trivial … 5 critical.
- Confidence — how well-specified the item is (has a clear description/acceptance).
  1 vague … 5 crisp.
- Effort  — size. From a `size:S|M|L` label or an estimate in the body.
  1 = large/expensive … 5 = tiny. If unknown, mark UNESTIMATED (see below).

Print each score as: `RICE 9.6 — R4 I5 C3 / E? (UNESTIMATED)` with one-line why.

## ICE fallback

When items lack any effort/size signal, use ICE = Impact × Confidence × Ease
(1–5 each) and label the table "ICE (no effort estimates present)".

## Unestimated handling

Never fail. Score what you can, mark the missing factor `?`, and list all
UNESTIMATED items separately so the team can add `size:*` labels.

## Velocity (health)

Count items currently in `state_group="completed"` whose `updated_at` falls in
each trailing 7-day window, over N windows (default 4). This approximates
"completed per week" (updated_at proxy — state it).
