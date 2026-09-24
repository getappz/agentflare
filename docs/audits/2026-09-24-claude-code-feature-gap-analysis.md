# Claude Code feature audit and gap analysis

Date: 2026-09-24 · Branch: `claude/lucid-dirac-2upntt`

Scope: every way agentflare uses Claude Code, measured against Claude Code's
full feature surface as documented at code.claude.com/docs in September 2026,
plus an assessment of which Claude-Code-style features agentflare already
offers to other agents and which it should.

Method: code intelligence via lean-ctx 3.10.2 (`lean-ctx read -m signatures`,
`lean-ctx grep`, graph/BM25 indexes over 560 files), one exploration pass over
the whole workspace, and one documentation pass over Claude Code's reference
pages (CLI, settings, hooks, memory, skills, subagents, MCP, plugins, sessions,
Agent SDK). The `ctx_*` MCP tools were not loaded in the auditing session, so
the lean-ctx CLI and shell hook were used instead. Every claim about agentflare
cites `path:line` on this branch.

---

## 1. Verdict

agentflare touches Claude Code at three layers, and they are very unequal.

**Layer A, Claude Code as an instrumented host** (`agentflare init --agent
claude-code`): deep. agentflare wires 9 of Claude Code's 33 hook events, a
status line, `permissions.allow`, `env.BASH_ENV`, `~/.claude/rules`, an MCP
server with 31 tools, 2 resources and 12 slash-command prompts, a skills
registry with intent-based injection, coaching rules, a live-session registry
with inter-agent messaging, and a completion gate. This is a serious use of the
hook system. It uses only two of the hook system's output verbs (`deny` and
`additionalContext`), only the `command` hook type, and only user-scope
settings.

**Layer B, Claude Code as a headless worker** (`agentflare work`, `agentflare
run --print`, `flare-workflow` steps, Apps, Telegram chat, `optimize output`):
shallow. agentflare launches `claude -p` with seven flags and reads three
fields off the reply. It does not use system-prompt injection, structured
output, tool allow/deny lists, permission modes, per-run MCP config, per-run
settings, subagent invocation, session naming or forking, effort or thinking
controls, native sandbox settings, or the Agent SDK. One launch path also has a
flag collision that defeats the session-resume and cost features it does use
(§2.3). Everything Claude Code offers to make a worker *role-shaped* is left on
the table, and role shaping is done by prose in the user prompt instead.

**Layer C, the agent-agnostic layer**: the registry knows 20 agents, but the
capability table is thin: 6 have a headless mode, 2 have JSON output and
resume, 1 has turn/budget caps. Hooks are emulated for Cursor (3 events),
Codex (1 event plus a 2-event plugin), Cline (a 4-callback JS plugin) and
OpenCode (a branch-guard plugin), versus 9 events on Claude Code. Most of the
value agentflare generates for Claude users through hooks reaches other agents
only when those agents call agentflare's MCP tools.

### Top findings

