// Run with: node crates/flare-insights/tests/dashboard_error.mjs
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../src/api.rs', import.meta.url), 'utf8');
const script = source.split('<script>')[1].split('</script>')[0];
const unhandled = [];
process.on('unhandledRejection', error => unhandled.push(error));

for (const failingUrl of ['/api/sessions?limit=20', '/api/stats']) {
  const elements = Object.fromEntries(['stats', 'sessions', 'status'].map(id => [id, {
    innerHTML: id === 'stats' ? 'Loading…' : '', textContent: '',
  }]));
  let failure = 'http';
  let poll;
  const context = vm.createContext({
    document: {
      getElementById: id => elements[id],
      querySelector: selector => {
        assert.equal(selector, '#sessions tbody');
        return elements.sessions;
      },
      addEventListener() {},
    },
    fetch: async url => {
      assert.ok(['/api/sessions?limit=20', '/api/stats'].includes(url));
      if (url === failingUrl && failure === 'network') throw new Error('Network unavailable');
      if (url === failingUrl && failure === 'json') {
        return new Response('invalid json', { status: 200 });
      }
      const body = url === failingUrl && failure === 'http'
        ? { error: '<b>database unavailable</b>' }
        : url === '/api/stats'
          ? { total_sessions: 1, total_tokens: 1234, total_cost_usd: 0.5, cache_read_share: 0.25 }
          : [{ project: 'Recovered project', source: 'claude', turn_count: 2,
            tool_call_count: 3, cost: { total_usd: 0.5 }, updated_at: '2026-11-13T10:00:00Z' }];
      return new Response(JSON.stringify(body), {
        status: url === failingUrl && failure === 'http' ? 500 : 200,
      });
    },
    setInterval: callback => { poll = callback; return 1; },
    clearInterval() {},
  });
  vm.runInContext(script, context);
  await new Promise(setImmediate);
  assert.match(elements.status.textContent, /failed|error|unavailable/i,
    `${failingUrl}: initial HTTP 500 must show a visible failure`);
  assert.equal(elements.status.innerHTML, '', 'error must use textContent');
  assert.equal(elements.stats.innerHTML, 'Loading…', 'failed refresh must not render bogus stats');

  failure = null;
  await poll();
  assert.match(elements.status.textContent, /^updated /);
  assert.match(elements.sessions.innerHTML, /Recovered project/);
  assert.match(elements.stats.innerHTML, /1\.2k/);
  const renderedStats = elements.stats.innerHTML;
  const renderedSessions = elements.sessions.innerHTML;

  for (const mode of ['http', 'network', 'json']) {
    failure = mode;
    await poll();
    assert.match(elements.status.textContent, /failed|error|unavailable/i, `${mode} must show a failure`);
    assert.equal(elements.stats.innerHTML, renderedStats);
    assert.equal(elements.sessions.innerHTML, renderedSessions);
    assert.equal(elements.status.innerHTML, '', 'error must use textContent');
    failure = null;
    await poll();
    assert.match(elements.status.textContent, /^updated /, 'later poll must recover');
  }
  console.log(`PASS ${failingUrl}: HTTP 500, network/JSON errors, and poll recovery`);
}
assert.deepEqual(unhandled, [], 'automatic initial loads must handle rejected requests');
