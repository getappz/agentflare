//! Averages, standard deviation, consensus score, disagreement detection,
//! seeded shuffle. Faithful port of `ai-consensus-core`'s `stats.ts`, except
//! `shuffle` is backed by the `rand` crate's seedable `StdRng` instead of a
//! hand-rolled `mulberry32` — this is a fresh Rust engine, not required to
//! reproduce the JS PRNG's bit sequence, so the ecosystem RNG wins.

use rand::rngs::StdRng;
use rand::seq::SliceRandom;

use crate::types::{Disagreement, Participant, ParticipantResponse};

/// Arithmetic mean. Returns 0 for empty input.
pub fn average(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Population standard deviation (divides by N, not N-1).
///
/// Population stddev, not sample stddev: the participant count is the
/// entire panel, not a sample from a larger distribution.
pub fn stddev(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mean = average(xs);
    let var_sum: f64 = xs.iter().map(|x| (x - mean).powi(2)).sum();
    (var_sum / xs.len() as f64).sqrt()
}

/// Consensus score: `round(clamp(avg - 0.5*stddev, 0, 100))`.
///
/// Penalizes disagreement (high stddev) against the pack's confidence.
pub fn consensus_score(confidences: &[f64]) -> i32 {
    if confidences.is_empty() {
        return 0;
    }
    let avg = average(confidences);
    let sd = stddev(confidences);
    (avg - sd * 0.5).clamp(0.0, 100.0).round() as i32
}

/// Pairwise disagreement detection. Any two non-errored responses whose
/// confidence differs by at least `threshold` (default 20) generates a
/// `Disagreement` entry. Deterministic (no text heuristic, no extra LLM calls).
pub fn detect_disagreements(
    round: u32,
    responses: &[ParticipantResponse],
    participants: &[Participant],
    threshold: u8,
) -> Vec<Disagreement> {
    let mut out = Vec::new();
    for i in 0..responses.len() {
        for j in (i + 1)..responses.len() {
            let a = &responses[i];
            let b = &responses[j];
            if a.error.is_some() || b.error.is_some() {
                continue;
            }
            let delta = a.confidence.abs_diff(b.confidence);
            if delta < threshold {
                continue;
            }
            let pa = participants.iter().find(|p| p.id == a.participant_id);
            let pb = participants.iter().find(|p| p.id == b.participant_id);
            let label = match (pa, pb) {
                (Some(pa), Some(pb)) => format!("{} vs {}", pa.persona.name, pb.persona.name),
                _ => "Confidence split".to_string(),
            };
            out.push(Disagreement {
                id: format!("r{round}-{}-{}", a.participant_id, b.participant_id),
                round,
                participant_a_id: a.participant_id.clone(),
                participant_b_id: b.participant_id.clone(),
                severity: delta,
                label,
            });
        }
    }
    out
}

/// Fisher–Yates shuffle. Pure (returns a new vec, `rng` is the only mutated
/// state) — pass the same `StdRng` across calls for a single advancing
/// stream (what a `random_seed`d run needs for reproducible replay).
pub fn shuffle<T: Clone>(input: &[T], rng: &mut StdRng) -> Vec<T> {
    let mut out = input.to_vec();
    out.shuffle(rng);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Persona;
    use rand::SeedableRng;

    fn persona(id: &str, name: &str) -> Persona {
        Persona {
            id: id.to_string(),
            name: name.to_string(),
            emoji: None,
            color: None,
            description: String::new(),
            system_prompt: "x".to_string(),
        }
    }

    fn participant(id: &str, persona_name: &str) -> Participant {
        Participant {
            id: id.to_string(),
            model_id: "m".to_string(),
            persona: persona(id, persona_name),
            label: None,
        }
    }

    fn response(participant_id: &str, confidence: u8, error: Option<&str>) -> ParticipantResponse {
        ParticipantResponse {
            participant_id: participant_id.to_string(),
            model_id: "m".to_string(),
            persona_id: "p".to_string(),
            round: 1,
            phase: crate::types::Phase::InitialAnalysis,
            content: format!("x\nCONFIDENCE: {confidence}"),
            confidence,
            error: error.map(str::to_string),
            usage: None,
            started_at: 0,
            completed_at: 0,
            duration_ms: 0,
        }
    }

    #[test]
    fn average_arithmetic_mean() {
        assert_eq!(average(&[10.0, 20.0, 30.0]), 20.0);
    }

    #[test]
    fn average_empty_is_zero() {
        assert_eq!(average(&[]), 0.0);
    }

    #[test]
    fn average_single_element() {
        assert_eq!(average(&[42.0]), 42.0);
    }

    #[test]
    fn stddev_identical_values_is_zero() {
        assert_eq!(stddev(&[50.0, 50.0, 50.0, 50.0]), 0.0);
    }

    #[test]
    fn stddev_known_case() {
        assert!((stddev(&[1.0, 2.0, 3.0, 4.0, 5.0]) - std::f64::consts::SQRT_2).abs() < 1e-9);
    }

    #[test]
    fn stddev_population_not_sample() {
        // [100, 0]: mean=50, population variance=2500 -> sigma=50 (sample sigma would be ~70.7).
        assert!((stddev(&[100.0, 0.0]) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn stddev_empty_is_zero() {
        assert_eq!(stddev(&[]), 0.0);
    }

    #[test]
    fn consensus_score_zero_stddev_is_the_mean() {
        assert_eq!(consensus_score(&[80.0, 80.0, 80.0]), 80);
    }

    #[test]
    fn consensus_score_penalises_disagreement() {
        // avg=50, sigma=50 -> 50 - 25 = 25
        assert_eq!(consensus_score(&[100.0, 0.0]), 25);
    }

    #[test]
    fn consensus_score_clamps_to_0_100() {
        assert_eq!(consensus_score(&[100.0, 100.0]), 100);
        assert_eq!(consensus_score(&[0.0, 0.0]), 0);
    }

    #[test]
    fn consensus_score_rounds_half_up() {
        // avg=85, sigma=5, score=82.5 -> rounds to 83.
        assert_eq!(consensus_score(&[90.0, 80.0]), 83);
    }

    #[test]
    fn consensus_score_empty_is_zero() {
        assert_eq!(consensus_score(&[]), 0);
    }

    #[test]
    fn detect_disagreements_flags_pairs_at_or_above_threshold() {
        let participants = vec![
            participant("a", "Risk Analyst"),
            participant("b", "First-Principles Engineer"),
        ];
        let responses = vec![response("a", 90, None), response("b", 50, None)];
        let out = detect_disagreements(2, &responses, &participants, 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, 40);
        assert_eq!(out[0].label, "Risk Analyst vs First-Principles Engineer");
        assert_eq!(out[0].id, "r2-a-b");
    }

    #[test]
    fn detect_disagreements_ignores_pairs_below_threshold() {
        let participants = vec![
            participant("a", "Risk Analyst"),
            participant("b", "Domain Expert"),
        ];
        let responses = vec![response("a", 80, None), response("b", 65, None)];
        assert!(detect_disagreements(1, &responses, &participants, 20).is_empty());
    }

    #[test]
    fn detect_disagreements_delta_exactly_at_threshold_counts() {
        let participants = vec![
            participant("a", "Risk Analyst"),
            participant("b", "Domain Expert"),
        ];
        let responses = vec![response("a", 80, None), response("b", 60, None)];
        let out = detect_disagreements(1, &responses, &participants, 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, 20);
    }

    #[test]
    fn detect_disagreements_excludes_errored_responses() {
        let participants = vec![
            participant("a", "Risk Analyst"),
            participant("b", "Domain Expert"),
        ];
        let responses = vec![
            response("a", 90, None),
            response("b", 0, Some("provider 503")),
        ];
        assert!(detect_disagreements(1, &responses, &participants, 20).is_empty());
    }

    #[test]
    fn detect_disagreements_all_pairs_when_every_pair_diverges() {
        let participants = vec![
            participant("a", "Risk Analyst"),
            participant("b", "First-Principles Engineer"),
            participant("c", "Domain Expert"),
        ];
        let responses = vec![
            response("a", 100, None),
            response("b", 50, None),
            response("c", 0, None),
        ];
        assert_eq!(
            detect_disagreements(1, &responses, &participants, 20).len(),
            3
        );
    }

    #[test]
    fn detect_disagreements_respects_custom_threshold() {
        let participants = vec![
            participant("a", "Risk Analyst"),
            participant("b", "Domain Expert"),
        ];
        let responses = vec![response("a", 80, None), response("b", 73, None)];
        let out = detect_disagreements(1, &responses, &participants, 5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, 7);
    }

    #[test]
    fn detect_disagreements_falls_back_to_neutral_label() {
        let responses = vec![response("x", 90, None), response("y", 50, None)];
        let out = detect_disagreements(1, &responses, &[], 20);
        assert_eq!(out[0].label, "Confidence split");
    }

    #[test]
    fn shuffle_does_not_mutate_input() {
        let input = vec![1, 2, 3, 4, 5];
        let mut rng = StdRng::seed_from_u64(1);
        let _ = shuffle(&input, &mut rng);
        assert_eq!(input, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn shuffle_returns_a_permutation() {
        let input = vec![1, 2, 3, 4, 5];
        let mut rng = StdRng::seed_from_u64(1);
        let mut out = shuffle(&input, &mut rng);
        out.sort_unstable();
        assert_eq!(out, input);
    }

    #[test]
    fn shuffle_is_deterministic_for_a_seed() {
        let mut rng_a = StdRng::seed_from_u64(42);
        let mut rng_b = StdRng::seed_from_u64(42);
        let a = shuffle(&[1, 2, 3, 4, 5], &mut rng_a);
        let b = shuffle(&[1, 2, 3, 4, 5], &mut rng_b);
        assert_eq!(a, b);
    }

    #[test]
    fn shuffle_handles_empty_and_single_element() {
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(shuffle::<i32>(&[], &mut rng), Vec::<i32>::new());
        assert_eq!(shuffle(&[7], &mut rng), vec![7]);
    }
}
