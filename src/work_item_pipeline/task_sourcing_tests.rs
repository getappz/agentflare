use super::load_or_synthesize_tasks;

#[test]
fn synthesizes_single_task_when_no_plan_doc() {
    let tasks = load_or_synthesize_tasks("Fix the null pointer in parser.rs", None);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, 0);
    assert_eq!(tasks[0].body, "Fix the null pointer in parser.rs");
}

#[test]
fn parses_task_list_from_plan_doc_headings() {
    let plan_doc = "\
# Some Plan

### Task 1: Add validation

Add input validation to the handler.

### Task 2: Add tests

Add unit tests for the validation.
";
    let tasks = load_or_synthesize_tasks("ignored when plan_doc present", Some(plan_doc));
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].title, "Add validation");
    assert_eq!(tasks[1].title, "Add tests");
    assert!(tasks[1].body.contains("unit tests"));
}

#[test]
fn empty_plan_doc_falls_back_to_synthesized_task() {
    let tasks = load_or_synthesize_tasks("Bump dependency version", Some(""));
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].body, "Bump dependency version");
}
