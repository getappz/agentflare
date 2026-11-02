import type { On } from 'claude-code'
import type { AutopilotClient } from './mcp'
import { MUTATING_TOOLS, OBSERVED_TOOLS, READ_ONLY_TOOLS } from './policy'

const EXCERPT_MAX = 2000
const UNAVAILABLE = 'agentflare MCP unavailable; mutating tool refused'

/** The last `max` chars of a tool result's text; an exit code is kept by the caller. */
export function excerpt(result: unknown, max = EXCERPT_MAX): string {
  const r = result as { text?: unknown; result?: { stdout?: unknown; stderr?: unknown } } | null
  // Core sets top-level `text`; a hook's own result carries only the Bash record { stdout, stderr }.
  const text = typeof r?.text === 'string'
    ? r.text
    : [r?.result?.stdout, r?.result?.stderr].filter((v): v is string => typeof v === 'string' && v !== '').join('\n')
  return text.slice(-max)
}

type GuardContext = {
  client: () => AutopilotClient | null
  agent: string
  // guardTimeoutMs is enforced in mcp.ts (connect), not here.
  options: { readOnlyAutoAllow: boolean; guardTimeoutMs: number }
}

export function registerGuard(on: On, { client, agent, options }: GuardContext): void {
  const mutating = new Set(MUTATING_TOOLS)
  const observed = new Set(OBSERVED_TOOLS)
  const tools = [...new Set([...MUTATING_TOOLS, ...OBSERVED_TOOLS])]

  on('tool.call', { tool: tools }, async ($, e, next) => {
    const c = client()
    const { tool, tool_use_id: _id, consent: _consent, ...tool_input } = e as { tool: string } & Record<string, unknown>
    const [cwd, session_id] = await Promise.all([$.session.cwd(), $.session.id()])

    if (mutating.has(tool)) {
      if (!c) return { deny: UNAVAILABLE }
      let verdict
      try {
        verdict = await c.check({ tool_name: tool, tool_input, cwd, session_id, agent })
      } catch {
        return { deny: UNAVAILABLE }
      }
      if (verdict.decision === 'deny') return { deny: verdict.reason ?? 'denied by agentflare guard' }
    }

    const r = await next(e)
    if (c && observed.has(tool)) {
      // The Bash record has no exit code; core marks a non-zero exit as isError. Never invent a 0.
      const reported = (r as { result?: { exitCode?: unknown } }).result?.exitCode
      const exitCode = typeof reported === 'number' ? reported : r.isError === true ? 1 : undefined
      // fire-and-forget: observe is evidence for later checks; a failure must not fail the tool.
      void c.observe({
        session_id, cwd, agent, tool_name: tool, tool_input,
        output_excerpt: excerpt(r),
        ...(exitCode !== undefined ? { exit_code: exitCode } : {}),
        ...(typeof tool_input.action === 'string' ? { item_action: tool_input.action, item_success: !r.isError } : {}),
      }).catch(() => {})
    }
    return r
  }).catch(($, e, next) => (next.called ? next(e) : { deny: 'agentflare guard failed closed' }))

  on('tool.check', { tool: [...READ_ONLY_TOOLS] }, ($, e, next) =>
    options.readOnlyAutoAllow ? { decision: 'allow' } : next(e),
  ).catch(() => ({ decision: 'deny' }))
}