| # | Finding | Severity |
|---|---------|----------|
| 1 | **Flag collision breaks resume and cost in `agentflare work`.** `run_headless_impl` adds `--output-format json` when `request_json` is set (`src/agent_launch.rs:847`), then `build_extra_args` appends `--output-format stream-json --verbose` (`src/cli/work.rs:253-258`). With the last flag winning, stdout is multi-line stream-json, `parse_json_reply` (`:676-698`) fails to parse it as one object and returns `session_id: None`, `cost_usd: None`. The SDD loop's `--resume` between fix rounds (`src/work_item_pipeline.rs:254-268`) and the per-job cost therefore never fire in real dispatch; the unit tests pass because they feed the parser a single object. | P0 bug |
| 2 | **Headless workers are launched bare.** No `--append-system-prompt(-file)`, `--allowedTools`/`--disallowedTools`, `--permission-mode`, `--mcp-config`/`--strict-mcp-config`, `--settings`/`--setting-sources`, `--agent`/`--agents`, `--add-dir`, `--json-schema`, `--effort`, `--name`, `--fork-session`. Role prompts, tool policy and MCP wiring for workers are done by text in the user prompt or by mutating the user's global `~/.claude` state, and verdicts are regex-matched on marker strings. | P0 |
| 3 | **`--dangerously-skip-permissions` is the only permission posture.** `--permission-mode` (`plan`, `acceptEdits`, `dontAsk`, `auto`), `permissions.deny`, `permissions.disableBypassPermissionsMode`, `autoMode` rules and `sandbox.*` are unused; agentflare relies solely on its own bwrap sandbox and PreToolUse branch guard. | P0 |
| 4 | **Personas are subagent files that are never invoked as subagents.** Apps project `personas/*.md` into `.claude/agents/` (`crates/agentflare-apps/src/project.rs:4-26`), but `apps/auto-company/workflow.json` prompts say "Read `.claude/agents/<persona>.md` and answer in that voice". `--agent <name>`, `--agents`, `@agent-name`, `SubagentStart`/`SubagentStop` (beyond `optimize code`), and agent frontmatter (`tools`, `model`, `permissionMode`, `maxTurns`, `skills`, `memory`, `hooks`, `mcpServers`, `isolation: worktree`) are unused. | P0 |
| 5 | **`PreCompact` is wired but a documented no-op** (`src/hook.rs:593-608`) while README and AGENTS.md still advertise `optimize context` as PreCompact compaction. `PostCompact` exists and is the event that can re-inject survival context. `SessionStart` ignores `source` (`startup|resume|clear|compact`), so the full briefing is re-injected after every compaction. | P1 |
| 6 | **Hook system used at 2 of 5 types and 2 of 9 output verbs.** Unused: `mcp_tool` hooks (call agentflare's own MCP server per event instead of spawning a process), `http` hooks (post to the daemon), `prompt`/`agent` hooks (the judge role), `permissionDecision: allow|ask|defer`, `updatedInput`, `PermissionRequest`, `PermissionDenied`+`retry`, `StopFailure` (API-error failover trigger), `Notification`, `TaskCreated`/`TaskCompleted`, `PreModelSwitch`/`PostModelSwitch`, `WorktreeCreate`/`WorktreeRemove`, `Setup`, the `if` field. | P1 |
| 7 | **Interactive `agentflare run claude-code --mode X` passes `--mode`** (`src/agent_launch.rs:101-103`), which is not a Claude Code flag; the real one is `--permission-mode`. | P1 bug |
| 8 | **Config-dir and uninstall hygiene.** `~/.claude` is hard-coded (`src/paths.rs:36-60`; only `crates/flare-code/src/config.rs:58-80` honours `CLAUDE_CONFIG_DIR`); auth-profile isolation overrides `HOME` (`src/auth.rs:952`), relocating hooks, memory and plugins too; `wire_optimize_claude_code` overwrites any existing `statusLine` (`src/init.rs:665`); `uninstall` leaves `PreCompact`, `PostToolUse`, `PostToolUseFailure`, `SubagentStart`, `statusLine`, `permissions.allow`, `env.BASH_ENV` and rule files behind (`src/uninstall.rs:55-110`). | P1 |
| 9 | **Distribution and interop.** No Claude plugin (`.claude-plugin/plugin.json`), only Codex and Cline plugins; no `claude mcp serve` adapter; no `--channels` push messaging; no Agent SDK; no GitHub Action or Routines recipe; `~/.claude/.credentials.json` is read directly for usage polling (`src/claude_usage.rs:23-64`). | P2 |

---

## 2. What agentflare uses today (evidence)

### 2.1 Host instrumentation: `agentflare init --agent claude-code`

| Surface | What agentflare does | Evidence |
|---|---|---|
| `SessionStart` (10s) | Briefing (identity, consent-needed components, coaching rules, open items, skill preload), registers the session for messaging, delivers pending messages, flushes vents. Emits `systemMessage` and `hookSpecificOutput.additionalContext`. | `src/init.rs:283-290`, `src/hook.rs:51-96`, `:98-226` |
| `UserPromptSubmit` (5s) | `/agentflare …` and `/pm …` toggles, turn count, router and hygiene nudges, `@mention` expansion, coaching auto-match, intent classification → top-3 skill injection, message delivery. `additionalContext` only. | `src/init.rs:291-298`, `src/hook.rs:635-847`, `src/skill_detect.rs:190-459` |
| `PreToolUse`, no matcher (5s) | In order: branch guard on `MUTATING_TOOLS` resolved against the target file's repo, `TodoWrite` → `item` redirect, spec-path → `asset` redirect, destructive `rm` of agentflare DBs; enforced coaching rules; completion gate (`item done`/`check_merge` need fresh verification + review + diagnosis); then nudges and messages. Emits `permissionDecision: deny` + reason. 2s fail-open budget. | `src/init.rs:299-306`, `src/hook.rs:409-548`, `src/hook_redirect.rs:26-39, 313-346, 408-442` |
| `PostToolUse`, matcher = Bash family ∪ `mcp__flare__tool` ∪ `mcp__flare__item` ∪ `ReportFindings` ∪ mutating tools (5s) | Records verification evidence, invalidates it on edits, records review evidence, shows the finishing-branch menu after `item done`. | `src/init.rs:263-271, 355-362`, `src/hook_completion_gate.rs:228-330` |
| `PostToolUseFailure`, matcher `Bash|Edit|Write` (5s) | Deterministic failure coaching via `additionalContext`; a retired `prompt`-type judge hook is removed on upgrade. | `src/init.rs:334-346`, `src/hook.rs:331-368` |
| `Stop` (5s) | `{"decision":"block","reason":<messages>}` to deliver inter-agent messages to an about-to-idle agent. | `src/init.rs:318-325`, `src/hook_messages.rs:123-141` |
| `SessionEnd` (5s) | Marks the session ended in the registry. | `src/init.rs:326-333`, `src/hook.rs:613-617` |
| `PreCompact` (5s) | Wired; handler is an intentional no-op. | `src/init.rs:307-314`, `src/hook.rs:593-608` |
| `SessionStart`/`SubagentStart`/`UserPromptSubmit` (optimize code) | Code-minimalism mode injection; `SubagentStart` skips read-only agent types by regex. | `src/init.rs:655-663`, `src/cli/optimize.rs:288-337` |
| `statusLine` | `optimize code hook statusline` badge; written unconditionally. | `src/init.rs:665-671` |
| `permissions.allow` | `mcp__flare__docs`, `mcp__flare__search`, `mcp__flare__tool`, `ToolSearch`; strips stale `mcp__lean-ctx__*`. | `src/components.rs:626-712` |
| `permissions.defaultMode` | Pinned via `write_pinned_mode`. | `src/components.rs:141-153` |
| `env.BASH_ENV` | `~/.bashenv` with the lean-ctx dispatcher and an `rm -rf`/force-push DEBUG-trap guard. | `src/bashenv.rs:167-195`, `src/components.rs:836` |
| `skillOverrides` | `name-only` per skill, behind an off-by-default cargo feature. | `src/components.rs:450-482` |
| `~/.claude/rules/*.md` | `exa.md`, `git.md`, `lean-ctx.md`, `flare-docs.md`, `browser.md`, coaching `<id>.md`. | `src/components.rs:287-366`, `src/rule_text.rs`, `src/paths.rs:46-48` |
| MCP registration | `claude mcp remove agentflare -s user; claude mcp add flare -s user -- <bin> mcp` (tools appear as `mcp__flare__*`); deletes the native `lean-ctx` entry once lean-ctx is gateway-routed. | `src/components.rs:945-946, 1019-1020`, `:96-106` |
| MCP server | stdio (default) or Streamable HTTP on `127.0.0.1:35274/mcp`; 31 tools, resources `agentflare://sessions` and `agentflare://nudges`, prompts `/flare:{optimize,artifact,handoff,git,pm,optimize-*}`; tool results piggyback messages and progress notifications. | `src/mcp_server.rs:230-1873, 1885-1960, 1998-2116`, `src/mcp_prompts.rs:43-86` |
| Skills | `.claude/skills` (7), `.claude/commands/pm.md` (also the `/flare:pm` prompt body); registry sources: builtin, `~/.claude/skills`, `<cwd>/.claude/skills`, `~/.claude/plugins/cache/**/skills`, plus Codex/Cursor/OpenCode dirs. | `crates/skill-registry/src/sources.rs:285-330`, `src/mcp_prompts.rs:322` |
| Transcripts | Cost roll-up per model and context size from `~/.claude/projects/**/*.jsonl`; session-id → transcript lookup; insights ingest. | `src/cost.rs`, `src/rollup.rs`, `src/cli/git.rs:871-985`, `crates/flare-insights/src/ingest/claude.rs` |
| Credentials | Reads `~/.claude/.credentials.json`, polls `api.anthropic.com/api/oauth/usage`; ≥70% of the 5h or 7d window routes dispatch to another agent. Profile rotation backs up and swaps the credential files. | `src/claude_usage.rs:18-143`, `src/quota/failover.rs:115-117`, `src/auth.rs:19-26` |
| Sandbox | bwrap with `~/.claude` mounted writable so OAuth refresh works. | `crates/agentflare-jobs/src/sandbox.rs:44-54` |

### 2.2 Headless worker

| Item | Value | Evidence |
|---|---|---|
| Print mode | `claude -p`, prompt on stdin (argv cap). | `crates/agent-registry/src/registry.rs:329-343`, `src/agent_launch.rs:587-603` |
| Autonomy | `--dangerously-skip-permissions` (`work`, daemon, supervisor). | `registry.rs:355-375`, `src/cli/work.rs:247-251` |
| Output | `--output-format json` (when `request_json`) and `--output-format stream-json --verbose` (from `build_extra_args`) on the same command; reply parsed for `result`, `session_id`, `total_cost_usd`. | `src/agent_launch.rs:846-850, 676-698, 1063-1081`, `src/cli/work.rs:253-258` |
| Caps | `--max-turns=N`, `--max-budget-usd=X` (Claude only). | `src/cli/work.rs:259-269` |
| Model | `--model <name>`. | `src/cli/work.rs:274-277` |
| Resume | `--resume <id>` between SDD rounds and per Telegram chat; stale-session retry. | `src/work_item_pipeline.rs:238-282`, `src/chat_channel.rs:139, 183, 210` |
| Environment | Strips `CARGO_TARGET_DIR`; sets `AGENTFLARE_AGENT`, `AGENTFLARE_CLAIM_OWNER`; no `ANTHROPIC_*`/`CLAUDE_*` set. | `src/agent_launch.rs:933-968` |
| Interactive | `agentflare run claude-code [--model] [--mode] [-- args]`; auth profiles isolated by overriding `HOME`. | `src/agent_launch.rs:97-106`, `src/auth.rs:952-991` |
| Roles | Implementer, review analyst, task reviewer, re-reviewer, judge: plain user prompts; verdicts via `REVIEW_APPROVED`, `DECISION: GO`, `SESSION_MARKER` strings. | `src/work_item_pipeline/prompt_builders.rs`, `src/work_item_pipeline.rs:225-252` |
| Workflow steps | `JsonStep {agent, prompt, model?, args?, mode, timeout_secs, error_mode, output_var}`. Apps project `.claude/agents`, `.claude/skills`, `.claude/settings.json {enableAllProjectMcpServers}` and `.mcp.json` into a scratch cwd. | `crates/flare-workflow/src/json.rs:94-140`, `crates/agentflare-apps/src/project.rs:4-26` |
| Other launchers | `optimize output` compresses via `claude --print` unless `ANTHROPIC_API_KEY` is set. | `crates/flare-output/src/llm.rs:25-64` |
| Failover | Quota/credit errors mark Claude unavailable and route to another agent (ClinePass model mapping). | `src/quota/failover.rs`, `registry.rs:395-408` |

Confirmed absent from every launch path (grep over `src/`, `crates/`, `apps/`):
`--append-system-prompt`, `--system-prompt`, `--allowedTools`,
`--disallowedTools`, `--permission-mode`, `--permission-prompts`,
`--mcp-config`, `--strict-mcp-config`, `--agent`, `--agents`, `--settings`,
`--setting-sources`, `--add-dir`, `--json-schema`, `--include-partial-messages`,
`--input-format`, `--fork-session`, `--session-id`, `--name`, `--continue`,
`--fallback-model`, `--effort`, `--bare`, `--no-session-persistence`,
`--worktree`, `--plugin-dir`, `CLAUDE_CONFIG_DIR` (outside flare-code),
`MAX_THINKING_TOKENS`, `claude-agent-sdk`.

### 2.3 The flag collision, step by step

1. `real_agent_send_hook` calls `run_headless_*` with `request_json = true`
   (`src/work_item_pipeline.rs:771, 781`).
2. `run_headless_impl` prepends `--output-format json`
   (`src/agent_launch.rs:847-849`) and appends `extra_args`.
3. `extra_args` came from `build_extra_args`, which already holds
   `--output-format stream-json --verbose` (`src/cli/work.rs:253-258`).
4. Claude Code's parser keeps the last value of a repeated option, so the
   process emits stream-json. `parse_json_reply` tries `serde_json::from_str`
   on the whole stdout and fails (`src/agent_launch.rs:692-696`).
5. `clean_agent_reply` recovers the text from the last line
   (`:1092-1098`), so the job "works", but `reply.session_id` is `None`, so
   `encode_session` writes no marker, `resume_args_for` returns nothing on the
   next round, and the run's cost is never recorded.

Fix sketch: in `run_headless_impl`, skip `json_output_args` when
`extra_args` already contains `--output-format`; and make
`parse_json_reply` fall back to the last-line parse (`parse_claude_reply`)
when the whole-stdout parse fails. Add a test that feeds a stream-json
transcript through `run_headless_impl`'s parsing path rather than through
`parse_claude_reply` directly. Confirm with one live `agentflare work` run
that `agent_sessions` gains a session id.

---

## 3. Feature-by-feature matrix

Legend: **Used** = built on; **Partial** = touched but not leveraged;
**Unused**; **N/A** = not applicable to an orchestrator.

### 3.1 CLI and headless mode

| Claude Code feature | Status | Gap and recommendation |
|---|---|---|
| `-p`, stdin prompt | Used | — |
| `--output-format stream-json`, `--verbose` | Used (buggy) | Fix §2.3. Then consume the whole event stream: `system/init` (model, tools, `mcp_servers`, `permissionMode`), assistant/user events for live progress, and the `result` object's `usage`, `num_turns`, `duration_ms`, `permission_denials`, `is_error`, `subtype` (`error_max_turns`, `error_max_budget_usd`). `agent_launch_progress.rs` today only sees bytes for stall detection. |
| `--input-format stream-json` | Unused | A persistent worker: send follow-up turns and mid-flight corrections (`src/work_item_pipeline.rs:452-462` prepends them to the next prompt today) without respawning. |
| `--json-schema` | Unused | Typed verdicts for judge, reviewer and go/no-go steps instead of `REVIEW_APPROVED`/`DECISION: GO` markers. |
| `--append-system-prompt`, `--append-system-prompt-file`, `--system-prompt(-file)` | Unused | Role identity, agentflare tool guidance and repo rules belong in the system prompt per dispatch, not in the user turn and not in the user's `~/.claude`. |
| `--allowedTools` / `--disallowedTools` | Unused | Reviewer and judge roles should be read-only; implementers get edit tools. |
| `--permission-mode` (`default`, `plan`, `acceptEdits`, `auto`, `dontAsk`, `bypassPermissions`, `manual`) | Unused | `plan` maps onto `submit_plan`/`approve_plan`; `acceptEdits` + deny rules is a safer default for `agentflare run` than skip-permissions. Also fixes the `--mode` bug. |
| `--permission-prompts none`, `--permission-prompt-tool` | Unused | The supported way to run unattended without bypassing permissions: route prompts to a tool (agentflare's MCP server) that applies the branch guard and completion gate. |
| `--dangerously-skip-permissions` | Used | Keep for sandboxed jobs; pair with `--disallowedTools` and `permissions.deny`. |
| `--max-turns`, `--max-budget-usd` | Used | Handle `subtype` explicitly. |
| `--model`, `--fallback-model` | Partial | `--fallback-model` gives in-process failover before agentflare's cross-agent failover. |
| `--effort` | Unused | `TaskModelTier` picks a model per task; effort is the cheaper second axis. |
| `--resume <id|name|path>`, `--continue` | Used / Unused | Resume by name or transcript path also works. |
| `-n/--name`, `--fork-session`, `--session-id`, `--from-pr` | Unused | Name sessions after `<item>:<role>:<round>` so they are addressable from the item record; fork one implementer session into parallel reviewers; `--from-pr` links sessions to PRs for `check_merge`. |
| `--mcp-config`, `--strict-mcp-config` | Unused | Per-job MCP wiring (flare, lean-ctx, GitHub) without mutating `~/.claude.json`. Apps already write a project `.mcp.json`; jobs should too. |
| `--settings`, `--setting-sources` | Unused | Job-scoped settings (hooks, permissions, `env`, `model`, `statusLine` off, `attribution.commit=false`). |
| `--add-dir` | Unused | Worktree jobs that need the main checkout or a sibling repo read-only. |
| `--agent`, `--agents` | Unused | See §3.6. |
| `--bare` | Unused | For deterministic CI-like jobs: skips hooks, skills, subagents, MCP and CLAUDE.md the user did not opt into. Note it also skips agentflare's own hooks, so pair with `--mcp-config` and `--settings`. |
| `--no-session-persistence`, `CLAUDE_CODE_SKIP_PROMPT_HISTORY` | Unused | For throwaway judge turns; keep persistence where cost roll-up depends on transcripts. |
| `--plugin-dir`, `--plugin-url` | Unused | Load agentflare as a plugin per job without installing it. |
| `--worktree`, `--cloud`, `--remote-control`, `--channels`, `--chrome`, `--tmux`, `--teammate-mode` | Unused | `--channels` matters: an MCP server with the `claude/channel` capability can push messages into a live session, replacing the `Stop`-block delivery hack. |
| `--init-only` / `Setup` hook | Unused | The supported way to run one-time setup (agentflare's install steps) inside Claude's own lifecycle. |
| `claude mcp add|remove|list|get|serve`, `claude plugin …`, `claude doctor`, `claude setup-token`, `claude project purge` | Partial | `mcp add/remove` used. `setup-token` is the supported way to mint long-lived tokens instead of reading `.credentials.json`. |

### 3.2 Settings

| Key | Status | Notes |
|---|---|---|
| `hooks` | Used | User scope only. Repo-specific gates belong in project `.claude/settings.json`. |
| `statusLine` | Used | Overwrites existing value. |
| `permissions.allow`, `permissions.defaultMode` | Used | |
| `env` | Used | `BASH_ENV` only. Also the right place for `AGENTFLARE_*` and proxy variables for workers. |
| `permissions.deny` / `ask` / `additionalDirectories` / `blockReadsOutsideWorkingDirectories` / `disableBypassPermissionsMode` | Unused | Branch guard and destructive-command classification can be expressed as `deny` rules as a first, hook-free layer; `disableBypassPermissionsMode` for interactive installs. |
| `autoMode`, `autoMode.classifyAllShell` | Unused | Claude's classifier-based auto mode overlaps with `hook_redirect::classify`. |
| `model`, `fallbackModel`, `effortLevel`, `maxEffortLevel`, `modelSettings`, `alwaysThinkingEnabled` | Unused | |
| `outputStyle` | Unused | `optimize output` (caveman) is prompt injection; an output style is the native mechanism and survives compaction. |
| `enabledPlugins`, `extraKnownMarketplaces`, `pluginConfigs` | Unused | See §3.8. |
| `allowedMcpServers`, `enabledMcpjsonServers`, `enableAllProjectMcpServers` | Partial | Apps set `enableAllProjectMcpServers`; jobs should approve only the servers they ship. |
| `sandbox.*` (filesystem, network, credentials) | Unused | Native sandbox duplicates part of `flare-sandbox`; at minimum detect `sandbox.enabled` and avoid double-wrapping bwrap in bwrap. |
| `attribution.commit`, `attribution.pr`, `includeGitInstructions` | Unused | AGENTS.md forbids co-author trailers; `attribution.commit: false` enforces it. |
| `cleanupPeriodDays` | Unused | Cost roll-ups depend on transcripts Claude deletes after this period. |
| `autoMemoryEnabled`, `autoMemoryDirectory` | Unused | See §3.4. |
| `autoCompactEnabled`, `autoCompactWindow`, `bashOutputMaxChars`, `skillListingBudgetFraction` | Unused | Context-budget knobs that `optimize` should tune for hosts it instruments. |
| `crossSessionInbound`, `isolatePeerMachines` | Unused | Native cross-session messaging; decide how it coexists with `message`. |
| `processWrapper` | Unused | Where a corporate launcher (or agentflare's sandbox) is meant to be declared. |
| `disableSideloadFlags`, `strictPluginOnlyCustomization`, `allowManagedHooksOnly` | Unused | In managed environments these reject `--agents`/`--mcp-config`/`--plugin-dir` and user hooks. Any P0 plan that relies on flags needs the plugin path as fallback. |
| `CLAUDE_CONFIG_DIR` | Partial | Honoured in `flare-code` only; `src/paths.rs` hard-codes `~/.claude`. |

### 3.3 Hooks

Claude Code has 33 events, 5 hook types (`command`, `http`, `prompt`,
`agent`, `mcp_tool`), an `if` filter, and 9 output verbs. agentflare uses 9
events, 1 type, 2 verbs (`deny`, `additionalContext`) plus `decision: block`
on `Stop` and `systemMessage` on `SessionStart`.

| Event / capability | Status | Notes |
|---|---|---|
| `SessionStart` | Used | Ignores `source`; on `compact` re-inject only survival context, on `resume` skip the consent banner. Use `CLAUDE_ENV_FILE` to export `AGENTFLARE_*`. |
| `UserPromptSubmit` | Used | `decision: block` unused (e.g. refuse `agentflare work` under an AI agent). |
| `PreToolUse` | Used | `allow` (pre-approve gateway tools, cut prompts), `ask`, `defer`, `updatedInput` (rewrite a call, as lean-ctx does) unused. `if` field could replace part of `classify`. |
| `PostToolUse`, `PostToolUseFailure`, `Stop`, `SessionEnd` | Used | `SessionEnd.reason` distinguishes crash from exit for the registry. `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` bounds the `Stop` block loop. |
| `PreCompact` | Wired, no-op | Remove or use to checkpoint `runtime.json` evidence into agentflare memory. |
| `PostCompact` | Unused | The event that can re-inject agentflare's survival context after compaction. |
| `SubagentStart` | Partial | `optimize code` only. |
| `SubagentStop` | Unused | Record a reviewer subagent's verdict for the completion gate. |
| `PermissionRequest`, `PermissionDenied` (+`retry`) | Unused | Answer permission prompts programmatically instead of skipping permissions. |
| `StopFailure` | Unused | Fires on API errors (quota, 5xx): the natural trigger for `quota::failover` instead of scraping stdout. |
| `Notification` | Unused | Route "waiting for input"/"idle" to `channel_send` and the dashboard. |
| `TaskCreated`, `TaskCompleted` | Unused | Sync Claude's native tasks with agentflare items (today `TodoWrite` is denied and redirected). |
| `PreModelSwitch`, `PostModelSwitch` | Unused | Enforce `[router]` policy and quota-aware model choice natively. |
| `WorktreeCreate`, `WorktreeRemove` | Unused | Reconcile Claude-created worktrees with agentflare claims. |
| `Setup`, `InstructionsLoaded`, `ConfigChange`, `CwdChanged`, `FileChanged`, `PostToolBatch`, `UserPromptExpansion`, `MessageDisplay`, `Elicitation*`, `TeammateIdle` | Unused | `Setup` for install; `ConfigChange` to detect a user editing hooks agentflare wrote. |
| Hook type `mcp_tool` | Unused | Call `mcp__flare__*` directly per event: no process spawn, shared state, and the hook logic already lives in the MCP server process. |
| Hook type `http` | Unused | Post to the daemon; needs `allowedHttpHookUrls`. |
| Hook types `prompt`, `agent` | Unused | Tried and retired for `PostToolUseFailure`; `agent` is the right tool for the judge role. |
| `async: true` | Unused | Vent consolidation and messaging sync in `SessionStart` block startup. |
| Input fields `permission_mode`, `effort`, `prompt_id`, `tool_use_id`, `scratchpad_dir` | Unused | `permission_mode` lets the branch guard relax under `plan`. |

### 3.4 Instructions and memory

| Feature | Status | Notes |
|---|---|---|
| `CLAUDE.md` hierarchy, `@imports`, `CLAUDE.local.md`, managed `claudeMd` | Partial | agentflare writes `~/.claude/rules/*.md` and reads `AGENTS.md`; never writes or verifies `CLAUDE.md`. Claude Code v2.1.277+ reads `AGENTS.md` directly, so agentflare's AGENTS.md is already loaded natively. |
| `.claude/rules/*.md` with `paths:` frontmatter | Unused | Path-scoped rules (e.g. lean-ctx rule only for source files, git rule only near `.git`). |
| Auto memory (`MEMORY.md`, topic files, `type:` frontmatter) | Unused | Bridge both ways: import Claude's auto memory into `memory(remember)`; export agentflare facts into `MEMORY.md` (200 lines / 25KB cap). |
| `/import` | Unused | Claude's own import of Cursor/Copilot config is the mirror image of agentflare's rule-file fan-out. |
| `/memory`, `/init`, `/context` | N/A | |

### 3.5 Skills and slash commands

| Feature | Status | Notes |
|---|---|---|
| `SKILL.md` skills, discovery scopes | Used | 7 shipped; registry covers user, project, plugin-cache paths. |
| Frontmatter beyond `name`/`description` (`allowed-tools`, `disallowed-tools`, `model`, `effort`, `context: fork`, `agent`, `background`, `paths`, `hooks`, `arguments`, `disable-model-invocation`, `user-invocable`) | Unused | The `pm` skill is a `context: fork` + `disable-model-invocation` candidate; `paths` gates language-specific skills. |
| `.claude/commands/*.md`, `$ARGUMENTS`, `!`shell``, `@file` | Used | `pm.md` only. |
| MCP prompts as `/flare:*` | Used | |
| `/skill-doctor`, `claude plugin eval` | Unused | Should back `agentflare skill eval` on Claude hosts. |
| `skillListingBudgetFraction`, `skillOverrides` | Partial | `skillOverrides` behind a cargo feature. |

### 3.6 Subagents and orchestration

| Feature | Status | Notes |
|---|---|---|
| `.claude/agents/*.md` | Partial | Apps project persona files there; workflow prompts bypass them. |
| `--agent <name>` (run the whole session as an agent), `--agents <json>`, `@agent-name` | Unused | Run each `auto-company` step as `claude -p --agent ceo-bezos`; each SDD role becomes an agent definition. |
| Frontmatter `tools`, `disallowedTools`, `model`, `permissionMode`, `maxTurns`, `skills`, `mcpServers`, `hooks`, `memory`, `effort`, `isolation: worktree`, `omitClaudeMd`, `initialPrompt`, `background` | Unused | Everything `RoleSpec` (§5) needs already exists as agent frontmatter. |
| Built-in `Explore`, `Plan`, `general-purpose` | Unused | The review-analyst prompt re-implements `Explore`. |
| Agent teams, `SendMessage`, `TeammateIdle`, `crossSessionInbound` | Unused | Decide the source of truth when Claude spawns its own team inside an agentflare job; agentflare's `message` tool and session registry should ingest `TeammateIdle`/`TaskCompleted`. |
| `/subtask`, fork mode | Unused | |
| Background agents, `claude agents`, `disableAgentView` | Unused | agentflare jobs are the durable equivalent. |
| Workflows tool, `/loop`, scheduled tasks | Unused | `flare-workflow` is agentflare's own engine; interop by letting a Claude workflow call `mcp__flare__workflow`. |
| Plan mode | Unused | Maps to `submit_plan`/`approve_plan`; `plansDirectory` is where Claude writes the plan file. |
| Worktrees (`--worktree`, `isolation: worktree`, `WorktreeCreate`) | Unused | agentflare provisions worktrees itself; document the nesting. |
| Checkpoints, `/rewind`, `fileCheckpointingEnabled` | N/A (own) | `agentflare git rewind` and shim snapshots are the universal equivalent. |

### 3.7 MCP

| Feature | Status | Notes |
|---|---|---|
| stdio and Streamable HTTP server | Used | |
| Scopes: user (`claude mcp add -s user`), project `.mcp.json` (Apps only) | Partial | A checked-in `.mcp.json` for the repo makes agentflare available to any collaborator without `init`. |
| Resources | Partial | `agentflare://sessions`, `agentflare://nudges`. Items, artifacts, memory facts and docs are natural resources (`@flare:item/123`). |
| Prompts | Used | |
| Tool search / deferred tools | Partial | agentflare's `tool(search|execute)` gateway is its own deferral layer and has fought Claude's native `ToolSearch` before (`fix: stop usetsearch blocking every native ToolSearch call`). Consider `alwaysLoad` and `_meta["anthropic/maxResultSizeChars"]` annotations instead. |
| `claude/channel` push messages (`--channels`, `channelsEnabled`) | Unused | Push inter-agent messages into a live session; retire the `Stop`-block path where available. |
| `claude mcp serve` | Unused | Turns Claude Code into a tool server any other agent can call: the cleanest way to give non-Claude agents a Claude worker. |
| `headersHelper`, OAuth, `ws` transport | Unused | Needed for the daemon to serve remote sessions. |
| Auto-background (>2 min), `MCP_TIMEOUT`, idle timeouts | Unused | `pr_wait` (60-120s) sits under the 2-minute auto-background threshold on purpose; document it. |

### 3.8 Plugins

| Feature | Status | Notes |
|---|---|---|
| `.claude-plugin/plugin.json`, marketplace | Unused | agentflare ships `.codex-plugin/plugin.json` and a Cline plugin but no Claude plugin, so hooks, rules, skills, agents and MCP are installed by editing user files. A plugin bundles them, versions them, `enabledPlugins` toggles them, and it is the only path under `strictPluginOnlyCustomization`. |
| Components: `skills/`, `agents/`, `hooks/hooks.json`, `.mcp.json`, `.lsp.json`, `monitors/monitors.json`, `bin/`, `settings.json` | Unused | `monitors` and `bin/` (PATH shims) map onto agentflare's dashboard signals and its PATH shim. |
| `claude plugin validate|eval` | Unused | |

### 3.9 Sessions, context and cost

| Feature | Status | Notes |
|---|---|---|
| Transcript parsing for cost | Used | Use `result.usage` from stream-json for jobs so cost survives `cleanupPeriodDays`. |
| `/compact [instructions]`, auto-compact, `autoCompactWindow` | Unused | For long jobs, send `/compact <focus>` over `--input-format stream-json` before context fills. |
| Session names, fork, `--from-pr`, `claude project purge`, `CLAUDE_CODE_PROJECT_DIR_NAME` | Unused | |
| Structured transcript access `claude -p --resume <id> --output-format json` | Unused | Documented alternative to parsing `.jsonl` (internal format, may break). |
| `/context`, `/cost`, `/usage`, `/insights` | N/A | `agentflare cost`, `insights`. |
| Usage polling via `.credentials.json` and an undocumented beta endpoint | Used, fragile | `claude setup-token`, or the `StopFailure` hook, are the supported signals. |

### 3.10 Cloud, CI and integrations

| Feature | Status | Notes |
|---|---|---|
| Claude Code on the web, `SessionStart` container caching, Routines, `--cloud`, `remote.defaultEnvironmentId` | Partial | Only this repo's own `.claude/hooks/session-start.sh` (installs lean-ctx, prefetches crates). Daemon jobs could target cloud sessions via Routines. |
| GitHub Action (`anthropics/claude-code`, `@claude`) | Unused | `.github/workflows` has no Claude job. A label-triggered `agentflare work` Action would complement `flare_git pr_wait`. |
| Remote Control, Slack Claude Tag | Unused | Overlaps with `message` + dashboard; `channel_send` is outbound only, Telegram is the only inbound chat. |
| Bedrock/Vertex/Foundry, `ANTHROPIC_BASE_URL`, OTEL | Unused | `flare-proxy` exposes an Anthropic-compatible endpoint but nothing points Claude Code at it. |
| Cowork | Partial | Host alias and `CLAUDE_CODE_IS_COWORK` detection only. |

### 3.11 Agent SDK

| Feature | Status | Notes |
|---|---|---|
| `query()` options: `agents`, `hooks` (in-process), `canUseTool`, `permissionPromptTool`, `systemPrompt`/`appendSystemPrompt`, `settingSources`, `mcpServers` (in-process servers), `outputFormat`/`jsonSchema`, `resume`/`forkSession`, streaming input, `sandbox`, `plugins`, `thinking`, `effort`, `abortController` | Unused | agentflare is Rust; the SDK is Node/Python. The CLI flags in §3.1 cover most of it. A thin Node sidecar (like the browser sidecar) would unlock `canUseTool` callbacks and in-process hooks without a process per event. |

---

## 4. Agent-agnostic layer today

From `crates/agent-registry/src/registry.rs`, `src/init.rs`,
`src/components.rs`, `crates/agentflare-jobs/src/sandbox.rs`,
`src/hook_messages.rs:18`:

| Capability | Claude Code | Codex | Cursor | OpenCode | Gemini | Cline | Windsurf / VS Code Copilot / Continue | Aider, Cody, Goose, Amp, Kiro, Antigravity, Grok, Kimi, Openclaw, Droid, GitHub Copilot CLI |
|---|---|---|---|---|---|---|---|---|
| Headless mode | `-p` | `exec` | `-p` | `run` | `-p` | stdin | — | — |
| Autonomy flag | skip-perms | `--full-auto` | `--force` | `--auto` | `--yolo` | `--auto-approve true` | — | — |
| JSON output parsed | yes | no | yes | no | no | no | — | — |
| Resume by id | yes | no | yes | no | no | no | — | — |
| Turn / budget caps | yes | no | no | no | no | no | — | — |
| Hooks | 9 events | `~/.codex/hooks.json` PreToolUse + plugin (SessionStart, UserPromptSubmit) | `sessionStart`, `beforeSubmitPrompt`, `preToolUse` (Write) + optimize | branch-guard plugin JS | — | JS plugin: beforeModel, beforeTool, afterTool, afterRun | — | — |
| Rules file | `~/.claude/rules` | `AGENTS.md` | `.cursor/rules/*.mdc` | rules dir + `opencode.jsonc` instructions | — | `.clinerules` | yes / yes / — | AGENTS.md only |
| MCP registration | `claude mcp add` | `codex mcp add` | `~/.cursor/mcp.json` | `opencode.jsonc` | — | cline settings | yes | — |
| Skills discovery | yes | yes | yes | yes | — | — | — | — |
| Message delivery | hook push | MCP piggyback | MCP piggyback | MCP piggyback | — | MCP piggyback | MCP piggyback | — |
| Sandbox state mounts | `.claude` | yes | yes | yes | yes | — | — | Aider, Grok, Kimi |

Already universal regardless of hook support: git PATH shim branch guard,
worktree provisioning, claims and items, memory, artifacts, handoff, review
consensus, `agentflare git rewind` snapshots, provenance trailers, dashboard.

Gaps in the universal layer:

1. Codex `exec --json`, Gemini `--output-format json` and OpenCode `run
   --format json` exist upstream and are not mapped (`json_output_args`
   covers 2 of 6 headless agents).
2. Codex `exec resume`, Gemini `--resume`, OpenCode `--session` exist and
   are not mapped (`resume_arg` covers 2 of 6).
3. Cursor hooks expose `postToolUse`, `stop`, `afterFileEdit`, which would
   carry the completion gate and messaging; only three events are wired.
4. Turn/budget caps: Claude only. `hard_cap`/`idle_timeout` is the universal
   cap, but a cap hit is not surfaced as a typed outcome.
5. Permission postures are not equivalent (Codex `--full-auto` keeps its
   sandbox; Claude skip-permissions removes every gate). Prefer each agent's
   sandboxed-auto mode where one exists and document the difference.
6. Only Claude gets hook-pushed messages (`host_injects_context`); other
   hosts see messages only when they happen to call an agentflare tool.
7. Cost is tracked for Claude transcripts only; Codex/Gemini/OpenCode JSON
   results carry usage too once (1) lands.

---

## 5. Claude-Code-style features worth building universally

Ranked by leverage. Each names the universal mechanism agentflare already has.

| Feature (Claude Code) | Universal mechanism in agentflare | Status | Size |
|---|---|---|---|
| Role definitions (subagent frontmatter: tools, model, permission mode, max turns, skills, memory, system prompt) | A `RoleSpec` compiled per agent by the registry: `--agent`/`--agents` + `--allowedTools` + `--permission-mode` + `--append-system-prompt-file` for Claude; `--sandbox`/approval policy + system prompt file for Codex; `--yolo` + prompt for Gemini; `.cursor/rules` + `--force` for Cursor; system prompt only for the rest. Personas in `apps/*/personas` become the first RoleSpecs. | Not started | M |
| Hooks (PreToolUse deny, PostToolUse evidence, Stop messaging) | Run the hook pipeline inside the MCP gateway (`tool(execute)`) so any agent that routes tool calls through agentflare gets the branch guard, completion gate, coaching and messages; keep native hooks where the host has them (`mcp_tool` hooks on Claude make this one code path). | Partial (gateway exists, hooks not run in it) | M |
| Structured output (`--json-schema`) | One schema per role; `--json-schema` on Claude, JSON-mode flags on Codex/Gemini/OpenCode, validating retry for agents without one. Removes marker parsing everywhere. | Not started | M |
| Permission modes / tool policy | `RoleSpec.tools` compiled to `--allowedTools`/`--disallowedTools` (Claude), approval policy (Codex), `--auto` deny list (OpenCode), and to the gateway's allow-list for everyone else; `plan` mode wired to `submit_plan`/`approve_plan`. | Not started | M |
| Compaction survival (`PostCompact`, `/compact`) | agentflare-side transcript summary written to memory on job checkpoints and re-injected on `SessionStart source=compact`/`PostCompact` (Claude) or on the next prompt-submit hook (Cursor/Codex). Replaces the no-op `optimize context`. | Regressed | M |
| Skills | Registry + intent injection already universal where a prompt hook exists; for hook-less agents inject via the `skill` tool result and rules files. | Mostly done | S |
| Memory (auto memory bridge) | agentflare memory is universal; add import/export with Claude's `MEMORY.md`. | Partial | S |
| Session naming and addressability | Name every headless session `<item>:<role>:<round>` (`--name` on Claude, session ids elsewhere) and store it on the item so transcripts, cost and resume are one lookup. | Not started | S |
| Push messaging | `--channels` on Claude; polling delivery piggybacked on gateway results elsewhere (exists). | Partial | S |
| Failover on API errors | `StopFailure` hook on Claude; stdout classification elsewhere (exists). | Partial | S |
| Checkpoints / rewind | `agentflare git rewind` + snapshots. | Done | — |
| Plan mode | `submit_plan/approve_plan` + `--permission-mode plan` during planning. | Partial | S |
| Cost and status visibility | Dashboard is universal; status line is Claude-only; Cursor/OpenCode have status surfaces worth wiring. | Partial | S |
| Scheduled work (Routines, `/loop`) | `agentflare-jobs` + daemon: add `agentflare job schedule` (cron) so Claude is not the only timer. | Partial | M |
| PR steward | `flare_git pr_wait` + `check_merge` cover the poll; a `pr_watch` job that re-dispatches on CI failure or review comments completes it for every agent. | Partial | M |
| Plugins | One manifest generating `.claude-plugin/`, `.codex-plugin/` and the Cline plugin. | Not started | M |

---

## 6. Roadmap

**P0: make the headless worker build on Claude Code**

1. Fix the `--output-format` collision and the last-line fallback (§2.3);
   add a live-shaped test; verify `agent_sessions` gets a session id.
2. Introduce `RoleSpec` in `build_extra_args` (`src/cli/work.rs:241`) and
   `real_agent_send_hook` (`src/work_item_pipeline.rs:725`); registry gains
   `role_args(agent, &RoleSpec)`, Claude first: `--append-system-prompt-file`,
   `--allowedTools`/`--disallowedTools`, `--permission-mode`, `--effort`,
   `--name`, `--json-schema`.
3. Replace marker parsing in the SDD loop with schema verdicts; parse the
   full stream-json `result` (usage, `num_turns`, `subtype`).
4. Job-scoped `--settings` and `--mcp-config` files generated under the
   worktree (Apps already do the `.mcp.json` half); stop depending on the
   user's `~/.claude` for jobs. Keep the plugin path (§6.10) as the fallback
   for `disableSideloadFlags` environments.
5. Run personas as agents: `claude -p --agent <persona>` per step, or
   `--agents` JSON; rewrite `apps/auto-company/workflow.json` prompts.

**P1: hooks, settings and hygiene**

6. `SessionStart` reads `source`; add `PostCompact` re-injection; delete or
   implement `PreCompact`; fix README/AGENTS.md wording for `optimize context`.
7. Replace `--mode` with `--permission-mode` in `run_launch_env`.
8. Honour `CLAUDE_CONFIG_DIR` in `src/paths.rs`; make auth isolation use it
   instead of `HOME`; make `statusLine` writes additive; make `uninstall`
   remove everything `init` wrote.
9. Add `PermissionRequest` (programmatic approvals), `StopFailure`
   (failover), `Notification` (channel routing), `SubagentStop` (reviewer
   evidence), `TaskCompleted` (item sync). Move per-event work to
   `mcp_tool` hooks where the logic already lives in the MCP server.
10. Express the branch guard and destructive-command rules also as
    `permissions.deny` in a project `.claude/settings.json`; set
    `attribution.commit: false`.
11. Auto-memory bridge; `.claude/rules` with `paths:` for language-scoped rules.

**P2: distribution and interop**

12. Publish a Claude plugin and marketplace entry generated from the same
    manifest as `.codex-plugin`.
13. `claude mcp serve` adapter so non-Claude agents can call a Claude worker;
    `--channels` push delivery.
14. GitHub Action recipe (`agentflare work` on label) and a Routines recipe
    for Claude Code on the web.
15. Map `json_output_args` / `resume_arg` for Codex, Gemini, OpenCode; type
    the cap-hit outcome.

---

## 7. Bugs and documentation drift found during the audit

- Double `--output-format` on `agentflare work` dispatch (§2.3):
  `src/agent_launch.rs:847`, `src/cli/work.rs:254`.
- `agentflare run --mode` passes `--mode` to `claude`
  (`src/agent_launch.rs:101-103`); Claude Code's flag is `--permission-mode`.
- `wire_optimize_claude_code` overwrites an existing `statusLine`
  (`src/init.rs:665-671`).
- `uninstall` leaves `PreCompact`, `PostToolUse`, `PostToolUseFailure`,
  `SubagentStart`, `statusLine`, `permissions.allow`, `env.BASH_ENV` and the
  `flare-docs.md`/coaching rule files (`src/uninstall.rs:55-110`).
- README "Flare optimize module" and AGENTS.md describe `optimize context` as
  PreCompact-driven compaction; the handler is a no-op (`src/hook.rs:593-608`).
- `src/cli/work.rs:267` warns about `--max-cost-usd` while the flag emitted
  is `--max-budget-usd`.
- `crates/agent-registry/src/registry.rs:102-123` says `AgentSpec` and
  `REGISTRY` are "not yet consumed outside tests"; they are the launch path.
- `docs-site/src/content/docs/compare.md:13` links lean-ctx to
  `github.com/getappz/lean-ctx`; upstream is `yvgude/lean-ctx`.
- `README.md:349` "What Gets Created" lists 3 rule files; `init` writes 5
  plus coaching rules.
