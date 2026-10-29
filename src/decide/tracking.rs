//! Shared decision envelope. Consumers supply their actual baseline and whether
//! the answer was applied; raw questions/state are captured only with opt-in.
use super::{Answer, DecideError, Outcome, Question, Usage, capture, shadow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Details {
    #[serde(default = "schema")]
    pub schema: u32,
    #[serde(default)]
    pub source_model: Option<String>,
    #[serde(default)]
    pub requested_model: Option<String>,
    #[serde(default)]
    pub reported_model: Option<String>,
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Types only: instructions/criteria may contain private task text.
    #[serde(default)]
    pub question_types: BTreeMap<String, String>,
    #[serde(default)]
    pub answer: Value,
}

fn schema() -> u32 {
    1
}

/// Comparable decisions, without confidence or probability metadata.
pub fn summary(answers: &BTreeMap<String, Answer>) -> String {
    let values: BTreeMap<_, _> = answers
        .iter()
        .map(|(id, answer)| {
            let value = match answer {
                Answer::Choice { choice, .. } => json!(choice),
                Answer::Score { score, .. } => json!(score),
                Answer::Noul { noul } => json!(*noul >= 0.5),
            };
            (id, value)
        })
        .collect();
    json!(values).to_string()
}

pub(crate) fn equivalent(a: &str, b: &str) -> bool {
    a == b
        || match (
            serde_json::from_str::<Value>(a),
            serde_json::from_str::<Value>(b),
        ) {
            (Ok(a), Ok(b)) => same_value(&a, &b),
            _ => false,
        }
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            a == b || ((a.is_f64() || b.is_f64()) && a.as_f64() == b.as_f64())
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_value(a, b)))
        }
        _ => a == b,
    }
}

pub fn decision_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:032x}-{:x}-{:x}",
        rand::random::<u128>(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// One semantic decision, including multi-question ranking. `record` is injected
/// by existing consumers/tests; production uses `shadow::record`.
pub struct Context<'a> {
    pub questions: &'a BTreeMap<String, Question>,
    pub answers: &'a BTreeMap<String, Answer>,
    pub source_model: Option<&'a str>,
    pub usage: &'a Usage,
    pub applied: bool,
}

pub fn finish(
    mut row: shadow::Row,
    context: Context<'_>,
    capture_input: Option<capture::Input<'_>>,
    record: &dyn Fn(&shadow::Row),
) -> String {
    row.details.schema = schema();
    row.details.requested_model = super::model_identity::requested();
    row.details.reported_model = context.source_model.map(str::to_string);
    row.details.source_model = row
        .details
        .reported_model
        .clone()
        .or_else(|| row.details.requested_model.clone());
    row.details.applied = context.applied;
    if row.details.status.is_none() {
        row.details.status = Some(
            if row.error.is_none() {
                "ok"
            } else {
                "fail-open"
            }
            .into(),
        );
    }
    row.details.input_tokens = context.usage.input_tokens;
    row.details.output_tokens = context.usage.output_tokens;
    row.details.answer = capture::norm::sanitize_features(json!(context.answers));
    row.details.question_types = context
        .questions
        .iter()
        .map(|(id, q)| {
            let kind = match q {
                Question::Choice { .. } => "choice",
                Question::Score { .. } => "score",
                Question::Noul { .. } => "noul",
            };
            (id.clone(), kind.into())
        })
        .collect();
    let id = row.decision_id.clone().unwrap_or_else(decision_id);
    row.decision_id = Some(id.clone());
    record(&row);
    if capture::enabled()
        && let Some(input) = capture_input
    {
        super::storage::capture(captured(&id, input, context.questions, context.answers));
    }
    id
}

pub(crate) fn captured(
    id: &str,
    input: capture::Input<'_>,
    questions: &BTreeMap<String, Question>,
    answers: &BTreeMap<String, Answer>,
) -> capture::Row {
    let mut row = capture::build_row(input);
    row.source_model = row.source_model.or_else(super::model_identity::requested);
    row.decision_id = Some(id.into());
    row.question = Some(capture::norm::sanitize_features(json!(questions)));
    row.answer = Some(capture::norm::sanitize_features(json!(answers)));
    row
}

pub fn status(error: &DecideError) -> &'static str {
    match error {
        DecideError::Unavailable => "breaker-open",
        DecideError::Status(402, _) => "402",
        DecideError::Transport(s) if is_timeout(s) => "timeout",
        _ => "fail-open",
    }
}

