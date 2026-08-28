#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JudgeAction {
    ContinueTask,
    FixRound,
    Escalate,
    ParkFinding,
    RuleAndContinue,
    InsertTask,
    SkipTask,
    AdvanceTask,
    CompletePipeline,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct JudgeDecision {
    pub action: JudgeAction,
    pub rationale: String,
    pub ledger_line: String,
    pub task_model_tier: Option<TaskModelTier>,
}

#[derive(Debug)]
pub(crate) enum JudgeParseError {
    InvalidJson(String),
}

impl std::fmt::Display for JudgeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JudgeParseError::InvalidJson(msg) => {
                write!(f, "judge reply is not valid decision JSON: {msg}")
            }
        }
    }
}

/// The first fenced code block in `reply` (` ```json ... ``` ` or a bare
/// ` ``` ... ``` `), if any -- an explicit fence is an unambiguous boundary
/// the judge only produces on purpose, so it's tried before brace-scanning.
fn extract_fenced_block(reply: &str) -> Option<&str> {
    let after_open = reply.find("```")? + 3;
    let rest = &reply[after_open..];
    // Skip an optional language tag (e.g. `json`) up to the fence's newline.
    let body_start = rest.find('\n').map(|i| i + 1).unwrap_or(0);
    let body = &rest[body_start..];
    let end = body.find("```")?;
    Some(body[..end].trim())
}

/// Scans forward from the first `{` for its own matching `}`, tracking
/// nesting depth and skipping brace-like bytes inside JSON string literals
/// (so a `{`/`}` embedded in a string value, or in unrelated commentary
/// after the object, can't extend or corrupt the span). Returns the first
/// complete top-level object instead of naively spanning from the first `{`
/// to the *last* `}` anywhere in the reply, which a second unrelated
/// brace-shaped span later in the text could throw off.
fn extract_first_balanced_object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let bytes = reply.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&reply[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The judge is prompted to reply with exactly one JSON object; this
/// tolerates a reply that wraps the object in prose, a fenced code block, or
/// trailing commentary containing its own unrelated braces, but does not
/// otherwise repair malformed JSON — a genuine parse failure (including
/// syntactically valid JSON missing a required field) is a step Failure,
/// retried by the step's own RetryPolicy.
pub(crate) fn parse_judge_decision(reply: &str) -> Result<JudgeDecision, JudgeParseError> {
    if let Some(fenced) = extract_fenced_block(reply)
        && let Ok(decision) = serde_json::from_str(fenced)
    {
        return Ok(decision);
    }
    let candidate = extract_first_balanced_object(reply).ok_or_else(|| {
        JudgeParseError::InvalidJson("no balanced '{...}' object found".to_string())
    })?;
    serde_json::from_str(candidate).map_err(|e| JudgeParseError::InvalidJson(e.to_string()))
}
