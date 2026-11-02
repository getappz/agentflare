import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const TURN = { reason: 'answer', answer: 'done', durationMs: 1, isAborted: false, turnId: 't1' } as const
const ready = { item: { id: 'abc', seq: 12, title: 'Fix X', description: 'd' }, claimed: true }
const HEALTH = { daemon: 'up', stale_binary: false, auth: 'ok', mcp_version: '1' }

type Opts = { messages?: any[]; usage?: any; reply?: (a: any) => unknown; connected?: boolean }

// Stubs every op session.start, the turn.complete hooks and /flare call; returns what the tests assert on.
function stub(on: any, o: Opts = {}) {
  const clock = mock.clock(on)
  const seen = { nextWork: [] as any[], woke: [] as string[], notify: [] as string[], toasts: [] as string[], state: [] as any[], briefing: 0 }
  const reply = o.reply ?? ((a: any) => (a.action === 'next_work' ? ready : a.action === 'health' ? HEALTH : { etag: 'e', text: 'B' }))
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('session.version', () => ({ value: { version: '2.1.316', base: '2.1.316' } }))
  on('session.cwd', () => ({ value: '/' }))
  on('session.id', () => ({ value: 's1' }))
  on('ui.log', () => ({ value: {} }))
  on('ui.toast', (_: any, e: any) => { seen.toasts.push(e.text ?? e.message ?? JSON.stringify(e)); return { value: {} } })
  on('ui.notify', (_: any, e: any) => { seen.notify.push(JSON.stringify(e)); return { value: {} } })
  on('ui.invalidate', () => ({ value: {} }))
  on('state.set', (_: any, e: any) => { seen.state.push(e); return { value: { isSet: true, version: 1 } } })
  on('mcp.connect', () => ({ value: o.connected === false ? { isConnected: false, reason: 'x' } : { isConnected: true, server: 'agentflare' } }))
  on('mcp.call', (_: any, e: any) => {
    if (e.args.action === 'next_work') seen.nextWork.push(e.args)
    if (e.args.action === 'briefing') seen.briefing++
    return { value: { content: [{ type: 'text', text: JSON.stringify(reply(e.args)) }] } }
  })
  on('model.classify', () => ({ value: 'none' }))
  on('session.usage', () => ({ value: o.usage ?? { cost: { usd: 1 }, rateLimits: [] } }))
  on('session.messages', () => ({ value: o.messages ?? [{ role: 'assistant', text: 'done', toolUses: [] }] }))
  on('prompt.submit', (_: any, e: any) => { seen.woke.push(e.text); return { text: e.text } })
  on('session.start', (_: any, e: any) => ({ cwd: e.cwd }))
  on('turn.complete', (_: any, e: any) => ({ text: e.answer }))
  return { clock, seen }
}

test('on + idle + ready item wakes with the item text', { options: { autopilot: 'on', idleMinutes: 0 } }, async ($, on) => {
  const { seen } = stub(on)
  await $.session.start(START)
  await $.turn.complete(TURN)
  expect(seen.woke.join('\n')).toContain('Work agentflare item #12 (abc): Fix X')
  expect(seen.state.some((s: any) => s.key === 'lastWakeAt')).toBe(true)
})

test('a pending human message blocks the wake', { options: { autopilot: 'on', idleMinutes: 0 } }, async ($, on) => {
  const { seen } = stub(on, { messages: [{ role: 'assistant', text: 'x', toolUses: [] }, { role: 'user', text: 'wait', toolUses: [] }] })
  await $.session.start(START)
  await $.turn.complete(TURN)
  expect(seen.nextWork).toHaveLength(0)
  expect(seen.woke).toHaveLength(0)
})

test('cost ceiling pauses with a toast and no next_work call', { options: { autopilot: 'on', idleMinutes: 0, maxCostUsd: 5 } }, async ($, on) => {
  const { seen } = stub(on, { usage: { cost: { usd: 6 }, rateLimits: [] } })
  await $.session.start(START)
  await $.turn.complete(TURN)
  expect(seen.toasts.join('\n')).toContain('budget')
  expect(seen.nextWork).toHaveLength(0)
  expect(seen.woke).toHaveLength(0)
  expect(seen.state.some((s: any) => s.key === 'autopilotPaused')).toBe(true)
})

