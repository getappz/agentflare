import type { EngineInterface, On } from 'claude-code'
import type { AutopilotClient } from './mcp'

const SECTION_ID = 'agentflare:briefing'
const ETAG = { plugin: 'agentflare', key: 'briefingEtag' } as const
// The server's briefing is the session-start notice, ANSI colors included. Sent bare, the model
// reads it as more hook output and cannot tell the section apart (probe C3), so it is headed and
// the terminal escapes are dropped.
const HEADING = '# agentflare briefing'
const ANSI = /\x1b\[[0-9;]*m/g

/** The briefing as the system prompt carries it. */
export function sectionText(raw: string): string {
  return `${HEADING}\n\n${raw.replace(ANSI, '').trim()}`
}

type BriefingContext = { client: () => AutopilotClient | null; agent: string }

// Module state resets on hot reload; the etag is only sent together with text we still hold,
// otherwise an `unchanged` reply would leave us with no text.
// (Module scope, not a registerBriefing return value: the kit's static check forbids keeping what
// a function taking `on` returns, so the refresh entry point is a separate export.)
let text: string | null = null
let etag: string | null = null

/** Re-fetches the briefing; invalidates the cached system prompt (prompt.section; prompt.compose is not invalidatable) only when the etag changed. */
export async function refreshBriefing($: EngineInterface, { client, agent }: BriefingContext): Promise<void> {
  const c = client()
  if (!c) return
  try {
    const [cwd, session_id] = await Promise.all([$.session.cwd(), $.session.id()])
    const reply = await c.briefing({ cwd, session_id, agent, ...(text !== null && etag ? { etag } : {}) })
    if (typeof reply.etag !== 'string' || reply.etag === etag) return
    if (typeof reply.text !== 'string') return
    text = sectionText(reply.text)
    etag = reply.etag
    // Informational until the text is persisted too: after a reload `text` is null, so nothing is sent.
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

  // After $.ui.invalidate('prompt.section') the engine re-asks this hook for the section's text.
  on('prompt.section', { name: SECTION_ID }, async ($, e, next) => (text === null ? next(e) : { text }))
}
