#!/usr/bin/env node
// Component registry: each entry knows how to check itself and, if needed,
// fix itself. session-start.js and prompt-submit.js just iterate this list —
// neither hook hardcodes per-tool logic, so adding a component means adding
// one entry here, not touching the hooks.
//
// `host` ('claude-code' | 'codex' | 'cursor') is passed explicitly by each
// manifest's hook command (as argv[2]) rather than guessed — Claude Code and
// Codex both set the same ${CLAUDE_PLUGIN_ROOT} env var, so environment
// alone can't distinguish them.
const fs = require('fs');
const path = require('path');
const os = require('os');
const { execSync, spawn } = require('child_process');
const { STATE_DIR } = require('./state.js');
const RULE_TEXT = require('../rule-text.js');

const HOME = os.homedir();
const CAVEMAN_CONFIG = path.join(HOME, '.config', 'caveman', 'config.json');
const PONYTAIL_CONFIG = path.join(HOME, '.config', 'ponytail', 'config.json');
const LEANCTX_LOG = path.join(STATE_DIR, 'leanctx-install.log');

function enabledPlugins() {
  // Only meaningful on Claude Code — Codex/Cursor don't have this settings
  // shape, so callers gate this behind host === 'claude-code' first.
  try {
    return JSON.parse(fs.readFileSync(path.join(HOME, '.claude', 'settings.json'), 'utf8')).enabledPlugins || {};
  } catch (_) { return {}; }
}

function writePinnedMode(configPath) {
  try {
    let current = null;
    try { current = JSON.parse(fs.readFileSync(configPath, 'utf8')).defaultMode; } catch (_) {}
    if (current) return false;
    fs.mkdirSync(path.dirname(configPath), { recursive: true });
    fs.writeFileSync(configPath, '{"defaultMode": "ultra"}\n');
    return true;
  } catch (_) { return false; }
}

// Per-host rule targets. Claude Code writes to its global rules folder
// (affects every project). Codex/Cursor have no such global folder — they
// write project-local files instead, and only when absent, since a project
// file is more sensitive to clobber than a per-user dotfile.
function ruleTargets(host) {
  if (host === 'claude-code') {
    const dir = path.join(HOME, '.claude', 'rules');
    return [
      { path: path.join(dir, 'exa.md'), content: RULE_TEXT.exa },
      { path: path.join(dir, 'git.md'), content: RULE_TEXT.git },
      { path: path.join(dir, 'lean-ctx.md'), content: RULE_TEXT.leanctx },
    ];
  }
  if (host === 'cursor') {
    return [{
      path: path.join(process.cwd(), '.cursor', 'rules', 'leanstack.mdc'),
      content: '---\nalwaysApply: true\n---\n\n' + [RULE_TEXT.exa, RULE_TEXT.git, RULE_TEXT.leanctx].join('\n\n'),
    }];
  }
  if (host === 'codex') {
    // Codex reads project-root AGENTS.md natively; only create if the
    // project doesn't already have one — never clobber existing content.
    return [{
      path: path.join(process.cwd(), 'AGENTS.md'),
      content: '# Rules (leanstack)\n\n' + [RULE_TEXT.exa, RULE_TEXT.git, RULE_TEXT.leanctx].join('\n\n') + '\n',
    }];
  }
  return [];
}

module.exports = function getComponents(host) {
  const claudeCodeOnly = host === 'claude-code';

  return [
    {
      id: 'rules',
      needsConsent: false,
      describe: () => 'usage rules for ' + host,
      check() {
        return ruleTargets(host).every(t => fs.existsSync(t.path));
      },
      apply() {
        const written = [];
        for (const t of ruleTargets(host)) {
          if (!fs.existsSync(t.path)) {
            fs.mkdirSync(path.dirname(t.path), { recursive: true });
            fs.writeFileSync(t.path, t.content + '\n');
            written.push(path.basename(t.path));
          }
        }
        return written.length ? `rules written: ${written.join(', ')}` : 'rules already present';
      },
    },
    {
      id: 'leanctx',
      needsConsent: true,
      describe: () => 'lean-ctx (context compression) — npm install -g lean-ctx-bin && lean-ctx onboard',
      check() {
        try { execSync('lean-ctx --version', { stdio: 'pipe' }); return true; } catch (_) { return false; }
      },
      apply() {
        if (fs.existsSync(LEANCTX_LOG)) return 'lean-ctx install already triggered — check ' + LEANCTX_LOG;
        fs.mkdirSync(path.dirname(LEANCTX_LOG), { recursive: true });
        const cmd = 'npm install -g lean-ctx-bin && lean-ctx onboard';
        const fd = fs.openSync(LEANCTX_LOG, 'a');
        const child = process.platform === 'win32'
          ? spawn('cmd', ['/c', cmd], { detached: true, stdio: ['ignore', fd, fd] })
          : spawn('sh', ['-c', cmd], { detached: true, stdio: ['ignore', fd, fd] });
        child.unref();
        return 'lean-ctx install started in background — ready next session (log: ' + LEANCTX_LOG + ')';
      },
    },
    // Ponytail/Caveman are Claude Code plugins installed via the `claude
    // plugin` CLI — no Codex/Cursor equivalent exists, so these components
    // report "satisfied" (nothing to do) on every other host rather than
    // nagging forever about something that can't be installed there.
    {
      id: 'ponytail-plugin',
      needsConsent: true,
      describe: () => 'Ponytail plugin — claude plugin marketplace add DietrichGebert/ponytail && claude plugin install ponytail@ponytail',
      check() {
        return !claudeCodeOnly || enabledPlugins()['ponytail@ponytail'] === true;
      },
      apply() {
        execSync('claude plugin marketplace add DietrichGebert/ponytail', { stdio: 'pipe' });
        execSync('claude plugin install ponytail@ponytail', { stdio: 'pipe' });
        return 'Ponytail plugin installed — restart to activate';
      },
    },
    {
      id: 'ponytail-mode',
      needsConsent: false,
      describe: () => 'pin Ponytail to ultra mode',
      check() {
        if (!claudeCodeOnly) return true;
        try { return !!JSON.parse(fs.readFileSync(PONYTAIL_CONFIG, 'utf8')).defaultMode; } catch (_) { return false; }
      },
      apply() {
        return writePinnedMode(PONYTAIL_CONFIG) ? 'Ponytail pinned to ultra' : 'Ponytail mode already set';
      },
    },
    {
      id: 'caveman-mode',
      needsConsent: false,
      describe: () => 'pin Caveman to ultra mode',
      check() {
        if (!claudeCodeOnly) return true;
        if (enabledPlugins()['caveman@caveman'] !== true) return true; // nothing to pin yet
        try { return JSON.parse(fs.readFileSync(CAVEMAN_CONFIG, 'utf8')).defaultMode === 'ultra'; } catch (_) { return false; }
      },
      apply() {
        return writePinnedMode(CAVEMAN_CONFIG) ? 'Caveman pinned to ultra' : 'Caveman mode already set';
      },
    },
  ];
};
