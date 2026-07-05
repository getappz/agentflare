#!/usr/bin/env node
// Single JSON state blob, host-neutral (~/.leanstack/, not ~/.claude/) since
// the same hooks now run under Claude Code, Codex, and Cursor — none of them
// should inherit another host's config directory by accident.
const fs = require('fs');
const path = require('path');
const os = require('os');

const STATE_DIR = path.join(os.homedir(), '.leanstack');
const STATE_PATH = path.join(STATE_DIR, 'state.json');

const DEFAULT_STATE = {
  active: true,
  confirmed: false,
};

function load() {
  try {
    return { ...DEFAULT_STATE, ...JSON.parse(fs.readFileSync(STATE_PATH, 'utf8')) };
  } catch (_) {
    return { ...DEFAULT_STATE };
  }
}

function save(state) {
  fs.mkdirSync(STATE_DIR, { recursive: true });
  fs.writeFileSync(STATE_PATH, JSON.stringify(state, null, 2) + '\n');
}

module.exports = { load, save, STATE_DIR };
