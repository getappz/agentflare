# flare-insights

Unified local-first observability for AI coding agent sessions, drawing on:

- `camtrik/agent-trail` · `jfan-nux/agentsview` · `naimjeem/agent-lens` · `dimstunt/codedash`
- `janzofx/vscode-extension-opencode-claude-code-monitor` · `waynesutton/opensync`
- `hoangsonww/Claude-Code-Agent-Monitor` · `cogpit` · `langfuse`/`helicone` hooks

## Features (consolidated)

| Layer | What |
|-------|------|
| **Ingest** | Adapters for Claude Code (`~/.claude/projects` JSONL), Codex (`~/.codex/sessions`), OpenCode (`opencode.db` SQLite), Cursor (`state.vscdb` composerData/bubbleId), and Antigravity transcripts; environment source overrides, file watcher + poll |
| **Store** | SQLite `observatory.db` with FTS5 trigram index, WAL, `turns_fts` triggers, retention prune |
| **Replay** | Turn, tool-call, and file-event timeline; retained subagent metadata in JSON session output |
| **Search** | FTS5 trigram substring search, short-query fallback, retained file/tool evidence, source/project filters, `GET /api/search?q=` |
| **Analytics** | Token/cost/cache totals and source/project/day/model breakdowns; provider costs where recorded, otherwise a bundled pricing estimate with coverage metadata |
| **Handoff** | Handoff documents at `minimal`, `standard`, `verbose`, and `full` verbosity; library session conversion |
| **Live** | File watching and polling, HTTP API and polling dashboard on 127.0.0.1 only; no telemetry |
| **Export** | JSON, JSONL, HTML, DeepEval, and OpenAI Evals; optional source filter |

## Crate layout

```
src/
  lib.rs          # re-exports
  model.rs        # Session/Turn/ToolCall/Subagent unified schema
  config.rs       # InsightsConfig + PricingTable (15/75 Opus, 3/15 Sonnet, 0.55/2.2 Kimi)
  store.rs        # rusqlite bundled, FTS5, triggers
  search.rs       # sanitize_fts_query, source/project filters
  analytics.rs    # compute_analytics, top_expensive, heatmap
  replay.rs       # SessionReplay builder
  handoff.rs      # handoff_doc + convert_session_json
  export.rs       # ExportFormat::{Json,Jsonl,Html,Deepeval,OpenAiEvals}
  ingest/
    mod.rs        # Adapter trait + IngestManager::scan_all (fail-open)
    claude.rs     # JSONL walk  depth 4
    codex.rs      # JSONL parent-child
    opencode.rs   # SQLite opencode.db
    watcher.rs    # notify + mpsc channel
  api.rs          # 127.0.0.1 HTTP API and polling dashboard
```

## CLI (`agentflare insights`)

```bash
agentflare insights sync                          # scan all sources → observatory.db
agentflare insights sync --prune-days 7           # + prune older than 7d
agentflare insights list --limit 20               # recent first
agentflare insights list --source claude_code --json
agentflare insights show <session_id> --json
agentflare insights search "websocket reconnection" --limit 10
agentflare insights stats --json                  # tokens/cost/cache + by_day_updated
agentflare insights export --format deepeval --output eval.jsonl
agentflare insights handoff <id> --target codex --verbosity full
agentflare insights serve --port 3456             # 127.0.0.1 HTTP + dashboard
```

Uses `agentflare-flare-insights` as lib: `flare_insights::store::InsightsStore::open_in_memory()` etc.

## MCP (`insights` tool)

The MCP tool exposes every Insights command through a structured `action`:

