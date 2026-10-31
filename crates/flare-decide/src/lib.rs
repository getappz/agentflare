//! `flare-decide` — Offline typed decision engine (`Choice`, `Score`, `Noul`)
//! reusing existing workspace dependencies (`agentflare-store` ONNX `ort` + `ndarray`
//! and `flare-search-kit` lexical primitives) with zero new crate dependencies.
pub mod bench;
pub mod engine;
pub mod types;

pub use engine::{DecideEngine, EngineConfig, Strategy, candidate_model_dirs, shared_onnx_engine};
pub use types::{Answer, DecideError, Outcome, Question, Request, Response, Usage};

use serde_json::Value;
use std::collections::BTreeMap;

/// Evaluate `questions` against `state` using the default `flare-decide` hybrid engine.
pub fn ask(state: &Value, questions: &BTreeMap<String, Question>) -> Result<Outcome, DecideError> {
    DecideEngine::default().ask(state, questions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn choice_score_and_noul_pass_contract_validation() {
        let mut qs = BTreeMap::new();
        qs.insert(
            "difficulty".into(),
            Question::choice(
                "How hard is this task for an AI coding agent?",
                [
                    ("easy", "Simple lookup or typo"),
                    ("medium", "Typical bugfix or feature"),
                    ("hard", "Architecture or security redesign"),
                ],
            ),
        );
        qs.insert(
            "severity".into(),
            Question::score(
                "Assess defect severity",
                ["None", "Minor", "Major", "Critical"],
            ),
        );
        qs.insert(
            "relevant".into(),
            Question::noul(
                "Would the skill \"sqlite-debug\" (Fix SQLite WAL and FTS5 issues) help with the task described in the state?",
            ),
        );

        let state = json!("Fix SQLite WAL lock contention and FTS5 migration bug in store.rs");
        let outcome = ask(&state, &qs).expect("valid local decision");
        outcome
            .response
            .validate(&qs)
            .expect("validates against contract");
        assert_eq!(outcome.response.usage.cost, Some(0.0));
    }

    #[test]
    fn order_invariance_zero_position_bias() {
        let engine = DecideEngine::new(EngineConfig::hybrid());
        let state = json!("fix typo in README.md header");

        let q1 = BTreeMap::from([(
            "d".into(),
            Question::choice(
                "How hard is this task for an AI coding agent?",
                [
                    ("a_easy", "Lookup, rename, typo, small mechanical edit"),
                    ("z_hard", "Architecture, deep debugging, security"),
                ],
            ),
        )]);
        let q2 = BTreeMap::from([(
            "d".into(),
            Question::choice(
                "How hard is this task for an AI coding agent?",
                [
                    ("z_easy", "Lookup, rename, typo, small mechanical edit"),
                    ("a_hard", "Architecture, deep debugging, security"),
                ],
            ),
        )]);

        let a1 = engine.ask(&state, &q1).unwrap();
        let a2 = engine.ask(&state, &q2).unwrap();

        let (c1, p1) = match &a1.response.answers["d"] {
            Answer::Choice {
                choice,
                probabilities,
                ..
            } => (choice.as_str(), probabilities["a_easy"]),
            _ => panic!("expected choice"),
        };
        let (c2, p2) = match &a2.response.answers["d"] {
            Answer::Choice {
                choice,
                probabilities,
                ..
            } => (choice.as_str(), probabilities["z_easy"]),
            _ => panic!("expected choice"),
        };

        // Each option is scored independently, so sort position cannot move the
        // decision. The key text IS embedded, so relabeling may shift probabilities
        // slightly; only the decision is required to be stable.
        assert_eq!(c1.trim_start_matches("a_"), "easy");
        assert_eq!(c2.trim_start_matches("z_"), "easy");
        assert!(
            (p1 - p2).abs() < 0.15,
            "relabel drift too large: {p1} vs {p2}"
        );
    }

    #[test]
    fn benchmark_suite_compares_jev_verdict_and_flare_decide() {
        let results = bench::run_all_benchmarks();
        let table = bench::format_markdown_table(&results);
        println!("\n{table}");

        // Only structural properties are asserted. Accuracy numbers come from a
        // small hand-written case set (and a simulated Jev), so they are reported,
        // not gated on. Relabel-invariance is asserted only for the rules-only
        // engine: the embedding rows read option names, and without the cached
        // model (CI) they differ from a dev machine.
        for m in results.iter().filter(|m| m.name.contains("Lexical")) {
            assert_eq!(m.position_flip_rate, 0.0, "{} flipped on relabel", m.name);
        }
    }

    // Held-out router evaluation. Ignored: needs HELDOUT=<json of {prompt,label}>
    // kept outside the repo (real prompt text).

    #[test]
    #[ignore]
    fn router_heldout() {
        let path = std::env::var("HELDOUT").expect("set HELDOUT");
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let q = Question::choice(
            "How hard is this task for an AI coding agent?",
            [
                (
                    "easy",
                    "Lookup, rename, typo, small mechanical edit, or a simple question",
                ),
                ("medium", "A typical feature or bugfix touching a few files"),
                (
                    "hard",
                    "Architecture, deep debugging, security, a large refactor, or an ambiguous design",
                ),
            ],
        );
        let qs = BTreeMap::from([("difficulty".to_string(), q)]);
        let mut preds = vec![];
        for r in &rows {
            let cfg = match std::env::var("ENGINE").as_deref() {
                Ok("lexical") => EngineConfig::lexical(),
                Ok("minilm") => EngineConfig::verdict_biencoder(),
                _ => EngineConfig::hybrid(),
            };
            let out = DecideEngine::new(cfg)
                .ask(&json!(r["prompt"]), &qs)
                .unwrap();
            let Answer::Choice { choice, .. } = &out.response.answers["difficulty"] else {
                panic!()
            };
            preds.push(choice.clone());
        }
        println!("PREDS {}", serde_json::to_string(&preds).unwrap());
    }
}
