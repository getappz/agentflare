//! `ConsensusEngine` — CVP orchestrator. Faithful port of
//! `ai-consensus-core`'s `engine.ts`, minus the tool-calling loop (see
//! `crate::types` for why).
//!
//! Drives the full protocol: round scheduling, phase prompts, blind/
//! sequential dispatch, confidence extraction, stats, disagreement
//! detection, early stopping, optional judge synthesis. Zero LLM-provider
//! coupling — `ModelCaller` is the single extension point.

use std::collections::HashSet;
use std::sync::Arc;

use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio_util::sync::CancellationToken;

use crate::events::{ConsensusEvent, EventBus, EventSubscriber};
use crate::parser::{extract_confidence, extract_judge_confidence, extract_judge_section};
use crate::personas::judge_persona;
use crate::prompts::{
    build_judge_system_prompt, build_judge_user_prompt, build_participant_system_prompt,
    get_round_meta,
};
use crate::stats::{average, consensus_score, detect_disagreements, shuffle, stddev};
use crate::types::{
    ConsensusOptions, ConsensusResult, EarlyStop, JudgeOptions, ModelCallRequest, ModelCaller,
    Participant, ParticipantResponse, Phase, RoundResult, StopReason, SynthesisResult, TokenSink,
};

// ── Defaults ────────────────────────────────────────────────────

pub struct ConsensusDefaults;

impl ConsensusDefaults {
    pub const MAX_ROUNDS: u32 = 4;
    pub const EARLY_STOP: bool = true;
    pub const CONVERGENCE_DELTA: f64 = 3.0;
    pub const DISAGREEMENT_THRESHOLD: u8 = 20;
    pub const BLIND_FIRST_ROUND: bool = true;
    pub const RANDOMIZE_ORDER: bool = true;
    pub const PARTICIPANT_TEMPERATURE: f64 = 0.7;
    pub const MAX_OUTPUT_TOKENS: u32 = 1500;
    pub const JUDGE_TEMPERATURE: f64 = 0.3;
    pub const JUDGE_MAX_OUTPUT_TOKENS: u32 = 1500;
}

