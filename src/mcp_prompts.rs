//! MCP "Prompts" for flare code — surfaces `/optimize*` as native Claude Code
//! slash commands via the MCP protocol (same mechanism lean-ctx uses for its
//! own `/lean-ctx*` commands), routed entirely through agentflare's own
//! optimize port. No dependency on the DietrichGebert/ponytail marketplace
//! plugin.

use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, Prompt, PromptArgument, PromptMessage,
    PromptMessageRole,
};

const SUB_SKILLS: &[(&str, &str)] = &[
    (
        "review",
        "Over-engineering review of the current diff/branch/repo",
    ),
    (
        "audit",
        "Whole-repo over-engineering audit: ranked list of what to delete",
    ),
    (
        "debt",
        "Harvest `flare-code:` shortcut comments into a tracked ledger",
    ),
    (
        "gain",
        "Measured-impact scoreboard: less code, less cost, more speed",
    ),
    (
        "help",
        "Quick-reference card for all flare code modes, skills, and commands",
    ),
    (
        "playbook",
        "TDD-aware project companion — red-green-refactor enforced",
    ),
    (
        "no-hallucination",
        "Reality-check layer: blocks invented APIs, deprecated methods, undeclared variables",
    ),
];

pub fn list_prompts() -> Vec<Prompt> {
    let mut prompts = vec![
        Prompt::new(
            "optimize",
            Some("Switch or report flare code lazy-dev mode"),
            Some(vec![PromptArgument::new("mode")
                .with_description("lite|full|ultra|off|status (omit to report current mode)")]),
        ),
        Prompt::new(
            "artifact",
            Some("Publish, list, get, update, or delete live-shareable artifact pages"),
            Some(vec![PromptArgument::new("command").with_description(
                "publish|list|get|update|delete plus options, e.g. `publish --type markdown --favicon 🚀` (omit for usage)",
            )]),
        ),
        Prompt::new(
            "handoff",
            Some("Hand a work product to another agent runtime, or check your inbox/threads"),
            Some(vec![PromptArgument::new("command").with_description(
                "`<recipient> <brief>` to send (e.g. `codex review the API design above`), `inbox [me]`, or `thread <id>` (omit for usage)",
            )]),
        ),
    ];
    prompts.extend(
        SUB_SKILLS
            .iter()
            .map(|(name, desc)| Prompt::new(format!("optimize-{name}"), Some(*desc), None)),
    );
    prompts
}

pub fn get_prompt(
    request: &GetPromptRequestParams,
    agent: Option<&str>,
) -> Option<GetPromptResult> {
    if request.name == "artifact" {
        return Some(get_artifact_command(request));
    }
    if request.name == "handoff" {
        return Some(get_handoff_command(request, agent));
    }
    if request.name == "optimize" {
        return Some(get_optimize_mode(request));
    }
    let skill = request.name.strip_prefix("optimize-")?;
    SUB_SKILLS
        .iter()
        .any(|(name, _)| *name == skill)
        .then(|| get_optimize_skill(skill))
}

fn assistant_text(msg: impl Into<String>) -> GetPromptResult {
    GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::Assistant,
        msg,
    )])
}

fn get_optimize_mode(request: &GetPromptRequestParams) -> GetPromptResult {
    let mode_arg = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();

    if mode_arg.is_empty() || mode_arg == "status" {
        let mode = crate::optimize::code::active_mode()
            .unwrap_or_else(crate::optimize::code::default_mode);
        return assistant_text(if mode == "off" {
            "flare code is off. Use /optimize mode=lite|full|ultra to activate.".to_string()
        } else {
            format!("FLARE CODE MODE ACTIVE — level: {mode}")
        });
    }
    if mode_arg == "off" {
        crate::optimize::code::clear_active();
        return assistant_text("flare code is now off.");
    }
    match crate::optimize::code::normalize_config_mode(&mode_arg) {
        Some(normalized) => match crate::optimize::code::set_active(normalized) {
            Ok(()) => {
                assistant_text(crate::optimize::code::build_instructions(normalized, None).body)
            }
            Err(e) => assistant_text(format!("Failed to persist flare code mode: {e}")),
        },
        None => assistant_text(format!(
            "Unknown flare code mode '{mode_arg}'. Use lite|full|ultra|off|status."
        )),
    }
}

