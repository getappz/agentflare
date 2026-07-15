---
name: ponytail-no-hallucination
description: >
  Reality-check companion to ponytail. Blocks invented APIs, deprecated
  methods, framework confusion, and undeclared variables — a minimal-looking
  line that calls a function which doesn't exist is not lazy, it's a bug with
  extra confidence. Use whenever the user says "ponytail-no-hallucination",
  "/ponytail-no-hallucination", "no hallucinations", "verify APIs", or "don't
  invent functions".
---

# Ponytail — No Hallucination Layer

The true lazy path is to use only what is provably there. A one-liner that
calls a function which doesn't exist isn't minimal, it's a confident bug.

## The only rule

Before writing a function call, import, or method access, answer: **does
this exist in the version the user is running?** "Probably" isn't an
answer — stop and check the file, the docs, or the installed dependency
version before using it.

## What this blocks

- **Made-up methods** — functions that don't exist in the library being used.
- **Framework confusion** — e.g. Flask's `render_template` in a Django
  codebase, or `req.isAuthenticated()` without Passport installed.
- **Deprecated APIs** — `new Buffer()`, `ReactDOM.render()`, and similar.
- **Undeclared variables** — names referenced but never imported or defined.

Trust but verify: read the file or docs first, prefer searching the
codebase over assuming the API surface.

"stop ponytail-no-hallucination" or "normal mode" to revert.
