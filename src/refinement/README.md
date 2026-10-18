# Harness refinement

`agentflare workflow refine request.json` runs the Rust Harness² refinement
loop. It snapshots the repository, probes harness proposals in detached
worktrees, composes revisions from observed behavior, compares successful
finals with the baseline, and saves evidence-backed lessons for later tasks in
the same repository and domain.

Create a request outside the task snapshot if it contains private evaluation
details. Replace the model placeholders with exact models supported by your
installed agent CLIs:

```json
{
  "task_id": "parser-regression",
  "domain": "rust",
  "prompt": "Fix the parser regression and verify the result.",
  "repository": "C:/path/to/repository",
  "solver": { "agent": "codex", "model": "YOUR_SOLVER_MODEL" },
  "improver": { "agent": "codex", "model": "YOUR_IMPROVER_MODEL" },
  "mode": "sequential",
  "k": 1,
  "checks": [["cargo", "test", "-p", "your-package"]],
  "limits": {
    "max_calls": 32,
    "call_timeout_secs": 600,
    "total_timeout_secs": 3600
  }
}
```

Commands:

```text
agentflare workflow refine request.json
agentflare workflow refine-status RUN_ID
agentflare workflow refine-resume RUN_ID
agentflare workflow refine-cancel RUN_ID
```

Each command accepts `--db-path` for a dedicated refinement database. The
default is `~/.agentflare/refinement.db`. Run artifacts, receipts, reports,
and the proposed `adopted/` harness live under
`~/.agentflare/refinement/RUN_ID/`. The source harness is never automatically
replaced. Worktrees and solver artifacts are retained for inspection and
receipt verification; deleting or modifying them can make resume unavailable.

`parallel` explores `k` general and `k` domain-specific proposals together,
then probes `k` compositions. `sequential` carries each composition and its
history into the next round. Both use `4k` solver trials and `3k` logical edit
stages before retries, plus selection and reflection. Increase `max_calls`
explicitly for larger `k`; retries also consume the allowance.

The editable surface is `AGENTS.md` and
`.agents/skills/<name>/SKILL.md`. Generated hooks, plugins, executable scripts,
and permission-policy changes are excluded. A successful baseline can remain
the adopted harness; if all acceptance checks fail, there is no adoption.
Acceptance commands are trusted user-supplied argv, executed directly and
kept out of model prompts. Commands and fixtures already present in the task
snapshot remain accessible according to the host's existing sandbox policy.

Following upstream Harness2's separate harness and deliverable views, solver
evidence excludes runner-supplied instruction changes. Task files are compared
with the frozen snapshot; `AGENTS.md` and skills are compared with the supplied
harness, so subsequent solver edits, additions, and deletions remain visible.

Call and elapsed-time limits work with supported headless agents. Optional
`max_cost_usd` requires both roles to use `claude-code`, whose CLI supports a
per-invocation budget. Unknown or interrupted costs consume the reserved
allowance; without a dollar ceiling, unknown dollar usage stays unknown.

Resume validates saved request/executor identities and artifact hashes. An
interrupted reservation is not silently executed again; a new, separately
budgeted attempt may replace it. Missing budget records fail closed. A failed
or cancelled workflow is terminal; start a new run to change its limits or
configuration.

Detached worktrees provide separate task files, not a new host security
boundary. The existing runner's platform-specific sandbox and approval
settings remain in force. No live-model quality improvement has been measured
by the implementation tests.
