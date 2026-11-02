import type { EngineInterface, On, Timer } from 'claude-code'
import { flareCommand } from './flare'
import type { AutopilotClient } from './mcp'
import { refreshBriefing } from './briefing'
import type { Current } from './compaction'

export type Mode = 'off' | 'once' | 'on'
export type StepResult = 'woke' | 'paused' | 'idle' | 'working' | 'blocked'

export type AutopilotContext = {
  client: () => AutopilotClient | null
  agent: string
  options: { autopilot: Mode; idleMinutes: number; maxCostUsd: number }
  current: () => Current
  setCurrent: (next: Current) => void
}

const RATE_LIMIT_PAUSE_PCT = 90

// Module state resets on hot reload (the timer is cancelled before it is re-armed, so at most one is live).
let mode: Mode = 'off'
let paused: string | null = null
let timer: Timer | null = null
const notified = new Set<string>()
let ctx: AutopilotContext

/** The effective mode, paused reason and a way to change the mode, for /flare. */
export const autopilotState = {
  mode: (): Mode => mode,
  paused: (): string | null => paused,
  setMode: (next: Mode): void => {
    mode = next
    paused = null
  },
}

/**
 * Wakes the session with a prompt of its own. prompt.submit is "the same call the engine makes for a
 * typed prompt; a turn of its own, once the session is idle" (claude-code.d.ts:3027-3036), so the
 * text is queued as a turn; $.command.run would only run a slash command. Not awaited: it resolves
 * with the turn, and this runs inside turn.complete.
 */
export function wake($: EngineInterface, text: string): void {
  $.prompt.submit({ text }).catch(err => $.ui.log(`agentflare: wake failed: ${String(err)}`))
}

async function pause($: EngineInterface, reason: string): Promise<'paused'> {
  paused = reason
  $.ui.toast(`agentflare autopilot paused: ${reason}`)
  await $.state.set({ plugin: 'agentflare', key: 'autopilotPaused' }, reason)
  return 'paused'
}

async function step($: EngineInterface): Promise<StepResult> {
  const { client, agent, options, current, setCurrent } = ctx
  const c = client()
  if (mode === 'off' || !c) return 'idle'
  const last = (await $.session.messages()).at(-1)
  if (last?.role === 'user' && last.text.trim()) return 'blocked'

  const usage = await $.session.usage()
  const pct = Math.max(0, ...usage.rateLimits.map(r => r.percentUsed))
  if (usage.cost && usage.cost.usd >= options.maxCostUsd) return pause($, 'cost budget reached')
  if (pct >= RATE_LIMIT_PAUSE_PCT) return pause($, 'rate-limit budget reached')
  if (paused !== null) {
    paused = null
    await $.state.set({ plugin: 'agentflare', key: 'autopilotPaused' }, null)
  }

  const r = await c.nextWork({ agent, cwd: await $.session.cwd(), max_cost_usd: options.maxCostUsd, rate_limit_pct: pct })
  if (r.blocked_reason === 'plan_approval') {
    const id = r.item?.id ?? 'unknown'
    if (!notified.has(id)) {
      notified.add(id)
      $.ui.toast('agentflare: an item is waiting for plan approval')
      $.ui.notify('agentflare: an item is waiting for plan approval')
    }
    return 'blocked'
  }
  if (!r.item || !r.claimed) return 'idle'
  const { id, seq, title } = r.item
  if (id === current().item?.id) return 'working'
  setCurrent({ item: { id, seq, title } })
  wake($, `Work agentflare item #${seq} (${id}): ${title}. Read it with the item tool first, work in its bound worktree, run the gate before done.`)
  await $.state.set({ plugin: 'agentflare', key: 'lastWakeAt' }, await $.clock.now())
  await refreshBriefing($, { client, agent })
  if (mode === 'once') mode = 'off'
  return 'woke'
}

async function guardedStep($: EngineInterface): Promise<void> {
  try {
    await step($)
  } catch (err) {
    $.ui.log(`agentflare: autopilot step failed: ${String(err)}`)
  }
}

export function registerAutopilot(on: On, context: AutopilotContext): { step($: EngineInterface): Promise<StepResult> } {
  ctx = context
  mode = context.options.autopilot

  // The session must be idle for idleMinutes before a pull: each finished main-loop turn restarts the wait.
  on('turn.complete', { reason: 'answer' }, async ($, e, next) => {
    const result = await next(e)
    if (e.agentId !== undefined) return result
    timer?.cancel()
    timer = null
    const { idleMinutes } = context.options
    if (idleMinutes <= 0) await guardedStep($)
    else timer = $.clock.after(idleMinutes * 60_000, () => void guardedStep($))
    return result
  })

  flareCommand(on, { client: context.client, current: context.current, agent: context.agent })
  return { step }
}
