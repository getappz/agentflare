import type { EngineInterface, On } from 'claude-code'
import { normalise, type AutopilotClient } from './mcp'
import { CHECKED_TOOLS, GATEWAY_TOOL, MUTATING_TOOLS, OBSERVED_TOOLS, READ_ONLY_TOOLS, SELF_TOOLS } from './policy'

const EXCERPT_MAX = 2000
const UNAVAILABLE = 'agentflare MCP unavailable; mutating tool refused'
const GATEWAY_UNREADABLE = "agentflare guard unavailable: this gateway call's inner tool could not be read back from the transcript; refused"
const NO_ALLOW = 'agentflare guard returned no allow verdict; refused'
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
 * assistant tool_use block with this id still holds it. Null when it cannot be found; the caller
 * then refuses the call. A subagent's call carries `agentId` and is read from that agent's own
 * transcript; one the session cannot read answers `{ deny }` (SessionMessagesDeny) and is refused.
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
    if (!recoveryLogged) $.ui.log(`agentflare: gateway input recovery failed, refusing the call: ${String(err)}`)
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
  const checked = new Set([...MUTATING_TOOLS, ...CHECKED_TOOLS])
  const observed = new Set(OBSERVED_TOOLS)
  const readOnly = new Set(READ_ONLY_TOOLS)

  // Unmatched, narrowed inside: agentflare's tools reach the model as `mcp__<server>__X` under
  // whichever name the server runs (`flare`, the plugin's `plugin_agentflare_agentflare`, a user's own
  // key), so names are normalised to `mcp__flare__X` before the sets and before Rust sees them.
  on('tool.call', async ($, e, next) => {
    const c = client()
    const { tool: raw, tool_use_id, consent: _c, agentId, requestMeta: _m, ...flat } = e as { tool: string } & Record<string, unknown>
    const tool = normalise(raw, c?.server ?? null)
    if (!checked.has(tool) && !observed.has(tool)) return next(e)

    // Only a gateway `execute` loses an argument (its inner `tool`); recover it from the transcript,
    // let the flattened args (what actually runs) win, and refuse when it cannot be recovered: without
    // the inner tool, Rust would see a bare gateway call and allow it.
    let tool_input: Record<string, unknown> = flat
    if (tool === GATEWAY_TOOL && flat.action === 'execute') {
      const recovered = typeof tool_use_id === 'string' ? await originalInput($, tool_use_id, agentId) : null
      if (!recovered) return { deny: GATEWAY_UNREADABLE }
      tool_input = { ...recovered, ...flat }
    }
    const [cwd, session_id] = await Promise.all([$.session.cwd(), $.session.id()])

    // Same order and reasons as the PreToolUse hook (redirect, claim, coaching, completion gate),
    // all in Rust. Without a connection only mutating tools are refused; with one, anything but an
    // explicit allow refuses (a failed, timed-out or malformed check included).
    if (checked.has(tool) && (c || mutating.has(tool))) {
      if (!c) return { deny: UNAVAILABLE }
      let verdict: unknown
      try {
        verdict = await c.check({ tool_name: tool, tool_input, cwd, session_id, agent })
      } catch {
        return { deny: UNAVAILABLE }
      }
      const v = (typeof verdict === 'object' && verdict !== null ? verdict : {}) as { decision?: unknown; reason?: unknown }
      if (v.decision !== 'allow') return { deny: typeof v.reason === 'string' && v.reason ? v.reason : NO_ALLOW }
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

  // One unmatched hook (names are normalised, so a static matcher cannot list every spelling):
  // 1. Read-only tools are allowed when readOnlyAutoAllow is on.
  // 2. The mod's own $.mcp.call rides the tool path and raises tool.check like any call, so headless
  //    auto mode's classifier refuses it and the guard then fails closed on every edit. Allowing it is
  //    not a policy decision: autopilot, memory and get_routing_suggestion on agentflare's server are
  //    the mod's control plane (verdicts, briefings, records), never a repo mutation. Only calls this
  //    plugin raised (next.origin) are allowed; the model's own calls to them keep the session's rules.
  //    No client is needed yet: connect's fallback health check runs before one exists.
  on('tool.check', ($, e, next) => {
    const tool = normalise(e.tool, client()?.server ?? null)
    if (options.readOnlyAutoAllow && readOnly.has(tool)) return { decision: 'allow' }
    const own = next.origin.plugin === PLUGIN && SELF_TOOLS.some(t => tool === `mcp__flare__${t}`)
    return own ? { decision: 'allow' } : next(e)
  }).catch(() => ({ decision: 'deny' }))
}