| Actions | Parameters and behavior |
|---------|-------------------------|
| `search`, `list` | `query` (search), `source`, `project`, `limit`, `offset` |
| `recall`, `show` | `session_id`, `source`, `project`, `turn_start`, `turn_limit`, `max_chars`; `show` also pages retained file/subagent metadata with `limit`/`offset` |
| `stats` | `source`, `project`, optional `since_days`; includes coverage and pricing basis |
| `forensics`, `discover` | `since_days`, `project`, `limit`, `offset`, `max_chars`; scalar totals are unchanged, evidence/examples are capped at three with omissions reported. Forensic value arrays have separate `value_limit`/`value_offset` pages and `value_pagination` metadata. Forensics also accepts `source`/`session_id`; discovery accepts `min_sessions`/`min_count` |
| `sync` | Explicitly refresh the index; `force` re-reads unchanged inputs. `prune_days` requires `yes=true` and `project="*"` because retention affects the whole index |
| `export` | `format`, `source`, `project`, optional `session_id`, `limit`/`offset`. Returns a bounded text preview; `output` with `yes=true` writes the full selected page, redacted, and refuses to overwrite files |
| `handoff` | `session_id`, `source`, `project`, `target`, `verbosity`, `max_chars`; generates text without publishing it |
| `doctor` | Inspects index readability and all five adapter source paths without creating an index |
| `reflect` | `operation=scan/list/show/accept/reject/snooze/apply`, `proposal_id`, `since_days` (scan/snooze), `status` (list). Decisions require `yes=true`; apply previews unless `yes=true`. `global` enables global config targets. Uses the existing CLI safeguards |
| `watch`, `serve` | `operation=start/status/stop`, `interval_secs` (watch), `port` (serve). Managed child processes stop when the MCP server exits normally; status reports process state and bounded startup output, with `log_truncated` when later/oversized lines were omitted. Use the CLI for continuous log output |

Read actions use the existing index without rescanning it. Project scope defaults
to the current directory (`sync` defaults to its Git root); `project="*"` selects
all projects. Reflect operates on the global proposal ledger and requires
`project="*"`. Watch/serve perform global background ingestion and also require
that explicit scope when starting. Stop/status control only children started by
this MCP server, never unrelated processes. Long queries run outside the async
runtime so other MCP requests can proceed.
Report text budgets are shared evenly across returned findings/proposals, so a
long first draft cannot hide every later preview. Searches filled by indexed
conversation hits avoid retained-evidence scans; searches needing file/tool
evidence still scan those retained rows.
Recurring-request and correction mining excludes automatic stop-hook feedback
and local-command context wrappers.

```json
{"action":"sync"}
{"action":"forensics","since_days":7,"limit":5}
{"action":"reflect","operation":"apply","proposal_id":7,"project":"*"}
{"action":"serve","operation":"start","project":"*","port":3456}
{"action":"serve","operation":"stop"}
```

Run the isolated MCP protocol checks against a built executable with
`python crates/flare-insights/tests/mcp_smoke.py target/debug/agentflare`
(`target/debug/agentflare.exe` on Windows). They use temporary source logs,
memory/index databases, exports, proposal targets, and local server ports.

When an AI agent runs `insights sync`, it automatically selects the invoking
Git worktree's project root (the current folder outside Git). Session project
paths must match that root or a directory below it; sessions without project
metadata are left for a manual global sync. OpenCode and Cursor check project
metadata before loading messages or bubbles, and JSONL readers inspect project
hints before parsing the transcript. Manual terminal sync remains global.

Project and global syncs keep separate cursors, so syncing one project cannot
hide another project's history. Unchanged SQLite databases and WAL files are
skipped. A changed database still reloads the eligible sessions; this is not
row-level incremental ingestion. `--force` refreshes the selected scope.

## Design decisions

- **Local-first**: reads only, never writes to `~/.claude` transcripts; binds 127.0.0.1.
- **Partial failures**: a source scan failure does not prevent other sources ingesting, but the sync command exits nonzero.
- **Pricing**: bundled model pricing estimates; provider `cost` preferred when present (OpenCode), unknown models remain unpriced.
- **No new dep**: `rusqlite/bundled`, `chrono`, `serde`, `tokio`, `notify`, `walkdir` — all already in workspace.

## Next steps

- [ ] Flesh `api.rs` axum + `tokio-tungstenite` WS (behind `api` feature)
- [x] Cursor adapter (`state.vscdb` composerData/bubbleId)
- [ ] Add `gemini/copilot` adapters when a consumer asks (SQLite/protobuf; not advertised until then)
- [ ] Hook server `127.0.0.1:4318` OTEL bridge (langfuse parity)
- [ ] Frontend Svelte5 dashboard (reuse agentsview assets)