test('a rate limit at 90 percent pauses', { options: { autopilot: 'on', idleMinutes: 0 } }, async ($, on) => {
  const { seen } = stub(on, { usage: { cost: { usd: 1 }, rateLimits: [{ kind: 'five_hour', percentUsed: 91 }] } })
  await $.session.start(START)
  await $.turn.complete(TURN)
  expect(seen.toasts.join('\n')).toContain('budget')
  expect(seen.nextWork).toHaveLength(0)
})

test('plan_approval blocks and notifies once per item', { options: { autopilot: 'on', idleMinutes: 0 } }, async ($, on) => {
  const blocked = { item: { id: 'abc', seq: 12, title: 'Fix X', description: 'd' }, claimed: false, blocked_reason: 'plan_approval' }
  const { seen } = stub(on, { reply: a => (a.action === 'next_work' ? blocked : {}) })
  await $.session.start(START)
  await $.turn.complete(TURN)
  await $.turn.complete(TURN)
  expect(seen.notify).toHaveLength(1)
  expect(seen.woke).toHaveLength(0)
})

test('once wakes a single time', { options: { autopilot: 'once', idleMinutes: 0 } }, async ($, on) => {
  let n = 0
  const { seen } = stub(on, { reply: a => (a.action === 'next_work' ? { ...ready, item: { ...ready.item, id: 'i' + n++ } } : {}) })
  await $.session.start(START)
  await $.turn.complete(TURN)
  await $.turn.complete(TURN)
  expect(seen.woke).toHaveLength(1)
})

test('the same held item wakes once', { options: { autopilot: 'on', idleMinutes: 0 } }, async ($, on) => {
  const { seen } = stub(on)
  await $.session.start(START)
  await $.turn.complete(TURN)
  await $.turn.complete(TURN)
  expect(seen.nextWork).toHaveLength(2)
  expect(seen.woke).toHaveLength(1)
})

test('mode off never calls next_work', { options: { autopilot: 'off', idleMinutes: 0 } }, async ($, on) => {
  const { seen } = stub(on)
  await $.session.start(START)
  await $.turn.complete(TURN)
  expect(seen.nextWork).toHaveLength(0)
})

test('/flare autopilot on then status reports on', { options: { autopilot: 'off' } }, async ($, on) => {
  stub(on)
  await $.session.start(START)
  const set = await $.command.run({ command: 'flare', args: 'autopilot on' })
  expect(set.text).toContain('on')
  const status = await $.command.run({ command: 'flare', args: 'status' })
  expect(status.text).toContain('autopilot: on')
  expect(status.text).toContain('daemon: up')
})

test('/flare doctor lists health fields and an unknown subcommand prints usage', async ($, on) => {
  stub(on)
  await $.session.start(START)
  const doctor = await $.command.run({ command: 'flare', args: 'doctor' })
  expect(doctor.text).toContain('mcp_version: 1')
  const bad = await $.command.run({ command: 'flare', args: 'nope' })
  expect(bad.text).toContain('usage')
})

test('/flare briefing refresh refreshes the briefing', async ($, on) => {
  const { seen } = stub(on)
  await $.session.start(START)
  const before = seen.briefing
  await $.command.run({ command: 'flare', args: 'briefing refresh' })
  expect(seen.briefing).toBeGreaterThan(before)
})

test('/flare with no MCP connection says so', async ($, on) => {
  stub(on, { connected: false })
  await $.session.start(START)
  const r = await $.command.run({ command: 'flare', args: 'status' })
  expect(r.text).toContain('not connected')
})

test('session.start arms no idle clock, even twice', { options: { autopilot: 'on', idleMinutes: 1 } }, async ($, on) => {
  const { clock, seen } = stub(on)
  await $.session.start(START)
  await $.session.start(START)
  await clock.advance(10 * 60_000)
  expect(seen.nextWork).toHaveLength(0)
})

test('only the latest turn.complete leaves a live idle timer', { options: { autopilot: 'on', idleMinutes: 1 } }, async ($, on) => {
  const { clock, seen } = stub(on)
  await $.session.start(START)
  await $.turn.complete(TURN)
  await $.turn.complete(TURN)
  expect(seen.nextWork).toHaveLength(0)
  await clock.advance(60_000)
  expect(seen.nextWork).toHaveLength(1)
})
