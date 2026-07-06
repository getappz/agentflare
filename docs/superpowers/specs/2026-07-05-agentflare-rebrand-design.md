# agentflare rebrand — design spec

Date: 2026-07-05
Status: approved (pending final user sign-off on this doc)

## Summary

Rebrand `leanstack` → **agentflare**. Reposition from "lean-ctx + engram
orchestrator" to "AI CLI agent cost/performance optimizer" — inspired by
pageflare.dev's build-time-optimization positioning, applied to AI coding
agents instead of static sites. Stays fully free/open (MIT, no tiering). No
new marketing website in this phase — README/CLI rebrand only.

Adds four new runtime-optimization features on top of the existing
lean-ctx/engram/caveman/ponytail orchestration:

1. Session hygiene enforcement
2. Model-tier routing (advisory)
3. Call-batching linter
4. Cache-window-aware `ScheduleWakeup` nudging

Targets Claude Code, Codex, and Cursor from day one, with real depth varying
by each host's hook capabilities (see table below) — no feature is faked
with generic text where a host can't actually support it.

## Non-goals (this phase)

- No marketing website (pageflare-style landing page) — future spec.
- No paid tier / licensing / billing infra.
- No migration shim for `~/.leanstack/` → `~/.agentflare/` state — small
  existing install base; document the reset instead of writing migration
  code for it.

## Rename mechanics

Mechanical, low-risk, touches:

- Cargo package + binary name: `leanstack` → `agentflare`
- Repo: `getappz/leanstack` → `getappz/agentflare` (GitHub auto-redirects
  git/web traffic from the old name)
- Homebrew tap: `getappz/homebrew-leanstack` → `getappz/homebrew-agentflare`
- Scoop manifest: `bucket/leanstack.json` → `bucket/agentflare.json`
- Install scripts (`install.sh`/`install.ps1`): internal binary/package name
  references
- Release asset names: `leanstack-${target}.tar.gz` → `agentflare-${target}.tar.gz`
- Slash command: `/leanstack off` → `/agentflare off`
- State dir: `~/.leanstack/` → `~/.agentflare/` (no migration shim, see
  Non-goals)
- README: new tagline/positioning, energetic-but-honest tone (keep the
  existing benchmarks-honesty ethos — flag unverifiable claims rather than
  copy pageflare's more hype-forward style wholesale)

## Runtime optimizer module (Approach B)

New `src/optimize.rs`, architecturally separate from `components.rs`
(`components.rs` stays scoped to one-shot setup/install concerns; this new
module owns ongoing runtime behavior monitoring).

### State

Own state file: `~/.agentflare/runtime-state.json`, distinct from the
existing setup-state file (`~/.agentflare/state.json`, renamed from
`~/.leanstack/state.json`).

```
RuntimeState { sessions: Map<session_id, SessionRecord> }
SessionRecord {
  start_ts: u64,
  turn_count: u32,
  recent_tool_calls: Vec<{ name: String, ts: u64 }>,
}
```

Pruning: sessions inactive >24h dropped on every write. No separate cleanup
hook or scheduled job — pruning piggybacks on the natural write path, keeping
the state file bounded without new infrastructure.

### Failure-safety

Corrupt or missing `runtime-state.json` is treated as an empty state — same
fail-open pattern as the existing `state.rs`. Runtime-optimizer logic must
never panic, error out, or block the host agent; worst case is a missed
nudge, never a broken hook.

### New hook subcommand

`agentflare hook pre-tool-use --agent claude-code` — wired by `init.rs` into
`~/.claude/settings.json`'s `PreToolUse` hook array (currently only
`SessionStart`/`UserPromptSubmit` are wired). This is additive; existing
hook wiring is untouched.

All four features emit **advisory nudges only** (injected via hook output
text) — none hard-block a tool call. A heuristic false-positive blocking a
legitimate call would be worse than an occasional missed nudge.

### Per-feature mechanism and host scope

| Feature | Mechanism | Claude Code | Codex / Cursor |
|---|---|---|---|
| Session hygiene | Turn/time counters via existing `session-start` + `prompt-submit` hooks; nudge past a turn-count or wall-clock threshold | Real | Real (same hooks already wired for these hosts) |
| Model-tier routing | Per-turn keyword heuristic on prompt text (`find`, `where is`, `search for`, `locate` → nudge toward a cheap-model subagent) via existing `prompt-submit` hook | Real (advisory) | Real (advisory, same hook) |
| Call-batching linter | New `pre-tool-use` hook; rolling window of recent tool calls per session, matched against a small hardcoded batch-eligible-tool registry (e.g. repeated solo `Read` calls where a `paths` array exists) | Real | **Not available** — no PreToolUse-equivalent currently wired for these hosts; not faked with generic non-call-aware text |
| Cache-window-aware scheduling | Same new `pre-tool-use` hook, matcher scoped to `ScheduleWakeup` calls, checks `delaySeconds` against the documented 270–1200s dead zone | Real | **Not applicable** — tool doesn't exist on these hosts |

## Testing

Pure-logic functions get unit tests, following this session's established
pattern (`LEANSTACK_HOME_OVERRIDE`-style temp-dir isolation via
`crate::paths::test_support`, no real filesystem side effects, no new
dependencies):

- Turn/time threshold logic — given a fake `SessionRecord`, does it cross
  the nudge threshold at the right point (and not before)?
- Batch-eligible-tool detection — given a fake recent-calls window, does it
  flag N consecutive solo calls to the same batchable tool, and not
  false-positive on genuinely independent calls?
- `ScheduleWakeup` dead-zone check — given a `delaySeconds`, 271–299 nudges;
  60–270 and 1200+ do not.
- Model-routing keyword heuristic — given prompt text, correctly flags
  "find X" / "where is Y" style prompts and does not flag unrelated ones.
- Runtime-state pruning — sessions inactive >24h are dropped; active ones
  are kept.

## Open questions carried into implementation planning

- Exact turn-count/wall-clock thresholds for session hygiene (needs a
  sensible default, likely tunable later; not blocking design approval).
- Whether Cursor's hook system has any PreToolUse-equivalent beyond
  `sessionStart`/`beforeSubmitPrompt` (currently wired) that could extend
  the batching linter there in a later phase — research spike, not required
  for v1.
