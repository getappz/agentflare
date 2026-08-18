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
