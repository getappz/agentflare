//! Core data types: personas, participants, the `ModelCaller` extension
//! point, and the round/result shapes the engine produces.
//!
//! Faithful port of `ai-consensus-core`'s `types.ts`, minus the tool-calling
//! surface (`ToolDefinition`/`ToolCall`/`ToolExecutor`/...) — not something
//! Suprmind-style consensus needs, and left out rather than half-built.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

// ── Persona / Participant ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    pub description: String,
    pub system_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub id: String,
    pub model_id: String,
    pub persona: Persona,
    #[serde(default)]
    pub label: Option<String>,
}

// ── Phase ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    InitialAnalysis,
    Counterarguments,
    EvidenceAssessment,
    Synthesis,
}

// ── Token usage ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

impl TokenUsage {
    pub fn merge(self, other: TokenUsage) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            total_tokens: self.total_tokens + other.total_tokens,
        }
    }
}

// ── ModelCaller — the one extension point ──────────────────────

/// Streams a partial token to whoever is watching this participant's turn
/// (the engine wires this to a `ParticipantToken`/`SynthesisToken` event).
pub type TokenSink = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

pub struct ModelCallRequest {
    /// Participant that originated the request, or "judge" for the synthesizer.
    pub participant_id: String,
    /// Opaque provider model id (e.g. "claude-opus-4-5", "gpt-4o") — forwarded
    /// verbatim to whatever backs the `ModelCaller` (flare-proxy, in flare-desktop).
    pub model_id: String,
    /// 1-based round index. Judge calls use the final round number.
    pub round: u32,
    /// Phase of this call; `Synthesis` is used for the judge.
    pub phase: Phase,
    /// Full system prompt (persona + round instructions).
    pub system: String,
    /// The user's question (CVP) or synthesis context (judge).
    pub user: String,
    /// Sampling temperature hint — 0.7 for participants, 0.3 for judge.
    pub temperature: f64,
    /// Maximum output token hint.
    pub max_output_tokens: u32,
    /// Propagates cancellation. Honor this.
    pub cancellation: Option<CancellationToken>,
    /// Optional streaming sink; callers MAY invoke this with partial tokens.
    pub on_token: Option<TokenSink>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelCallResponse {
    /// Full assistant content, including the trailing `CONFIDENCE: N` line.
    pub content: String,
    /// Optional token usage, if the provider surfaces it.
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ModelCallError(pub String);

/// The single extension point of this crate. Zero provider coupling —
/// `flare-desktop` implements this against `flare-proxy`.
#[async_trait]
pub trait ModelCaller: Send + Sync {
    async fn call(&self, request: ModelCallRequest) -> Result<ModelCallResponse, ModelCallError>;
}

// ── Per-participant response ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantResponse {
    pub participant_id: String,
    pub model_id: String,
    pub persona_id: String,
    pub round: u32,
    pub phase: Phase,
    pub content: String,
    /// 0-100, parsed from the `CONFIDENCE: N` trailing line. Defaults to 50 if absent.
    pub confidence: u8,
    /// If the `ModelCaller` failed, present and non-empty. Responses with
    /// errors are excluded from consensus score and disagreement detection.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_ms: i64,
}

// ── Disagreement (confidence-split heuristic) ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disagreement {
    /// Stable id: `r<round>-<a>-<b>`.
    pub id: String,
    pub round: u32,
    pub participant_a_id: String,
    pub participant_b_id: String,
    /// Absolute confidence delta (0-100).
    pub severity: u8,
    /// Short human label (e.g. "Risk Analyst vs Optimistic Futurist").
    pub label: String,
}

// ── Round result ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundResult {
    pub round: u32,
    pub phase: Phase,
    pub label: String,
    pub blind: bool,
    pub responses: Vec<ParticipantResponse>,
    pub average_confidence: f64,
    pub stddev: f64,
    /// Consensus score: `round(clamp(avg - 0.5 * stddev, 0, 100))`.
    pub score: i32,
    pub disagreements: Vec<Disagreement>,
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_ms: i64,
}

// ── Synthesis (judge) result ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisResult {
    pub model_id: String,
    pub content: String,
    pub majority_position: String,
    pub minority_positions: String,
    pub unresolved_disputes: String,
    /// 0-100, from the `JUDGE_CONFIDENCE: N` trailing line. Defaults to 50 if absent.
    pub judge_confidence: u8,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_ms: i64,
}

// ── Final consensus result ─────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    MaxRounds,
    Converged,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EarlyStop {
    pub round: u32,
    pub delta: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusResult {
    pub question: String,
    pub participants: Vec<Participant>,
    pub rounds: Vec<RoundResult>,
    pub rounds_completed: u32,
    pub final_score: i32,
    pub final_average_confidence: f64,
    pub final_stddev: f64,
    pub stop_reason: StopReason,
    #[serde(default)]
    pub early_stop: Option<EarlyStop>,
    #[serde(default)]
    pub synthesis: Option<SynthesisResult>,
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_ms: i64,
}

// ── Engine options ──────────────────────────────────────────────

pub struct JudgeOptions {
    /// Judge model id (passed to the `ModelCaller`).
    pub model_id: String,
    /// Optional override. If omitted, the engine's default `ModelCaller` is used.
    pub caller: Option<std::sync::Arc<dyn ModelCaller>>,
    /// Temperature for judge. Defaults to 0.3.
    pub temperature: Option<f64>,
    /// Max output tokens for judge. Defaults to 1500.
    pub max_output_tokens: Option<u32>,
    /// Override the judge system prompt. Defaults to `JUDGE_PERSONA.system_prompt`.
    ///
    /// Contract: the override must instruct the model to emit the same four
    /// `## Majority Position` / `## Minority Positions` / `## Unresolved Disputes`
    /// / `## Synthesis Confidence` headings and a trailing `JUDGE_CONFIDENCE: N`
    /// line. `extract_judge_section` and `extract_judge_confidence` key off
    /// those markers — break the contract and the corresponding fields on
    /// `SynthesisResult` will come back empty / default to 50.
    pub system_prompt: Option<String>,
}

pub struct ConsensusOptions {
    /// The question/prompt to run consensus on. Required, non-empty.
    pub question: String,
    /// Ordered list of participants. At least two are required.
    pub participants: Vec<Participant>,
    /// Max number of rounds. Bounded to \[1, 10\]. Defaults to 4.
    pub max_rounds: Option<u32>,
    /// Enable early stopping when |Δscore| ≤ `convergence_delta`. Defaults to true.
    pub early_stop: Option<bool>,
    /// Convergence threshold (consensus-score delta). Defaults to 3.
    pub convergence_delta: Option<f64>,
    /// Confidence-delta threshold for disagreement detection. Defaults to 20.
    pub disagreement_threshold: Option<u8>,
    /// Run round 1 in parallel with no cross-visibility. Defaults to true.
    pub blind_first_round: Option<bool>,
    /// Shuffle speaking order on rounds 2+. Defaults to true.
    pub randomize_order: Option<bool>,
    /// Temperature for participant calls. Defaults to 0.7.
    pub participant_temperature: Option<f64>,
    /// Max output tokens per participant call. Defaults to 1500.
    pub max_output_tokens: Option<u32>,
    /// Optional judge synthesis. If provided, runs after the final round.
    pub judge: Option<JudgeOptions>,
    /// If set, uses a seeded PRNG so round-order randomization is deterministic.
    pub random_seed: Option<u64>,
    /// Propagates cancellation to every `ModelCaller` and aborts the loop.
    pub cancellation: Option<CancellationToken>,
}
