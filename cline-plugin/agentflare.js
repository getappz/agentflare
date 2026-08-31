// agentflare Cline plugin - thin shim over the `agentflare` binary.
//
// Why a plugin instead of Cline's file hooks (~/.cline/hooks/): file hooks
// can only *block* a tool (PreToolUse) and even then only by cancelling the
// whole run - they get no way to inject context into the model or to skip a
// single tool gracefully. Cline plugins (`AgentExtension`) get the full hook
// surface: beforeModel can prepend context, beforeTool can skip one tool
// with a model-visible reason. This mirrors exactly what agentflare already
// does for Claude Code's hooks (PreToolUse/SessionStart/PostToolFailure via
// ~/.claude/settings.json), reusing the same single entry point -
// `agentflare hook <event> --agent cline` - so there is one classifier
// (src/hook_redirect.rs, backed by flare_git_core::branch::is_protected_branch)
// consulted from every harness.
//
// Fail-open: if the agentflare binary is missing or the call fails, hooks
// return undefined and the run proceeds untouched (same philosophy as the
// opencode branch-guard plugin). Agentflare's own hook handlers are also
// fail-open on their side, so a dead plugin never blocks Cline.
//
// Cline discovers plugins automatically: any .js/.ts file dropped into
// ~/.cline/plugins/ (or <workspace>/.cline/plugins) is loaded and its
// `default` (or named `plugin`) export is the AgentExtension. No registry,
// no `cline plugin install` needed. This file is written there by
// `agentflare init` via the `cline-plugin` component (src/components.rs).

const { spawnSync } = require("node:child_process")

const AGENTFLARE_BIN = process.env.AGENTFLARE_BIN || "agentflare"
const HOOK_TIMEOUT_MS = 5000

// Tools that may mutate the working tree on the protected branch.
const GUARDED_TOOLS = new Set(["write", "edit", "apply_patch", "patch"])

// Session-start context is injected once per session (not per model call).
const injectedSessions = new Set()

function runHook(event, payload) {
  try {
    const res = spawnSync(
      AGENTFLARE_BIN,
      ["hook", event, "--agent", "cline"],
      {
        input: JSON.stringify(payload),
        encoding: "utf8",
        timeout: HOOK_TIMEOUT_MS,
        windowsHide: true,
        maxBuffer: 4 * 1024 * 1024,
      },
    )
    if (res.error || res.status !== 0 || !res.stdout) return null
    try {
      return JSON.parse(res.stdout)
    } catch {
      return null // no JSON on stdout (e.g. a nudge-only run) - nothing to act on
    }
  } catch {
    return null // agentflare not on PATH, or the call failed - fail open
  }
}

function buildUserMessage(text) {
  return {
    id: `agentflare-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    role: "user",
    content: [{ type: "text", text }],
    createdAt: Date.now(),
  }
}

const plugin = {
  name: "agentflare",
  manifest: { capabilities: ["hooks"] },

  setup(_api, ctx) {
    // workspaceInfo carries git state (branch, commit, remotes) sourced by the
    // host without ever shelling out to git - which is exactly the point: the
    // agentflare git shim denies cross-repo git calls, so the plugin must not
    // call git itself. Capture it for session-start context.
    const info = ctx?.workspaceInfo
    if (info?.rootPath) {
      global.__agentflareWorkspace = {
        rootPath: info.rootPath,
        branch: info.latestGitBranchName,
        commit: info.latestGitCommitHash,
        remotes: info.associatedRemoteUrls,
      }
    }
  },

  hooks: {
    // Inject agentflare session-start context (pending items, health nudges,
    // on/off state) into the first model call of the session by prepending a
    // user message - the file-hook equivalent has no way to do this.
    beforeModel(ctx) {
      const sessionId = ctx?.snapshot?.sessionId || "unknown"
      if (injectedSessions.has(sessionId)) return undefined
      injectedSessions.add(sessionId)

      const ws = global.__agentflareWorkspace || {}
      const out = runHook("session-start", {
        session_id: sessionId,
        workspace: ws,
      })
      if (!out) return undefined

      const text =
        out.hookSpecificOutput?.additionalContext ||
        out.systemMessage ||
        ""
      if (!text) return undefined

      const messages = ctx?.request?.messages
      if (!Array.isArray(messages) || messages.length === 0) return undefined
      return { messages: [buildUserMessage(text), ...messages] }
    },

    // Branch guard: block write/edit tools on the protected branch by skipping
    // just this tool with a model-visible reason - not the whole run (file
    // hooks can only cancel the entire run, which is why we're a plugin).
    beforeTool(ctx) {
      const sessionId = ctx?.snapshot?.sessionId || "unknown"
      const toolName = ctx?.tool?.name || ctx?.toolCall?.toolName
      if (!toolName || !GUARDED_TOOLS.has(toolName)) return undefined

      const out = runHook("pre-tool-use", {
        session_id: sessionId,
        tool_name: toolName,
        tool_input: ctx?.input ?? {},
      })
      const decision = out?.hookSpecificOutput
      if (decision?.permissionDecision === "deny") {
        return {
          skip: true,
          reason: `[agentflare] ${decision.permissionDecisionReason || "blocked"}`,
        }
      }
      return undefined
    },

    // Forward tool failures to the PostToolFailure pipeline (friction capture)
    // so agentflare sees Cline's errors the same way it sees Claude Code's.
    afterTool(ctx) {
      if (!ctx?.result?.isError) return undefined
      const sessionId = ctx?.snapshot?.sessionId || "unknown"
      runHook("post-tool-failure", {
        session_id: sessionId,
        tool_name: ctx?.tool?.name || ctx?.toolCall?.toolName,
        tool_input: ctx?.input ?? {},
      })
      return undefined
    },

    // Session end - best effort, never blocks.
    afterRun(ctx) {
      const sessionId = ctx?.snapshot?.sessionId || "unknown"
      runHook("session-end", { session_id: sessionId })
    },
  },
}

module.exports = plugin
module.exports.default = plugin
module.exports.plugin = plugin