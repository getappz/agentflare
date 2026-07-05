#!/usr/bin/env node
const fs = require('fs');
const path = require('path');
const os = require('os');
const { execSync, spawn } = require('child_process');

const claudeDir = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
const rulesDir = path.join(claudeDir, 'rules');
const settingsPath = path.join(claudeDir, 'settings.json');
const cavemanConfigDir = path.join(os.homedir(), '.config', 'caveman');
const cavemanConfigPath = path.join(cavemanConfigDir, 'config.json');
const ponytailConfigDir = path.join(os.homedir(), '.config', 'ponytail');
const ponytailConfigPath = path.join(ponytailConfigDir, 'config.json');
const rulesDoneFlag = path.join(claudeDir, '.leanstack-rules-done');
const confirmFlag = path.join(claudeDir, '.leanstack-confirmed');
const leanctxInstallLog = path.join(claudeDir, '.leanstack-leanctx-install.log');

const RULES = {
  'exa.md': [
    'ONLY use Exa MCP for internet search: `mcp__exa__web_search_exa`, `mcp__exa__get_code_context_exa`, `mcp__exa__company_research_exa`.',
    'NEVER use `WebFetch`, `WebSearch`, or the `websearch` agent.',
    'Exa is the ONLY web access tool. ALL sessions, ALL agents, ALL contexts.',
    'Verify online before answering uncertain topics.',
  ].join('\n'),
  'git.md': [
    'NEVER add "Generated with Claude Code" signature.',
    'NEVER add "Co-Authored-By: Claude <noreply@anthropic.com>".',
    'Commit messages CLEAN — only the message, nothing else.',
    'Use `git commit -m "message"` format only.',
  ].join('\n'),
  'lean-ctx.md': [
    'ALWAYS use lean-ctx ctx_* MCP tools instead of native equivalents:',
    '- `ctx_read` instead of `Read`/`cat` (cached, compresses on re-read)',
    '- `ctx_shell` instead of `Bash`/`Shell` (95+ compression patterns)',
    '- `ctx_search` instead of `Grep`/`rg` (compact results)',
    '- `ctx_glob` instead of `Glob`/`find`',
    '- `ctx_compose` FIRST to orient in unfamiliar code — replaces a manual search→read→search chain in one call.',
    '- `ctx_callgraph` for "who calls X" — not grep.',
    'Applies to ALL agents, ALL subagents, ALL exploration/edit tasks.',
  ].join('\n'),
};

function readSettings() {
  try { return JSON.parse(fs.readFileSync(settingsPath, 'utf8')); } catch (_) { return {}; }
}

// Static config only — no package/plugin installs, safe to run unconditionally
// on every machine that installs this plugin.
function writeRules() {
  if (fs.existsSync(rulesDoneFlag)) return [];
  const installed = [];
  fs.mkdirSync(rulesDir, { recursive: true });
  for (const [name, content] of Object.entries(RULES)) {
    const filePath = path.join(rulesDir, name);
    if (!fs.existsSync(filePath)) {
      fs.writeFileSync(filePath, content + '\n', { mode: 0o644 });
      installed.push(`rules/${name}`);
    }
  }
  fs.writeFileSync(rulesDoneFlag, new Date().toISOString());
  return installed;
}

// Detection only — no side effects. Used both to describe what's pending
// (before consent) and to decide what to actually install (after consent).
function detectPending() {
  const pending = [];

  let leanctxFound = false;
  try { execSync('lean-ctx --version', { stdio: 'pipe' }); leanctxFound = true; } catch (_) {}
  if (!leanctxFound && !fs.existsSync(leanctxInstallLog)) {
    pending.push({
      key: 'leanctx',
      label: 'lean-ctx (context compression)',
      command: 'npm install -g lean-ctx-bin && lean-ctx onboard',
    });
  }

  const ep = readSettings().enabledPlugins || {};
  if (ep['ponytail@ponytail'] !== true) {
    pending.push({
      key: 'ponytail',
      label: 'Ponytail plugin (code-writing discipline)',
      command: 'claude plugin marketplace add DietrichGebert/ponytail && claude plugin install ponytail@ponytail',
    });
  }

  return pending;
}

// Only called after the user has explicitly typed `/leanstack confirm`.
function applyInstall() {
  const pending = detectPending();
  const installed = [];
  const failed = [];

  for (const item of pending) {
    if (item.key === 'leanctx') {
      try {
        const child = spawn(
          process.platform === 'win32' ? 'cmd' : 'sh',
          process.platform === 'win32' ? ['/c', item.command] : ['-c', item.command],
          { detached: true, stdio: ['ignore', fs.openSync(leanctxInstallLog, 'a'), fs.openSync(leanctxInstallLog, 'a')] }
        );
        child.unref();
        installed.push('lean-ctx installing in background (ready next session, log: ' + leanctxInstallLog + ')');
      } catch (e) {
        failed.push('lean-ctx: ' + e.message);
      }
    } else if (item.key === 'ponytail') {
      try {
        execSync('claude plugin marketplace add DietrichGebert/ponytail', { stdio: 'pipe' });
        execSync('claude plugin install ponytail@ponytail', { stdio: 'pipe' });
        installed.push('Ponytail plugin installed (restart to activate)');
      } catch (e) {
        failed.push('Ponytail: ' + e.message);
      }
    }
  }

  // Pin default modes for whichever of Caveman/Ponytail are now present —
  // config writes only, not installs, safe unconditionally.
  try {
    const ep = readSettings().enabledPlugins || {};
    if (ep['caveman@caveman'] === true) {
      let needsConfig = true;
      try { if (JSON.parse(fs.readFileSync(cavemanConfigPath, 'utf8')).defaultMode === 'ultra') needsConfig = false; } catch (_) {}
      if (needsConfig) {
        fs.mkdirSync(cavemanConfigDir, { recursive: true });
        fs.writeFileSync(cavemanConfigPath, '{"defaultMode": "ultra"}\n');
        installed.push('Caveman → ultra default');
      }
    }
  } catch (_) {}
  try {
    fs.mkdirSync(ponytailConfigDir, { recursive: true });
    let needsConfig = true;
    try { if (JSON.parse(fs.readFileSync(ponytailConfigPath, 'utf8')).defaultMode) needsConfig = false; } catch (_) {}
    if (needsConfig) {
      fs.writeFileSync(ponytailConfigPath, '{"defaultMode": "ultra"}\n');
      installed.push('Ponytail → ultra default');
    }
  } catch (_) {}

  fs.writeFileSync(confirmFlag, new Date().toISOString());
  return { installed, failed };
}

module.exports = { writeRules, detectPending, applyInstall, confirmFlag };
