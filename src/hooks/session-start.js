#!/usr/bin/env node
const state = require('./state.js');
const getComponents = require('./components.js');

const host = process.argv[2] || 'claude-code';
const components = getComponents(host);

const s = state.load();
s.active = true;

const lines = [];
const stillPending = [];

for (const c of components) {
  if (c.check()) continue;
  if (!c.needsConsent) {
    lines.push(c.apply());
  } else if (s.confirmed) {
    lines.push(c.apply());
  } else {
    stillPending.push(c);
  }
}

if (stillPending.length && !s.confirmed) {
  lines.push('');
  lines.push('leanstack: the following need one-time confirmation before installing:');
  for (const c of stillPending) lines.push('  - ' + c.describe());
  lines.push('Type `/leanstack confirm` to install them. Nothing runs until you do.');
} else if (!stillPending.length && !s.confirmed) {
  s.confirmed = true; // nothing needed consent — mark done, no need to ask
}

state.save(s);

lines.push('');
lines.push('LEANSTACK ACTIVE — lean-ctx tools, Exa search, clean git commits. Off: /leanstack off.');

process.stdout.write(lines.join('\n'));
