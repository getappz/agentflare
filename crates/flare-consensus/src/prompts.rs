//! Faithful port of the CVP prompt templates (`ai-consensus-core`'s
//! `prompts.ts`). The exact strings here shape model behavior — keep edits
//! deliberate.

use crate::parser::strip_confidence_line;
use crate::types::{Participant, ParticipantResponse, Phase};

pub struct RoundMeta {
    pub phase: Phase,
    pub label: String,
}

/// Map a round number to its phase and human label.
///
///   Round 1      → initial-analysis     "Initial Analysis"
///   Round 2      → counterarguments     "Counterarguments"
///   Round 3      → evidence-assessment  "Evidence Assessment"
///   Round 4..N-1 → synthesis            "Synthesis & Refinement (Round N)"
///   Round N      → synthesis            "Final Synthesis"
pub fn get_round_meta(round: u32, total_rounds: u32) -> RoundMeta {
    match round {
        1 => RoundMeta {
            phase: Phase::InitialAnalysis,
            label: "Initial Analysis".to_string(),
        },
        2 => RoundMeta {
            phase: Phase::Counterarguments,
            label: "Counterarguments".to_string(),
        },
        3 => RoundMeta {
            phase: Phase::EvidenceAssessment,
            label: "Evidence Assessment".to_string(),
        },
        _ => RoundMeta {
            phase: Phase::Synthesis,
            label: if round == total_rounds {
                "Final Synthesis".to_string()
            } else {
                format!("Synthesis & Refinement (Round {round})")
            },
        },
    }
}

fn phase_instructions(phase: Phase, round: u32, total: u32) -> String {
    match phase {
        Phase::InitialAnalysis => format!(
            "This is Round {round}/{total}: INITIAL ANALYSIS.\n\
Provide your initial analysis of the prompt. Share your perspective, key observations, and preliminary assessment. State your confidence level (0-100) at the end."
        ),
        Phase::Counterarguments => format!(
            "This is Round {round}/{total}: COUNTERARGUMENTS.\n\
Review the initial analyses from all participants below. Identify weaknesses, biases, and blind spots. Offer substantive counterarguments. Challenge assumptions. State your updated confidence level (0-100) at the end."
        ),
        Phase::EvidenceAssessment => format!(
            "This is Round {round}/{total}: EVIDENCE ASSESSMENT.\n\
Evaluate the strength of evidence and reasoning presented so far. Distinguish well-supported claims from speculation. Identify areas where consensus is forming and where disagreement remains substantive. State your confidence level (0-100) at the end."
        ),
        Phase::Synthesis => {
            let is_final = round == total;
            let header = if is_final {
                "FINAL SYNTHESIS"
            } else {
                "SYNTHESIS & REFINEMENT"
            };
            let directive = if is_final {
                "Provide your final, considered position."
            } else {
                "Refine your position based on the strongest arguments presented."
            };
            format!(
                "This is Round {round}/{total}: {header}.\n\
Synthesize the discussion so far into a coherent assessment. Acknowledge remaining uncertainties. {directive} State your final confidence level (0-100) at the end."
            )
        }
    }
}

/// Format the block of previous responses a participant sees at the start
/// of rounds 2+. Matches the "PREVIOUS ROUND RESPONSES" fence so models
/// can't latch onto a different delimiter across versions.
pub fn format_previous_responses(responses: &[ParticipantResponse]) -> String {
    if responses.is_empty() {
        return String::new();
    }
    let blocks: Vec<String> = responses
        .iter()
        .map(|r| {
            format!(
                "[Participant {} | Confidence: {}%]\n{}",
                r.participant_id, r.confidence, r.content
            )
        })
        .collect();
    format!(
        "\n\n--- PREVIOUS ROUND RESPONSES ---\n{}\n--- END PREVIOUS RESPONSES ---",
        blocks.join("\n\n---\n\n")
    )
}

/// Build the full system prompt for a single participant call.
///
/// Shape (as text, not code — rustdoc would otherwise try to run it):
///
/// ```text
/// {persona.system_prompt}
///
/// {phase instructions}{previous-responses block, if any}
///
/// IMPORTANT: End your response with a line in exactly this format:
/// CONFIDENCE: [number 0-100]
/// ```
pub fn build_participant_system_prompt(
    persona_system_prompt: &str,
    phase: Phase,
    round: u32,
    total_rounds: u32,
    previous_responses: &[ParticipantResponse],
) -> String {
    let instructions = phase_instructions(phase, round, total_rounds);
    let previous_context = format_previous_responses(previous_responses);
    format!(
        "{persona_system_prompt}\n\n{instructions}{previous_context}\n\n\
IMPORTANT: End your response with a line in exactly this format:\n\
CONFIDENCE: [number 0-100]"
    )
}

const JUDGE_CONFIDENCE_DIRECTIVE: &str = "\n\n\
IMPORTANT: End your response with a line in exactly this format:\n\
JUDGE_CONFIDENCE: [number 0-100]";

/// Build the judge's system prompt: the judge persona's instructions plus
/// the original debated question.
///
/// Idempotently appends the `JUDGE_CONFIDENCE: [number 0-100]` directive so
/// `parser::extract_judge_confidence` always finds a real value to parse
/// rather than silently returning its 50 default. If the caller's prompt
/// already mentions `JUDGE_CONFIDENCE` (as `JUDGE_PERSONA.system_prompt`
/// does), the directive is not duplicated.
pub fn build_judge_system_prompt(judge_system_prompt: &str, question: &str) -> String {
    let base = format!(
        "{judge_system_prompt}\n\nThe original prompt that was debated was:\n\"\"\"\n{question}\n\"\"\""
    );
    if base.to_lowercase().contains("judge_confidence") {
        base
    } else {
        format!("{base}{JUDGE_CONFIDENCE_DIRECTIVE}")
    }
}

/// Build the judge's user content: the final-round responses, labelled with
/// persona name and model id, with the trailing `CONFIDENCE: N` line
/// stripped from each body (participant confidence is surfaced in the heading).
pub fn build_judge_user_prompt(
    final_responses: &[ParticipantResponse],
    participants: &[Participant],
) -> String {
    let blocks: Vec<String> = final_responses
        .iter()
        .map(|r| {
            let label = participants
                .iter()
                .find(|p| p.id == r.participant_id)
                .map(|p| format!("{} ({})", p.persona.name, p.model_id))
                .unwrap_or_else(|| r.participant_id.clone());
            let body = strip_confidence_line(&r.content);
            format!(
                "### {label} — self-reported confidence {}%\n{body}",
                r.confidence
            )
        })
        .collect();
    format!(
        "Below are the final-round responses from every participant. Synthesize them per your instructions.\n\n{}",
        blocks.join("\n\n---\n\n")
    )
}
