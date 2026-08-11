---
name: system-architecture-design
description: Use when a change is big enough to need a deliberate structural decision first — new service boundaries, a data model that will be expensive to change later, a technology choice, or anything crossing multiple components — and you want the trade-offs and rationale written down, not just the diagram. Skip for changes confined to one module/file with an obvious approach; skip for the low-level "how do I implement this function" question, which is algorithm-pseudocode-design's job instead.
---

# System Architecture Design

Produce architecture decisions that explain *why*, not just *what* —
diagrams and component lists go stale, but a documented trade-off analysis
still answers "why did we do it this way" a year later.

## When to use

- Designing service boundaries, a data model, or an API contract that other
  code will depend on and that's expensive to change once built.
- Evaluating a technology or pattern choice (microservices vs. monolith,
  sync vs. event-driven, SQL vs. NoSQL) where the wrong call is costly.
- Skip for changes scoped to a single module where the approach is already
  obvious — write the code, don't write an ADR for it.

## Key responsibilities

1. Design scalable, maintainable structure — not the most elaborate one
   that fits the requirements, the simplest one that does.
2. Document the decision with the rationale, not just the conclusion.
3. Produce diagrams that show component interactions and data flow.
4. Evaluate real trade-offs between options, including "do nothing" and
   "the boring option."
5. Account for operational concerns (deployment, monitoring, rollback) —
   architecture that's elegant to draw but painful to operate is a net
   loss.

## Decision framework

Answer these before committing to a design, not after:

- What quality attributes actually matter here (throughput, consistency,
  latency, operability) — and which ones don't, so you're not over-building
  for them?
- What are the real constraints and assumptions (team size, existing
  infrastructure, deadline)?
- What are the trade-offs of each option under consideration — not just
  the chosen one?
- How does this align with (or complicate) existing architecture?
- What are the risks, and what's the mitigation or rollback plan if the
  decision turns out wrong?

## Deliverables

1. **Architecture Decision Record (ADR)** — the decision, the alternatives
   considered, and why this one won. Short-form is fine; the rationale is
   the point, not the ceremony.
2. **Component interaction diagram** — who talks to whom, and how (sync
   call, event, shared store).
3. **Data flow diagram** — where data originates, transforms, and lands.
4. **Technology evaluation** (when applicable) — options compared against
   the actual requirements, not against feature checklists.

## Best practices

- Consider non-functional requirements explicitly (performance, security,
  scalability) — don't let them stay implicit until they cause an incident.
- Write the ADR even for decisions that feel obvious in the moment; the
  obviousness rarely survives six months.
- Prefer standard diagramming notation (C4, UML) over ad hoc boxes-and-
  arrows — it's faster for someone else to read.
- Design for the extensibility you actually expect, not every
  hypothetical future requirement — YAGNI applies to architecture too.
- Think about deployment and monitoring as part of the design, not as a
  follow-up task.

## Handoff

Once the structural decision is made, `requirements-specification` (if the
requirements themselves are still fuzzy) or direct implementation planning
via `superpowers:writing-plans` picks up from here. Use
`algorithm-pseudocode-design` for the lower-level "how does this one
function/algorithm work" question — that's a different altitude than a
structural decision.
