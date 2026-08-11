---
name: requirements-specification
description: Use when a feature request or bug report is vague enough that "what does done look like" isn't obvious yet — turns a fuzzy ask into testable functional/non-functional requirements, constraints, and Gherkin-style acceptance criteria before any design or code starts. Skip for small, already-unambiguous changes, and skip once a spec exists — use superpowers:writing-plans to turn an existing spec into an implementation plan instead of re-deriving requirements.
---

# Requirements Specification

Write requirements precisely enough that two different people implementing
from the same spec would build the same thing. A vague spec doesn't save
time — it just moves the ambiguity (and the rework) later.

## When to use

- The ask is underspecified: "add notifications," "make search better,"
  "support teams" — before any architecture or code, pin down what these
  actually mean.
- Multiple stakeholders (or a stakeholder and an implementer) could
  reasonably disagree about scope.
- Skip when the change is small and unambiguous (a one-line bug fix, a
  well-scoped refactor) — writing a formal spec for those is pure overhead.

## Process

### 1. Requirements gathering

Split every requirement into **functional** (what the system does) and
**non-functional** (how well it does it — latency, availability, security).
Each one needs an id, a priority, and criteria specific enough to fail a
"is this actually done?" test:

```
FR-001: System shall authenticate users via OAuth2
  priority: high
  acceptance:
    - Users can log in with Google/GitHub
    - Session persists for 24 hours
    - Refresh tokens auto-renew

NFR-001 (performance): API p95 latency <200ms
NFR-002 (security): All data encrypted in transit and at rest
```

"Fast" and "user-friendly" are not requirements — they're not testable.
Replace them with a number and a measurement method.

### 2. Constraint analysis

Separate constraints by kind, since each kind trades off differently:

- **Technical** — must use existing DB, must run on current infra, language/
  framework version floors.
- **Business** — deadline, budget, team size.
- **Regulatory** — compliance regimes (GDPR, SOC2), accessibility (WCAG).

### 3. Use cases

For anything with a nontrivial flow, write the actor, preconditions, the
numbered step-by-step flow, postconditions, and — critically — the
exceptions (what happens when a step fails). Most spec gaps live in the
exceptions nobody wrote down.

### 4. Acceptance criteria (Gherkin)

Given/When/Then scenarios make "done" checkable by someone who didn't write
the code:

```gherkin
Scenario: Failed login - wrong password
  Given I am on the login page
  When I enter a valid email and the wrong password
  Then I should see "Invalid credentials"
  And I should remain on the login page
  And the failed attempt should be logged
```

Write the failure scenarios, not just the happy path — that's usually where
the real design decisions hide.

## Validation checklist

Before treating a spec as done:

- [ ] Every requirement is testable (not "fast," not "intuitive")
- [ ] Acceptance criteria are unambiguous
- [ ] Edge cases and failure modes are documented, not just the happy path
- [ ] Non-functional requirements have a number and a measurement method
- [ ] Constraints (technical/business/regulatory) are captured, not assumed
- [ ] Dependencies on other in-flight work are identified

## Best practices

1. **Be specific** — ban words like "fast," "user-friendly," "robust"
   unless immediately followed by a number or test.
2. **Make it testable** — every requirement should have a clear pass/fail.
3. **Consider edge cases explicitly** — walk the failure paths, not just
   the success path.
4. **Think end-to-end** — trace the full user journey, not one screen.
5. **Version the spec** — when requirements change mid-implementation,
   that's a signal worth recording, not silently overwriting.

## After the spec

Once requirements and acceptance criteria are pinned down, hand off to
`superpowers:writing-plans` for the implementation plan, or
`superpowers:brainstorming` if the *approach* (not just the requirements)
is still open. Persist durable facts or decisions from this pass with
`mcp__flare__memory` (action="remember") so later work doesn't re-derive
them.

Remember: a good spec prevents misunderstandings and rework. Time spent
here is cheaper than time spent re-implementing after a wrong guess.
