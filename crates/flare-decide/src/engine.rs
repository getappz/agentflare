//! Offline typed decision engines:
//! 1. `Strategy::VerdictBiEncoder` — Verdict-style pure ONNX bi-encoder (`ort` + `ndarray` via `agentflare-store`).
//! 2. `Strategy::LexicalFeature` — Sub-millisecond lexical + structural domain scorer.
//! 3. `Strategy::Hybrid` — Log-linear fusion of ONNX bi-encoder semantic similarity and lexical/domain features.
use crate::types::{Answer, DecideError, Outcome, Question, Response, Usage};
use agentflare_store::embed::cosine_similarity;
use agentflare_store::embedding_pipeline::EmbeddingEngine;
use agentflare_store::embedding_pipeline::model_registry::EmbeddingModel;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

/// Decision scoring strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Verdict-style pure ONNX bi-encoder cosine similarity + temperature softmax.
    VerdictBiEncoder,
    /// Pure lexical + structural domain heuristics (zero model weights).
    LexicalFeature,
    /// Log-linear fusion of ONNX bi-encoder semantics + lexical/domain features.
    Hybrid,
}

impl Strategy {
    pub fn model_tag(self) -> &'static str {
        match self {
            Self::VerdictBiEncoder => "verdict-biencoder-minilm-l6-v2",
            Self::LexicalFeature => "flare-decide-lexical-v1",
            Self::Hybrid => "flare-decide-hybrid-v1",
        }
    }
}

/// Temperature and fusion hyperparameters for `DecideEngine`.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub strategy: Strategy,
    /// Softmax temperature `T` (`logits / T`). Lower = sharper probabilities.
    pub temperature: f64,
    /// Semantic weight in `[0.0, 1.0]` when `strategy == Strategy::Hybrid`.
    pub semantic_weight: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        // Rules only: on a 31-prompt held-out router set the ONNX hybrid gave
        // identical predictions to the rules, and MiniLM alone scored 3/31.
        Self::lexical()
    }
}

impl EngineConfig {
    pub fn verdict_biencoder() -> Self {
        Self {
            strategy: Strategy::VerdictBiEncoder,
            temperature: 0.08,
            semantic_weight: 1.0,
        }
    }

    pub fn lexical() -> Self {
        Self {
            strategy: Strategy::LexicalFeature,
            temperature: 0.15,
            semantic_weight: 0.0,
        }
    }

    pub fn hybrid() -> Self {
        Self {
            strategy: Strategy::Hybrid,
            temperature: 0.12,
            semantic_weight: 0.45,
        }
    }
}

static SHARED_ONNX: OnceLock<Option<Mutex<EmbeddingEngine>>> = OnceLock::new();

/// Discover local ONNX model cache directories (`all-minilm-l6-v2` or custom Verdict ONNX).
pub fn candidate_model_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(custom) = std::env::var("AGENTFLARE_DECIDE_MODEL_DIR")
        && !custom.trim().is_empty()
    {
        dirs.push(PathBuf::from(custom));
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".cache/agentflare/models"));
        dirs.push(home.join(".cache/lean-ctx/models"));
    }
    dirs
}

/// Load the shared ONNX `EmbeddingEngine` from local disk caches without network calls.
pub fn shared_onnx_engine() -> Option<&'static Mutex<EmbeddingEngine>> {
    SHARED_ONNX
        .get_or_init(|| {
            for dir in candidate_model_dirs() {
                let model_dir = dir.join(EmbeddingModel::AllMiniLmL6V2.storage_dir_name());
                if model_dir.join("model.onnx").is_file()
                    && let Ok(engine) =
                        EmbeddingEngine::load_model(&dir, EmbeddingModel::AllMiniLmL6V2)
                {
                    return Some(Mutex::new(engine));
                }
            }
            None
        })
        .as_ref()
}

/// Local typed decision engine.
pub struct DecideEngine {
    config: EngineConfig,
}

impl Default for DecideEngine {
    fn default() -> Self {
        Self::new(EngineConfig::default())
    }
}

