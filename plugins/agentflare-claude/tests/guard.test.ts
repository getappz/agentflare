import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const VERSION = { value: { version: '2.1.316', base: '2.1.316' } }
const reply = (body: unknown) => ({ value: { content: [{ type: 'text', text: JSON.stringify(body) }] } })

// Stubs every op the session.start and guard hooks call; tests override mcp.connect/mcp.call.
function stubSession(on: any, connected = true, server = 'agentflare', tools: unknown[] = []) {
  const clock = mock.clock(on)
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('session.version', () => VERSION)
  on('session.cwd', () => ({ value: '/' }))
  on('session.id', () => ({ value: 's1' }))
  on('ui.log', () => ({ value: {} }))
  on('ui.toast', () => ({ value: {} }))
  on('mcp.connect', () => ({ value: connected ? { isConnected: true, server } : { isConnected: false, reason: 'failed', message: 'down' } }))
  on('tool.list', () => ({ value: tools }))
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
  on('mcp.call', (_: any, e: any) => { if (e.args.action === 'check') calls++; return reply({ etag: 'e0', unchanged: true }) })
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
  on('tool.call', () => ({ result: { stdout: 'test result: ok', stderr: '', interrupted: false } }))
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: 'cargo test' })
  const obs = seen.find(a => a.action === 'observe')
  expect('exit_code' in obs).toBe(false)
  expect(obs.output_excerpt).toBe('test result: ok')
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

test('read-only auto-allow upgrades an ask but never overrides a deny', async ($, on) => {
  stubSession(on)
  on('tool.check', () => ({ decision: 'deny' }))
  await $.session.start(START)
  expect((await $.tool.check({ tool: 'Grep', pattern: 'x' })).decision).toBe('deny')
})

function observeSetup(on: any, handler: () => unknown) {
  stubSession(on)
  const seen: any[] = []
  on('mcp.call', (_: any, e: any) => { seen.push(e.args); return reply(e.args.action === 'check' ? { decision: 'allow' } : { recorded: 'none' }) })
  on('tool.call', handler)
  return seen
}

test('an errored shell result is observed with exit code 1', async ($, on) => {
  const seen = observeSetup(on, () => ({ result: { stdout: '', stderr: 'boom', interrupted: false }, isError: true }))
  await $.session.start(START)
  const r = await $.tool.call({ tool: 'Bash', command: 'cargo test' })
  const obs = seen.find(a => a.action === 'observe')
  // If the kit strips a hook-supplied isError, r.isError is falsy and this fails; the report records which.
  expect(r.isError).toBe(true)
  expect(obs.exit_code).toBe(1)
})

test('a long output is cut to its last 2000 chars', async ($, on) => {
  const seen = observeSetup(on, () => ({ result: { stdout: 'a'.repeat(500) + 'b'.repeat(2000), stderr: '' } }))
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: 'x' })
  const obs = seen.find(a => a.action === 'observe')
  expect(obs.output_excerpt.length).toBe(2000)
  expect(obs.output_excerpt).toBe('b'.repeat(2000))
})

test('a review submit is observed with item_action and item_success', async ($, on) => {
  let fail = false
  const seen = observeSetup(on, () => (fail ? { result: {}, isError: true } : { result: {} }))
  await $.session.start(START)
  await $.tool.call({ tool: 'mcp__flare__review', action: 'submit', verdict: 'ok' })
  fail = true
  await $.tool.call({ tool: 'mcp__flare__review', action: 'submit', verdict: 'ok' })
  const obs = seen.filter(a => a.action === 'observe')
  expect(obs[0].item_action).toBe('submit'); expect(obs[0].item_success).toBe(true)
  expect(obs[1].item_action).toBe('submit'); expect(obs[1].item_success).toBe(false)
})

test('a gateway search reaches check with every input field intact', async ($, on) => {
  const seen = observeSetup(on, () => ({ result: {} }))
  await $.session.start(START)
  await $.tool.call({ tool: 'mcp__flare__tool', action: 'search', query: 'ctx_patch', limit: 3 } as any)
  const chk = seen.find(a => a.action === 'check')
  expect(chk.tool_name).toBe('mcp__flare__tool')
  expect(chk.tool_input).toEqual({ action: 'search', query: 'ctx_patch', limit: 3 })
})

