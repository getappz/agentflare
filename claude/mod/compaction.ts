import type { On } from 'claude-code'
import type { AutopilotClient } from './mcp'

export type Current = { item?: { id: string; seq: number; title: string }; branch?: string; worktree?: string }

type CompactionContext = { client: () => AutopilotClient | null; agent: string; current: () => Current }

const KEEP =
  'Keep verbatim: the agentflare item id and seq, the branch and worktree path, whether tests and review are fresh, open decisions, and the next step.'

function minimalText({ item, branch }: Current): string {
  return `[agentflare checkpoint] item ${item?.seq ?? 'none'}; branch ${branch ?? 'unknown'}; gate state unknown; re-run the gate before item done.`
}

export function registerCompaction(on: On, { client, agent, current }: CompactionContext): void {
  on('session.compact', async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    const instructions = e.instructions ? `${e.instructions}\n\n${KEEP}` : KEEP
    const now = current()
    let text = minimalText(now)
    try {
      const c = e.trigger === 'precompute' ? null : client()
      if (c) {
        const [cwd, session_id] = await Promise.all([$.session.cwd(), $.session.id()])
        // Gate freshness is left out on purpose: the server's session record is the only source, absent means stale.
        const reply = await c.checkpoint({ session_id, cwd, agent, payload: { ...(now.item ? { item: now.item } : {}), ...(now.branch ? { branch: now.branch } : {}), ...(now.worktree ? { worktree: now.worktree } : {}), decisions: [], remaining: [] } })
        if (typeof reply.text === 'string' && reply.text) text = reply.text
      }
    } catch {
      // Keep the minimal text; a checkpoint failure must never block compaction.
    }
    const r = await next({ ...e, instructions })
    if (!r.messages) return r
    return { ...r, messages: [...r.messages, { role: 'user' as const, text, toolUses: [] }] }
  })
}
