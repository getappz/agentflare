//! MCP "Prompts" for ponytail — surfaces `/ponytail*` as native Claude Code
//! slash commands via the MCP protocol (same mechanism lean-ctx uses for its
//! own `/lean-ctx*` commands), routed entirely through agentflare's own
//! ponytail port. No dependency on the DietrichGebert/ponytail marketplace
//! plugin.

use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, Prompt, PromptArgument, PromptMessage,
    PromptMessageRole,
};

const SUB_SKILLS: &[(&str, &str)] = &[
    ("review", "Over-engineering review of the current diff/branch/repo"),
    ("audit", "Whole-repo over-engineering audit: ranked list of what to delete"),
    ("debt", "Harvest `ponytail:` shortcut comments into a tracked ledger"),
    ("gain", "Measured-impact scoreboard: less code, less cost, more speed"),
    ("help", "Quick-reference card for all ponytail modes, skills, and commands"),
    ("playbook", "TDD-aware project companion — red-green-refactor enforced"),
    ("no-hallucination", "Reality-check layer: blocks invented APIs, deprecated methods, undeclared variables"),
];

pub fn list_prompts() -> Vec<Prompt> {
    let mut prompts = vec![
        Prompt::new(
            "ponytail",
            Some("Switch or report Ponytail lazy-dev mode"),
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
            .map(|(name, desc)| Prompt::new(format!("ponytail-{name}"), Some(*desc), None)),
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
    if request.name == "ponytail" {
        return Some(get_ponytail_mode(request));
    }
    let skill = request.name.strip_prefix("ponytail-")?;
    SUB_SKILLS
        .iter()
        .any(|(name, _)| *name == skill)
        .then(|| get_ponytail_skill(skill))
}

fn assistant_text(msg: impl Into<String>) -> GetPromptResult {
    GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::Assistant, msg)])
}

fn get_ponytail_mode(request: &GetPromptRequestParams) -> GetPromptResult {
    let mode_arg = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();

    if mode_arg.is_empty() || mode_arg == "status" {
        let mode = ponytail::active_mode().unwrap_or_else(ponytail::default_mode);
        return assistant_text(if mode == "off" {
            "ponytail is off. Use /ponytail mode=lite|full|ultra to activate.".to_string()
        } else {
            format!("PONYTAIL MODE ACTIVE — level: {mode}")
        });
    }
    if mode_arg == "off" {
        ponytail::clear_active();
        return assistant_text("ponytail is now off.");
    }
    match ponytail::normalize_config_mode(&mode_arg) {
        Some(normalized) => match ponytail::set_active(normalized) {
            Ok(()) => assistant_text(ponytail::build_instructions(normalized, None).body),
            Err(e) => assistant_text(format!("Failed to persist ponytail mode: {e}")),
        },
        None => assistant_text(format!(
            "Unknown ponytail mode '{mode_arg}'. Use lite|full|ultra|off|status."
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
            "Artifact commands (live-shareable local pages) — pass as this command's argument:\n\
             publish [--name N] [--type html|markdown|mermaid|diagram|text] [--session S] [--label L] [--description D] [--favicon 🚀] — publish preceding/attached content\n\
             update <id> [--base-version N] [options] — update in place (open tabs live-reload)\n\
             list [--session S]\n\
             get <id> [--version N]\n\
             delete <id>",
        );
    }

    assistant_text(format!(
        "Artifact command requested: `{command}`\n\n\
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

fn get_handoff_command(
    request: &GetPromptRequestParams,
    agent: Option<&str>,
) -> GetPromptResult {
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

    if command.is_empty() {
        return assistant_text(format!(
            "Handoff — agent-to-agent work exchange via artifacts. Pass as this command's argument:\n\
             <recipient> <brief> — hand the relevant work product to that agent (e.g. `codex review the API design above`)\n\
             inbox [me] — list artifacts addressed to an agent (default: {me})\n\
             thread <id> — show a handoff thread's artifacts in order\n\
             Work products only — facts and decisions belong in engram memory.",
        ));
    }

    assistant_text(format!(
        "Handoff command: `{command}`\n\n\
         Grammar: first word is a subcommand (`inbox`, `thread`) or a recipient; the rest is the brief.\n\
         - `<recipient> <brief>` → artifact_publish with recipient=<recipient>, sender={me}, \
         a fresh thread_id (or the current one when continuing an exchange), name from the brief, \
         and content = the work product the brief points at (the preceding conversation content, \
         diff, review, or document — ask only if genuinely ambiguous). Prepend the brief to the \
         content so the recipient knows what is being asked. When answering an item from your \
         inbox, set reply_to=<that artifact id> and reuse its thread_id.\n\
         - `inbox [me]` → artifact_list with recipient=<me or {me}>; summarize sender, \
         name, and brief for each.\n\
         - `thread <id>` → artifact_list with thread_id=<id>; present in chronological order with \
         reply lineage.\n\
         Report the resulting URL (or listing) afterwards. Work products only — facts/decisions \
         go to engram, not artifacts."
    ))
}

fn get_ponytail_skill(skill: &str) -> GetPromptResult {
    if let Err(e) = ponytail::set_active(skill) {
        return assistant_text(format!("Failed to persist ponytail mode: {e}"));
    }
    let body = ponytail::sub_skills::get(skill).unwrap_or_default();
    assistant_text(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_ponytail_and_all_sub_skills() {
        let prompts = list_prompts();
        let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"ponytail"));
        assert!(names.contains(&"ponytail-review"));
        assert!(names.contains(&"ponytail-no-hallucination"));
        // ponytail + artifact + handoff + one per sub-skill
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
        assert!(text.contains("artifact_publish"), "{text}");
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
        assert!(text.contains("publish --type markdown --favicon 🚀"), "{text}");
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
        assert!(text.contains("sender=opencode"), "{text}");
        assert!(text.contains("recipient=<me or opencode>"), "{text}");
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
        assert!(text.contains("sender=claude-code"), "{text}");
    }

    #[test]
    fn bare_handoff_usage_names_agent_identity() {
        let result = get_prompt(&GetPromptRequestParams::new("handoff"), Some("opencode")).unwrap();
        let text = format!("{:?}", result.messages[0].content);
        assert!(text.contains("default: opencode"), "{text}");
    }

    #[test]
    fn ponytail_review_returns_full_skill_body() {
        let result = get_prompt(&GetPromptRequestParams::new("ponytail-review"), None).unwrap();
        let PromptMessage { content, .. } = &result.messages[0];
        let text = format!("{content:?}");
        assert!(text.contains("ponytail-review"));
    }

    #[test]
    fn bare_ponytail_without_mode_reports_without_crashing() {
        let result = get_prompt(&GetPromptRequestParams::new("ponytail"), None).unwrap();
        assert_eq!(result.messages.len(), 1);
    }

    #[test]
    fn ponytail_with_unknown_mode_reports_error_text() {
        use rmcp::model::JsonObject;
        let mut args = JsonObject::new();
        args.insert("mode".to_string(), serde_json::json!("bogus-mode"));
        let params = GetPromptRequestParams::new("ponytail").with_arguments(args);
        let result = get_prompt(&params, None).unwrap();
        let PromptMessage { content, .. } = &result.messages[0];
        let text = format!("{content:?}");
        assert!(text.contains("Unknown ponytail mode"));
    }
}
