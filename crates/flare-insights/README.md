# flare-insights

Unified local-first observability for AI coding agent sessions — `agentflare`'s superset of:

- `camtrik/agent-trail` · `jfan-nux/agentsview` · `naimjeem/agent-lens` · `dimstunt/codedash`
- `janzofx/vscode-extension-opencode-claude-code-monitor` · `waynesutton/opensync`
- `hoangsonww/Claude-Code-Agent-Monitor` · `cogpit` · `langfuse`/`helicone` hooks

## Features (consolidated)

| Layer | What |
|-------|------|
| **Ingest** | Adapters for Claude Code (`~/.agentflare/projects/*.jsonl` + hook), Codex (`~/.codex/sessions`), OpenCode (`opencode.db` SQLite), Cursor/Gemini/Copilot/Amp/Pi/Qoder/OpenClaw/Kimi — `CLAUDE_PROJECTS_DIR` env overrides, file watcher + poll, fail-open |
| **Store** | SQLite `observatory.db` with FTS5 trigram index, WAL, `turns_fts` triggers, retention prune |
| **Replay** | `Turn → Generation → ToolSpan` timeline, tool payload renderers (Bash terminal, Read chip, Edit diff, Grep matches, MCP kv), subagent tree, file timeline, queued-input placement |
| **Search** | FTS5 + trigram fuzzy + hybrid-ready (`flare-search-kit`), `GET /api/search?q=` + `local-session-search` skill pattern |
| **Analytics** | Tokens `in/out/cache_read/cache_write/reasoning` per turn/day/model, OpenRouter pricing, `cost` col preferred, daily cost line, stacked bars, heatmap (weekday×hour), donuts, cache hit-rate, top expensive |
| **Organize** | star/pin, tags, deepeval/openai/jsonl/html export, `claude↔codex` convert, handoff doc `minimal(3)/standard(10)/verbose(20)/full(50)` |
| **Live** | WS push (poll fallback), Kanban `Active/Waiting/Completed/Error/Abandoned`, 127.0.0.1 only, no telemetry |
| **Export** | Langfuse/Helicone OTLP sink (`TRACE_TO_LANGFUSE=true` gate), DeepEval/OpenAI Evals/Gist/`tar.gz` bundle |

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
  api.rs          # (feature = "api") 127.0.0.1 REST+WS scaffold
```

## CLI (`agentflare insights`)

```bash
agentflare insights sync                          # scan all sources → observatory.db
agentflare insights sync --prune-days 7           # + prune older than 7d
agentflare insights list --limit 20               # recent first
agentflare insights list --source claude_code --json
agentflare insights show <session_id> --json
agentflare insights search "websocket reconnection" --limit 10
agentflare insights stats --json                  # tokens/cost/cache + by_day
agentflare insights export --format deepeval --output eval.jsonl
agentflare insights handoff <id> --target codex --verbosity full
agentflare insights serve --port 3456             # 127.0.0.1 REST + WS
```

Uses `agentflare-flare-insights` as lib: `flare_insights::store::InsightsStore::open_in_memory()` etc.

## Design decisions

- **Local-first**: reads only, never writes to `~/.claude` transcripts; binds 127.0.0.1.
- **Fail-open**: one adapter crash never blocks others (opensync/cogpit parity).
- **Pricing**: OpenRouter live refresh fallback, per-agent defaults, `cost` col preferred when present (OpenCode).
- **No new dep**: `rusqlite/bundled`, `chrono`, `serde`, `tokio`, `notify`, `walkdir` — all already in workspace.

## Next steps

- [ ] Flesh `api.rs` axum + `tokio-tungstenite` WS (behind `api` feature)
- [ ] Add `cursor/gemini/copilot` adapters (SQLite/protobuf)
- [ ] Hook server `127.0.0.1:4318` OTEL bridge (langfuse parity)
- [ ] Frontend Svelte5 dashboard (reuse agentsview assets)