pub(super) fn is_timeout(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("timed out") || message.contains("timeout")
}

pub fn failure(
    mut row: shadow::Row,
    questions: &BTreeMap<String, Question>,
    elapsed_ms: u64,
    record: &dyn Fn(&shadow::Row),
) -> String {
    row.latency_ms = Some(elapsed_ms);
    // A failure has no answer to train on, but retains its typed question.
    finish(
        row,
        Context {
            questions,
            answers: &BTreeMap::new(),
            source_model: None,
            usage: &Usage::default(),
            applied: false,
        },
        None,
        record,
    )
}

/// Keep the key even when Jev failed so consumers can append a later outcome.
pub struct Tracked {
    pub decision_id: Option<String>,
    pub result: Result<Outcome, DecideError>,
}

/// Required entrypoint for new sites: records success and every fail-open path.
/// `baseline` and the answer use the same typed JSON representation. A consumer
/// which applies only confident answers can use `ask_with` and its own predicate.
pub fn ask_with(
    site: &str,
    baseline: &str,
    applied: impl FnOnce(&Outcome) -> bool,
    state: &Value,
    questions: &BTreeMap<String, Question>,
    ask: impl FnOnce() -> Result<Outcome, DecideError>,
) -> Tracked {
    super::model_identity::begin(None);
    let start = Instant::now();
    let result = ask();
    if matches!(result, Err(DecideError::Disabled)) {
        return Tracked {
            decision_id: None,
            result,
        };
    }
    let input = state.to_string();
    let id = match &result {
        Ok(out) => {
            let answer = summary(&out.response.answers);
            let confidence = out
                .response
                .answers
                .values()
                .filter_map(Answer::confidence)
                .reduce(f64::min);
            finish(
                shadow::Row::answered(
                    site,
                    &input,
                    baseline,
                    &answer,
                    confidence,
                    out.elapsed_ms,
                    out.response.usage.cost,
                ),
                Context {
                    questions,
                    answers: &out.response.answers,
                    source_model: out.response.model.as_deref(),
                    usage: &out.response.usage,
                    applied: applied(out),
                },
                Some(capture::Input {
                    site,
                    features: if capture::enabled() {
                        state.clone()
                    } else {
                        Value::Null
                    },
                    norm_input: &input,
                    label: json!({"summary":answer}),
                    confidence,
                    baseline,
                    source_model: out.response.model.as_deref(),
                }),
                &shadow::record,
            )
        }
        Err(error) => {
            let mut row = shadow::Row::failed(site, &input, baseline, status(error));
            row.details.status = Some(status(error).into());
            row.latency_ms = Some(start.elapsed().as_millis() as u64);
            failure(
                row,
                questions,
                start.elapsed().as_millis() as u64,
                &shadow::record,
            )
        }
    };
    Tracked {
        decision_id: Some(id),
        result,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    HumanOverride,
    PrReviewResult,
    ItemFinalState,
    IncidentConfirmed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundTruth {
    pub record_type: String,
    pub decision_id: String,
    pub ts: i64,
    pub kind: OutcomeKind,
    /// Answer in the same domain as `baseline`/`jev`. No free-form notes.
    pub answer: String,
}

pub fn outcome_add(
    path: &std::path::Path,
    id: &str,
    kind: OutcomeKind,
    answer: &str,
) -> Result<(), String> {
    if id.len() > 128 || id.is_empty() || answer.len() > 4096 {
        return Err("invalid decision id or answer size".into());
    }
    if !super::storage::flush(std::time::Duration::from_secs(1)) {
        return Err("decision writes are pending; retry labeling".into());
    }
    // An earlier hook's writer may still be appending the row we label.
    let deadline = Instant::now() + std::time::Duration::from_secs(1);
    while !shadow::load(path)
        .iter()
        .any(|r| r.decision_id.as_deref() == Some(id))
    {
        if Instant::now() >= deadline {
            return Err("decision id not found in retained ledger".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let outcome = GroundTruth {
        record_type: "outcome".into(),
        decision_id: id.into(),
        ts: chrono::Utc::now().timestamp(),
        kind,
        answer: crate::mcp_server::secret_scan::redact(answer),
    };
    super::storage::append(path, &outcome, super::storage::LEDGER_CAP).map_err(|e| e.to_string())
}
