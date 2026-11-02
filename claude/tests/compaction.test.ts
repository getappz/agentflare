import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const MSGS = [{ role: 'user', text: 'hi', toolUses: [] }]
const compact = ($: any, args: object = {}) => $.session.compact({ trigger: 'manual', messages: MSGS, ...args })
const reply = (body: unknown) => ({ value: { content: [{ type: 'text', text: JSON.stringify(body) }] } })

// Stubs every op the session.start and compaction hooks call; tests override mcp.call and session.compact.
function stubSession(on: any, connected = true) {
  mock.clock(on)
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
}

test('compaction passes keep instructions and appends the server checkpoint', async ($, on) => {
  stubSession(on)
  on('mcp.call', (_: any, e: any) => e.args.action === 'checkpoint'
    ? reply({ handoff_id: 'h1', text: '[agentflare checkpoint] item #12' })
    : reply({}))
  let seenInstructions = ''
  on('session.compact', (_: any, e: any) => { seenInstructions = e.instructions ?? ''; return { messages: [{ role: 'assistant', text: 'summary', toolUses: [] }] } })
  await $.session.start(START)
  const r = await compact($)
  expect(seenInstructions).toContain('Keep verbatim')
  expect(r.messages?.at(-1)).toMatchObject({ role: 'user', text: '[agentflare checkpoint] item #12' })
  expect(r.messages).toHaveLength(2)
})

test('a failed checkpoint call still appends a minimal checkpoint and sends the instructions', async ($, on) => {
  stubSession(on)
  on('mcp.call', (_: any, e: any) => { if (e.args.action === 'checkpoint') throw new Error('down'); return reply({}) })
  let seenInstructions = ''
  on('session.compact', (_: any, e: any) => { seenInstructions = e.instructions ?? ''; return { messages: [{ role: 'assistant', text: 'summary', toolUses: [] }] } })
  await $.session.start(START)
  const r = await compact($)
  expect(seenInstructions).toContain('Keep verbatim')
  expect(r.messages?.at(-1)?.text).toBe('[agentflare checkpoint] item none; branch unknown; gate state unknown; re-run the gate before item done.')
})

test('a null client appends the minimal checkpoint without calling the server', async ($, on) => {
  stubSession(on, false)
  let checkpoints = 0
  on('mcp.call', (_: any, e: any) => { if (e.args.action === 'checkpoint') checkpoints++; return reply({}) })
  on('session.compact', () => ({ messages: [{ role: 'assistant', text: 'summary', toolUses: [] }] }))
  await $.session.start(START)
  const r = await compact($)
  expect(r.messages?.at(-1)?.text).toMatch(/^\[agentflare checkpoint\].*gate state unknown/)
  expect(checkpoints).toBe(0)
})

test('existing instructions are kept, ours follow after a blank line', async ($, on) => {
  stubSession(on)
  on('mcp.call', () => reply({ handoff_id: null, text: '[agentflare checkpoint] x' }))
  let seen = ''
  on('session.compact', (_: any, e: any) => { seen = e.instructions ?? ''; return { messages: MSGS } })
  await $.session.start(START)
  await compact($, { instructions: 'the plan' })
  expect(seen).toMatch(/^the plan\n\nKeep verbatim/)
})

test('a subagent compaction is left alone', async ($, on) => {
  stubSession(on)
  let checkpoints = 0
  on('mcp.call', (_: any, e: any) => { if (e.args.action === 'checkpoint') checkpoints++; return reply({}) })
  on('session.compact', () => ({ messages: [{ role: 'assistant', text: 'summary', toolUses: [] }] }))
  await $.session.start(START)
  const r = await compact($, { agentId: 'a1' })
  expect(r.messages).toHaveLength(1)
  expect(checkpoints).toBe(0)
})
