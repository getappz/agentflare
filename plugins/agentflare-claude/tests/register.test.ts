import { expect, test } from 'claude-code/testing'

test('session.start twice registers /flare once and connects MCP once', async ($, on) => {
  let registered = 0, connected = 0
  on('command.list', () => ({ value: [] }))
  on('command.register', () => { registered++; return { value: { command: 'flare' } } })
  on('mcp.connect', () => { connected++; return { value: { isConnected: true, server: 'agentflare' } } })
  on('session.version', () => ({ value: { version: '2.1.316', base: '2.1.316' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  expect(registered).toBe(1); expect(connected).toBe(1)
})

test('session.start with MCP down toasts once and still returns', async ($, on) => {
  let toasts = 0
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('mcp.connect', () => ({ value: { isConnected: false, reason: 'not configured' } }))
  on('ui.toast', () => { toasts++; return { value: {} } })
  on('session.version', () => ({ value: { version: '2.1.316', base: '2.1.316' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  expect(toasts).toBe(1)
})

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const VERSION = { value: { version: '2.1.316', base: '2.1.316' } }

test('an existing /flare command is not registered again', async ($, on) => {
  let registered = 0
  on('command.list', () => ({ value: [{ name: 'flare', description: '', source: 'plugin' }] }))
  on('command.register', () => { registered++; return { value: { command: 'flare' } } })
  on('session.version', () => VERSION)
  on('mcp.connect', () => ({ value: { isConnected: true, server: 'agentflare' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  await $.session.start(START)
  expect(registered).toBe(0)
})

test('a failed /flare registration is retried and does not break session.start', async ($, on) => {
  let attempts = 0
  on('command.list', () => ({ value: [] }))
  on('command.register', () => {
    attempts++
    if (attempts === 1) throw new Error('boom')
    return { value: { command: 'flare' } }
  })
  on('session.version', () => VERSION)
  on('ui.log', () => ({ value: {} }))
  on('mcp.connect', () => ({ value: { isConnected: true, server: 'agentflare' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  const first = await $.session.start(START)
  expect(first.cwd).toBe('/')
  await $.session.start(START)
  expect(attempts).toBe(2)
})

test('an MCP connect that throws toasts once and session.start still resolves', async ($, on) => {
  let toasts = 0
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('session.version', () => VERSION)
  on('ui.log', () => ({ value: {} }))
  on('ui.toast', () => { toasts++; return { value: {} } })
  on('mcp.connect', () => { throw new Error('down') })
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  const r = await $.session.start(START)
  expect(r.cwd).toBe('/')
  expect(toasts).toBe(1)
})
