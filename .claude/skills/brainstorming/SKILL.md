---
name: brainstorming
description: Use before any creative or ambiguous request — a new feature, a "can we..." feasibility question, a vague bug report with no clear fix yet — to classify how much process it needs and block implementation until that process is done and approved. Skip once the ask is already concrete and scoped; requirements-specification picks up from there. Skip for a one-line, obviously-correct fix where there is nothing to classify.
---

# Brainstorming

Classify a request before touching it, then gate implementation on that
classification. `requirements-specification` turns an already-concrete ask
into testable requirements; this skill runs *before* that — while the ask
is still "build me X" or "can we..." and scope isn't pinned down yet.

## The gate

<HARD-GATE>
Do not write code, scaffold a project, or invoke an implementation skill
until the request is classified, its path's design step is done, and — for
Bounded and Architectural — the user has approved it. This holds for every
path below; only the size of the design artifact changes, never whether
approval is required.
</HARD-GATE>

The gate is one-way: if a task turns out bigger than its classification
mid-work, stop and reclassify upward. Never reclassify downward to skip a
step you've already started.

## Classify first

State the classification out loud before asking anything else — "this
reads as Bounded, so I'll sketch the design in chat rather than write a
spec" — so the user can correct it immediately.

- **Spike** — a feasibility question ("can we...", "is X possible...",
  "quick and dirty is fine"). The output is an answer, not code to keep.
  State the question and the 2-3 sentence probe plan, get a nod, then
  investigate as cheaply as correctness allows. No design doc. Report back
  as a recommendation; label anything you built as throwaway.
- **Bounded** — a well-scoped change to a flow that already exists in this
  repo: a new flag, a small endpoint, a one-file fix. "I understand this
  kind of app" is not enough — bounded means the code you'd change is
  already there to read. Ask the clarifying questions that matter, present
  a short design in chat (a few sentences to a short paragraph: approach,
  files touched, how you'll verify it), then stop for approval. No spec
  file.
- **Architectural** — a new subsystem, a new project, or a change that
  restructures how components fit together or alters an interface other
  code depends on. Ask clarifying questions, propose 2-3 approaches with
  trade-offs and a recommendation, then stop for approval. Once approved,
  hand off to `system-architecture-design` for the structural decision
  and/or `requirements-specification` for the spec — no implementation
  plan exists until that handoff's output is itself approved.

When torn between two paths, take the heavier one — reaching for the
lighter label to skip a step is itself the signal that the step is needed.

## Worked examples

- *"Can we get sub-100ms p95 on this endpoint with the current DB?"* —
  feasibility question, no artifact wanted → **Spike**. Probe it, report
  numbers and a recommendation.
- *"The retry logic in `worker.rs` doesn't back off, it just spins."* — a
  fix to a flow that's already in the repo, one file → **Bounded**. Confirm
  the intended backoff behavior, present the fix in chat, get a yes, then
  implement.
- *"We need a plugin marketplace: submission, review queue, billing,
  install."* — several new subsystems with no existing flow to anchor to →
  **Architectural**. Propose approaches, get approval, then hand off to
  `system-architecture-design` / `requirements-specification` before any
  code.

## Anti-pattern: "too simple to need approval"

A todo list, a single-function helper, a config toggle — the design may be
one sentence, but present it and wait for a yes. The artifact scales down
with simplicity; the approval gate does not. Presenting the design and
starting implementation in the same message skips the gate — it isn't a
gate if nothing can veto it.

## Red flags

| Thought | Reality |
|---|---|
| "It's obviously bounded, I'll start while they read the design" | The gate is the approval, not the design's length. Stop until you hear yes. |
| "I'll call it bounded to skip the spec" | Reaching for the lighter label to avoid work is the doubt itself — take the heavier path. |
| "The spike worked, I'll just keep the code" | A spike's output is an answer. Keeping the code is a new request — reclassify it. |
| "It grew mid-task but I'm almost done" | Hidden complexity upgrades the path immediately, not after you finish. |
| "They approved the spike, so the follow-up is pre-approved" | Every new task gets its own classification and its own approval. |

## Process by path

**Spike:** state the question + probe plan (2-3 sentences) → get a nod →
investigate as cheaply as correctness allows → report findings as a
recommendation, throwaway code labeled as such.

**Bounded:** check the existing flow you're changing → ask only the
clarifying questions that matter, one at a time → present the short design
in chat (approach, files touched, how you'll verify) → stop for explicit
approval → implement via the normal development workflow (tests still
apply; no plan document needed).

**Architectural:** check project context (recent commits, related docs) →
ask clarifying questions on purpose/constraints/success criteria → propose
2-3 approaches with trade-offs and a recommendation → hand off to
`system-architecture-design` for the structural decision and
`requirements-specification` for the testable spec → only once that spec is
approved does an implementation plan get created (PM mode, or direct
implementation for a solo session).

## After the design

Once the path's design step is approved:

- **Spike** terminates in a reported recommendation — there is no handoff.
- **Bounded** proceeds directly to implementation; no other skill is
  needed.
- **Architectural** hands off to `system-architecture-design` when the
  structural decision itself still needs to be made, then in every case to
  `requirements-specification` (spec still needs pinning down) or PM mode
  (spec is done, work needs to be created and dispatched) — never to any
  skill outside this project's own set.

Persist a durable decision or constraint that came out of this pass with
`mcp__flare__memory` so a later session doesn't have to re-derive it.
