import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const prepare = readFileSync(new URL('../src/browser_prepare.js', import.meta.url), 'utf8');
const chunk = readFileSync(new URL('../src/browser_chunk.js', import.meta.url), 'utf8');

function context(events, language = 'en') {
  return vm.createContext({
    window: { ytInitialPlayerResponse: { captions: { playerCaptionsTracklistRenderer: {
      captionTracks: [{ languageCode: language, baseUrl: 'https://www.youtube.com/api/timedtext?v=test' }]
    } } } },
    document: { querySelector() { return null; } },
    URL, setTimeout, AbortSignal,
    fetch: async () => ({ ok: true, json: async () => ({ events }) })
  });
}

test('extracts actual JSON3 captions and preserves Unicode and timestamps', async () => {
  const ctx = context([
    { tStartMs: 1234, segs: [{ utf8: 'こんにちは ' }, { utf8: 'world' }] },
    { tStartMs: 2000, segs: [{ utf8: '\n' }] }
  ]);
  const result = JSON.parse(await vm.runInContext(prepare.replace('__LANGUAGES__', '["en"]'), ctx));
  assert.equal(result.count, 1);
  const page = JSON.parse(vm.runInContext(chunk.replace('__OFFSET__', '0'), ctx));
  assert.equal(page.segments[0].text, 'こんにちは world');
  assert.equal(page.segments[0].start, 1.234);
  assert.equal(page.next, 1);
});

test('chunked extraction retains every segment beyond browser output cap', () => {
  const segments = Array.from({ length: 500 }, (_, i) => ({ start: i, text: '字幕 " \\ '.repeat(100) }));
  const ctx = vm.createContext({ window: { __agentflareTranscript: { segments } } });
  const recovered = [];
  for (let offset = 0; offset < segments.length;) {
    const output = vm.runInContext(chunk.replace('__OFFSET__', String(offset)), ctx);
    assert.ok(output.length < 10000);
    const page = JSON.parse(output);
    assert.ok(page.next > offset);
    recovered.push(...page.segments);
    offset = page.next;
  }
  assert.deepEqual(recovered, segments);
});

test('fails rather than returning captions in an unrequested language', async () => {
  const ctx = context([{ tStartMs: 0, segs: [{ utf8: 'Bonjour' }] }], 'fr');
  await assert.rejects(vm.runInContext(prepare.replace('__LANGUAGES__', '["en"]'), ctx), /language/i);
});

test('matches caption language tags without regard to case', async () => {
  for (const requested of ['EN-us', 'EN-gb']) {
    const ctx = context([{ tStartMs: 0, segs: [{ utf8: 'Hello' }] }], 'en-US');
    const result = JSON.parse(await vm.runInContext(
      prepare.replace('__LANGUAGES__', JSON.stringify([requested])), ctx));
    assert.equal(result.language, 'en-US');
    assert.equal(result.count, 1);
  }
});

function panelContext(initialSegments) {
  const ctx = context([]);
  const renderer = { data: { initialSegments } };
  const panel = { querySelector(selector) {
    if (selector === 'ytd-transcript-segment-renderer') return {};
    if (selector === 'ytd-transcript-segment-list-renderer') return renderer;
    return null;
  } };
  let clicked = false;
  ctx.fetch = async () => { throw new Error('blocked'); };
  ctx.document = {
    querySelector(selector) { return selector === 'ytd-transcript-renderer' && clicked ? panel : null; },
    querySelectorAll() { return [{ getAttribute: () => 'Show transcript', click() { clicked = true; } }]; }
  };
  return ctx;
}

test('falls back to transcript panel data when in-page fetch is blocked', async () => {
  const ctx = panelContext([
    { transcriptSegmentRenderer: { startMs: '1500', snippet: { runs: [{ text: 'Panel caption' }] } } }
  ]);
  const result = JSON.parse(await vm.runInContext(prepare.replace('__LANGUAGES__', '["en"]'), ctx));
  assert.equal(result.count, 1);
  const page = JSON.parse(vm.runInContext(chunk.replace('__OFFSET__', '0'), ctx));
  assert.deepEqual(page.segments, [{ start: 1.5, text: 'Panel caption' }]);
});

test('refuses partial transcript panel data with a continuation', async () => {
  const ctx = panelContext([
    { transcriptSegmentRenderer: { startMs: '0', snippet: { simpleText: 'Partial caption' } } },
    { continuationItemRenderer: {} }
  ]);
  await assert.rejects(vm.runInContext(prepare.replace('__LANGUAGES__', '["en"]'), ctx), /complete/i);
});
