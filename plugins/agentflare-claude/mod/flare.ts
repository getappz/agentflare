import type { On } from 'claude-code'
import { autopilotState, clearCurrentItem, setAutopilotMode, type Mode } from './autopilot'
import type { Current } from './compaction'
import { refreshBriefing } from './briefing'
import type { AutopilotClient } from './mcp'

type FlareContext = {
  client: () => AutopilotClient | null
  current: () => Current
  agent: string
}

const USAGE = 'usage: /flare status | autopilot on|off|once | doctor | briefing refresh | clear'
const MODES: readonly string[] = ['on', 'off', 'once']

export function flareCommand(on: On, { client, current, agent }: FlareContext): void {
  // e.args is "everything after the name, as typed" (claude-code.d.ts, CommandRunInput.args).
  on('command.run', { command: 'flare' }, async ($, e) => {
    const c = client()
    if (!c) return { text: 'agentflare MCP is not connected.' }
    const [sub, arg] = e.args.trim().split(/\s+/)
    if (sub === 'clear' || sub === 'done') {
      await clearCurrentItem($)
      return { text: 'current item cleared' }
    }
    if (sub === 'autopilot' && MODES.includes(arg ?? '')) {
      setAutopilotMode($, arg as Mode)
      return { text: `autopilot: ${arg}` }
    }
    if (sub === 'briefing' && arg === 'refresh') {
      await refreshBriefing($, { client, agent })
      return { text: 'briefing refreshed' }
    }
    if (sub === 'status' || sub === 'doctor') {
      // A throwing hook is skipped and the engine then reports that no hook answered /flare (seen in
      // -p when the health call was refused), so a failed call is answered as text instead.
      let h
      try {
        h = await c.health()
      } catch (err) {
        return { text: `agentflare: health check failed: ${String(err)}` }
      }
      if (sub === 'doctor') return { text: Object.entries(h).map(([k, v]) => `${k}: ${v}`).join('\n') }
      const item = current().item
      return {
        text: [
          `daemon: ${h.daemon}`,
          `auth: ${h.auth}`,
          `stale_binary: ${h.stale_binary}`,
          `item: ${item ? `#${item.seq} ${item.title}` : 'none'}`,
          `autopilot: ${autopilotState.mode()}${autopilotState.paused() ? ` (paused: ${autopilotState.paused()})` : ''}`,
        ].join('\n'),
      }
    }
    return { text: USAGE }
  })
}
