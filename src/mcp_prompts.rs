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
    let mut prompts = vec![Prompt::new(
        "ponytail",
        Some("Switch or report Ponytail lazy-dev mode"),
        Some(vec![PromptArgument::new("mode")
            .with_description("lite|full|ultra|off|status (omit to report current mode)")]),
    )];
    prompts.extend(
        SUB_SKILLS
            .iter()
            .map(|(name, desc)| Prompt::new(format!("ponytail-{name}"), Some(*desc), None)),
    );
    prompts
}

pub fn get_prompt(request: &GetPromptRequestParams) -> Option<GetPromptResult> {
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
        assert_eq!(names.len(), 1 + SUB_SKILLS.len());
    }

    #[test]
    fn unknown_prompt_name_returns_none() {
        assert!(get_prompt(&GetPromptRequestParams::new("not-a-real-prompt")).is_none());
    }

    #[test]
    fn ponytail_review_returns_full_skill_body() {
        let result = get_prompt(&GetPromptRequestParams::new("ponytail-review")).unwrap();
        let PromptMessage { content, .. } = &result.messages[0];
        let text = format!("{content:?}");
        assert!(text.contains("ponytail-review"));
    }

    #[test]
    fn bare_ponytail_without_mode_reports_without_crashing() {
        let result = get_prompt(&GetPromptRequestParams::new("ponytail")).unwrap();
        assert_eq!(result.messages.len(), 1);
    }

    #[test]
    fn ponytail_with_unknown_mode_reports_error_text() {
        use rmcp::model::JsonObject;
        let mut args = JsonObject::new();
        args.insert("mode".to_string(), serde_json::json!("bogus-mode"));
        let params = GetPromptRequestParams::new("ponytail").with_arguments(args);
        let result = get_prompt(&params).unwrap();
        let PromptMessage { content, .. } = &result.messages[0];
        let text = format!("{content:?}");
        assert!(text.contains("Unknown ponytail mode"));
    }
}
