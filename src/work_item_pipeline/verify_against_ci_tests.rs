use super::detect_verify_against_ci;
use serde_json::json;

#[test]
fn bugfix_task_type_enables_verify_against_ci() {
    assert!(detect_verify_against_ci(
        "fix the thing",
        &json!({"task_type": "bugfix"})
    ));
}

#[test]
fn metadata_flag_enables_verify_against_ci() {
    assert!(detect_verify_against_ci(
        "plain task",
        &json!({"verify_against_ci": true})
    ));
}

#[test]
fn description_mentioning_cargo_fmt_enables() {
    assert!(detect_verify_against_ci(
        "The PR fmt CI check keeps failing; run cargo fmt --check",
        &json!({})
    ));
}

#[test]
fn description_mentioning_gh_pr_checks_enables() {
    assert!(detect_verify_against_ci(
        "Check gh pr checks against PR #800 before claiming done",
        &json!({})
    ));
}

#[test]
fn unrelated_description_defaults_to_false() {
    assert!(!detect_verify_against_ci(
        "Add a new CLI flag for verbose output",
        &json!({})
    ));
}
