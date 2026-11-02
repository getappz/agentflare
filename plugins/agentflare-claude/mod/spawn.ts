import type { On } from 'claude-code'

export const SUBAGENT_BRIEF =
  'Reach lean-ctx through the flare gateway — mcp__flare__tool(action="execute", server="leanctx", tool="<name>", args={...}); mcp__lean-ctx__* is never listed directly. Orient with ctx_compose(task=...) FIRST — one call replaces the search→read→search chain. Then prefer ctx_read (mode=lines:N-M / signatures) over Read, ctx_search (action="symbol" for definitions) over Grep, ctx_callgraph for callers, ctx_shell over Bash for foreground commands — they compress output heavily.'

const MARKER = 'Reach lean-ctx through the flare gateway'
const ROUTING_TIMEOUT_MS = 1500
const TIMED_OUT = Symbol('timed out')

type SpawnContext = { server: () => string | null; agent: string }

export function registerSpawn(on: On, { server, agent }: SpawnContext): void {
  on('agent.spawn', async ($, e, next) => {
    const prompt = e.prompt.includes(MARKER) ? e.prompt : `${SUBAGENT_BRIEF}\n\n${e.prompt}`
    let model = e.model
    const name = server()
    if (model === undefined && name) {
      try {
        // The reply is JSON in the first text block: { agent, model, effort, advisory }; model is null when unrouted.
        const reply = await Promise.race([
          $.mcp.call(name, 'get_routing_suggestion', { prompt: e.prompt, agent }),
          $.clock.sleep(ROUTING_TIMEOUT_MS).then(() => TIMED_OUT),
        ])
        const text = reply === TIMED_OUT || reply.isError ? undefined : reply.content.find(b => b.type === 'text')?.text
        const suggested = text === undefined ? undefined : (JSON.parse(text) as { model?: unknown }).model
        if (typeof suggested === 'string' && suggested) model = suggested
      } catch {
        // Routing is advisory; keep the caller's model (unset) on any failure.
      }
    }
    return next({ ...e, prompt, model })
  })
}
