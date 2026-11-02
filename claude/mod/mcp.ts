import type { EngineInterface } from 'claude-code'

export type Decision = { decision: 'allow' | 'deny'; reason?: string; rule?: string }
export type Briefing = { etag: string; text?: string; unchanged?: true }
export type Checkpoint = { handoff_id: string; text: string }
export type NextWork = {
  item?: { id: string; seq: number; title: string; description: string; worktree?: string }
  claimed: boolean
  blocked_reason?: 'plan_approval' | 'claim_held' | 'none_ready'
}
export type Observed = { recorded: 'verification' | 'review' | 'diagnosis' | 'none' }

export type AutopilotClient = {
  check(a: { tool_name: string; tool_input: unknown; cwd: string; session_id: string; agent: string }): Promise<Decision>
  observe(a: { session_id: string; cwd: string; agent: string; tool_name: string; tool_input: unknown; exit_code?: number; output_excerpt: string; item_action?: string; item_success?: boolean }): Promise<Observed>
  briefing(a: { cwd: string; session_id: string; agent: string; etag?: string }): Promise<Briefing>
  checkpoint(a: { session_id: string; cwd: string; payload: unknown }): Promise<Checkpoint>
  nextWork(a: { agent: string; cwd: string; max_cost_usd: number; rate_limit_pct: number }): Promise<NextWork>
  health(): Promise<{ daemon: string; stale_binary: boolean; auth: string; mcp_version: string }>
}

const TIMED_OUT = Symbol('timed out')

/** Connects the plugin's own `agentflare` MCP server; null when it is not connected. */
export async function connect($: EngineInterface, timeoutMs: number): Promise<AutopilotClient | null> {
  const conn = await $.mcp.connect('agentflare')
  if (!conn.isConnected) return null
  const server = conn.server

  // Every action replies with JSON in the first text block; a slow server is cut off at timeoutMs.
  async function call<T>(action: string, args: Record<string, unknown> = {}): Promise<T> {
    const reply = await Promise.race([
      $.mcp.call(server, 'autopilot', { action, ...args }),
      $.clock.sleep(timeoutMs).then(() => TIMED_OUT),
    ])
    if (reply === TIMED_OUT) throw new Error(`autopilot ${action} timed out after ${timeoutMs}ms`)
    const text = reply.content.find(b => b.type === 'text')?.text
    if (reply.isError || text === undefined) throw new Error(`autopilot ${action} failed: ${text ?? 'no text content'}`)
    return JSON.parse(text) as T
  }

  return {
    check: a => call('check', a),
    observe: a => call('observe', a),
    briefing: a => call('briefing', a),
    checkpoint: a => call('checkpoint', a),
    nextWork: a => call('next_work', a),
    health: () => call('health'),
  }
}
