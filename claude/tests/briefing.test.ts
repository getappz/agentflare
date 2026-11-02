import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const COMPOSE = { model: 'm', promptModel: 'm', surfaces: [], tools: [], outputStyle: null, traits: [] } as any
const reply = (body: unknown) => ({ value: { content: [{ type: 'text', text: JSON.stringify(body) }] } })

// Stubs every op the session.start and briefing hooks call; tests override mcp.call.
function stubSession(on: any, connected = true) {
  mock.clock(on)
  let stored: string | undefined
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('session.version', () => ({ value: { version: '2.1.316', base: '2.1.316' } }))
  on('session.cwd', () => ({ value: '/' }))
  on('session.id', () => ({ value: 's1' }))
  on('ui.log', () => ({ value: {} }))
  on('ui.toast', () => ({ value: {} }))
  on('mcp.connect', () => ({ value: connected ? { isConnected: true, server: 'agentflare' } : { isConnected: false, reason: 'down' } }))
  on('session.start', (_: any, e: any) => ({ cwd: e.cwd }))
  on('state.get', () => ({ value: { value: stored, version: 1 } }))
  on('state.set', (_: any, e: any) => { stored = e.value; return { value: { isSet: true, version: 2 } } })
  on('prompt.compose', () => ({ sections: [{ id: 'intro', text: 'core', scope: 'shared' }] }))
}

test('prompt.compose appends one agentflare:briefing session section', async ($, on) => {
  stubSession(on)
  on('mcp.call', () => reply({ etag: 'e1', text: 'BRIEF' }))
  on('ui.invalidate', () => ({ value: {} }))
  await $.session.start(START)
  const { sections } = await $.prompt.compose(COMPOSE)
  expect(sections.at(-1)).toEqual({ id: 'agentflare:briefing', text: 'BRIEF', scope: 'session' })
  expect(sections.filter((s: any) => s.id === 'agentflare:briefing')).toHaveLength(1)
})

test('an unchanged etag does not invalidate', async ($, on) => {
  stubSession(on)
  let invalidations = 0
  on('mcp.call', (_: any, e: any) => reply(e.args.etag === 'e1' ? { etag: 'e1', unchanged: true } : { etag: 'e1', text: 'BRIEF' }))
  on('ui.invalidate', () => { invalidations++; return { value: {} } })
  await $.session.start(START)
  await $.session.start(START)
  expect(invalidations).toBe(1)
})

test('briefing failure keeps the previous text', async ($, on) => {
  stubSession(on)
  let calls = 0
  let invalidations = 0
  on('mcp.call', () => { if (++calls > 1) throw new Error('boom'); return reply({ etag: 'e1', text: 'BRIEF' }) })
  on('ui.invalidate', () => { invalidations++; return { value: {} } })
  await $.session.start(START)
  await $.session.start(START)
  const { sections } = await $.prompt.compose(COMPOSE)
  expect(calls).toBe(2)
  expect(sections.at(-1)?.text).toBe('BRIEF')
  expect(invalidations).toBe(1)
})

test('prompt.section serves the current text, and the new text after an etag change', async ($, on) => {
  stubSession(on)
  let n = 0
  on('mcp.call', () => reply(++n === 1 ? { etag: 'e1', text: 'BRIEF' } : { etag: 'e2', text: 'BRIEF2' }))
  on('ui.invalidate', () => ({ value: {} }))
  on('prompt.section', () => ({ text: 'core' }))
  await $.session.start(START)
  expect((await $.prompt.section({ name: 'agentflare:briefing', text: null })).text).toBe('BRIEF')
  await $.session.start(START)
  expect((await $.prompt.section({ name: 'agentflare:briefing', text: null })).text).toBe('BRIEF2')
})

test('a reply with a changed etag but no text is ignored', async ($, on) => {
  stubSession(on)
  let n = 0
  let invalidations = 0
  on('mcp.call', () => reply(++n === 1 ? { etag: 'e1', text: 'BRIEF' } : { etag: 'e2' }))
  on('ui.invalidate', () => { invalidations++; return { value: {} } })
  await $.session.start(START)
  await $.session.start(START)
  expect(invalidations).toBe(1)
  expect((await $.prompt.compose(COMPOSE)).sections.at(-1)?.text).toBe('BRIEF')
})

test('a null client omits the section and never asks for a briefing', async ($, on) => {
  stubSession(on, false)
  let briefings = 0
  on('mcp.call', (_: any, e: any) => { if (e.args.action === 'briefing') briefings++; return reply({}) })
  await $.session.start(START)
  expect((await $.prompt.compose(COMPOSE)).sections.map((s: any) => s.id)).toEqual(['intro'])
  expect(briefings).toBe(0)
})
