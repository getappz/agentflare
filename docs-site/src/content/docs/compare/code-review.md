---
title: Code review
description: How agentflare's review consensus compares with CodeRabbit, Greptile, Graphite Diamond, Cursor BugBot, Copilot review, and PR-Agent.
---

AI code review splits into two products that are easy to confuse: a hosted bot
that comments on pull requests, and a local workflow that tells the agents you
are already running how to review each other's work.

## What agentflare provides

- **Findings and rounds** (MCP `review`) — submit structured findings against a
  diff, run several reviewers, and record rounds so repeated reviews do not
  restart from zero.
- **Consensus** — verifies each finding's cited line against the diff, clusters
  nearby findings, and tags each cluster `CONFIRMED` (multiple agents, matching
  severity), `DISPUTED` (multiple agents, conflicting severity), `UNIQUE` (one
  agent, verified citation), or `UNVERIFIED` (citation not on a changed line).
  Results are ranked Confirmed → Disputed → Unique → Unverified. There is no
  confidence score or majority/minority verdict in the payload.
- **Per-criterion verification** — acceptance criteria get `pass`, `fail`, or
  `uncertain` verdicts with a required evidence citation. A pass without
  evidence is demoted to uncertain.
- **PR integration** (MCP `flare_git`) — read a PR's checks, reviews, and
  comments in one call, post comments, and record the outcome of each review
  thread.
- **Stamped PRs** (CLI `agentflare pr`) — open PRs that carry the originating
  instance, so several agents sharing a repository can tell whose PR is whose.

agentflare does not ship a hosted review bot and does not bundle a linter suite.
It coordinates reviewers — human or agent — that you already run.

## How the alternatives differ

| | agentflare | CodeRabbit | Greptile | Graphite Diamond | Cursor BugBot | GitHub Copilot | PR-Agent |
|---|---|---|---|---|---|---|---|
| Where it runs | Locally, alongside your agents | Hosted app | Hosted app | Hosted, inside Graphite | Cursor / cloud | Hosted, on GitHub | Your CI, Docker, or CLI |
| Trigger | You call it | Webhook on PR events | Webhook on PR events | Graphite review flow | PR events in Cursor | PR review request | Comment command or action |
| Context | Whatever the reviewers gather | Diff plus retrieved context and linters | Full-repo code graph | PR inside a stacked-PR workflow | Diff plus `BUGBOT.md` rules | Diff in GitHub | Diff plus configurable context |
| Deterministic checks | None built in | Bundled linters and analyzers | None documented | None documented | None documented | None documented | Depends on your CI |
| Multi-agent consensus | Yes | Single product verdict | Single product verdict | Single product verdict | Single product verdict | Single product verdict | Single model call per tool |
| Open source | Yes | No | No | No | No | No | Yes |

**CodeRabbit** ([coderabbit.ai](https://www.coderabbit.ai)) posts a PR summary
plus line-level comments and runs bundled linters and analyzers before the
model. It supports GitHub, GitLab, Bitbucket, and Azure DevOps. It is the
lowest-effort option if you want review on every PR without touching CI.

**Greptile** ([greptile.com](https://www.greptile.com)) indexes the repository
into a graph and reviews the diff against it, which catches cross-file breakage
that a diff-only reviewer misses. That context is the product.

**Graphite Diamond** ([graphite.dev](https://graphite.dev)) reviews inside
Graphite's stacked-PR workflow and is tuned for fewer, higher-signal comments.
Its value is concentrated in teams already using Graphite.

**Cursor BugBot** ([cursor.com/bugbot](https://cursor.com/bugbot)) is
editor-native: review rules can live in `BUGBOT.md` next to the code, and the
tight loop with Cursor is the draw.

**GitHub Copilot code review** is native to GitHub with almost no setup, which
makes it the default baseline many teams compare against.

**PR-Agent** ([The-PR-Agent/pr-agent](https://github.com/The-PR-Agent/pr-agent))
is the open-source reference implementation: command-driven (`/review`,
`/improve`, `/ask`), runs as a GitHub Action, CLI, or self-hosted webhook, and
routes to any LiteLLM-supported model.

## Honest trade-offs

- A hosted bot reviews every PR even when no agent is running. agentflare only
  reviews when something invokes it.
- Bundled linters catch style and security classes cheaply; agentflare assumes
  your CI already does that.
- Consensus is useful when reviewers are genuinely independent. Pointing three
  copies of the same model at a diff buys less than one good reviewer with
  repository context.
- The local model means the raw review data stays on the machine, which matters
  for private repositories but is not a substitute for a team audit trail.

## When to choose what

- **agentflare** — agents and humans review each other's work locally, and you
  want consensus plus evidence-backed verdicts.
- **CodeRabbit** — you want a hosted bot covering every PR across several Git
  platforms.
- **Greptile** — cross-file impact is your recurring review failure mode.
- **Graphite Diamond** — your team already lives in stacked PRs on Graphite.
- **Cursor BugBot** — the whole team is on Cursor.
- **PR-Agent** — you want open-source, self-hosted review you can customize.

## Sources

- [CodeRabbit](https://www.coderabbit.ai)
- [Greptile](https://www.greptile.com)
- [Graphite](https://graphite.dev)
- [Cursor BugBot](https://cursor.com/bugbot)
- [PR-Agent](https://github.com/The-PR-Agent/pr-agent)
