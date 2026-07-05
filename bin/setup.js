#!/usr/bin/env node
// One-shot setup for tools with no hook mechanism (Windsurf, VS Code/Copilot,
// Cline, Continue) plus Cursor (which has real hooks but no marketplace to
// install a plugin from, so its hook scripts get copied in here too).
// Running this script IS the consent — no confirm-gate needed, unlike the
// live Claude Code/Codex plugin hooks which install without being asked to run.
const fs = require('fs');
const path = require('path');
const os = require('os');
const { execSync } = require('child_process');
const RULE_TEXT = require('../src/rule-text.js');

const HOME = os.homedir();
const CWD = process.cwd();
const LEANCTX_MCP_ENTRY = { command: 'lean-ctx', args: ['serve'] };
const RULES_BLOCK = Object.values(RULE_TEXT).join('\n\n') + '\n';

function which(cmd) {
  try {
    execSync(process.platform === 'win32' ? `where ${cmd}` : `which ${cmd}`, { stdio: 'pipe' });
    return true;
  } catch (_) { return false; }
}

function leanctxInstalled() {
  try { execSync('lean-ctx --version', { stdio: 'pipe' }); return true; } catch (_) { return false; }
}

function mergeJson(filePath, patch) {
  let existing = {};
  try { existing = JSON.parse(fs.readFileSync(filePath, 'utf8')); } catch (_) {}
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify({ ...existing, ...patch }, null, 2) + '\n');
}

function writeIfAbsent(filePath, content) {
  if (fs.existsSync(filePath)) return false;
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, content);
  return true;
}

function leanctxNote() {
  return leanctxInstalled()
    ? null
    : 'lean-ctx not installed — skipped MCP registration. Run: npm install -g lean-ctx-bin && lean-ctx onboard';
}

const TOOLS = {
  cursor: {
    detect: () => fs.existsSync(path.join(HOME, '.cursor')) || which('cursor'),
    setup() {
      const out = [];
      const hooksDest = path.join(CWD, '.cursor', 'leanstack');
      fs.mkdirSync(hooksDest, { recursive: true });
      for (const f of ['state.js', 'components.js', 'session-start.js', 'prompt-submit.js']) {
        fs.copyFileSync(path.join(__dirname, '..', 'src', 'hooks', f), path.join(hooksDest, f));
      }
      // components.js does require('../rule-text.js') relative to .cursor/leanstack/,
      // so the copy lands one level up, at .cursor/rule-text.js.
      fs.copyFileSync(path.join(__dirname, '..', 'src', 'rule-text.js'), path.join(CWD, '.cursor', 'rule-text.js'));
      out.push('.cursor/leanstack/*.js (hook scripts copied in)');

      const hooksJsonPath = path.join(CWD, '.cursor', 'hooks.json');
      const wrote = writeIfAbsent(hooksJsonPath, JSON.stringify({
        version: 1,
        hooks: {
          sessionStart: [{ command: 'node ./.cursor/leanstack/session-start.js cursor', type: 'command', timeout: 30 }],
          beforeSubmitPrompt: [{ command: 'node ./.cursor/leanstack/prompt-submit.js cursor', type: 'command', timeout: 10 }],
        },
      }, null, 2) + '\n');
      out.push(wrote ? '.cursor/hooks.json' : '.cursor/hooks.json (exists, skipped)');

      const note = leanctxNote();
      if (note) { out.push(note); }
      else {
        mergeJson(path.join(HOME, '.cursor', 'mcp.json'), { mcpServers: { 'lean-ctx': LEANCTX_MCP_ENTRY } });
        out.push('~/.cursor/mcp.json (lean-ctx registered)');
      }
      return out;
    },
  },
  windsurf: {
    detect: () => fs.existsSync(path.join(HOME, '.codeium', 'windsurf')),
    setup() {
      const out = [];
      const note = leanctxNote();
      if (note) { out.push(note); }
      else {
        mergeJson(path.join(HOME, '.codeium', 'windsurf', 'mcp_config.json'), { mcpServers: { 'lean-ctx': LEANCTX_MCP_ENTRY } });
        out.push('windsurf mcp_config.json (lean-ctx registered)');
      }
      const wrote = writeIfAbsent(path.join(CWD, '.windsurf', 'rules', 'leanstack.md'), RULES_BLOCK);
      out.push(wrote ? '.windsurf/rules/leanstack.md' : '.windsurf/rules/leanstack.md (exists, skipped)');
      return out;
    },
  },
  vscode: {
    detect: () => which('code'),
    setup() {
      const out = [];
      const note = leanctxNote();
      if (note) { out.push(note); }
      else {
        const payload = JSON.stringify({ name: 'lean-ctx', command: 'lean-ctx', args: ['serve'] });
        try {
          execSync(`code --add-mcp ${JSON.stringify(payload)}`, { stdio: 'pipe', shell: true });
          out.push('lean-ctx registered via code --add-mcp');
        } catch (e) {
          out.push('code --add-mcp failed (' + e.message.split('\n')[0] + ') — add manually to .vscode/mcp.json: ' + payload);
        }
      }
      const wrote = writeIfAbsent(path.join(CWD, '.github', 'copilot-instructions.md'), RULES_BLOCK);
      out.push(wrote ? '.github/copilot-instructions.md' : '.github/copilot-instructions.md (exists, skipped)');
      return out;
    },
  },
  cline: {
    detect: () => fs.existsSync(path.join(HOME, '.cline')),
    setup() {
      const out = [];
      const note = leanctxNote();
      if (note) { out.push(note); }
      else {
        mergeJson(path.join(HOME, '.cline', 'mcp.json'), { mcpServers: { 'lean-ctx': LEANCTX_MCP_ENTRY } });
        out.push('~/.cline/mcp.json (lean-ctx registered)');
      }
      const wrote = writeIfAbsent(path.join(CWD, '.clinerules', 'leanstack.md'), RULES_BLOCK);
      out.push(wrote ? '.clinerules/leanstack.md' : '.clinerules/leanstack.md (exists, skipped)');
      return out;
    },
  },
  continue: {
    detect: () => fs.existsSync(path.join(CWD, '.continue')),
    setup() {
      const out = [];
      const note = leanctxNote();
      if (note) { out.push(note); }
      else {
        const wrote = writeIfAbsent(path.join(CWD, '.continue', 'mcpServers', 'leanstack.json'), JSON.stringify(LEANCTX_MCP_ENTRY, null, 2) + '\n');
        out.push(wrote ? '.continue/mcpServers/leanstack.json' : '.continue/mcpServers/leanstack.json (exists, skipped)');
      }
      return out;
    },
  },
};

function main() {
  const requested = process.argv.slice(2).filter(a => !a.startsWith('-'));
  const targets = requested.length ? requested : Object.keys(TOOLS).filter(name => TOOLS[name].detect());

  if (!targets.length) {
    console.log('leanstack setup: no supported tool detected (Cursor/Windsurf/VS Code/Cline/Continue).');
    console.log('Run with an explicit name to force it, e.g.: npx github:getappz/leanstack cursor');
    return;
  }

  for (const name of targets) {
    const tool = TOOLS[name];
    if (!tool) { console.log(`unknown tool: ${name} (known: ${Object.keys(TOOLS).join(', ')})`); continue; }
    console.log(`\n${name}:`);
    for (const line of tool.setup()) console.log('  ' + line);
  }
}

main();