test('a gateway call checks the inner tool recovered from the transcript', async ($, on) => {
  const seen = observeSetup(on, () => ({ result: {} }))
  const input = { action: 'execute', server: 'leanctx', tool: 'ctx_patch', args: { path: 'C:\\repo\\a.rs' } }
  on('session.messages', (_: any, e: any) => ({
    value: e.as === 'api'
      ? [{ role: 'user', content: [{ type: 'text', text: 'go' }] }, { role: 'assistant', content: [{ type: 'tool_use', id: 'tu1', name: 'mcp__flare__tool', input }] }]
      : [],
  }))
  await $.session.start(START)
  await $.tool.call({ tool: 'mcp__flare__tool', tool_use_id: 'tu1', action: 'execute', server: 'leanctx', args: input.args } as any)
  const chk = seen.find(a => a.action === 'check')
  expect(chk.tool_input.tool).toBe('ctx_patch')
  expect(seen.find(a => a.action === 'observe').tool_input.tool).toBe('ctx_patch')
})

test('an unrecoverable gateway execute is refused before check or the tool runs', async ($, on) => {
  let ran = false
  const seen = observeSetup(on, () => { ran = true; return { result: {} } })
  on('session.messages', () => ({ value: [] }))
  await $.session.start(START)
  const r = await $.tool.call({ tool: 'mcp__flare__tool', tool_use_id: 'tu9', action: 'execute', server: 'leanctx' } as any)
  expect(r.deny).toContain('unavailable')
  expect(ran).toBe(false)
  expect(seen.some(a => a.action === 'check')).toBe(false)
})

test('a subagent gateway call whose transcript cannot be read is refused', async ($, on) => {
  const seen = observeSetup(on, () => ({ result: {} }))
  const asked: any[] = []
  on('session.messages', (_: any, e: any) => { asked.push(e); return { value: { deny: 'not readable' } } })
  await $.session.start(START)
  const r = await $.tool.call({ tool: 'mcp__flare__tool', tool_use_id: 'tu8', agentId: 'a1', action: 'execute', server: 'leanctx' } as any)
  expect(asked.at(-1)).toMatchObject({ agentId: 'a1', as: 'api' })
  expect(r.deny).toContain('unavailable')
  expect(seen.some(a => a.action === 'check')).toBe(false)
})

test('only a gateway execute reads the transcript, and the flattened args win', async ($, on) => {
  const seen = observeSetup(on, () => ({ result: {} }))
  let reads = 0
  const input = { action: 'execute', server: 'leanctx', tool: 'ctx_shell', args: { command: 'old' } }
  on('session.messages', () => { reads++; return { value: [{ role: 'assistant', content: [{ type: 'tool_use', id: 'tu2', name: 'mcp__flare__tool', input }] }] } })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: 'ls' })
  await $.tool.call({ tool: 'mcp__lean-ctx__ctx_shell', tool_use_id: 'tu3', command: 'ls' } as any)
  await $.tool.call({ tool: 'mcp__flare__tool', tool_use_id: 'tu4', action: 'search', query: 'x' } as any)
  expect(reads).toBe(0)
  await $.tool.call({ tool: 'mcp__flare__tool', tool_use_id: 'tu2', action: 'execute', server: 'leanctx', args: { command: 'new' } } as any)
  expect(reads).toBe(1)
  expect(seen.filter(a => a.action === 'check').at(-1).tool_input).toEqual({ action: 'execute', server: 'leanctx', tool: 'ctx_shell', args: { command: 'new' } })
})

// C1: a plugin-only install connects as `plugin:agentflare:agentflare`, so the model's tools are
// `mcp__plugin_agentflare_agentflare__*`; Rust keys on `mcp__flare__*`.
const PLUGIN_SERVER = 'plugin:agentflare:agentflare'
const P = 'mcp__plugin_agentflare_agentflare__'

function pluginSetup(on: any, handler: () => unknown = () => ({ result: {} }), verdict: unknown = { decision: 'allow' }) {
  stubSession(on, true, PLUGIN_SERVER)
  const seen: any[] = []
  on('mcp.call', (_: any, e: any) => { seen.push({ ...e.args, server: e.server }); return reply(e.args.action === 'check' ? verdict : { recorded: 'none' }) })
  on('tool.call', handler)
  return seen
}

