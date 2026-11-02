import type { EngineInterface, On } from 'claude-code'
import { toolName, type AutopilotClient } from './mcp'
import { MUTATING_TOOLS, OBSERVED_TOOLS, READ_ONLY_TOOLS, SELF_TOOLS } from './policy'

const EXCERPT_MAX = 2000
const UNAVAILABLE = 'agentflare MCP unavailable; mutating tool refused'
const PLUGIN = 'agentflare'

/** The last `max` chars of a tool result's text; an exit code is kept by the caller. */
export function excerpt(result: unknown, max = EXCERPT_MAX): string {
  const r = result as { text?: unknown; result?: { stdout?: unknown; stderr?: unknown } } | null
  // Core sets top-level `text`; a hook's own result carries only the Bash record { stdout, stderr }.
  const text = typeof r?.text === 'string'
    ? r.text
    : [r?.result?.stdout, r?.result?.stderr].filter((v): v is string => typeof v === 'string' && v !== '').join('\n')
  return text.slice(-max)
}

let recoveryLogged = false

/**
 * The model's own input for an MCP call, read back from the transcript. tool.call flattens an MCP
 * tool's arguments beside the reserved `tool` key (claude-code.d.ts McpToolCallInputFallback), so a
 * gateway's own `tool` argument (`mcp__flare__tool { tool: "ctx_patch" }`) is lost in `e`; the
 * assistant tool_use block with this id still holds it. Null when it cannot be found.
 */
async function originalInput($: EngineInterface, id: string, agentId: unknown): Promise<Record<string, unknown> | null> {
  try {
    const messages = await $.session.messages(typeof agentId === 'string' ? { agentId, as: 'api' } : { as: 'api' })
    if (!Array.isArray(messages)) throw new Error(messages.deny)
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role !== 'assistant') continue
      const block = messages[i].content.find(b => b.type === 'tool_use' && b.id === id)
      if (block && typeof block.input === 'object' && block.input !== null) return block.input as Record<string, unknown>
    }
    throw new Error(`no tool_use block ${id}`)
  } catch (err) {
    if (!recoveryLogged) $.ui.log(`agentflare: MCP input recovery failed, checking the flattened input: ${String(err)}`)
    recoveryLogged = true
    return null
  }
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
    const { tool, tool_use_id, consent: _c, agentId, requestMeta: _m, ...flat } = e as { tool: string } & Record<string, unknown>
    const recovered = tool.startsWith('mcp__') && typeof tool_use_id === 'string' ? await originalInput($, tool_use_id, agentId) : null
    const tool_input = recovered ?? flat
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

  // The mod's own $.mcp.call rides the tool path and raises tool.check like any call, so headless auto
  // mode's classifier refuses it and the guard then fails closed on every edit. Allowing it is not a
  // policy decision: autopilot, memory and get_routing_suggestion on the connected agentflare server are
  // the mod's control plane (verdicts, briefings, records), never a repo mutation. Only calls this
  // plugin raised (next.origin) are allowed; the model's own calls to them keep the session's rules.
  // The name follows the server connect answered, so a deduplicated twin (`flare`) is covered.
  on('tool.check', ($, e, next) => {
    const c = client()
    const own = c !== null && next.origin.plugin === PLUGIN && SELF_TOOLS.some(t => toolName(c.server, t) === e.tool)
    return own ? { decision: 'allow' } : next(e)
  }).catch(() => ({ decision: 'deny' }))
}
