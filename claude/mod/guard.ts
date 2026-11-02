import type { On } from 'claude-code'
import type { AutopilotClient } from './mcp'
import { MUTATING_TOOLS, OBSERVED_TOOLS, READ_ONLY_TOOLS } from './policy'

const EXCERPT_MAX = 2000
const UNAVAILABLE = 'agentflare MCP unavailable; mutating tool refused'

/** The last `max` chars of a tool result's text; an exit code is kept by the caller. */
export function excerpt(result: unknown, max = EXCERPT_MAX): string {
  const text = (result as { text?: unknown } | null)?.text
  return typeof text === 'string' ? text.slice(-max) : ''
}

type GuardContext = {
  client: () => AutopilotClient | null
  agent: string
  options: { readOnlyAutoAllow: boolean; guardTimeoutMs: number }
}

export function registerGuard(on: On, { client, agent, options }: GuardContext): void {
  const mutating = new Set(MUTATING_TOOLS)
  const observed = new Set(OBSERVED_TOOLS)
  const tools = [...new Set([...MUTATING_TOOLS, ...OBSERVED_TOOLS])]

  on('tool.call', { tool: tools }, async ($, e, next) => {
    const c = client()
    const { tool, ...tool_input } = e as { tool: string } & Record<string, unknown>
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
      const exitCode = (r as { result?: { exitCode?: unknown } }).result?.exitCode
      // fire-and-forget: observe is evidence for later checks; a failure must not fail the tool.
      void c.observe({
        session_id, cwd, agent, tool_name: tool, tool_input,
        output_excerpt: excerpt(r),
        ...(typeof exitCode === 'number' ? { exit_code: exitCode } : {}),
        ...(tool === 'mcp__flare__item' ? { item_action: String(tool_input.action ?? ''), item_success: !r.isError } : {}),
      }).catch(() => {})
    }
    return r
  }).catch(($, e, next) => (next.called ? next(e) : { deny: 'agentflare guard failed closed' }))

  on('tool.check', { tool: [...READ_ONLY_TOOLS] }, ($, e, next) =>
    options.readOnlyAutoAllow ? { decision: 'allow' } : next(e),
  ).catch(() => ({ decision: 'deny' }))
}
