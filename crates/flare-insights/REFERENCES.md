# flare-insights — References

All external OSS projects, docs, and APIs referenced to build `flare-insights` (`crates/flare-insights`).

## 1. Local dashboards — read / replay / search (primary inspiration)

| Project | URL | What we reused |
|---------|-----|----------------|
| **agent-trail** (camtrik) | https://github.com/camtrik/agent-trail | Unified dashboard for Claude Code, Codex, OpenCode, OpenClaw, Qoder — token/cost, FTS, subagent tree, `GET /api/v1/sessions/search`, `local-session-search` skill |
| **agentsview** (jfan-nux) | https://github.com/jfan-nux/agentsview | Go + Svelte5 + SQLite FTS5, file watcher, live SSE, heatmap, `~/.claude/projects`, `~/.codex/sessions`, `~/.local/share/opencode` parsers |
| **agentsview** (itga fork) | https://github.com/itga/agentsview | Adds Copilot CLI / Amp / VSCode Copilot adapters — same FTS5 schema |
| **agent-lens** (naimjeem) | https://github.com/naimjeem/agent-lens | 8-agent dashboard (Claude/Codex/Gemini/OpenCode/Kimi/Cursor/Antigravity/Copilot) — cache hit-rate, OpenRouter pricing |
| **codedash** (dimstunt) | https://github.com/dimstunt/codedash | Browser dashboard + CLI (`handoff`/`convert`/`search`/`stats`/`export`), trigram + full-text, handoff verbosity levels |
| **codedash** (k00lagin fork) | https://github.com/k00lagin/codedash | Same — verified `handoff`/`convert` semantics |
| **vscode-extension-opencode-claude-code-monitor** (janzofx) | https://github.com/janzofx/vscode-extension-opencode-claude-code-monitor | VS Code webview, `active/idle/completed/waiting` lifecycle, `127.0.0.1` hook server, 7-day retention, fleet filtering |
| **opensync** (waynesutton) | https://github.com/waynesutton/opensync | Cloud-synced dashboards + sync plugins (`opencode-sync-plugin`, `claude-code-sync`, `codex-sync`, `cursor-open-sync`, `droid-sync`) — REST `POST /sync/session`, eval export (DeepEval/OpenAI JSONL) |
| **Claude-Code-Agent-Monitor** (hoangsonww) | https://github.com/hoangsonww/Claude-Code-Agent-Monitor | Node+React+SQLite+WebSocket, Kanban (Working/Waiting/Completed/Error/Abandoned), `awaiting_reason`, MCP server `mcp/` |
| **Cogpit** | https://cogpit.dev/ | Open-source control room for Claude Code (SDK) + Codex (App Server) — live turns/tools/diffs, approve/interrupt/steer/rewind/branch, worktree↔PR link |

## 2. Observability / tracing — export sinks

| Project | URL | What we reused |
|---------|-----|----------------|
| **Langfuse** (self-host) | https://github.com/langfuse/langfuse | OSS LLM engineering platform (ClickHouse+Postgres+Redis), tracing model `Turn trace → Generation spans → Tool spans`, self-host `https://cloud.langfuse.com` |
| **Langfuse — Claude Code tracing** | https://langfuse.com/integrations/developer-tools/claude-code | Stop-hook pipeline, `TRACE_TO_LANGFUSE=true` gate, `LANGFUSE_PUBLIC_KEY`/`SECRET_KEY` |
| **Langfuse — claude-observability-plugin** | https://github.com/langfuse/claude-observability-plugin | Marketplace plugin `claude plugin add langfuse/Claude-Observability-Plugin` — reference hook impl |
| **Langfuse — Tracing coding agents guide** | https://langfuse.com/resources/engineering/coding-agent-tracing | Mechanism table (Claude Code Stop hook, Codex plugin hooks, Copilot OTel, Cursor/Kiro/OpenCode) — pricing for `TRACE_TO_LANGFUSE` |
| **Helicone** | https://github.com/helicone/helicone | OSS AI Gateway + observability (100+ models, cost/latency, sessions), OTLP, `docs.helicone.ai` |
| **claude-code-langfuse-template** (doneyli) | https://github.com/doneyli/claude-code-langfuse-template | Self-hosted vs Cloud template, incremental state tracking, `settings-examples/global-settings-uv.json` |
| **claude-code-observability** (lifegenieai) | https://github.com/lifegenieai/claude-code-observability | OTEL bridge `lainra/claude-code-telemetry` `localhost:4318` → Langfuse |
| **agent-exporter-to-langfuse** (aliyun) | https://github.com/aliyun/agent-exporter-to-langfuse | Plug-and-play exporter for Claude/Qoder/OpenCode/Codex/Cursor/Pi via per-agent hooks + `langstash` buffer daemon |
| **langfuse-coding-agents** (BAEM1N) | https://github.com/BAEM1N/langfuse-coding-agents | Monorepo for 5 runtimes (Claude Code, Codex 0.128+, oh-my-codex, OpenCode, Gemini) — shared `langfuse_hook.py` pipeline |

