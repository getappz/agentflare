import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const VERSION = { value: { version: '2.1.316', base: '2.1.316' } }
const reply = (body: unknown) => ({ value: { content: [{ type: 'text', text: JSON.stringify(body) }] } })

// Stubs every op the session.start and guard hooks call; tests override mcp.connect/mcp.call.
function stubSession(on: any, connected = true) {
  const clock = mock.clock(on)
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('session.version', () => VERSION)
  on('session.cwd', () => ({ value: '/' }))
  on('session.id', () => ({ value: 's1' }))
  on('ui.log', () => ({ value: {} }))
  on('ui.toast', () => ({ value: {} }))
  on('mcp.connect', () => ({ value: connected ? { isConnected: true, server: 'agentflare' } : { isConnected: false, reason: 'down' } }))
  on('session.start', (_: any, e: any) => ({ cwd: e.cwd }))
  return clock
}

test('a denied check refuses the tool with the server reason', async ($, on) => {
  stubSession(on)
  on('mcp.call', () => reply({ decision: 'deny', reason: 'master is protected', rule: 'branch_guard' }))
  let ran = false
  on('tool.call', () => { ran = true; return { result: {} } })
  await $.session.start(START)
  const r = await $.tool.call({ tool: 'Edit', file_path: '/r/a.rs', old_string: 'a', new_string: 'b' })
  expect(r.deny).toContain('master is protected')
  expect(ran).toBe(false)
})

test('a read tool never calls check', async ($, on) => {
  stubSession(on)
  let calls = 0
  on('mcp.call', () => { calls++; return { value: { content: [] } } })
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  await $.tool.call({ tool: 'Read', file_path: '/x' })
  expect(calls).toBe(0)
})

test('MCP unavailable denies a mutating tool and allows a read', async ($, on) => {
  stubSession(on, false)
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  expect((await $.tool.call({ tool: 'Write', file_path: '/r/a', content: 'x' })).deny).toBeDefined()
  expect((await $.tool.call({ tool: 'Read', file_path: '/r/a' })).deny).toBeUndefined()
})

test('a check that times out denies a mutating tool', async ($, on) => {
  const clock = stubSession(on)
  on('mcp.call', async () => { await $.clock.sleep(10_000); return { value: { content: [] } } })
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  const p = $.tool.call({ tool: 'Edit', file_path: '/r/a', old_string: 'a', new_string: 'b' })
  await clock.advance(3_001)
  expect((await p).deny).toBeDefined()
})

test('a passing shell result is observed with its exit code and command', async ($, on) => {
  stubSession(on)
  const seen: any[] = []
  on('mcp.call', (_: any, e: any) => { seen.push(e.args); return reply(e.args.action === 'check' ? { decision: 'allow' } : { recorded: 'verification' }) })
  on('tool.call', () => ({ result: { text: 'test result: ok', exitCode: 0 } }))
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: 'cargo test' })
  const obs = seen.find(a => a.action === 'observe')
  expect(obs.exit_code).toBe(0)
  expect(obs.agent).toBe('claude-code')
  expect(obs.tool_input.command).toBe('cargo test')
})

test('a throwing guard fails closed', async ($, on) => {
  stubSession(on)
  on('mcp.call', () => { throw new Error('boom') })
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  expect((await $.tool.call({ tool: 'Edit', file_path: '/r/a', old_string: 'a', new_string: 'b' })).deny).toBeDefined()
})

test('tool.check allows a read-only tool and denies on failure', async ($, on) => {
  stubSession(on)
  on('tool.check', () => ({ decision: 'ask' }))
  await $.session.start(START)
  expect((await $.tool.check({ tool: 'Grep', pattern: 'x' })).decision).toBe('allow')
  expect((await $.tool.check({ tool: 'Bash', command: 'ls' })).decision).not.toBe('allow')
})
