import type { Register } from 'claude-code'
import { registerAutopilot } from './autopilot'
import { refreshBriefing, registerBriefing } from './briefing'
import { registerCapture } from './capture'
import { registerCompaction, type Current } from './compaction'
import { registerGuard } from './guard'
import { connect, type AutopilotClient } from './mcp'
import { registerSpawn } from './spawn'

export const AGENT = 'claude-code'

// The mod needs Claude Code 2.1.287 or newer. The generated types (claude-code.d.ts) declare no
// manifest field for a version floor, so session.start checks it with $.session.version().
// $.ui.notice is tied to a tool_use_id (claude-code.d.ts:2382), so the warning is a toast.
const MIN_VERSION = [2, 1, 287]

function isBelowFloor(version: string): boolean {
  const parts = version.split(/[.-]/).slice(0, 3).map(Number)
  for (let i = 0; i < 3; i++) {
    if ((parts[i] ?? 0) !== MIN_VERSION[i]) return (parts[i] ?? 0) < MIN_VERSION[i]
  }
  return false
}

// Module-scope guards: session.start can fire again (resume, clear); register and connect once.
// The client is handed to the guard, briefing, compaction, spawn, capture and autopilot modules as they land.
let client: AutopilotClient | null = null
let registered = false
let warned = false
let current: Current = {}

/** The item the mod is working on, for the compaction checkpoint; branch and worktree stay unknown in M1. */
export function setCurrent(next: Current): void {
  current = next
}

const DEFAULT_GUARD_TIMEOUT_MS = 3000

export const register: Register = (on, options) => {
  const guardTimeoutMs = Number(options.guardTimeoutMs) || DEFAULT_GUARD_TIMEOUT_MS
  registerGuard(on, {
    client: () => client,
    agent: AGENT,
    options: { readOnlyAutoAllow: options.readOnlyAutoAllow !== false, guardTimeoutMs },
  })

  const briefingCtx = { client: () => client, agent: AGENT }
  registerBriefing(on, briefingCtx)
  registerCompaction(on, { ...briefingCtx, current: () => current })
  registerSpawn(on, { server: () => client?.server ?? null, agent: AGENT })
  const memoryCapture = options.memoryCapture === 'ask' || options.memoryCapture === 'off' ? options.memoryCapture : 'auto'
  registerCapture(on, { server: () => client?.server ?? null, options: { memoryCapture }, current: () => current })
  const autopilot = options.autopilot === 'on' || options.autopilot === 'once' ? options.autopilot : 'off'
  registerAutopilot(on, {
    client: () => client,
    agent: AGENT,
    options: { autopilot, idleMinutes: options.idleMinutes === undefined ? 3 : Number(options.idleMinutes), maxCostUsd: Number(options.maxCostUsd) || Infinity },
    current: () => current,
    setCurrent,
  })

  on('session.start', async ($, e, next) => {
    // A hot reload resets the module flag, so the command list is the real guard against a second /flare.
    if (!registered) {
      try {
        const commands = await $.command.list()
        if (!commands.some(c => c.name.replace(/^\//, '') === 'flare')) {
          await $.command.register({ name: 'flare', description: 'agentflare autopilot status and controls' })
        }
        registered = true
      } catch (err) {
        $.ui.log(`agentflare: /flare registration failed: ${String(err)}`)
      }
      try {
        const { base, version } = await $.session.version()
        if (isBelowFloor(base ?? version)) {
          $.ui.toast(`agentflare needs Claude Code ${MIN_VERSION.join('.')} or newer (running ${version}).`)
        }
      } catch (err) {
        $.ui.log(`agentflare: version check failed: ${String(err)}`)
      }
    }
    if (!client) {
      try {
        client = await connect($, guardTimeoutMs)
      } catch (err) {
        $.ui.log(`agentflare: MCP connect failed: ${String(err)}`)
      }
      if (!client && !warned) {
        warned = true
        $.ui.toast('agentflare MCP server is not connected; autopilot is inactive this session.')
      }
    }
    await refreshBriefing($, briefingCtx)
    return next(e)
  })
}
