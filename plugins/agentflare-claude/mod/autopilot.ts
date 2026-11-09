import type { EngineInterface, On, Timer } from 'claude-code'
import { captureTurn, type CaptureContext } from './capture'
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
  capture?: CaptureContext
}

const RATE_LIMIT_PAUSE_PCT = 90
const CURRENT_ITEM = { plugin: 'agentflare', key: 'currentItemId' } as const
const AUTOPILOT_MODE = { plugin: 'agentflare', key: 'autopilotMode' } as const

// Module state resets on hot reload (the timer is cancelled before it is re-armed, so at most one is live).
let mode: Mode = 'off'
// Persisted mode is restored once per module load (hot reload); afterwards memory is the source of truth.
let modeRestored = false
let paused: string | null = null
let timer: Timer | null = null
const notified = new Set<string>()
let ctx: AutopilotContext

/** The effective mode and paused reason, for /flare status. */
export const autopilotState = {
  mode: (): Mode => mode,
  paused: (): string | null => paused,
}

/**
 * /flare autopilot <mode>. Switching to on/once arms the idle timer now: a session already idle
 * since its last turn would otherwise never poll. A zero wait still goes through a timer, because
 * the host refuses prompt.submit (the wake) from inside the command.run hook. Off cancels the timer.
 */
export function setAutopilotMode($: EngineInterface, next: Mode): void {
  mode = next
  modeRestored = true
  paused = null
  timer?.cancel()
  timer = null
  void $.state.set(AUTOPILOT_MODE, next).catch(() => {})
  if (next !== 'off') armIdleTimer($)
}

/** Clears the current item state when marked done or released. */
export async function clearCurrentItem($: EngineInterface): Promise<void> {
  ctx?.setCurrent?.({})
  try {
    await $.state.set(CURRENT_ITEM, null)
  } catch {
    // The module state is already cleared; state write is best-effort.
  }
}

/**
 * Wakes the session with a prompt of its own. prompt.submit is "the same call the engine makes for a
 * typed prompt; a turn of its own, once the session is idle" (claude-code.d.ts:3027-3036), so the
 * text is queued as a turn; $.command.run would only run a slash command. Not awaited: it resolves
 * with the turn, and this runs inside turn.complete.
 */
export async function wake($: EngineInterface, text: string): Promise<void> {
  try {
    await $.prompt.submit({ text })
  } catch (err) {
    // Forget the item so the next step can retry it instead of reporting 'working' forever.
    ctx.setCurrent({})
    $.ui.log(`agentflare: wake failed: ${String(err)}`)
    try {
      await $.state.set(CURRENT_ITEM, null)
    } catch {
      // The module state is already cleared; the persisted id only matters after a reload.
    }
  }
}

async function pause($: EngineInterface, reason: string): Promise<'paused'> {
  if (paused !== reason) $.ui.toast(`agentflare autopilot paused: ${reason}`)
  paused = reason
  await $.state.set({ plugin: 'agentflare', key: 'autopilotPaused' }, reason)
  return 'paused'
}

async function restoreMode($: EngineInterface): Promise<void> {
  if (modeRestored) return
  modeRestored = true
  try {
    const saved = (await $.state.get(AUTOPILOT_MODE)).value as Mode | undefined
    if (saved === 'on' || saved === 'once' || saved === 'off') mode = saved
  } catch {
    // Non-fatal if state read fails.
  }
}

async function step($: EngineInterface): Promise<StepResult> {
  const { client, agent, options, current, setCurrent } = ctx
  const c = client()

  await restoreMode($)
  if (mode === 'off' || !c) return 'idle'

  // Never switch items while one is in progress (no next_work call while currentItemId is set).
  const held = current().item?.id ?? (await $.state.get(CURRENT_ITEM)).value
  if (held) return 'working'

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
  setCurrent({ item: { id, seq, title } })
  await $.state.set(CURRENT_ITEM, id)
  void wake($, `Work agentflare item #${seq} (${id}): ${title}. Read it with the item tool first, work in its bound worktree, run the gate before done.`)
  // Consume `once` right away: a throw below must not leave the mode armed for a second wake.
  if (mode === 'once') {
    mode = 'off'
    await $.state.set(AUTOPILOT_MODE, 'off')
  }
  await $.state.set({ plugin: 'agentflare', key: 'lastWakeAt' }, await $.clock.now())
  await refreshBriefing($, { client, agent })
  return 'woke'
}

async function guardedStep($: EngineInterface): Promise<StepResult> {
  try {
    return await step($)
  } catch (err) {
    $.ui.log(`agentflare: autopilot step failed: ${String(err)}`)
    return 'idle'
  }
}

// Cancel-before-arm keeps exactly one live timer.
function armIdleTimer($: EngineInterface): void {
  if (mode === 'off') return
  timer?.cancel()
  timer = $.clock.after(Math.max(0, ctx.options.idleMinutes) * 60_000, () => void pollAndRearm($))
}

// Every result re-polls after another idle period. A wake re-arms too: its prompt may be rejected
// (no turn.complete then), and a turn that does run cancels and re-arms the timer itself. With no
// idle wait (idleMinutes <= 0) the step runs at each turn.complete instead; re-arming a zero timer
// here would poll in a tight loop.
async function pollAndRearm($: EngineInterface): Promise<void> {
  await guardedStep($)
  if (mode !== 'off' && ctx.options.idleMinutes > 0) armIdleTimer($)
}

export function registerAutopilot(on: On, context: AutopilotContext): { step($: EngineInterface): Promise<StepResult> } {
  ctx = context
  mode = context.options.autopilot
  modeRestored = false

  // Cancel the idle timer at turn start so it cannot fire while a turn runs.
  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) {
      timer?.cancel()
      timer = null
    }
    return next(e)
  })

  // The session must be idle for idleMinutes before a pull: finished, aborted, or errored turns restart the wait.
  on('turn.complete', async ($, e, next) => {
    const result = await next(e)
    if (context.capture) {
      await captureTurn($, e, context.capture)
    }
    if (e.agentId !== undefined) return result
    await restoreMode($)
    timer?.cancel()
    timer = null
    if (mode !== 'off') {
      if (e.reason === 'answer' && context.options.idleMinutes <= 0) {
        await guardedStep($)
      } else {
        armIdleTimer($)
      }
    }
    return result
  })

  flareCommand(on, { client: context.client, current: context.current, agent: context.agent })
  return { step }
}
