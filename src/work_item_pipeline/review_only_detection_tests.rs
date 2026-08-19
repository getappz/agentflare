use super::detect_review_only;
use serde_json::json;

#[test]
fn explicit_task_type_metadata_wins() {
    assert!(detect_review_only(
        "implement the fix",
        &json!({"task_type": "review"})
    ));
}

#[test]
fn review_only_phrase_in_description_is_detected() {
    assert!(detect_review_only(
        "REVIEW ONLY — do not fix, do not push, do not open a PR.",
        &json!({}),
    ));
}

#[test]
fn plain_implementation_request_is_not_review_only() {
    assert!(!detect_review_only(
        "Fix the null pointer in parser.rs",
        &json!({}),
    ));
}

#[test]
fn missing_metadata_field_does_not_panic() {
    assert!(!detect_review_only("implement it", &json!({"size": "M"})));
}

#[test]
fn hyphenated_review_only_phrase_is_detected() {
    assert!(detect_review_only(
        "State it's review-only up front. Report findings, don't fix.",
        &json!({}),
    ));
}

#[test]
fn design_spec_task_type_metadata_is_detected() {
    assert!(detect_review_only(
        "propose an approach",
        &json!({"task_type": "design-spec"})
    ));
    assert!(detect_review_only(
        "propose an approach",
        &json!({"task_type": "design_spec"})
    ));
}

#[test]
fn design_spec_phrase_in_description_is_detected() {
    assert!(detect_review_only(
        "Task type: design-spec (no code). Do not propose implementation code — findings/proposal only.",
        &json!({}),
    ));
}

#[test]
fn plain_design_mention_without_spec_framing_is_not_review_only() {
    assert!(!detect_review_only(
        "Redesign the login button to be blue",
        &json!({}),
    ));
}

#[test]
fn design_specification_phrase_is_not_review_only() {
    assert!(!detect_review_only(
        "Update the API design specification to add pagination",
        &json!({}),
    ));
    assert!(!detect_review_only(
        "Write design specs for the new caching layer, then implement it",
        &json!({}),
    ));
}
