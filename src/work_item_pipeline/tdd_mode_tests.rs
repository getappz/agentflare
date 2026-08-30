use super::detect_tdd_mode;
use serde_json::json;

#[test]
fn missing_tdd_field_defaults_to_false() {
    assert!(!detect_tdd_mode(&json!({})));
}

#[test]
fn tdd_true_is_detected() {
    assert!(detect_tdd_mode(&json!({"tdd": true})));
}

#[test]
fn tdd_false_is_detected() {
    assert!(!detect_tdd_mode(&json!({"tdd": false})));
}

#[test]
fn non_bool_tdd_value_does_not_panic_and_defaults_to_false() {
    assert!(!detect_tdd_mode(&json!({"tdd": "yes"})));
}