pub const MAX_ROUNDS_CAP: u32 = 10;
const MIN_PARTICIPANTS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum ConsensusError {
    #[error("ConsensusEngine: {0}")]
    InvalidOptions(String),
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn is_cancelled(token: &Option<CancellationToken>) -> bool {
    token.as_ref().is_some_and(CancellationToken::is_cancelled)
}

// ── Public engine ───────────────────────────────────────────────

pub struct ConsensusEngine {
    caller: Arc<dyn ModelCaller>,
    events: Arc<EventBus>,
}

impl ConsensusEngine {
    pub fn new(caller: Arc<dyn ModelCaller>) -> Self {
        Self {
            caller,
            events: Arc::new(EventBus::new()),
        }
    }

    pub async fn subscribe(&self, subscriber: Arc<dyn EventSubscriber>) {
        self.events.subscribe(subscriber).await;
    }

    /// Run the Consensus Validation Protocol end-to-end.
    ///
    /// Emits events throughout (see [`ConsensusEvent`]) to every subscriber.
    /// Resolves with the final [`ConsensusResult`] even on cancellation —
    /// `ConsensusOptions::cancellation` firing mid-run produces a partial
    /// result with `stop_reason: Aborted` rather than an error. Only
    /// malformed options (empty question, <2 participants, duplicate ids)
    /// return `Err`.
    pub async fn run(&self, options: ConsensusOptions) -> Result<ConsensusResult, ConsensusError> {
        let opts = normalize_options(options)?;
        let started_at = now_millis();
        let mut rng = StdRng::seed_from_u64(opts.random_seed.unwrap_or_else(rand::random));

        let mut all_responses: Vec<ParticipantResponse> = Vec::new();
        let mut rounds: Vec<RoundResult> = Vec::new();
        let mut round_scores: Vec<i32> = Vec::new();
        let mut stop_reason = StopReason::MaxRounds;
        let mut early_stop: Option<EarlyStop> = None;

        for round in 1..=opts.max_rounds {
            if is_cancelled(&opts.cancellation) {
                stop_reason = StopReason::Aborted;
                break;
            }

            let meta = get_round_meta(round, opts.max_rounds);
            let blind = round == 1 && opts.blind_first_round;

            let order: Vec<Participant> = if !blind && opts.randomize_order && round > 1 {
                shuffle(&opts.participants, &mut rng)
            } else {
                opts.participants.clone()
            };

            self.events
                .publish(ConsensusEvent::RoundStart {
                    round,
                    phase: meta.phase,
                    label: meta.label.clone(),
                    blind,
                    participant_ids: order.iter().map(|p| p.id.clone()).collect(),
                })
                .await;

            let round_started_at = now_millis();
            let previous_responses: Vec<ParticipantResponse> = all_responses
                .iter()
                .filter(|r| r.round < round)
                .cloned()
                .collect();

            let round_responses = self
                .run_round(
                    round,
                    meta.phase,
                    blind,
                    &order,
                    &previous_responses,
                    opts.max_rounds,
                    &opts.question,
                    opts.participant_temperature,
                    opts.max_output_tokens,
                    opts.cancellation.clone(),
                )
                .await;

            let round_completed_at = now_millis();
            all_responses.extend(round_responses.iter().cloned());

            let confidences: Vec<f64> = round_responses
                .iter()
                .filter(|r| r.error.is_none())
                .map(|r| r.confidence as f64)
                .collect();
            let avg = average(&confidences);
            let sd = stddev(&confidences);
            let score = consensus_score(&confidences);
            round_scores.push(score);

            let disagreements = detect_disagreements(
                round,
                &round_responses,
                &opts.participants,
                opts.disagreement_threshold,
            );
            for d in &disagreements {
                self.events
                    .publish(ConsensusEvent::DisagreementDetected {
                        round,
                        disagreement: d.clone(),
                    })
                    .await;
            }

            let duration_ms = round_completed_at - round_started_at;
            rounds.push(RoundResult {
                round,
                phase: meta.phase,
                label: meta.label.clone(),
                blind,
                responses: round_responses.clone(),
                average_confidence: avg,
                stddev: sd,
                score,
                disagreements: disagreements.clone(),
                started_at: round_started_at,
                completed_at: round_completed_at,
                duration_ms,
            });

            self.events
                .publish(ConsensusEvent::RoundComplete {
                    round,
                    phase: meta.phase,
                    average_confidence: avg,
                    stddev: sd,
                    score,
                    disagreements,
                    responses: round_responses,
                    duration_ms,
                })
                .await;

            if opts.early_stop && round >= 2 && round < opts.max_rounds && round_scores.len() >= 2 {
                let prev = round_scores[round_scores.len() - 2];
                let delta = (score - prev).unsigned_abs() as f64;
                if delta <= opts.convergence_delta {
                    let reason = format!(
                        "Consensus score delta {delta:.1} between rounds {} and {round} is at or below the convergence threshold ({}).",
                        round - 1,
                        opts.convergence_delta
                    );
                    early_stop = Some(EarlyStop {
                        round,
                        delta,
                        reason: reason.clone(),
                    });
                    stop_reason = StopReason::Converged;
                    self.events
                        .publish(ConsensusEvent::EarlyStop {
                            round,
                            delta,
                            reason,
                        })
                        .await;
                    break;
                }
            }
        }

        // Judge synthesis (optional).
        let (last_round_score, last_round_responses, last_round_num) = match rounds.last() {
            Some(r) => (r.score, r.responses.clone(), r.round),
            None => (0, Vec::new(), 0),
        };

        let mut synthesis: Option<SynthesisResult> = None;
        if let Some(judge) = &opts.judge
            && !rounds.is_empty()
        {
            if is_cancelled(&opts.cancellation) {
                stop_reason = StopReason::Aborted;
            } else {
                synthesis = Some(
                    self.run_judge(
                        judge,
                        &last_round_responses,
                        &opts.participants,
                        &opts.question,
                        last_round_num,
                        opts.cancellation.clone(),
                    )
                    .await,
                );
            }
        }

        let completed_at = now_millis();
        let final_confidences: Vec<f64> = last_round_responses
            .iter()
            .filter(|r| r.error.is_none())
            .map(|r| r.confidence as f64)
            .collect();

        let result = ConsensusResult {
            question: opts.question.clone(),
            participants: opts.participants.clone(),
            rounds_completed: rounds.len() as u32,
            final_score: last_round_score,
            final_average_confidence: average(&final_confidences),
            final_stddev: stddev(&final_confidences),
            stop_reason,
            early_stop,
            synthesis,
            started_at,
            completed_at,
            duration_ms: completed_at - started_at,
            rounds,
        };

        self.events
            .publish(ConsensusEvent::FinalResult {
                result: result.clone(),
            })
            .await;
        Ok(result)
    }

    // ── Round orchestration ────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    async fn run_round(
        &self,
        round: u32,
        phase: Phase,
        blind: bool,
        order: &[Participant],
        previous_responses: &[ParticipantResponse],
        total_rounds: u32,
        question: &str,
        temperature: f64,
        max_output_tokens: u32,
        cancellation: Option<CancellationToken>,
    ) -> Vec<ParticipantResponse> {
        if blind {
            let calls = order.iter().map(|participant| {
                self.call_participant(
                    participant,
                    round,
                    phase,
                    total_rounds,
                    question,
                    &[],
                    temperature,
                    max_output_tokens,
                    cancellation.clone(),
                    &[],
                )
            });
            return futures::future::join_all(calls).await;
        }

        let mut collected: Vec<ParticipantResponse> = Vec::new();
        for participant in order {
            if is_cancelled(&cancellation) {
                break;
            }
            let mut visible = previous_responses.to_vec();
            visible.extend(collected.iter().cloned());
            let confidences_so_far: Vec<f64> = collected
                .iter()
                .filter(|r| r.error.is_none())
                .map(|r| r.confidence as f64)
                .collect();
            let response = self
                .call_participant(
                    participant,
                    round,
                    phase,
                    total_rounds,
                    question,
                    &visible,
                    temperature,
                    max_output_tokens,
                    cancellation.clone(),
                    &confidences_so_far,
                )
                .await;
            collected.push(response);
        }
        collected
    }

    // ── Single participant call ─────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    async fn call_participant(
        &self,
        participant: &Participant,
        round: u32,
        phase: Phase,
        total_rounds: u32,
        question: &str,
        previous_responses: &[ParticipantResponse],
        temperature: f64,
        max_output_tokens: u32,
        cancellation: Option<CancellationToken>,
        running_confidences: &[f64],
    ) -> ParticipantResponse {
        let system = build_participant_system_prompt(
            &participant.persona.system_prompt,
            phase,
            round,
            total_rounds,
            previous_responses,
        );

        self.events
            .publish(ConsensusEvent::ParticipantStart {
                round,
                phase,
                participant_id: participant.id.clone(),
                model_id: participant.model_id.clone(),
                persona_id: participant.persona.id.clone(),
            })
            .await;

        let started_at = now_millis();
        let on_token = self.token_sink(
            round,
            participant.id.clone(),
            |round, participant_id, token| ConsensusEvent::ParticipantToken {
                round,
                participant_id,
                token,
            },
        );

        let request = ModelCallRequest {
            participant_id: participant.id.clone(),
            model_id: participant.model_id.clone(),
            round,
            phase,
            system,
            user: question.to_string(),
            temperature,
            max_output_tokens,
            cancellation,
            on_token: Some(on_token),
        };

        let (mut content, error, usage) = match self.caller.call(request).await {
            Ok(resp) => (resp.content, None, resp.usage),
            Err(err) => (String::new(), Some(err.0), None),
        };
        if let Some(err) = &error
            && content.is_empty()
        {
            content = format!("[Error from {}: {err}]", participant.model_id);
        }

        let completed_at = now_millis();
        let confidence = if error.is_some() {
            0
        } else {
            extract_confidence(&content)
        };

        let response = ParticipantResponse {
            participant_id: participant.id.clone(),
            model_id: participant.model_id.clone(),
            persona_id: participant.persona.id.clone(),
            round,
            phase,
            content,
            confidence,
            error,
            usage,
            started_at,
            completed_at,
            duration_ms: completed_at - started_at,
        };

        self.events
            .publish(ConsensusEvent::ParticipantComplete {
                round,
                phase,
                response: response.clone(),
            })
            .await;

        if response.error.is_none() {
            let mut with_self: Vec<f64> = running_confidences.to_vec();
            with_self.push(confidence as f64);
            self.events
                .publish(ConsensusEvent::ConfidenceUpdate {
                    round,
                    participant_id: participant.id.clone(),
                    confidence,
                    running_average: average(&with_self),
                })
                .await;
        }

        response
    }

    // ── Judge synthesizer ───────────────────────────────────────

    async fn run_judge(
        &self,
        judge: &JudgeOptions,
        final_responses: &[ParticipantResponse],
        participants: &[Participant],
        question: &str,
        last_round_number: u32,
        cancellation: Option<CancellationToken>,
    ) -> SynthesisResult {
        self.events
            .publish(ConsensusEvent::SynthesisStart {
                model_id: judge.model_id.clone(),
            })
            .await;

        let judge_system_prompt = judge
            .system_prompt
            .clone()
            .unwrap_or_else(|| judge_persona().system_prompt);
        let system = build_judge_system_prompt(&judge_system_prompt, question);
        let user = build_judge_user_prompt(final_responses, participants);

        let caller: &Arc<dyn ModelCaller> = judge.caller.as_ref().unwrap_or(&self.caller);
        let temperature = judge
            .temperature
            .unwrap_or(ConsensusDefaults::JUDGE_TEMPERATURE);
        let max_output_tokens = judge
            .max_output_tokens
            .unwrap_or(ConsensusDefaults::JUDGE_MAX_OUTPUT_TOKENS);

        let on_token = self.token_sink(
            last_round_number,
            "judge".to_string(),
            |_round, _pid, token| ConsensusEvent::SynthesisToken { token },
        );

        let started_at = now_millis();
        let request = ModelCallRequest {
            participant_id: "judge".to_string(),
            model_id: judge.model_id.clone(),
            round: last_round_number,
            phase: Phase::Synthesis,
            system,
            user,
            temperature,
            max_output_tokens,
            cancellation,
            on_token: Some(on_token),
        };

        let (content, usage) = match caller.call(request).await {
            Ok(resp) => (resp.content, resp.usage),
            Err(err) => (
                format!("[Judge error from {}: {}]", judge.model_id, err.0),
                None,
            ),
        };

        let completed_at = now_millis();
        let synthesis = SynthesisResult {
            model_id: judge.model_id.clone(),
            majority_position: extract_judge_section(&content, "Majority Position"),
            minority_positions: extract_judge_section(&content, "Minority Positions"),
            unresolved_disputes: extract_judge_section(&content, "Unresolved Disputes"),
            judge_confidence: extract_judge_confidence(&content),
            content,
            usage,
            started_at,
            completed_at,
            duration_ms: completed_at - started_at,
        };

        self.events
            .publish(ConsensusEvent::SynthesisComplete {
                synthesis: synthesis.clone(),
            })
            .await;
        synthesis
    }

    /// Bridge the `ModelCaller`'s synchronous `on_token` callback to the
    /// async `EventBus`: each invocation spawns a short-lived task that
    /// publishes one event. `make_event` builds the event from
    /// `(round, participant_id, token)` — the judge case ignores the first
    /// two and closes over its own `model_id` via `SynthesisToken`.
    fn token_sink(
        &self,
        round: u32,
        participant_id: String,
        make_event: fn(u32, String, String) -> ConsensusEvent,
    ) -> TokenSink {
        let events = Arc::clone(&self.events);
        Arc::new(move |token: &str| {
            let events = Arc::clone(&events);
            let participant_id = participant_id.clone();
            let token = token.to_string();
            tokio::spawn(async move {
                events
                    .publish(make_event(round, participant_id, token))
                    .await;
            });
        })
    }
}