## 3. Agent runtimes — file formats we parse

| Runtime | Docs / source path | Notes |
|---------|-------------------|-------|
| **Claude Code** | `~/.claude/projects/**/*.jsonl` | `type: user/human/assistant`, `message.usage {input_tokens, output_tokens, cache_read_input_tokens}`, `message.content[].type=="tool_use"` — our `ingest/claude.rs` handles both top-level and nested `message` |
| **Codex CLI** | `~/.codex/sessions/2025/12/27/rollout-*.jsonl` | `type: session_meta/response_item`, `payload.role`, `payload.type=="function_call"` — `ingest/codex.rs` max_depth 5 |
| **OpenCode** | `~/.local/share/opencode/opencode.db` (SQLite) | Tables `session`, `message`, `part` (not `sessions`) — dynamic `column_names()` map handles `project_id`/`projectID`, `opencode.rs` |
| **Cursor** | `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` (`cursorDiskKV`) — from `agent-lens` | Not yet ingested (stub) |
| **Gemini CLI** | `~/.gemini` | 11-event hook system — future `ingest/gemini.rs` |
| **Kimi / Antigravity / Qoder / Pi / Droid** | `~/.kimi`, `~/.gemini/antigravity`, etc. — from `agent-trail`/`agent-lens` | Reserved adapters |

## 4. Internal — agentflare context

| Doc | Path |
|-----|------|
| Flare optimize module | `AGENTS.md` — `agentflare optimize` (output/code/context/runtime) |
| lean-ctx | `~/.config/opencode/skills/lean-ctx` — `ctx_*` shadow mode, `ctx_compose` first |
| Cargo target-dir isolation | `AGENTS.md` — `CARGO_TARGET_DIR` stripping, `sccache` |
| Workspace hack | `agentflare-workspace-hack` |
| Existing crates | `crates/flare-output`, `flare-process`, `flare-workflow`, `agentflare-store`, `flare-search-kit` — patterns copied for `flare-insights` |

## 5. Direct URLs (plain list for citation)

```
https://github.com/camtrik/agent-trail
https://github.com/janzofx/vscode-extension-opencode-claude-code-monitor
https://github.com/waynesutton/opensync
https://www.opensync.dev/
https://github.com/waynesutton/opencode-sync-plugin
https://github.com/waynesutton/claude-code-sync
https://github.com/waynesutton/codex-sync-plugin
https://github.com/dimstunt/codedash
https://github.com/k00lagin/codedash
https://github.com/naimjeem/agent-lens
https://github.com/jfan-nux/agentsview
https://agentsview.io/
https://github.com/itga/agentsview
https://github.com/hoangsonww/Claude-Code-Agent-Monitor
https://cogpit.dev/
https://github.com/langfuse/langfuse
https://langfuse.com/integrations/developer-tools/claude-code
https://github.com/langfuse/claude-observability-plugin
https://langfuse.com/resources/engineering/coding-agent-tracing
https://github.com/helicone/helicone
https://docs.helicone.ai/gateway/integrations/langfuse.md
https://github.com/doneyli/claude-code-langfuse-template
https://github.com/lifegenieai/claude-code-observability
https://github.com/lainra/claude-code-telemetry
https://github.com/aliyun/agent-exporter-to-langfuse
https://github.com/BAEM1N/langfuse-coding-agents
https://github.com/wesm/agent-session-viewer  # predecessor of agentsview
```

## 6. How references map to `flare-insights` modules

```
REFERENCES.md:1-2  → src/ingest/{claude,codex,opencode}.rs  (adapter per runtime)
         + watcher.rs (notify + poll, from agentsview/codedash)
       → src/store.rs (SQLite FTS5 trigram, from agentsview + agent-trail)
       → src/search.rs (FTS5 + trigram + hybrid via flare-search-kit)
       → src/analytics.rs (pricing tables from agent-lens/OpenRouter, daily_costs, heatmap)
       → src/replay.rs (turn→generation→tool span, from hoangsonww/Cogpit)
       → src/handoff.rs (verbosity minimal/standard/verbose/full, convert claude↔codex, from codedash)
       → src/export.rs (DeepEval/OpenAI/HTML/Gist/tar.gz, from opensync)
       → src/api.rs (REST + WS, 127.0.0.1 only, from agent-trail/agentsview)
       → src/cli/insights.rs (sync/list/show/search/stats/export/handoff/serve)
       → config.rs (env overrides CLAUDE_PROJECTS_DIR/CODEX_SESSIONS_DIR/OPENCODE_DIR, PricingTable)
```

*Last updated: 2026-08-26 — flare-insights v0.1.0*
