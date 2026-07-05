#!/usr/bin/env node
const fs = require('fs');
const path = require('path');
const os = require('os');

const claudeDir = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
const flagPath = path.join(claudeDir, '.leanstack-active');

function safeWriteFlag(fp, content) {
  try {
    fs.mkdirSync(path.dirname(fp), { recursive: true });
    try { if (fs.lstatSync(fp).isSymbolicLink()) return; } catch (e) { if (e.code !== 'ENOENT') return; }
    const tmp = path.join(path.dirname(fp), `.leanstack-active.${process.pid}.${Date.now()}`);
    const O_NOFOLLOW = typeof fs.constants.O_NOFOLLOW === 'number' ? fs.constants.O_NOFOLLOW : 0;
    let fd;
    try {
      fd = fs.openSync(tmp, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | O_NOFOLLOW, 0o600);
      fs.writeSync(fd, String(content));
    } finally { if (fd !== undefined) fs.closeSync(fd); }
    fs.renameSync(tmp, fp);
  } catch (_) {}
}

safeWriteFlag(flagPath, 'on');

const { writeRules, detectPending, confirmFlag } = require('./install.js');

let out = '';

try {
  const newRules = writeRules();
  if (newRules.length) out += 'leanstack rules installed: ' + newRules.join(', ') + '\n\n';
} catch (_) {}

try {
  if (!fs.existsSync(confirmFlag)) {
    const pending = detectPending();
    if (pending.length) {
      out += 'leanstack: the following need your one-time confirmation to install:\n';
      pending.forEach(p => out += `  - ${p.label}: \`${p.command}\`\n`);
      out += 'Type `/leanstack confirm` to install them. Nothing runs until you do.\n\n';
    } else {
      // Nothing pending — nothing to confirm, mark done silently.
      fs.writeFileSync(confirmFlag, new Date().toISOString());
    }
  }
} catch (_) {}

out += 'LEANSTACK ACTIVE — prefer lean-ctx ctx_* tools, Exa for search, clean git commits. Off: /leanstack off.';

process.stdout.write(out);
