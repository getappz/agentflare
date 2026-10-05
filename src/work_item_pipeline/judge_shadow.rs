// Shadow comparison of the SDD judge against Jev (item #705). Included into
// `work_item_pipeline` like its siblings, so it shares that module's types.
//
// The Claude judge's decision is ALWAYS the one applied. After it parses, this
// asks Jev the same question (one Choice over the nine actions) with a trimmed
// state and appends both answers to the shadow log (site `sdd_judge`), so
// `agentflare decide report --site sdd_judge` can show the agreement rate
// before anyone considers skipping the Claude judge turn.
//
// Opt-in with `AGENTFLARE_JEV_JUDGE=shadow` (plus `AGENTFLARE_JEV=1`): the
// trimmed state, which can include parts of a role reply, leaves the machine.
// The call is detached and best-effort: it can never delay or alter the loop.

const JUDGE_SHADOW_SITE: &str = "sdd_judge";
const JUDGE_SHADOW_REPLY_CHARS: usize = 3000;
const JUDGE_SHADOW_TASK_BODY_CHARS: usize = 600;
const JUDGE_SHADOW_TITLE_CHARS: usize = 120;
const JUDGE_SHADOW_LEDGER_LINES: usize = 5;
const JUDGE_SHADOW_LEDGER_LINE_CHARS: usize = 300;
const JUDGE_SHADOW_PLAN_ENTRIES: usize = 20;
const JUDGE_SHADOW_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

fn judge_shadow_clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A small, bounded view of the loop state: titles instead of bodies, the last
/// few ledger lines, and a clipped reply. Jev has a 32K window and gets worse
/// with irrelevant detail, so this deliberately is not the judge's full prompt.
pub(crate) fn judge_shadow_state(
    tasks: &[SddTask],
    current_task_index: usize,
    ledger: &[String],
    role_reply: &str,
    review_only: bool,
    design_spec: bool,
) -> serde_json::Value {
    let plan: Vec<String> = tasks
        .iter()
        .enumerate()
        .take(JUDGE_SHADOW_PLAN_ENTRIES)
        .map(|(i, t)| {
            format!(
                "{i}. {}{}",
                judge_shadow_clip(&t.title, JUDGE_SHADOW_TITLE_CHARS),
                if i == current_task_index { " <- current" } else { "" }
            )
        })
        .collect();
    let ledger_tail: Vec<String> = ledger
        .iter()
        .skip(ledger.len().saturating_sub(JUDGE_SHADOW_LEDGER_LINES))
        .map(|l| judge_shadow_clip(l, JUDGE_SHADOW_LEDGER_LINE_CHARS))
        .collect();
    serde_json::json!({
        "mode": if design_spec { "design_spec" } else if review_only { "review_only" } else { "implement" },
        "plan": plan,
        "current_task": tasks.get(current_task_index).map(|t| serde_json::json!({
            "title": judge_shadow_clip(&t.title, JUDGE_SHADOW_TITLE_CHARS),
            "body": judge_shadow_clip(&t.body, JUDGE_SHADOW_TASK_BODY_CHARS),
        })),
        "ledger_tail": ledger_tail,
        "latest_role_reply": judge_shadow_clip(role_reply, JUDGE_SHADOW_REPLY_CHARS),
    })
}

