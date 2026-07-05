#!/usr/bin/env node
const fs = require('fs');
const path = require('path');
const os = require('os');

const claudeDir = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
const flagPath = path.join(claudeDir, '.leanstack-active');

const { detectPending, applyInstall, confirmFlag } = require('./install.js');

let input = '';
process.stdin.on('data', chunk => { input += chunk; });
process.stdin.on('end', () => {
  try {
    const data = JSON.parse(input);
    const prompt = (data.prompt || '').trim().toLowerCase();

    if (prompt === '/leanstack off' || prompt === '/leanstack stop') {
      try { fs.unlinkSync(flagPath); } catch (_) {}
      return;
    }

    if (prompt === '/leanstack on') {
      try { fs.writeFileSync(flagPath, 'on', { mode: 0o600 }); } catch (_) {}
    }

    if (prompt === '/leanstack confirm') {
      if (fs.existsSync(confirmFlag)) {
        process.stdout.write(JSON.stringify({
          hookSpecificOutput: {
            hookEventName: 'UserPromptSubmit',
            additionalContext: 'leanstack: already confirmed, nothing pending.',
          },
        }));
        return;
      }
      const { installed, failed } = applyInstall();
      const lines = ['leanstack install confirmed.'];
      if (installed.length) lines.push('Installed: ' + installed.join(', '));
      if (failed.length) lines.push('Failed: ' + failed.join(', '));
      process.stdout.write(JSON.stringify({
        hookSpecificOutput: {
          hookEventName: 'UserPromptSubmit',
          additionalContext: lines.join('\n'),
        },
      }));
      return;
    }

    let active = false;
    try {
      const st = fs.lstatSync(flagPath);
      active = st.isFile() && !st.isSymbolicLink();
    } catch (_) {}

    if (active) {
      const bits = ['LEANSTACK ACTIVE.', 'Prefer lean-ctx ctx_* tools (ctx_read/ctx_search/ctx_shell/ctx_glob/ctx_compose) over native Read/Grep/Bash/Glob.', 'Exa is the only web search tool.', 'Clean git commits, no AI signature.'];
      if (!fs.existsSync(confirmFlag) && detectPending().length) {
        bits.push('Reminder: `/leanstack confirm` to finish install.');
      }
      process.stdout.write(JSON.stringify({
        hookSpecificOutput: {
          hookEventName: 'UserPromptSubmit',
          additionalContext: bits.join(' '),
        },
      }));
    }
  } catch (_) {}
});