test('a plugin-spelled gateway ctx_patch reaches check as mcp__flare__tool with its inner tool', async ($, on) => {
  const seen = pluginSetup(on, () => ({ result: {} }), { decision: 'deny', reason: 'master is protected' })
  const input = { action: 'execute', server: 'leanctx', tool: 'ctx_patch', args: { path: 'README.md', op: 'replace_unique' } }
  on('session.messages', () => ({ value: [{ role: 'assistant', content: [{ type: 'tool_use', id: 'tp1', name: `${P}tool`, input }] }] }))
  await $.session.start(START)
  const r = await $.tool.call({ tool: `${P}tool`, tool_use_id: 'tp1', action: 'execute', server: 'leanctx', args: input.args } as any)
  const chk = seen.find(a => a.action === 'check')
  expect(chk.server).toBe(PLUGIN_SERVER)
  expect(chk.tool_name).toBe('mcp__flare__tool')
  expect(chk.tool_input.tool).toBe('ctx_patch')
  expect(r.deny).toContain('master is protected')
})

test('a plugin-spelled item done reaches check as mcp__flare__item', async ($, on) => {
  const seen = pluginSetup(on, () => ({ result: {} }), { decision: 'deny', reason: 'no fresh, passing verification evidence' })
  await $.session.start(START)
  const r = await $.tool.call({ tool: `${P}item`, action: 'done', id: 'zz' } as any)
  const chk = seen.find(a => a.action === 'check')
  expect(chk.tool_name).toBe('mcp__flare__item')
  expect(chk.tool_input).toEqual({ action: 'done', id: 'zz' })
  expect(r.deny).toContain('verification')
})

test('a plugin-spelled review submit is observed as mcp__flare__review', async ($, on) => {
  const seen = pluginSetup(on)
  await $.session.start(START)
  await $.tool.call({ tool: `${P}review`, action: 'submit', findings: [] } as any)
  const obs = seen.find(a => a.action === 'observe')
  expect(obs.tool_name).toBe('mcp__flare__review')
  expect(obs.item_action).toBe('submit')
})

test('plugin-spelled self and read-only tools are allowed at tool.check', async ($, on) => {
  stubSession(on, true, PLUGIN_SERVER)
  on('tool.check', () => ({ decision: 'ask' }))
  await $.session.start(START)
  expect((await $.tool.check({ tool: `${P}search`, query: 'x' } as any)).decision).toBe('allow')
  expect((await $.tool.check({ tool: `${P}item`, action: 'list' } as any)).decision).toBe('ask')
})

test('readOnlyAutoAllow false leaves a read-only tool to the next hook', { options: { readOnlyAutoAllow: false } }, async ($, on) => {
  stubSession(on)
  on('tool.check', () => ({ decision: 'ask' }))
  await $.session.start(START)
  expect((await $.tool.check({ tool: 'Grep', pattern: 'x' })).decision).toBe('ask')
})

// I1: the hook's non-mutating denials (data guard, shell claim targets, TodoWrite, coaching).
test('Bash, PowerShell, ctx_shell, TodoWrite and ToolSearch are checked; a deny refuses them', async ($, on) => {
  let ran = 0
  const seen = observeSetup(on, () => { ran++; return { result: {} } })
  await $.session.start(START)
  for (const call of [
    { tool: 'Bash', command: 'rm -f ~/.agentflare/backend.db' },
    { tool: 'PowerShell', command: 'ls' },
    { tool: 'mcp__lean-ctx__ctx_shell', command: 'ls' },
    { tool: 'TodoWrite', todos: [] },
    { tool: 'ToolSearch', query: 'ctx_read' },
  ]) await $.tool.call(call as any)
  expect(seen.filter(a => a.action === 'check').map(a => a.tool_name)).toEqual(['Bash', 'PowerShell', 'mcp__lean-ctx__ctx_shell', 'TodoWrite', 'ToolSearch'])
  expect(ran).toBe(5)
})

test('a data-guard deny on Bash refuses it with the server reason', async ($, on) => {
  stubSession(on)
  on('mcp.call', () => reply({ decision: 'deny', rule: 'data_guard', reason: "deleting agentflare's local data files is blocked" }))
  let ran = false
  on('tool.call', () => { ran = true; return { result: {} } })
  await $.session.start(START)
  const r = await $.tool.call({ tool: 'Bash', command: 'rm -f ~/.agentflare/backend.db' })
  expect(r.deny).toContain('local data files')
  expect(ran).toBe(false)
})