fn get_artifact_command(request: &GetPromptRequestParams) -> GetPromptResult {
    let command = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if command.is_empty() {
        // Client-agnostic: Claude Code renders this prompt's name differently
        // across versions (/agentflare:artifact vs /mcp__agentflare__artifact),
        // so the usage card only shows the argument part.
        return assistant_text(
            "Artifact commands (live-shareable local pages) — pass as this command's argument.\n\
             Deprecated for agent-to-agent handoffs: `/handoff` now assigns items and attaches \
             content as versioned assets instead of publishing artifacts. This command remains \
             for standalone shareable pages (dashboards, reports) — kept for reference/backward \
             compatibility, not the recommended path for new agent-to-agent work.\n\
             publish [--name N] [--type html|markdown|mermaid|diagram|text] [--session S] [--label L] [--description D] [--favicon 🚀] — publish preceding/attached content\n\
             update <id> [--base-version N] [options] — update in place (open tabs live-reload)\n\
             list [--session S]\n\
             get <id> [--version N]\n\
             delete <id>",
        );
    }

    assistant_text(format!(
        "Artifact command requested: `{command}`\n\n\
         Deprecated for agent-to-agent handoffs (use `/handoff` instead); still fine for \
         standalone shareable pages.\n\n\
         Parse the subcommand and options, then execute with the agentflare MCP tools \
         (load via ToolSearch if deferred):\n\
         - publish → artifact_publish; content is the inline content if given, otherwise \
         the most relevant content from the conversation (ask if genuinely ambiguous). \
         Map --name, --type, --session (session_id), --label, --description, --favicon.\n\
         - update <id> → artifact_publish with update_id=<id>; honor --base-version (base_version).\n\
         - list → artifact_list, honoring --session.\n\
         - get <id> → artifact_get, honoring --version.\n\
         - delete <id> → artifact_delete.\n\
         After the call, report the resulting URL (or listing/content) to the user."
    ))
}

fn get_handoff_command(request: &GetPromptRequestParams, agent: Option<&str>) -> GetPromptResult {
    // Identity comes from AGENTFLARE_AGENT baked into the MCP entry by
    // `agentflare init --agent <name>`; claude-code is the legacy default.
    let me = agent.unwrap_or("claude-code");
    let command = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    // Bare `/handoff` checks your own inbox rather than printing a usage
    // card — that's the common case, and the grammar below already covers
    // `inbox` alongside the other subcommands.
    let command = if command.is_empty() {
        "inbox".to_string()
    } else {
        command
    };

    assistant_text(format!(
        "Handoff command: `{command}`\n\n\
         Grammar: first word is a subcommand (`inbox`, `thread`) or a recipient; the rest is the brief.\n\
         - `<recipient> <brief>` → call the `handoff` tool with recipient=<recipient>, \
         name from the brief, content = the work product the brief points at (the preceding \
         conversation content, diff, review, or document — ask only if genuinely ambiguous), \
         and a thread_id when continuing an exchange. This assigns/creates an item for the \
         recipient and attaches the content to it as a versioned asset — prepend the brief to \
         the content so the recipient knows what is being asked (sender is set to your \
         identity, {me}, automatically). Use the `handoff` tool, not a bare item update, so \
         recipient can't be omitted. When answering an item from your inbox, set \
         item_id=<that item's id> (so the reply becomes the next asset version instead of a new \
         item) and reply_to=<id of the specific message you're answering>, reusing its \
         thread_id.\n\
         If the work already lives on some other existing item (not just your \
         own inbox reply), pass that item's id as item_id too — omitting it \
         always creates a new item, even when one covering this work already \
         exists. And if this is just a plain-text status update with no \
         versioned artifact to attach, skip `handoff` entirely: call `comment` \
         (action=create, item_id=<id>) plus `item` (action=update, id=<id>, \
         assignee_agent=<recipient>) instead — lighter, no new item, no asset.\n\
         - `inbox [me]` → call the `item` tool (action=list, state_group=\"backlog,unstarted,started\" \
         by default to hide completed/cancelled items — omit state_group only if the command \
         explicitly says `all`) — already scoped to this repo's linked project — and filter to \
         items where assignee_agent is <me or {me}> or unassigned; summarize name, state, and \
         brief per item. Pull an item's full content only if you need it, via the `asset` tool \
         (action=list, item_id=<id>) and asset get on the latest version.\n\
         - `thread <id>` → call `item` (action=list), filter client-side to items whose \
         metadata.thread matches <id>, then pull each item's assets (asset tool) for content; \
         present in chronological order with reply lineage.\n\
         Report the resulting listing afterwards. Work products only — facts/decisions go to \
         memory (memory_remember), not items."
    ))
}

