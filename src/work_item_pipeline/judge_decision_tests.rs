use super::*;

#[test]
fn parses_valid_decision() {
    let reply = r#"{"action":"advance_task","rationale":"spec met","ledger_line":"Task 0: complete","task_model_tier":null}"#;
    let decision = parse_judge_decision(reply).expect("valid JSON parses");
    assert_eq!(decision.action, JudgeAction::AdvanceTask);
    assert_eq!(decision.ledger_line, "Task 0: complete");
}

#[test]
fn parses_decision_wrapped_in_prose_by_stripping_to_the_json_object() {
    // Agents sometimes wrap JSON in a sentence despite instructions;
    // strip to the first {...} span before parsing.
    let reply = "Here is my decision:\n{\"action\":\"complete_pipeline\",\"rationale\":\"all tasks done\",\"ledger_line\":\"Pipeline: complete\",\"task_model_tier\":null}\nDone.";
    let decision = parse_judge_decision(reply).expect("parses after stripping");
    assert_eq!(decision.action, JudgeAction::CompletePipeline);
}

#[test]
fn rejects_malformed_json() {
    let err = parse_judge_decision("not json at all").unwrap_err();
    assert!(matches!(err, JudgeParseError::InvalidJson(_)));
}

#[test]
fn rejects_unknown_action_value() {
    let reply =
        r#"{"action":"do_a_barrel_roll","rationale":"x","ledger_line":"x","task_model_tier":null}"#;
    let err = parse_judge_decision(reply).unwrap_err();
    assert!(matches!(err, JudgeParseError::InvalidJson(_)));
}

#[test]
fn parses_decision_from_a_json_fenced_code_block() {
    let reply = "Here's my decision:\n```json\n{\"action\":\"advance_task\",\"rationale\":\"spec met\",\"ledger_line\":\"Task 0: complete\",\"task_model_tier\":null}\n```\nThanks.";
    let decision = parse_judge_decision(reply).expect("parses fenced block");
    assert_eq!(decision.action, JudgeAction::AdvanceTask);
}

#[test]
fn parses_decision_from_a_bare_fenced_code_block_with_no_language_tag() {
    let reply = "```\n{\"action\":\"skip_task\",\"rationale\":\"x\",\"ledger_line\":\"x\",\"task_model_tier\":null}\n```";
    let decision = parse_judge_decision(reply).expect("parses bare fenced block");
    assert_eq!(decision.action, JudgeAction::SkipTask);
}

#[test]
fn extracts_only_the_first_object_when_trailing_prose_has_its_own_unrelated_braces() {
    // A naive first-`{`-to-last-`}` span would run from the real
    // object's opening brace all the way through the unrelated `{cfg}`
    // in the trailing sentence, producing invalid combined JSON.
    let reply = "{\"action\":\"advance_task\",\"rationale\":\"spec met\",\"ledger_line\":\"Task 0: complete\",\"task_model_tier\":null}\nNote: this respects the {cfg} override.";
    let decision = parse_judge_decision(reply).expect("parses despite trailing unrelated braces");
    assert_eq!(decision.action, JudgeAction::AdvanceTask);
}

#[test]
fn a_brace_inside_a_json_string_value_does_not_confuse_balance_tracking() {
    let reply = r#"{"action":"advance_task","rationale":"uses a {placeholder} pattern","ledger_line":"x","task_model_tier":null}"#;
    let decision = parse_judge_decision(reply).expect("brace inside string value is inert");
    assert_eq!(decision.action, JudgeAction::AdvanceTask);
}

#[test]
fn rejects_syntactically_valid_json_missing_a_required_field() {
    // Reproduction of the live 2026-08-15 production failure (item
    // #478): a well-formed JSON object that's simply missing `action`
    // must still be a genuine, retried parse failure -- not silently
    // defaulted.
    let reply = r#"{"rationale":"x","ledger_line":"x","task_model_tier":null}"#;
    let err = parse_judge_decision(reply).unwrap_err();
    assert!(matches!(err, JudgeParseError::InvalidJson(msg) if msg.contains("action")));
}

#[test]
fn rejects_an_empty_reply() {
    let err = parse_judge_decision("").unwrap_err();
    assert!(matches!(err, JudgeParseError::InvalidJson(_)));
}
