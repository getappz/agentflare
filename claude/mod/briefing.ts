import type { EngineInterface, On } from 'claude-code'
import type { AutopilotClient } from './mcp'

const SECTION_ID = 'agentflare:briefing'
const ETAG = { plugin: 'agentflare', key: 'briefingEtag' } as const

type BriefingContext = { client: () => AutopilotClient | null; agent: string }

// Module state resets on hot reload while $.state survives, so the stored etag is only sent
// together with text we still hold; otherwise an `unchanged` reply would leave us with no text.
// (Module scope, not a registerBriefing return value: the kit's static check forbids keeping what
// a function taking `on` returns, so the refresh entry point is a separate export.)
let text: string | null = null
let etag: string | null = null

/** Re-fetches the briefing with the stored etag; invalidates the cached system prompt (prompt.section; prompt.compose is not invalidatable) only when the etag changed. */
export async function refreshBriefing($: EngineInterface, { client, agent }: BriefingContext): Promise<void> {
  const c = client()
  if (!c) return
  try {
    const [cwd, session_id, stored] = await Promise.all([$.session.cwd(), $.session.id(), $.state.get(ETAG)])
    const sent = text !== null ? stored.value : undefined
    const reply = await c.briefing({ cwd, session_id, agent, ...(sent ? { etag: sent } : {}) })
    if (reply.etag === etag) return
    if (reply.text !== undefined) text = reply.text
    etag = reply.etag
    await $.state.set(ETAG, reply.etag)
    $.ui.invalidate('prompt.section')
  } catch {
    // Keep the previous text; a briefing failure must never disturb the session.
  }
}

export function registerBriefing(on: On, { client }: BriefingContext): void {
  on('prompt.compose', async ($, e, next) => {
    const r = await next(e)
    if (!client() || text === null) return r
    return { ...r, sections: [...r.sections, { id: SECTION_ID, text, scope: 'session' as const }] }
  })
}
