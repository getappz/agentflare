---
name: systematic-debugging
description: Use when encountering any bug, test failure, or unexpected behavior, before proposing fixes — walks through root-cause investigation, pattern comparison, and a single tested hypothesis before any code changes. Skip when there's no failure to explain yet (greenfield feature work) — use algorithm-pseudocode-design or system-architecture-design instead.
---

# Systematic Debugging

## The Iron Law

```
NO FIX WITHOUT ROOT-CAUSE INVESTIGATION FIRST
```

A fix proposed before Phase 1 is complete is a guess, not a fix. Guesses that
happen to work are indistinguishable from guesses that don't until the bug
resurfaces somewhere else.

## When to use

Any technical issue: a failing test, a production bug, unexpected output, a
performance regression, a build failure, an integration that doesn't behave
as documented.

**Especially** when under time pressure, when "just one quick change" seems
obvious, or when a previous fix attempt didn't work — those are exactly the
conditions where guessing feels fastest and costs the most.

**Don't skip because the bug looks simple.** Simple bugs have root causes
too, and finding one usually takes minutes, not hours.

## Phase 1: Root-cause investigation

Before touching any code:

1. **Read the actual error.** Full stack trace, not the first line. Note
   exact file paths, line numbers, and error codes — they often name the
   fix directly.
2. **Reproduce it.** Confirm exact, repeatable steps. If it won't reproduce
   reliably, that's information — gather more data (logging, a tighter
   repro case) rather than guessing at a fix for a moving target.
3. **Check recent changes.** `git log`/`git diff` on the affected paths,
   recent dependency bumps, config or environment drift. Most bugs are
   introduced by something that changed, not something that was always
   broken.
4. **Trace the data flow backward.** Find where the bad value or bad state
   actually originates, not just where it's first observed. Follow it
   through each call/component boundary until you reach the source, and fix
   there — not at the symptom.

## Phase 2: Pattern analysis

If a working example exists (a similar endpoint, a sibling test, a
reference implementation), read it completely before comparing — skimming
guarantees you miss the difference that matters:

1. Locate the closest working analog in the same codebase.
2. Read it in full, not just the parts that look relevant.
3. List every difference from the broken code, however small. Don't
   discard one because "that can't matter" — verify it doesn't.
4. Note what the working version depends on (config, environment, call
   order) that the broken version might be missing.

## Phase 3: Single hypothesis, minimal test

1. State the hypothesis explicitly: "X is the root cause because Y." Vague
   hypotheses ("something about the config") aren't testable — sharpen
   them until they are.
2. Make the smallest possible change that tests it — one variable, not a
   bundle of plausible fixes.
3. Confirmed → proceed to Phase 4. Not confirmed → form a new hypothesis
   and repeat this phase. Don't layer a second fix on top of an unconfirmed
   first one.
4. If you genuinely don't understand a piece of the system, say so and
   investigate further rather than proceeding on a guess.

## Phase 4: Implementation

1. Write a failing test that reproduces the bug, if the codebase has a test
   framework in reach — see `superpowers:test-driven-development`. If a
   proper test isn't practical, a minimal repro script still counts; either
   way, confirm it fails before touching the fix.
2. Apply one fix addressing the confirmed root cause. No bundled cleanup,
   no "while I'm here" changes — those hide whether the fix actually
   worked.
3. Verify: the new test passes, no other tests regressed, the original
   symptom is gone. Use `superpowers:verification-before-completion` before
   claiming the bug is fixed.
4. **If the fix doesn't work, stop and count attempts.** Fewer than three →
   return to Phase 1 with what you just learned. Three or more failed fixes
   on the same bug is not bad luck — it's a signal to stop fixing and
   question the architecture: is the pattern you're patching around
   fundamentally sound, or is each fix surfacing new coupling/shared state
   somewhere else? Raise that with the user before attempting a fourth fix.

## Red flags — stop and return to Phase 1

- "Quick fix now, investigate properly later"
- "Let me just try changing X and see"
- "I don't fully understand this but it might work"
- Proposing a fix before tracing the data flow to its source
- A third attempt at fixing the same symptom

## When investigation genuinely finds no root cause

Rare, but real for timing-dependent or external failures. Only claim this
after actually completing Phases 1–3 — most "no root cause" conclusions are
an incomplete investigation, not an environmental bug. If it really is
environmental: document what was checked, implement the appropriate
handling (retry, timeout, explicit error), and add logging so the next
occurrence isn't starting from zero.
