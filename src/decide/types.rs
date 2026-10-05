use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One typed question. Question ids are NOT sent to the model, so each
/// question must carry its full meaning in `instructions` and `criteria`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// Pick one option; `criteria` maps option -> description.
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    /// Degree on an ordered rubric; `criteria` lists the levels low -> high.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// Is a yes/no statement true; answer is P(yes).
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

/// One typed answer. `confidence` is optional on the wire (providers differ,
/// and noul has none): a missing confidence means "not confident enough to
/// act", never zero.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice {
        choice: String,
        confidence: Option<f64>,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        confidence: Option<f64>,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
    },
    Noul {
        noul: f64,
    },
}

impl Answer {
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Choice { confidence, .. } | Self::Score { confidence, .. } => *confidence,
            Self::Noul { .. } => None,
        }
    }

    /// Short human-readable form for CLI output and shadow logs.
    pub fn summary(&self) -> String {
        let conf = |c: &Option<f64>| c.map_or(String::new(), |c| format!(" (confidence {c:.2})"));
        match self {
            Self::Choice {
                choice, confidence, ..
            } => format!("choice={choice}{}", conf(confidence)),
            Self::Score {
                score, confidence, ..
            } => format!("score={score:.2}{}", conf(confidence)),
            Self::Noul { noul } => format!("noul={noul:.2}"),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Only OpenRouter reports a dollar cost.
    #[serde(default)]
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Usage,
}