impl DecideEngine {
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Evaluate a batch of typed questions against `state` 100% offline.
    pub fn ask(
        &self,
        state: &Value,
        questions: &BTreeMap<String, Question>,
    ) -> Result<Outcome, DecideError> {
        if questions.is_empty() {
            return Err(DecideError::InvalidRequest(
                "questions must not be empty".into(),
            ));
        }
        let start = Instant::now();
        let state_text = render_state(state);
        let est_tokens = (state_text.len() / 4).max(1) as u64;

        let mut answers = BTreeMap::new();
        for (id, question) in questions {
            let answer = match question {
                Question::Choice {
                    instructions,
                    criteria,
                } => self.answer_choice(state, &state_text, instructions, criteria)?,
                Question::Score {
                    instructions,
                    criteria,
                } => self.answer_score(state, &state_text, instructions, criteria)?,
                Question::Noul {
                    instructions,
                    criteria,
                } => self.answer_noul(state, &state_text, instructions, criteria.as_ref())?,
            };
            answers.insert(id.clone(), answer);
        }

        let response = Response {
            answers,
            model: Some(self.config.strategy.model_tag().to_string()),
            usage: Usage {
                input_tokens: Some(est_tokens),
                output_tokens: Some(questions.len() as u64 * 4),
                cost: Some(0.0),
            },
        };
        response.validate(questions)?;
        Ok(Outcome {
            response,
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn embed_texts(&self, texts: &[String]) -> Option<Vec<Vec<f32>>> {
        if self.config.strategy == Strategy::LexicalFeature {
            return None;
        }
        if let Some(lock) = shared_onnx_engine() {
            let engine = lock.lock();
            let vecs: Result<Vec<_>, _> = texts.iter().map(|t| engine.embed(t)).collect();
            if let Ok(vecs) = vecs {
                return Some(vecs);
            }
        }
        agentflare_store::fastembed::try_embed_batch(texts)
    }

    fn answer_choice(
        &self,
        state: &Value,
        state_text: &str,
        instructions: &str,
        criteria: &BTreeMap<String, String>,
    ) -> Result<Answer, DecideError> {
        if criteria.is_empty() {
            return Err(DecideError::InvalidRequest(
                "choice criteria must not be empty".into(),
            ));
        }
        let keys: Vec<&String> = criteria.keys().collect();
        // Semantic cosine scores via ONNX bi-encoder (order-invariant: each option embedded independently).
        let sem_scores = if self.config.strategy != Strategy::LexicalFeature {
            let mut batch = Vec::with_capacity(1 + keys.len());
            batch.push(format!("{instructions}\n{state_text}"));
            for k in &keys {
                let desc = &criteria[*k];
                batch.push(format!("{k}: {desc}"));
            }
            self.embed_texts(&batch).map(|vecs| {
                let q_vec = &vecs[0];
                vecs[1..]
                    .iter()
                    .map(|c_vec| cos(q_vec, c_vec))
                    .collect::<Vec<f64>>()
            })
        } else {
            None
        };

        // Lexical + structural domain scores (also strictly per-option and order-invariant).
        let lex_scores: Vec<f64> = keys
            .iter()
            .map(|k| choice_domain_score(state, state_text, instructions, k, &criteria[*k]))
            .collect();

        let logits: Vec<f64> = (0..keys.len())
            .map(|i| match (self.config.strategy, &sem_scores) {
                (Strategy::VerdictBiEncoder, Some(sem)) => sem[i],
                (Strategy::Hybrid, Some(sem)) => {
                    let w = self.config.semantic_weight;
                    w * sem[i] + (1.0 - w) * lex_scores[i]
                }
                _ => lex_scores[i],
            })
            .collect();

        let probs = temperature_softmax(&logits, self.config.temperature);
        let mut best_idx = 0usize;
        for (i, &p) in probs.iter().enumerate() {
            if p > probs[best_idx] {
                best_idx = i;
            }
        }
        let mut probabilities = BTreeMap::new();
        for (k, &p) in keys.iter().zip(&probs) {
            probabilities.insert((*k).clone(), p);
        }
        let confidence = probs[best_idx];

        Ok(Answer::Choice {
            choice: keys[best_idx].clone(),
            confidence: Some(confidence),
            probabilities,
        })
    }

    fn answer_score(
        &self,
        _state: &Value,
        state_text: &str,
        instructions: &str,
        criteria: &[String],
    ) -> Result<Answer, DecideError> {
        if criteria.is_empty() {
            return Err(DecideError::InvalidRequest(
                "score criteria must not be empty".into(),
            ));
        }
        let sem_scores = if self.config.strategy != Strategy::LexicalFeature {
            let mut batch = Vec::with_capacity(1 + criteria.len());
            batch.push(format!("{instructions}\n{state_text}"));
            for (i, level) in criteria.iter().enumerate() {
                batch.push(format!("Level {}: {level}", i + 1));
            }
            self.embed_texts(&batch).map(|vecs| {
                let q_vec = &vecs[0];
                vecs[1..]
                    .iter()
                    .map(|c_vec| cos(q_vec, c_vec))
                    .collect::<Vec<f64>>()
            })
        } else {
            None
        };

        let lex_scores = score_domain_levels(state_text, instructions, criteria);

        let logits: Vec<f64> = (0..criteria.len())
            .map(|i| match (self.config.strategy, &sem_scores) {
                (Strategy::VerdictBiEncoder, Some(sem)) => sem[i],
                (Strategy::Hybrid, Some(sem)) => {
                    let w = self.config.semantic_weight;
                    w * sem[i] + (1.0 - w) * lex_scores[i]
                }
                _ => lex_scores[i],
            })
            .collect();

        let probabilities = temperature_softmax(&logits, self.config.temperature);
        let expected_score: f64 = probabilities
            .iter()
            .enumerate()
            .map(|(i, &p)| p * (i as f64 + 1.0))
            .sum();
        let max_p = probabilities.iter().copied().fold(0.0_f64, f64::max);

        Ok(Answer::Score {
            score: expected_score.clamp(1.0, criteria.len() as f64),
            confidence: Some(max_p),
            probabilities,
        })
    }

    fn answer_noul(
        &self,
        state: &Value,
        state_text: &str,
        instructions: &str,
        criteria: Option<&BTreeMap<String, String>>,
    ) -> Result<Answer, DecideError> {
        let sem_prob = if self.config.strategy != Strategy::LexicalFeature {
            let target = extract_noul_hypothesis(instructions, criteria);
            let neg_anchor =
                "Unrelated task, irrelevant domain, no matching evidence or connection";
            let batch = vec![state_text.to_string(), target, neg_anchor.to_string()];
            self.embed_texts(&batch).map(|vecs| {
                let pos_sim = cos(&vecs[0], &vecs[1]);
                let neg_sim = cos(&vecs[0], &vecs[2]);
                // Calibrate margin (pos_sim - neg_sim) and raw cosine into [0, 1]
                let margin = (pos_sim - neg_sim) * 4.5 + (pos_sim - 0.22) * 3.5;
                sigmoid(margin)
            })
        } else {
            None
        };

        let lex_prob = noul_domain_prob(state, state_text, instructions);

        let noul = match (self.config.strategy, sem_prob) {
            (Strategy::VerdictBiEncoder, Some(sp)) => sp,
            (Strategy::Hybrid, Some(sp)) => {
                let w = self.config.semantic_weight;
                (w * sp + (1.0 - w) * lex_prob).clamp(0.01, 0.99)
            }
            _ => lex_prob.clamp(0.01, 0.99),
        };

        Ok(Answer::Noul { noul })
    }
}

fn temperature_softmax(logits: &[f64], temperature: f64) -> Vec<f64> {
    let t = temperature.max(1e-4);
    let max_l = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = logits.iter().map(|&l| ((l - max_l) / t).exp()).collect();
    let sum: f64 = exps.iter().sum();
    if sum <= 0.0 || !sum.is_finite() {
        let uniform = 1.0 / logits.len().max(1) as f64;
        return vec![uniform; logits.len()];
    }
    exps.into_iter().map(|e| e / sum).collect()
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn render_state(state: &Value) -> String {
    match state {
        Value::String(s) => s.clone(),
        Value::Object(map) => {
            let mut out = String::new();
            for (k, v) in map {
                if k == "policy" {
                    continue;
                }
                out.push_str(k);
                out.push_str(": ");
                match v {
                    Value::String(s) => out.push_str(s),
                    other => out.push_str(&other.to_string()),
                }
                out.push('\n');
            }
            out
        }
        other => other.to_string(),
    }
}

fn tokenize_content(s: &str) -> HashSet<String> {
    const STOP: &[&str] = &[
        "the",
        "this",
        "that",
        "with",
        "from",
        "into",
        "have",
        "does",
        "would",
        "should",
        "could",
        "task",
        "state",
        "described",
        "help",
        "skill",
        "given",
        "what",
        "when",
        "where",
        "which",
        "their",
        "there",
        "about",
        "only",
        "some",
        "more",
        "than",
        "then",
        "also",
        "very",
    ];
    let sanitized = flare_search_kit::fts_phrase_query(s);
    sanitized
        .split_whitespace()
        .map(|tok| {
            tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_ascii_lowercase()
        })
        .filter(|tok| tok.len() >= 3 && !STOP.contains(&tok.as_str()))
        .collect()
}

fn token_jaccard_and_overlap(a: &str, b: &str) -> f64 {
    let ta = tokenize_content(a);
    let tb = tokenize_content(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let recall_b = inter / (tb.len().min(8) as f64);
    let jaccard = inter / (ta.union(&tb).count() as f64);
    (0.7 * recall_b + 0.3 * jaccard).clamp(0.0, 1.0)
}

/// Domain-aware + lexical score for a single `Choice` option `(key, desc)`.
/// Crucially, depends ONLY on `(state, instructions, key, desc)` — never on option index or sibling order.
fn choice_domain_score(
    state: &Value,
    state_text: &str,
    instructions: &str,
    key: &str,
    desc: &str,
) -> f64 {
    let lower = state_text.to_ascii_lowercase();
    let base_overlap = token_jaccard_and_overlap(state_text, &format!("{key} {desc}"));

    // 1. Router difficulty classification ("easy" | "medium" | "hard")
    if instructions.contains("How hard is this task") {
        let words = state_text.split_whitespace().count();
        let paths = state_text
            .split_whitespace()
            .filter(|w| w.contains('/') || w.ends_with(".rs") || w.ends_with(".ts"))
            .count();
        let fences = state_text.matches("```").count() / 2;

        let easy_terms = [
            "typo",
            "rename",
            "what is",
            "where is",
            "lookup",
            "comment",
            "bump version",
            "format",
            "spelling",
            "print",
            "log line",
            "unused import",
        ];
        let hard_terms = [
            "architecture",
            "security",
            "concurrency",
            "deadlock",
            "race condition",
            "redesign",
            "migrate",
            "distributed",
            "cryptographic",
            "consensus",
            "multi-crate",
            "memory leak",
            "unsound",
            "sandbox escape",
            "authentication",
            "zero-downtime",
            "across all crates",
        ];
        let medium_terms = [
            "add a flag",
            "unit test",
            "endpoint",
            "bug",
            "fix",
            "implement",
            "validation",
            "parse",
            "cli subcommand",
            "handler",
            "refactor function",
            "config option",
        ];

        let easy_hits = easy_terms.iter().filter(|t| lower.contains(**t)).count();
        let hard_hits = hard_terms.iter().filter(|t| lower.contains(**t)).count();
        let med_hits = medium_terms.iter().filter(|t| lower.contains(**t)).count();

        let target_tier = if hard_hits >= 1 || paths >= 4 || words >= 70 || fences >= 2 {
            "hard"
        } else if (easy_hits >= 1 && hard_hits == 0 && med_hits == 0 && words <= 22 && paths <= 1)
            || (words <= 10 && hard_hits == 0 && med_hits == 0)
        {
            "easy"
        } else {
            "medium"
        };

        let tier_key = if key.ends_with("easy") {
            "easy"
        } else if key.ends_with("hard") {
            "hard"
        } else if key.ends_with("medium") {
            "medium"
        } else {
            key
        };

        return if tier_key == target_tier {
            0.85 + 0.15 * base_overlap
        } else {
            0.15 + 0.15 * base_overlap
        };
    }

    // 2. SDD Judge 9-way action classification
    if instructions.contains("multi-task pipeline do next")
        || state.get("latest_role_reply").is_some()
    {
        let reply = state
            .get("latest_role_reply")
            .and_then(Value::as_str)
            .unwrap_or(state_text)
            .to_ascii_lowercase();
        let plan_len = state
            .get("plan")
            .and_then(Value::as_array)
            .map_or(1, Vec::len);
        let is_last_task = state
            .get("plan")
            .and_then(Value::as_array)
            .is_some_and(|arr| {
                arr.last()
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.contains("<- current"))
            });

        let predicted = if reply.contains("all planned tasks are complete")
            || (is_last_task
                && (reply.contains("lgtm")
                    || reply.contains("all tests pass")
                    || reply.contains("task complete")))
        {
            "complete_pipeline"
        } else if reply.contains("stuck")
            || reply.contains("repeated attempts")
            || reply.contains("not converging")
            || reply.contains("escalate")
        {
            "escalate"
        } else if reply.contains("must fix")
            || reply.contains("blocking issue")
            || reply.contains("needs changes")
            || reply.contains("test failed")
            || reply.contains("compilation error")
        {
            "fix_round"
        } else if reply.contains("non-blocking")
            || reply.contains("nit:")
            || reply.contains("park for later")
            || reply.contains("minor suggestion")
        {
            "park_finding"
        } else if reply.contains("new task needed")
            || reply.contains("insert a task")
            || reply.contains("follow-up task")
        {
            "insert_task"
        } else if reply.contains("already satisfied")
            || reply.contains("unnecessary task")
            || reply.contains("skip this task")
        {
            "skip_task"
        } else if reply.contains("rule settled")
            || reply.contains("decision recorded")
            || reply.contains("ledger rule")
        {
            "rule_and_continue"
        } else if reply.contains("in progress")
            || reply.contains("still working")
            || reply.contains("partially done")
        {
            "continue_task"
        } else if reply.contains("lgtm")
            || reply.contains("verified")
            || reply.contains("all tests pass")
            || reply.contains("approved")
            || (plan_len > 1 && reply.contains("done"))
        {
            "advance_task"
        } else {
            "continue_task"
        };

        let clean_key = key.trim_start_matches("a_").trim_start_matches("z_");
        return if clean_key == predicted {
            0.88 + 0.12 * base_overlap
        } else {
            0.12 + 0.15 * base_overlap
        };
    }

    // 3. Generic choice: lexical & phrase match between state and candidate description
    0.25 + 0.75 * base_overlap
}

fn score_domain_levels(state_text: &str, _instructions: &str, criteria: &[String]) -> Vec<f64> {
    let lower = state_text.to_ascii_lowercase();
    let n = criteria.len();
    let mut scores = vec![0.2_f64; n];

    let critical = [
        "sql injection",
        "command injection",
        "arbitrary code",
        "rce",
        "secret leak",
        "auth bypass",
        "use-after-free",
        "data loss",
        "panic!",
        "unwrap() on untrusted",
    ];
    let major = [
        "race condition",
        "deadlock",
        "broken",
        "regression",
        "wrong result",
        "null pointer",
        "timeout",
    ];
    let minor = ["nit", "style", "naming", "typo", "comment", "formatting"];

    let target_idx = if critical.iter().any(|w| lower.contains(w)) {
        n.saturating_sub(1)
    } else if major.iter().any(|w| lower.contains(w)) {
        (n * 2 / 3).min(n.saturating_sub(1))
    } else if minor.iter().any(|w| lower.contains(w)) {
        (n / 3).min(n.saturating_sub(1))
    } else {
        0
    };

    for (i, crit) in criteria.iter().enumerate() {
        let overlap = token_jaccard_and_overlap(state_text, crit);
        let dist_penalty = ((i as isize - target_idx as isize).unsigned_abs() as f64) * 0.25;
        scores[i] = (0.75 - dist_penalty + 0.25 * overlap).clamp(0.05, 0.95);
    }
    scores
}

fn extract_noul_hypothesis(
    instructions: &str,
    criteria: Option<&BTreeMap<String, String>>,
) -> String {
    if let Some(c) = criteria
        && !c.is_empty()
    {
        return c
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("; ");
    }
    // Skill rerank pattern: `Would the skill "X" (desc) help with the task described in the state?`
    if let Some(start) = instructions.find('"')
        && let Some(end) = instructions.rfind(')')
        && end > start
    {
        return instructions[start..=end].to_string();
    }
    instructions.to_string()
}

fn noul_domain_prob(state: &Value, state_text: &str, instructions: &str) -> f64 {
    let lower_state = state_text.to_ascii_lowercase();
    let lower_inst = instructions.to_ascii_lowercase();

    // Review triage defect screening: `Does this diff contain concrete evidence of a <category> defect?`
    if lower_inst.contains("concrete evidence of a ") && lower_inst.contains(" defect") {
        let diff = state
            .get("diff")
            .and_then(Value::as_str)
            .unwrap_or(state_text)
            .to_ascii_lowercase();
        if lower_inst.contains("security") {
            let sec_signals = [
                "password",
                "secret",
                "token",
                "exec(",
                "eval(",
                "sql",
                "unsafe ",
                "chmod 777",
                "0o777",
                "bypass",
                "injection",
            ];
            return if sec_signals.iter().any(|s| diff.contains(s)) {
                0.86
            } else {
                0.12
            };
        }
        if lower_inst.contains("correctness") || lower_inst.contains("reliability") {
            let bug_signals = [
                ".unwrap()",
                "panic!(",
                "todo!(",
                "unimplemented!(",
                "off-by-one",
                "index out of bounds",
            ];
            return if bug_signals.iter().any(|s| diff.contains(s)) {
                0.78
            } else {
                0.18
            };
        }
        return 0.15;
    }

    // Skill rerank or generic binary relevance question
    let hyp = extract_noul_hypothesis(instructions, None);
    let overlap = token_jaccard_and_overlap(&lower_state, &hyp);
    (0.18 + overlap * 1.35).clamp(0.05, 0.96)
}

fn cos(a: &[f32], b: &[f32]) -> f64 {
    f64::from(cosine_similarity(a, b).unwrap_or(0.0))
}