// ── Option normalization ───────────────────────────────────────

struct NormalizedOptions {
    question: String,
    participants: Vec<Participant>,
    max_rounds: u32,
    early_stop: bool,
    convergence_delta: f64,
    disagreement_threshold: u8,
    blind_first_round: bool,
    randomize_order: bool,
    participant_temperature: f64,
    max_output_tokens: u32,
    judge: Option<JudgeOptions>,
    random_seed: Option<u64>,
    cancellation: Option<CancellationToken>,
}

fn normalize_options(options: ConsensusOptions) -> Result<NormalizedOptions, ConsensusError> {
    if options.question.trim().is_empty() {
        return Err(ConsensusError::InvalidOptions(
            "`question` must be a non-empty string.".to_string(),
        ));
    }
    if options.participants.len() < MIN_PARTICIPANTS {
        return Err(ConsensusError::InvalidOptions(format!(
            "at least {MIN_PARTICIPANTS} participants are required (got {}).",
            options.participants.len()
        )));
    }
    let mut ids = HashSet::new();
    for p in &options.participants {
        if !ids.insert(p.id.clone()) {
            return Err(ConsensusError::InvalidOptions(format!(
                "duplicate participant id \"{}\".",
                p.id
            )));
        }
    }

    let max_rounds = options
        .max_rounds
        .unwrap_or(ConsensusDefaults::MAX_ROUNDS)
        .clamp(1, MAX_ROUNDS_CAP);

    Ok(NormalizedOptions {
        question: options.question,
        participants: options.participants,
        max_rounds,
        early_stop: options.early_stop.unwrap_or(ConsensusDefaults::EARLY_STOP),
        convergence_delta: options
            .convergence_delta
            .unwrap_or(ConsensusDefaults::CONVERGENCE_DELTA),
        disagreement_threshold: options
            .disagreement_threshold
            .unwrap_or(ConsensusDefaults::DISAGREEMENT_THRESHOLD),
        blind_first_round: options
            .blind_first_round
            .unwrap_or(ConsensusDefaults::BLIND_FIRST_ROUND),
        randomize_order: options
            .randomize_order
            .unwrap_or(ConsensusDefaults::RANDOMIZE_ORDER),
        participant_temperature: options
            .participant_temperature
            .unwrap_or(ConsensusDefaults::PARTICIPANT_TEMPERATURE),
        max_output_tokens: options
            .max_output_tokens
            .unwrap_or(ConsensusDefaults::MAX_OUTPUT_TOKENS),
        judge: options.judge,
        random_seed: options.random_seed,
        cancellation: options.cancellation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCallError, ModelCallResponse, Persona};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    fn persona(id: &str, name: &str, system_prompt: &str) -> Persona {
        Persona {
            id: id.to_string(),
            name: name.to_string(),
            emoji: None,
            color: None,
            description: String::new(),
            system_prompt: system_prompt.to_string(),
        }
    }

    fn participant(id: &str, name: &str, confidence: u8) -> (Participant, u8) {
        (
            Participant {
                id: id.to_string(),
                model_id: format!("model-{id}"),
                persona: persona(id, name, "You are a participant."),
                label: None,
            },
            confidence,
        )
    }

    /// Always answers with a fixed `CONFIDENCE: N` per participant id, so
    /// round-over-round scores are deterministic and assertable.
    struct FixedConfidenceCaller {
        confidences: std::collections::HashMap<String, u8>,
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ModelCaller for FixedConfidenceCaller {
        async fn call(
            &self,
            request: ModelCallRequest,
        ) -> Result<ModelCallResponse, ModelCallError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if let Some(sink) = &request.on_token {
                sink("partial ");
            }
            let confidence = self
                .confidences
                .get(&request.participant_id)
                .copied()
                .unwrap_or(50);
            Ok(ModelCallResponse {
                content: format!(
                    "Analysis for {}.\nCONFIDENCE: {confidence}",
                    request.participant_id
                ),
                usage: None,
            })
        }
    }

    struct RecordingSubscriber {
        events: Mutex<Vec<ConsensusEvent>>,
    }

    #[async_trait::async_trait]
    impl EventSubscriber for RecordingSubscriber {
        async fn on_event(&self, event: &ConsensusEvent) {
            self.events.lock().await.push(event.clone());
        }
    }

    #[tokio::test]
    async fn runs_two_rounds_and_detects_a_disagreement() {
        let (pa, ca) = participant("a", "Risk Analyst", 90);
        let (pb, cb) = participant("b", "Optimist", 50);
        let mut confidences = std::collections::HashMap::new();
        confidences.insert(pa.id.clone(), ca);
        confidences.insert(pb.id.clone(), cb);

        let caller = Arc::new(FixedConfidenceCaller {
            confidences,
            call_count: AtomicUsize::new(0),
        });
        let engine = ConsensusEngine::new(caller.clone());

        let recorder = Arc::new(RecordingSubscriber {
            events: Mutex::new(Vec::new()),
        });
        engine.subscribe(recorder.clone()).await;

        let result = engine
            .run(ConsensusOptions {
                question: "Should we ship it?".to_string(),
                participants: vec![pa, pb],
                max_rounds: Some(2),
                early_stop: Some(true),
                convergence_delta: None,
                disagreement_threshold: None,
                blind_first_round: Some(true),
                randomize_order: Some(false),
                participant_temperature: None,
                max_output_tokens: None,
                judge: None,
                random_seed: Some(7),
                cancellation: None,
            })
            .await
            .expect("valid options");

        assert_eq!(result.rounds_completed, 2);
        assert_eq!(result.stop_reason, StopReason::MaxRounds);
        assert_eq!(result.rounds[0].disagreements.len(), 1);
        assert_eq!(result.rounds[0].disagreements[0].severity, 40);
        assert_eq!(result.final_score, consensus_score(&[90.0, 50.0]));
        assert_eq!(caller.call_count.load(Ordering::SeqCst), 4); // 2 participants * 2 rounds

        // `EventBus::publish` is fire-and-forget (spawns per subscriber), so
        // delivery isn't guaranteed to have landed the instant `run()`
        // resolves — poll briefly rather than asserting on it immediately.
        let mut got_final = false;
        for _ in 0..100 {
            if recorder
                .events
                .lock()
                .await
                .iter()
                .any(|e| matches!(e, ConsensusEvent::FinalResult { .. }))
            {
                got_final = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            got_final,
            "FinalResult event was never delivered to the subscriber"
        );

        let events = recorder.events.lock().await;
        let token_events = events
            .iter()
            .filter(|e| matches!(e, ConsensusEvent::ParticipantToken { .. }))
            .count();
        assert!(
            token_events >= 1,
            "expected at least one streamed token event"
        );
    }

    #[tokio::test]
    async fn runs_judge_synthesis_after_final_round() {
        let (pa, _) = participant("a", "Risk Analyst", 80);
        let (pb, _) = participant("b", "Optimist", 85);
        let mut confidences = std::collections::HashMap::new();
        confidences.insert(pa.id.clone(), 80);
        confidences.insert(pb.id.clone(), 85);
        confidences.insert("judge".to_string(), 50);

        let caller = Arc::new(FixedConfidenceCaller {
            confidences,
            call_count: AtomicUsize::new(0),
        });
        let engine = ConsensusEngine::new(caller);

        let result = engine
            .run(ConsensusOptions {
                question: "Is this design sound?".to_string(),
                participants: vec![pa, pb],
                max_rounds: Some(1),
                early_stop: Some(false),
                convergence_delta: None,
                disagreement_threshold: None,
                blind_first_round: Some(true),
                randomize_order: Some(false),
                participant_temperature: None,
                max_output_tokens: None,
                judge: Some(JudgeOptions {
                    model_id: "judge-model".to_string(),
                    caller: None,
                    temperature: None,
                    max_output_tokens: None,
                    system_prompt: None,
                }),
                random_seed: None,
                cancellation: None,
            })
            .await
            .expect("valid options");

        // FixedConfidenceCaller doesn't emit the judge's markdown headings, so
        // the section extractors legitimately return "" — this test only
        // asserts the judge ran and its confidence line parsed.
        let synthesis = result.synthesis.expect("judge should have run");
        assert_eq!(synthesis.model_id, "judge-model");
        assert_eq!(synthesis.judge_confidence, 50);
    }

    #[tokio::test]
    async fn rejects_fewer_than_two_participants() {
        let caller = Arc::new(FixedConfidenceCaller {
            confidences: std::collections::HashMap::new(),
            call_count: AtomicUsize::new(0),
        });
        let engine = ConsensusEngine::new(caller);
        let (pa, _) = participant("a", "Solo", 50);

        let err = engine
            .run(ConsensusOptions {
                question: "q".to_string(),
                participants: vec![pa],
                max_rounds: None,
                early_stop: None,
                convergence_delta: None,
                disagreement_threshold: None,
                blind_first_round: None,
                randomize_order: None,
                participant_temperature: None,
                max_output_tokens: None,
                judge: None,
                random_seed: None,
                cancellation: None,
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ConsensusError::InvalidOptions(_)));
    }

    #[tokio::test]
    async fn rejects_empty_question() {
        let caller = Arc::new(FixedConfidenceCaller {
            confidences: std::collections::HashMap::new(),
            call_count: AtomicUsize::new(0),
        });
        let engine = ConsensusEngine::new(caller);
        let (pa, _) = participant("a", "A", 50);
        let (pb, _) = participant("b", "B", 50);

        let err = engine
            .run(ConsensusOptions {
                question: "   ".to_string(),
                participants: vec![pa, pb],
                max_rounds: None,
                early_stop: None,
                convergence_delta: None,
                disagreement_threshold: None,
                blind_first_round: None,
                randomize_order: None,
                participant_temperature: None,
                max_output_tokens: None,
                judge: None,
                random_seed: None,
                cancellation: None,
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ConsensusError::InvalidOptions(_)));
    }

    #[tokio::test]
    async fn stops_immediately_when_already_cancelled() {
        let caller = Arc::new(FixedConfidenceCaller {
            confidences: std::collections::HashMap::new(),
            call_count: AtomicUsize::new(0),
        });
        let engine = ConsensusEngine::new(caller.clone());
        let (pa, _) = participant("a", "A", 50);
        let (pb, _) = participant("b", "B", 50);

        let token = CancellationToken::new();
        token.cancel();

        let result = engine
            .run(ConsensusOptions {
                question: "q".to_string(),
                participants: vec![pa, pb],
                max_rounds: Some(3),
                early_stop: None,
                convergence_delta: None,
                disagreement_threshold: None,
                blind_first_round: None,
                randomize_order: None,
                participant_temperature: None,
                max_output_tokens: None,
                judge: None,
                random_seed: None,
                cancellation: Some(token),
            })
            .await
            .expect("valid options");

        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(result.rounds_completed, 0);
        assert_eq!(caller.call_count.load(Ordering::SeqCst), 0);
    }
}
