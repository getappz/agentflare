/// Builds the prompt for the implementer role: given a task, it must implement
/// it. If `fix_context` is provided (a prior reviewer's findings), the prompt
/// instructs them to address those issues. When `tdd` is set (item #179),
/// appends explicit red-green-refactor instructions.
pub(crate) fn build_implementer_prompt(
    task: &SddTask,
    fix_context: Option<&str>,
    tdd: bool,
) -> String {
    let mut prompt = format!(
        "You are implementing one task from a larger plan.\n\nTask: {}\n\n{}\n",
        task.title, task.body
    );
    if let Some(ctx) = fix_context {
        prompt.push_str(&format!(
            "\nA reviewer found issues with your prior attempt:\n{ctx}\n\nAddress them, re-run any tests you touched, and reply with your status.\n"
        ));
    }
    if tdd {
        prompt.push_str(
            "\nFollow test-driven development for this task: write a failing test first, confirm it fails, then write the minimal code to pass it, then refactor. Do not write implementation code before its test.\n"
        );
    }
    prompt.push_str("\nReply with a short status: what you did, tests run, and any concerns.\n");
    prompt
}

/// Review-only counterpart of `build_implementer_prompt` (item #507): same
/// fix-round re-dispatch shape, but the role is constrained to analysis —
/// it must never write, edit, or commit code, or open a pull request.
pub(crate) fn build_review_analyst_prompt(task: &SddTask, fix_context: Option<&str>) -> String {
    let mut prompt = format!(
        "You are reviewing one task from a larger plan — analysis only. Do not write, edit, or commit any code, and do not open a pull request.\n\nTask: {}\n\n{}\n",
        task.title, task.body
    );
    if let Some(ctx) = fix_context {
        prompt.push_str(&format!(
            "\nA second reviewer flagged gaps in your prior analysis:\n{ctx}\n\nAddress them and reply with your updated findings.\n"
        ));
    }
    prompt.push_str(
        "\nReply with your findings: what you reviewed and any issues found (or none).\n",
    );
    prompt
}

/// Builds the prompt for the task reviewer role: given a task and the
/// implementer's report, review it for spec compliance and code quality.
/// When `tdd` is set (item #179), also requires test-first evidence in the
/// implementer's report as a review criterion.
pub(crate) fn build_task_reviewer_prompt(
    task: &SddTask,
    implementer_report: &str,
    tdd: bool,
) -> String {
    let tdd_note = if tdd {
        " Also check for test-first evidence: the report must show a failing test was written and confirmed before the implementation change, not just tests added at the end. Missing that sequence is a REVIEW_ISSUES finding even if the code otherwise works."
    } else {
        ""
    };
    format!(
        "Review this task's implementation for spec compliance and code quality.{tdd_note}\n\nTask: {}\n{}\n\nImplementer's report:\n{implementer_report}\n\nReply REVIEW_APPROVED if both spec and quality pass, or REVIEW_ISSUES: followed by a bulleted list of findings.\n",
        task.title, task.body
    )
}

/// Review-only counterpart of `build_task_reviewer_prompt` (item #507):
/// checks the analyst's findings for completeness/accuracy instead of "spec
/// compliance and code quality" — there's no code for this second pass to
/// check, only the first pass's own analysis.
pub(crate) fn build_review_of_analysis_prompt(task: &SddTask, analyst_report: &str) -> String {
    format!(
        "Review this analysis for completeness and accuracy — is anything missing or wrong?\n\nTask: {}\n{}\n\nAnalyst's report:\n{analyst_report}\n\nReply REVIEW_APPROVED if the analysis is thorough and accurate, or REVIEW_ISSUES: followed by a bulleted list of gaps.\n",
        task.title, task.body
    )
}

/// Builds the prompt for the re-reviewer role: given a task, the original
/// findings, and a fix report, re-review only those specific findings.
pub(crate) fn build_re_reviewer_prompt(task: &SddTask, findings: &str, fix_report: &str) -> String {
    format!(
        "Re-review a fix for this task's findings only — do not look for new issues.\n\nTask: {}\n\nOriginal findings:\n{findings}\n\nFix report:\n{fix_report}\n\nReply REVIEW_APPROVED if every finding is addressed, or REVIEW_ISSUES: followed by what remains.\n",
        task.title
    )
}

/// Builds the prompt for the judge: given the task list, current task index,
/// ledger history, and the latest role reply, the judge decides what happens next.
pub(crate) fn build_judge_prompt(
    tasks: &[SddTask],
    current_task_index: usize,
    ledger: &[String],
    role_reply: &str,
    review_only: bool,
) -> String {
    let task_list: String = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            format!(
                "{}. {}{}\n",
                i,
                t.title,
                if i == current_task_index {
                    " <- current"
                } else {
                    ""
                }
            )
        })
        .collect();
    let ledger_text: String = ledger.join("\n");
    let mode_note = if review_only {
        "This is a review-only task: no code should be written; the role's job is to analyze and report findings, not implement fixes.\n\n"
    } else {
        ""
    };
    format!(
        "You are the judge for an autonomous multi-task execution pipeline.\n\n{mode_note}Plan:\n{task_list}\n\nLedger so far:\n{ledger_text}\n\nLatest role reply:\n{role_reply}\n\nDecide what happens next. Reply with ONE JSON object and nothing else, matching exactly:\n{{\"action\": \"continue_task|fix_round|escalate|park_finding|rule_and_continue|insert_task|skip_task|advance_task|complete_pipeline\", \"rationale\": \"...\", \"ledger_line\": \"...\", \"task_model_tier\": \"mechanical|integration|architecture|null\"}}\n"
    )
}
