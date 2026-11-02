import { expect, test } from 'claude-code/testing'

test('session.start twice registers /flare once and connects MCP once', async ($, on) => {
  let registered = 0, connected = 0
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
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('mcp.connect', () => ({ value: { isConnected: false, reason: 'not configured' } }))
  on('ui.toast', () => { toasts++; return { value: {} } })
  on('session.version', () => ({ value: { version: '2.1.316', base: '2.1.316' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  expect(toasts).toBe(1)
})
