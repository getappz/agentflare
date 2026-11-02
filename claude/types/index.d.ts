declare module 'claude-code' {
  interface PluginState {
    agentflare: {
      briefingEtag: string | null
      lastWakeAt: number
      autopilotPaused: string | null
    }
  }
}
