// Tool names the guard cares about. Names only; the logic lives in guard.ts.

export const READ_ONLY_TOOLS: readonly string[] = [
  'Read', 'Glob', 'Grep', 'LSP',
  'mcp__lean-ctx__ctx_read', 'mcp__lean-ctx__ctx_search', 'mcp__lean-ctx__ctx_glob',
  'mcp__lean-ctx__ctx_tree', 'mcp__lean-ctx__ctx_compose', 'mcp__lean-ctx__ctx_callgraph',
  'mcp__flare__search', 'mcp__flare__docs',
]

// Mirrors MUTATING_TOOLS in src/hook_redirect.rs, plus the two flare gateways that can change state.
export const MUTATING_TOOLS: readonly string[] = [
  'Write', 'write', 'Edit', 'edit', 'NotebookEdit', 'notebookedit', 'MultiEdit', 'multiedit',
  'patch', 'apply_patch',
  'mcp__lean-ctx__ctx_patch', 'mcp__lean-ctx__ctx_edit',
  'mcp__lean_ctx__ctx_patch', 'mcp__lean_ctx__ctx_edit',
  'mcp__flare__item', 'mcp__flare__tool',
]

// The tools the mod itself calls on its connected agentflare server (mcp.ts, capture.ts, spawn.ts):
// agentflare's own control plane, never a repo mutation. Bare names; guard.ts prefixes the server.
export const SELF_TOOLS: readonly string[] = ['autopilot', 'memory', 'get_routing_suggestion']

// Tools whose result is reported to the server as evidence after they run.
export const OBSERVED_TOOLS: readonly string[] = [
  'Bash', 'PowerShell', 'mcp__lean-ctx__ctx_shell', 'mcp__flare__tool', 'mcp__flare__item',
  'ReportFindings', 'mcp__flare__review',
]
