//! Wire and domain types for typed structured decisions (`Choice`, `Score`, `Noul`).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Allowed slack when checking that a probability distribution sums to 1.0.
const PROBABILITY_SUM_EPSILON: f64 = 0.02;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Request<'a> {
    pub state: &'a Value,
    pub questions: &'a BTreeMap<String, Question>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
}

/// One typed question. Serializes with `"type": "choice" | "score" | "noul"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Pick one option from `criteria` (`option_name -> description`).
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    /// Rate on a 1..=N scale defined by `criteria` (ordered level descriptions).
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// Binary probability in `[0.0, 1.0]`.
    Noul {
        instructions: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<BTreeMap<String, String>>,
    },
}

impl Question {
    pub fn choice<'a>(
        instructions: &str,
        options: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        Self::Choice {
            instructions: instructions.to_string(),
            criteria: options
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    pub fn score<'a>(instructions: &str, levels: impl IntoIterator<Item = &'a str>) -> Self {
        Self::Score {
            instructions: instructions.to_string(),
            criteria: levels.into_iter().map(str::to_string).collect(),
        }
    }

    pub fn noul(instructions: &str) -> Self {
        Self::Noul {
            instructions: instructions.to_string(),
            criteria: None,
        }
    }
}

/// One answer entry, untagged by shape (`choice`, `score`, or `noul`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Answer {
    Choice {
        choice: String,
        #[serde(default)]
        confidence: Option<f64>,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        #[serde(default)]
        confidence: Option<f64>,
        #[serde(default)]
        probabilities: Vec<f64>,
    },
    Noul {
        noul: f64,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub response: Response,
    pub elapsed_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    #[error("decide backend is disabled")]
    Disabled,
    #[error("no decide credentials configured")]
    NoCredentials,
    #[error("decide backend unavailable or timed out")]
    Unavailable,
    #[error("invalid decide request: {0}")]
    InvalidRequest(String),
    #[error("decide HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("invalid decide response: {0}")]
    InvalidResponse(String),
}

fn valid_prob(p: f64) -> bool {
    p.is_finite() && (0.0..=1.0).contains(&p)
}

fn valid_distribution<'a>(
    probs: impl IntoIterator<Item = &'a f64>,
    expected_len: usize,
    id: &str,
) -> Result<(), DecideError> {
    let mut count = 0usize;
    let mut sum = 0.0_f64;
    for &p in probs {
        if !valid_prob(p) {
            return Err(DecideError::InvalidResponse(format!(
                "question '{id}': probability {p} out of [0,1]"
            )));
        }
        sum += p;
        count += 1;
    }
    if count != expected_len {
        return Err(DecideError::InvalidResponse(format!(
            "question '{id}': expected {expected_len} probabilities, got {count}"
        )));
    }
    if (sum - 1.0).abs() > PROBABILITY_SUM_EPSILON {
        return Err(DecideError::InvalidResponse(format!(
            "question '{id}': probabilities sum to {sum:.4}, expected ~1.0"
        )));
    }
    Ok(())
}

impl Response {
    /// Enforce the `/v1/systemone` response contract against the asked questions.
    pub fn validate(&self, questions: &BTreeMap<String, Question>) -> Result<(), DecideError> {
        for (id, q) in questions {
            let Some(ans) = self.answers.get(id) else {
                return Err(DecideError::InvalidResponse(format!(
                    "missing answer for question '{id}'"
                )));
            };
            match (q, ans) {
                (
                    Question::Choice { criteria, .. },
                    Answer::Choice {
                        choice,
                        confidence,
                        probabilities,
                    },
                ) => {
                    if !criteria.contains_key(choice) {
                        return Err(DecideError::InvalidResponse(format!(
                            "question '{id}': choice '{choice}' is not in criteria"
                        )));
                    }
                    if let Some(c) = confidence
                        && !valid_prob(*c)
                    {
                        return Err(DecideError::InvalidResponse(format!(
                            "question '{id}': confidence {c} out of [0,1]"
                        )));
                    }
                    if !probabilities.is_empty() {
                        if probabilities.keys().any(|k| !criteria.contains_key(k)) {
                            return Err(DecideError::InvalidResponse(format!(
                                "question '{id}': probabilities contain unknown option key"
                            )));
                        }
                        valid_distribution(probabilities.values(), criteria.len(), id)?;
                    }
                }
                (
                    Question::Score { criteria, .. },
                    Answer::Score {
                        score,
                        confidence,
                        probabilities,
                    },
                ) => {
                    let max = criteria.len().max(1) as f64;
                    if !score.is_finite() || *score < 1.0 - 1e-6 || *score > max + 1e-6 {
                        return Err(DecideError::InvalidResponse(format!(
                            "question '{id}': score {score} out of 1..={max}"
                        )));
                    }
                    if let Some(c) = confidence
                        && !valid_prob(*c)
                    {
                        return Err(DecideError::InvalidResponse(format!(
                            "question '{id}': confidence {c} out of [0,1]"
                        )));
                    }
                    if !probabilities.is_empty() {
                        valid_distribution(probabilities.iter(), criteria.len(), id)?;
                    }
                }
                (Question::Noul { .. }, Answer::Noul { noul }) => {
                    if !valid_prob(*noul) {
                        return Err(DecideError::InvalidResponse(format!(
                            "question '{id}': noul {noul} out of [0,1]"
                        )));
                    }
                }
                _ => {
                    return Err(DecideError::InvalidResponse(format!(
                        "question '{id}': answer variant does not match question type"
                    )));
                }
            }
        }
        Ok(())
    }
}
