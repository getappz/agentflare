import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const reply = (body: unknown) => ({ value: { content: [{ type: 'text', text: JSON.stringify(body) }] } })

// Stubs every op the session.start and spawn hooks call; tests override mcp.call and agent.spawn.
function stubSession(on: any, connected = true) {
  const clock = mock.clock(on)
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('session.version', () => ({ value: { version: '2.1.316', base: '2.1.316' } }))
  on('session.cwd', () => ({ value: '/' }))
  on('session.id', () => ({ value: 's1' }))
  on('ui.log', () => ({ value: {} }))
  on('ui.toast', () => ({ value: {} }))
  on('ui.invalidate', () => ({ value: {} }))
  on('state.set', () => ({ value: { isSet: true, version: 1 } }))
  on('mcp.connect', () => ({ value: connected ? { isConnected: true, server: 'agentflare' } : { isConnected: false, reason: 'down' } }))
  on('session.start', (_: any, e: any) => ({ cwd: e.cwd }))
  return clock
}

test('spawn prepends the brief once and routes when model is unset', async ($, on) => {
  stubSession(on)
  let routed: any
  on('mcp.call', (_: any, e: any) => {
    if (e.tool === 'get_routing_suggestion') { routed = e; return reply({ agent: 'claude-code', model: 'haiku', effort: null, advisory: true }) }
    return reply({})
  })
  let seen: any
  on('agent.spawn', (_: any, e: any) => { seen = e; return { model: e.model ?? 'sonnet', agentId: 'a1' } })
  await $.session.start(START)
  await $.agent.spawn({ prompt: 'find callers of X', subagentType: 'Explore' })
  expect(seen.prompt.startsWith('Reach lean-ctx through the flare gateway')).toBe(true)
  expect(seen.prompt.match(/Reach lean-ctx/g)).toHaveLength(1)
  expect(seen.prompt.endsWith('\n\nfind callers of X')).toBe(true)
  expect(seen.model).toBe('haiku')
  expect(routed.args).toEqual({ prompt: 'find callers of X', agent: 'claude-code' })
})

test('a caller-set model is kept and not routed', async ($, on) => {
  stubSession(on)
  let routings = 0
  on('mcp.call', (_: any, e: any) => { if (e.tool === 'get_routing_suggestion') routings++; return reply({ model: 'haiku' }) })
  let seen: any
  on('agent.spawn', (_: any, e: any) => { seen = e; return { model: e.model ?? 'sonnet', agentId: 'a1' } })
  await $.session.start(START)
  await $.agent.spawn({ prompt: 'x', model: 'opus' })
  expect(seen.model).toBe('opus')
  expect(routings).toBe(0)
})

test('a prompt that already carries the brief is not prefixed again', async ($, on) => {
  stubSession(on)
  on('mcp.call', () => reply({ model: 'haiku' }))
  let seen: any
  on('agent.spawn', (_: any, e: any) => { seen = e; return { model: e.model ?? 'sonnet', agentId: 'a1' } })
  await $.session.start(START)
  const prompt = 'Reach lean-ctx through the flare gateway - custom.\n\ndo it'
  await $.agent.spawn({ prompt })
  expect(seen.prompt).toBe(prompt)
})

test('a routing call that times out leaves the model unset', async ($, on) => {
  const clock = stubSession(on)
  on('mcp.call', async (_: any, e: any) => {
    if (e.tool === 'get_routing_suggestion') await $.clock.sleep(10_000)
    return reply({ model: 'haiku' })
  })
  let seen: any
  on('agent.spawn', (_: any, e: any) => { seen = e; return { model: e.model ?? 'sonnet', agentId: 'a1' } })
  await $.session.start(START)
  const p = $.agent.spawn({ prompt: 'x' })
  await clock.advance(1_501)
  await p
  expect(seen.model).toBeUndefined()
  expect(seen.prompt.startsWith('Reach lean-ctx')).toBe(true)
})

test('a failing routing call or a missing server still shapes the prompt', async ($, on) => {
  stubSession(on, false)
  let seen: any
  on('agent.spawn', (_: any, e: any) => { seen = e; return { model: e.model ?? 'sonnet', agentId: 'a1' } })
  await $.session.start(START)
  await $.agent.spawn({ prompt: 'x' })
  expect(seen.model).toBeUndefined()
  expect(seen.prompt.startsWith('Reach lean-ctx')).toBe(true)
})
