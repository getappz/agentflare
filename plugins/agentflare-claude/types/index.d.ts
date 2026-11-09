declare module 'claude-code' {
  interface PluginState {
    agentflare: {
      briefingEtag: string | null
      lastWakeAt: number
      autopilotPaused: string | null
      autopilotMode: 'off' | 'once' | 'on' | null
      // The item the last wake started, so a hot reload (module state reset) does not wake it again.
      currentItemId: string | null
    }
  }
}
