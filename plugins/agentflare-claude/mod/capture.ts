import type { EngineInterface, On } from 'claude-code'

const LABELS = ['decision', 'bugfix', 'discovery', 'none']
const MEMORY_TIMEOUT_MS = 3000
const TITLE_MAX = 120
const CONTENT_MAX = 2000
const TIMED_OUT = Symbol('timed out')

export type CaptureContext = {
  server: () => string | null
  options: { memoryCapture: 'auto' | 'ask' | 'off' }
  current: () => { item?: { id: string } }
}

export async function captureTurn(
  $: EngineInterface,
  e: { answer?: string; reason?: string; agentId?: string },
  { server, options, current }: CaptureContext
): Promise<void> {
  // e.answer is the assistant's final visible text, so tool-result blocks are never in it.
  const answer = e.answer?.trim()
  if (options.memoryCapture === 'off' || e.agentId !== undefined || e.reason !== 'answer' || !answer) return
  const name = server()
  if (!name) return
  try {
    const label = await $.model.classify(answer, LABELS)
    // Only a known label becomes a memory type and topic key.
    if (!label || label === 'none' || !LABELS.includes(label)) return
    if (options.memoryCapture === 'ask' && (await $.ui.ask(`Remember this as a ${label}?`, ['yes', 'no'])) !== 'yes') return
    const title = (answer.split('\n').find(l => l.trim()) ?? answer).trim().slice(0, TITLE_MAX)
    // No project source exists in the mod yet, so `project` is omitted.
    const reply = await Promise.race([
      $.mcp.call(name, 'memory', {
        action: 'remember',
        type: label,
        title,
        content: answer.slice(0, CONTENT_MAX),
        // A shared key upserts; with no project, a null project matches every row. Upsert only
        // within one item; free-standing captures each get their own row.
        ...(current().item ? { topic_key: `autopilot:${current().item!.id}:${label}:${title.slice(0, 40)}` } : {}),
      }),
      $.clock.sleep(MEMORY_TIMEOUT_MS).then(() => TIMED_OUT),
    ])
    if (reply === TIMED_OUT || reply.isError) $.ui.log('agentflare: memory capture did not complete')
  } catch (err) {
    $.ui.log(`agentflare: memory capture failed: ${String(err)}`)
  }
}

export function registerCapture(on: On, context: CaptureContext): void {
  on('turn.complete', async ($, e, next) => {
    const result = await next(e)
    await captureTurn($, e, context)
    return result
  })
}
