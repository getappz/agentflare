use super::*;

fn sample_task() -> SddTask {
    SddTask {
        id: 0,
        title: "Add flag".to_string(),
        body: "Add --verbose".to_string(),
        model_tier: None,
    }
}

#[test]
fn implementer_prompt_includes_task_body() {
    let prompt = build_implementer_prompt(&sample_task(), None);
    assert!(prompt.contains("Add --verbose"));
}

#[test]
fn implementer_prompt_includes_fix_context_when_present() {
    let prompt = build_implementer_prompt(&sample_task(), Some("Reviewer found: missing test"));
    assert!(prompt.contains("Reviewer found: missing test"));
}

#[test]
fn review_analyst_prompt_forbids_writing_code() {
    let prompt = build_review_analyst_prompt(&sample_task(), None);
    assert!(prompt.contains("analysis only"));
    assert!(prompt.contains("Do not write, edit, or commit any code"));
    assert!(prompt.contains("Add --verbose"));
    assert!(!prompt.contains("You are implementing"));
}

#[test]
fn review_analyst_prompt_includes_fix_context_when_present() {
    let prompt =
        build_review_analyst_prompt(&sample_task(), Some("Second reviewer found: gap in X"));
    assert!(prompt.contains("Second reviewer found: gap in X"));
}

#[test]
fn review_of_analysis_prompt_does_not_mention_code_quality() {
    let prompt = build_review_of_analysis_prompt(&sample_task(), "Found no issues");
    assert!(prompt.contains("Found no issues"));
    assert!(prompt.contains("REVIEW_APPROVED"));
    assert!(!prompt.contains("code quality"));
}

#[test]
fn judge_prompt_notes_review_only_mode_when_set() {
    let prompt = build_judge_prompt(&[sample_task()], 0, &[], "Findings: none", true);
    assert!(prompt.contains("review-only task"));
}

#[test]
fn judge_prompt_omits_review_only_note_by_default() {
    let prompt = build_judge_prompt(&[sample_task()], 0, &[], "DONE: implemented flag", false);
    assert!(!prompt.contains("review-only task"));
}

#[test]
fn judge_prompt_instructs_json_only_output() {
    let prompt = build_judge_prompt(&[sample_task()], 0, &[], "DONE: implemented flag", false);
    assert!(prompt.contains("JSON"));
    assert!(prompt.contains("DONE: implemented flag"));
}

#[test]
fn judge_prompt_includes_ledger_history() {
    let ledger = vec!["Task 0: fix round 1/5 (1 addressed)".to_string()];
    let prompt = build_judge_prompt(&[sample_task()], 0, &ledger, "REVIEW_APPROVED", false);
    assert!(prompt.contains("fix round 1/5"));
}

#[test]
fn judge_prompt_formats_multiple_tasks_on_separate_lines() {
    let tasks = vec![
        SddTask {
            id: 0,
            title: "Add flag".to_string(),
            body: "Add --verbose".to_string(),
            model_tier: None,
        },
        SddTask {
            id: 1,
            title: "Fix bug".to_string(),
            body: "Fix null pointer".to_string(),
            model_tier: None,
        },
        SddTask {
            id: 2,
            title: "Add docs".to_string(),
            body: "Document the flag".to_string(),
            model_tier: None,
        },
    ];
    let prompt = build_judge_prompt(&tasks, 1, &[], "Test role reply", false);

    // Split the prompt by newlines and verify each task appears on its own line
    let lines: Vec<&str> = prompt.lines().collect();

    // Find the "Plan:" section and verify tasks are listed with proper line breaks
    let task_lines: Vec<&str> = lines
        .iter()
        .filter(|line| {
            line.contains("Add flag") || line.contains("Fix bug") || line.contains("Add docs")
        })
        .copied()
        .collect();

    // All three task titles should appear as separate lines (not concatenated)
    assert_eq!(
        task_lines.len(),
        3,
        "Expected 3 separate lines for 3 tasks, got: {:?}",
        task_lines
    );
    assert!(task_lines[0].contains("Add flag"));
    assert!(task_lines[1].contains("Fix bug") && task_lines[1].contains("<- current"));
    assert!(task_lines[2].contains("Add docs"));
}