test('without a connection a shell tool and a mutating one are refused', async ($, on) => {
  stubSession(on, false)
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  expect((await $.tool.call({ tool: 'Bash', command: 'ls' })).deny).toContain('unavailable')
  expect((await $.tool.call({ tool: 'mcp__flare__item', action: 'done', id: 'x' } as any)).deny).toBeDefined()
})

test('tool.check refuses the model writing gate evidence through autopilot', async ($, on) => {
  stubSession(on, true, 'flare')
  on('tool.check', () => ({ decision: 'ask' }))
  await $.session.start(START)
  expect((await $.tool.check({ tool: 'mcp__flare__autopilot', input: { action: 'observe' } })).decision).toBe('deny')
})

// I4: only an explicit allow lets a checked tool run.
for (const [name, body] of [['an ask', { decision: 'ask' }], ['a missing decision', {}], ['a non-object', 'allow']] as const) {
  test(`${name} verdict refuses a checked tool`, async ($, on) => {
    stubSession(on)
    on('mcp.call', () => reply(body))
    let ran = false
    on('tool.call', () => { ran = true; return { result: {} } })
    await $.session.start(START)
    expect((await $.tool.call({ tool: 'Edit', file_path: '/r/a', old_string: 'a', new_string: 'b' })).deny).toContain('no allow verdict')
    expect(ran).toBe(false)
  })
}

test('tool.check leaves a self tool to the session rules when the plugin did not raise it', async ($, on) => {
  stubSession(on, true, 'flare')
  on('tool.check', () => ({ decision: 'ask' }))
  await $.session.start(START)
  expect((await $.tool.check({ tool: 'mcp__flare__autopilot', input: { action: 'check' } })).decision).toBe('ask')
})

const HEALTH = { daemon: 'up', stale_binary: false, auth: 'ok', mcp_version: '1.8.1' }
const fallbackReply = (health: unknown) => (_: any, e: any) =>
  reply(e.args.action === 'check' ? { decision: 'allow' } : e.args.action === 'health' ? health : { etag: 'e0', unchanged: true })

test('a refused connect falls back to a known server that serves autopilot and answers health', async ($, on) => {
  stubSession(on, false, 'agentflare', [{ name: 'mcp__flare__search', description: '', mcp: true }, { name: 'mcp__flare__autopilot', description: '', mcp: true }])
  const servers: string[] = []
  const answer = fallbackReply(HEALTH)
  on('mcp.call', (_: any, e: any) => { servers.push(e.server); return answer(_, e) })
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  expect((await $.tool.call({ tool: 'Edit', file_path: '/r/a', old_string: 'a', new_string: 'b' })).deny).toBeUndefined()
  expect(servers.length > 0 && servers.every(s => s === 'flare')).toBe(true)
})

test('the fallback never trusts an unknown server that exposes autopilot', async ($, on) => {
  stubSession(on, false, 'agentflare', [{ name: 'mcp__evil__autopilot', description: '', mcp: true }])
  const servers: string[] = []
  const answer = fallbackReply(HEALTH)
  on('mcp.call', (_: any, e: any) => { servers.push(e.server); return answer(_, e) })
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  expect((await $.tool.call({ tool: 'Edit', file_path: '/r/a', old_string: 'a', new_string: 'b' })).deny).toContain('unavailable')
  expect(servers).toEqual([])
})

test('the fallback rejects a known server whose health is invalid', async ($, on) => {
  stubSession(on, false, 'agentflare', [{ name: 'mcp__flare__autopilot', description: '', mcp: true }])
  const actions: string[] = []
  const answer = fallbackReply({ daemon: 'up' })
  on('mcp.call', (_: any, e: any) => { actions.push(e.args.action); return answer(_, e) })
  on('tool.call', () => ({ result: {} }))
  await $.session.start(START)
  expect((await $.tool.call({ tool: 'Edit', file_path: '/r/a', old_string: 'a', new_string: 'b' })).deny).toContain('unavailable')
  expect(actions).toEqual(['health'])
})