fn get_optimize_skill(skill: &str) -> GetPromptResult {
    if let Err(e) = crate::optimize::code::set_active(skill) {
        return assistant_text(format!("Failed to persist flare code mode: {e}"));
    }
    let body = crate::optimize::code::sub_skills::get(skill).unwrap_or_default();
    assistant_text(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_optimize_and_all_sub_skills() {
        let prompts = list_prompts();
        let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"optimize"));
        assert!(names.contains(&"optimize-review"));
        assert!(names.contains(&"optimize-no-hallucination"));
        // optimize + artifact + handoff + one per sub-skill
        assert_eq!(names.len(), 3 + SUB_SKILLS.len());
    }

    #[test]
    fn unknown_prompt_name_returns_none() {
        assert!(get_prompt(&GetPromptRequestParams::new("not-a-real-prompt"), None).is_none());
    }

    #[test]
    fn lists_artifact_prompt() {
        let prompts = list_prompts();
        assert!(prompts.iter().any(|p| p.name == "artifact"));
    }

    #[test]
    fn bare_artifact_prompt_returns_usage() {
        let result = get_prompt(&GetPromptRequestParams::new("artifact"), None).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("publish"), "{text}");
        assert!(text.contains("list"), "{text}");
    }

    #[test]
    fn lists_handoff_prompt() {
        let prompts = list_prompts();
        assert!(prompts.iter().any(|p| p.name == "handoff"));
    }

    #[test]
    fn bare_handoff_prompt_returns_usage() {
        let result = get_prompt(&GetPromptRequestParams::new("handoff"), None).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("<recipient>"), "{text}");
        assert!(text.contains("inbox"), "{text}");
        assert!(text.contains("thread"), "{text}");
    }

    #[test]
    fn handoff_prompt_embeds_command_and_tool_mapping() {
        use rmcp::model::JsonObject;
        let mut args = JsonObject::new();
        args.insert(
            "command".to_string(),
            serde_json::json!("codex review the API design above"),
        );
        let params = GetPromptRequestParams::new("handoff").with_arguments(args);
        let result = get_prompt(&params, None).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("codex review the API design above"), "{text}");
        assert!(text.contains("`handoff` tool"), "{text}");
        assert!(text.contains("recipient"), "{text}");
        assert!(text.contains("reply_to"), "{text}");
    }

    #[test]
    fn artifact_prompt_embeds_command_and_tool_mapping() {
        use rmcp::model::JsonObject;
        let mut args = JsonObject::new();
        args.insert(
            "command".to_string(),
            serde_json::json!("publish --type markdown --favicon 🚀"),
        );
        let params = GetPromptRequestParams::new("artifact").with_arguments(args);
        let result = get_prompt(&params, None).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(
            text.contains("publish --type markdown --favicon 🚀"),
            "{text}"
        );
        assert!(text.contains("artifact_publish"), "{text}");
        assert!(text.contains("artifact_delete"), "{text}");
    }

    #[test]
    fn handoff_grammar_uses_agent_identity_for_sender_and_inbox() {
        use rmcp::model::JsonObject;
        let mut args = JsonObject::new();
        args.insert("command".to_string(), serde_json::json!("inbox"));
        let params = GetPromptRequestParams::new("handoff").with_arguments(args);
        let result = get_prompt(&params, Some("opencode")).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("identity, opencode"), "{text}");
        assert!(
            text.contains("assignee_agent is <me or opencode>"),
            "{text}"
        );
        assert!(!text.contains("claude-code"), "{text}");
    }

    #[test]
    fn handoff_identity_falls_back_to_claude_code() {
        use rmcp::model::JsonObject;
        let mut args = JsonObject::new();
        args.insert("command".to_string(), serde_json::json!("inbox"));
        let params = GetPromptRequestParams::new("handoff").with_arguments(args);
        let result = get_prompt(&params, None).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("identity, claude-code"), "{text}");
    }

    #[test]
    fn bare_handoff_defaults_to_inbox_for_the_calling_agent() {
        let result = get_prompt(&GetPromptRequestParams::new("handoff"), Some("opencode")).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("Handoff command: `inbox`"), "{text}");
        assert!(text.contains("identity, opencode"), "{text}");
    }

    #[test]
    fn bare_handoff_command_matches_explicit_inbox_command() {
        use rmcp::model::JsonObject;
        let bare = get_prompt(&GetPromptRequestParams::new("handoff"), Some("codex")).unwrap();
        let mut args = JsonObject::new();
        args.insert("command".to_string(), serde_json::json!("inbox"));
        let params = GetPromptRequestParams::new("handoff").with_arguments(args);
        let explicit = get_prompt(&params, Some("codex")).unwrap();
        assert_eq!(
            format!("{:?}", bare.messages[0].content),
            format!("{:?}", explicit.messages[0].content),
        );
    }

    #[test]
    fn inbox_grammar_defaults_state_group_to_open_states_unless_all() {
        let result = get_prompt(&GetPromptRequestParams::new("handoff"), None).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("backlog,unstarted,started"), "{text}");
        assert!(text.contains("`all`"), "{text}");
    }

    #[test]
    fn optimize_review_returns_full_skill_body() {
        let result = get_prompt(&GetPromptRequestParams::new("optimize-review"), None).unwrap();
        let PromptMessage { content, .. } = &result.messages[0];
        let text = format!("{content:?}");
        assert!(text.contains("review"));
    }

    #[test]
    fn bare_optimize_without_mode_reports_without_crashing() {
        let result = get_prompt(&GetPromptRequestParams::new("optimize"), None).unwrap();
        assert_eq!(result.messages.len(), 1);
    }

    #[test]
    fn optimize_with_unknown_mode_reports_error_text() {
        use rmcp::model::JsonObject;
        let mut args = JsonObject::new();
        args.insert("mode".to_string(), serde_json::json!("bogus-mode"));
        let params = GetPromptRequestParams::new("optimize").with_arguments(args);
        let result = get_prompt(&params, None).unwrap();
        let PromptMessage { content, .. } = &result.messages[0];
        let text = format!("{content:?}");
        assert!(text.contains("Unknown flare code mode"));
    }
}
