#!/usr/bin/env node
const state = require('./state.js');
const getComponents = require('./components.js');

const host = process.argv[2] || 'claude-code';
const components = getComponents(host);

function emit(text) {
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: { hookEventName: 'UserPromptSubmit', additionalContext: text },
  }));
}

// Field name for the submitted prompt text is documented as "prompt" for
// Claude Code; Codex/Cursor are confirmed to share the same stdin/stdout
// hook contract, but exact key naming wasn't independently verified for
// them, so fall back across the plausible alternatives rather than assume.
function promptText(data) {
  return (data.prompt || data.text || data.message || '').trim().toLowerCase();
}

let input = '';
process.stdin.on('data', chunk => { input += chunk; });
process.stdin.on('end', () => {
  let prompt = '';
  try { prompt = promptText(JSON.parse(input)); } catch (_) { return; }

  const s = state.load();

  if (prompt === '/leanstack off' || prompt === '/leanstack stop') {
    s.active = false;
    state.save(s);
    return;
  }
  if (prompt === '/leanstack on') {
    s.active = true;
    state.save(s);
  }

  if (prompt === '/leanstack confirm') {
    if (s.confirmed) { emit('leanstack: already confirmed, nothing pending.'); return; }
    const results = [];
    for (const c of components) {
      if (c.needsConsent && !c.check()) results.push(c.apply());
    }
    s.confirmed = true;
    state.save(s);
    emit(results.length ? 'leanstack install confirmed.\n' + results.join('\n') : 'leanstack: nothing was pending.');
    return;
  }

  if (!s.active) return;

  const bits = [
    'LEANSTACK ACTIVE.',
    'Prefer lean-ctx ctx_* tools over native Read/Grep/Bash/Glob.',
    'Exa is the only web search tool.',
    'Clean git commits, no AI signature.',
  ];
  if (!s.confirmed) {
    const pending = components.some(c => c.needsConsent && !c.check());
    if (pending) bits.push('Reminder: `/leanstack confirm` to finish install.');
  }
  emit(bits.join(' '));
});
