import { expect, mock, test } from 'claude-code/testing'

const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const ANSWER = 'Fixed the race in store::open by adding busy_timeout.\nSecond line.'

// Stubs every op session.start and the capture hook call; tests override what they assert on.
function stubSession(on: any, label: string | undefined = 'bugfix', answer = 'yes', memoryThrows = false) {
  const clock = mock.clock(on)
  const seen = { classify: 0, ask: [] as any[], memory: [] as any[] }
  on('command.list', () => ({ value: [] }))
  on('command.register', () => ({ value: { command: 'flare' } }))
  on('session.version', () => ({ value: { version: '2.1.316', base: '2.1.316' } }))
  on('session.cwd', () => ({ value: '/' }))
  on('session.id', () => ({ value: 's1' }))
  on('ui.log', () => ({ value: {} }))
  on('ui.toast', () => ({ value: {} }))
  on('ui.invalidate', () => ({ value: {} }))
  on('state.set', () => ({ value: { isSet: true, version: 1 } }))
  on('mcp.connect', () => ({ value: { isConnected: true, server: 'agentflare' } }))
  on('model.classify', () => { seen.classify++; return { value: label } })
  // $.ui.ask is not a stubbable op: it is a tool.call of AskUserQuestion, so the test plays the user there.
  on('tool.call', (_: any, e: any) => {
    seen.ask.push(e)
    const q = e.questions[0]
    return { result: { questions: e.questions, answers: { [q.question]: answer } }, text: answer }
  })
  on('mcp.call', (_: any, e: any) => { if (e.tool === 'memory') { seen.memory.push(e); if (memoryThrows) throw new Error('boom') } return { value: { content: [] } } })
  on('session.start', (_: any, e: any) => ({ cwd: e.cwd }))
  on('turn.complete', (_: any, e: any) => ({ text: e.answer }))
  return { clock, seen }
}

test('auto mode remembers a bugfix with a topic key', async ($, on) => {
  const { seen } = stubSession(on)
  await $.session.start(START)
  const r = await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1' })
  expect(r.text).toBe(ANSWER)
  expect(seen.memory).toHaveLength(1)
  expect(seen.memory[0].args).toEqual({
    action: 'remember',
    type: 'bugfix',
    title: 'Fixed the race in store::open by adding busy_timeout.',
    content: ANSWER,
    topic_key: 'autopilot:session:bugfix',
  })
})

test('none skips the write', async ($, on) => {
  const { seen } = stubSession(on, 'none')
  await $.session.start(START)
  await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1' })
  expect(seen.classify).toBe(1)
  expect(seen.memory).toHaveLength(0)
})

test('an unknown label skips the write', async ($, on) => {
  const { seen } = stubSession(on, 'Decision!')
  await $.session.start(START)
  await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1' })
  expect(seen.memory).toHaveLength(0)
})

test('off mode never classifies', { options: { memoryCapture: 'off' } }, async ($, on) => {
  const { seen } = stubSession(on)
  await $.session.start(START)
  await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1' })
  expect(seen.classify).toBe(0)
  expect(seen.memory).toHaveLength(0)
})

test('ask mode writes only after yes', { options: { memoryCapture: 'ask' } }, async ($, on) => {
  const { seen } = stubSession(on, 'decision', 'yes')
  await $.session.start(START)
  await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1' })
  expect(seen.ask[0].questions[0].question).toBe('Remember this as a decision?')
  expect(seen.ask[0].questions[0].options.map((o: any) => o.label)).toEqual(['yes', 'no'])
  expect(seen.memory).toHaveLength(1)
})

test('ask mode answering no writes nothing', { options: { memoryCapture: 'ask' } }, async ($, on) => {
  const { seen } = stubSession(on, 'decision', 'no')
  await $.session.start(START)
  await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1' })
  expect(seen.ask).toHaveLength(1)
  expect(seen.memory).toHaveLength(0)
})

test('subagent turns and non-answer reasons never classify', async ($, on) => {
  const { seen } = stubSession(on)
  await $.session.start(START)
  await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1', agentId: 'a1' })
  await $.turn.complete({ reason: 'aborted', answer: ANSWER, durationMs: 1, isAborted: true, turnId: 't2' })
  await $.turn.complete({ reason: 'error', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't3' })
  expect(seen.classify).toBe(0)
  expect(seen.memory).toHaveLength(0)
})

test('title and content are bounded', async ($, on) => {
  const { seen } = stubSession(on)
  await $.session.start(START)
  await $.turn.complete({ reason: 'answer', answer: '\n\n' + 'x'.repeat(5000), durationMs: 1, isAborted: false, turnId: 't1' })
  expect(seen.memory[0].args.title).toHaveLength(120)
  expect(seen.memory[0].args.content).toHaveLength(2000)
})

test('a throwing memory call still returns the turn result', async ($, on) => {
  stubSession(on, 'bugfix', 'yes', true)
  await $.session.start(START)
  const r = await $.turn.complete({ reason: 'answer', answer: ANSWER, durationMs: 1, isAborted: false, turnId: 't1' })
  expect(r.text).toBe(ANSWER)
})