/// The nine `JudgeAction`s. The descriptions are inferred from the judge prompt
/// and from how the loop applies each action; if Jev disagrees systematically,
/// the shadow report is where a wrong description shows up. Options are sent in
/// alphabetical order and Jev leans toward the first one, hence shadow-only.
pub(crate) fn judge_shadow_questions()
-> std::collections::BTreeMap<String, crate::decide::Question> {
    std::collections::BTreeMap::from([(
        "action".to_string(),
        crate::decide::Question::choice(
            "Given the plan, ledger and the role's latest reply in the state, what should the multi-task pipeline do next?",
            [
                ("advance_task", "The current task is complete and verified; move on to the next task"),
                ("complete_pipeline", "Every planned task is done; finish the whole pipeline"),
                ("continue_task", "The current task is not finished yet; let the role keep working on it unchanged"),
                ("escalate", "Repeated attempts are not converging or the role is stuck; escalate to a stronger approach"),
                ("fix_round", "The work has concrete problems or review issues to fix; run another fix pass on this task"),
                ("insert_task", "Extra work is needed that deserves its own new task added to the plan"),
                ("park_finding", "A non-blocking finding was reported; record it for later and do not act on it now"),
                ("rule_and_continue", "A decision or rule was settled that should be noted in the ledger; then continue the same task"),
                ("skip_task", "The current task is unnecessary or already satisfied; skip it without more work"),
            ],
        ),
    )])
}

fn judge_shadow_label(action: JudgeAction) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The backend call, injectable for tests.
type JudgeShadowAsk<'a> = dyn Fn(
        &serde_json::Value,
        &std::collections::BTreeMap<String, crate::decide::Question>,
    ) -> Result<crate::decide::Outcome, crate::decide::DecideError>
    + 'a;

/// Ask Jev and record the comparison. `ask`/`record` are injectable for tests.
pub(crate) fn judge_shadow_compare(
    state: &serde_json::Value,
    claude_action: &str,
    ask: &JudgeShadowAsk<'_>,
    record: &dyn Fn(&crate::decide::shadow::Row),
) {
    use crate::decide::{Answer, DecideError, shadow};
    let input = state.to_string();
    let outcome = match ask(state, &judge_shadow_questions()) {
        Ok(outcome) => outcome,
        // Not configured: nothing to compare, nothing to log.
        Err(DecideError::Disabled | DecideError::NoCredentials) => return,
        Err(e) => {
            record(&shadow::Row::failed(
                JUDGE_SHADOW_SITE,
                &input,
                claude_action,
                &e.to_string(),
            ));
            return;
        }
    };
    match outcome.response.answers.get("action") {
        Some(answer @ Answer::Choice {
            choice, confidence, ..
        }) => {
            record(&shadow::Row::answered(
                JUDGE_SHADOW_SITE,
                &input,
                claude_action,
                choice,
                *confidence,
                outcome.elapsed_ms,
                outcome.response.usage.cost,
            ));
            let (features, norm_input) = crate::decide::capture::judge_features(state);
            crate::decide::capture::record(crate::decide::capture::Input {
                site: JUDGE_SHADOW_SITE,
                features,
                norm_input: &norm_input,
                label: crate::decide::capture::label_of(answer),
                confidence: *confidence,
                baseline: claude_action,
                source_model: outcome.response.model.as_deref(),
            });
        }
        _ => record(&shadow::Row::failed(
            JUDGE_SHADOW_SITE,
            &input,
            claude_action,
            "unexpected answer shape",
        )),
    }
}

/// Fire-and-forget shadow comparison for one judged step. No-op unless opted
/// in; never blocks the loop and never touches its state.
pub(crate) fn spawn_judge_shadow(
    tasks: &[SddTask],
    current_task_index: usize,
    ledger: &[String],
    role_reply: &str,
    review_only: bool,
    design_spec: bool,
    action: JudgeAction,
) {
    if std::env::var("AGENTFLARE_JEV_JUDGE").as_deref() != Ok("shadow") {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let state = judge_shadow_state(
        tasks,
        current_task_index,
        ledger,
        role_reply,
        review_only,
        design_spec,
    );
    let claude_action = judge_shadow_label(action);
    // fire-and-forget: a best-effort log row; losing it on shutdown is fine and
    // it must never delay or alter the judge loop.
    drop(runtime.spawn_blocking(move || {
        judge_shadow_compare(
            &state,
            &claude_action,
            &|state, questions| {
                crate::decide::ask_within(state, questions, JUDGE_SHADOW_BUDGET)
            },
            &crate::decide::shadow::record,
        );
    }));
}
