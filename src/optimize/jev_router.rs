//! `JevRouter`: ask Jev how hard the prompt is and nudge toward a cheaper (or a
//! premium) model, instead of the keyword/length heuristics.
//!
//! Select with `AGENTFLARE_ROUTER=jev` (plus `AGENTFLARE_JEV=1`: the prompt is
//! sent off-machine, truncated). Every failure, a disabled backend or a
//! low-confidence answer falls back to the `KeywordRouter` decision, and every
//! real Jev answer is written to the shadow log (site `router`) next to what
//! the keyword router said, so `agentflare decide report` shows the agreement.
//!
//! Nudges are only emitted when they are useful, because a nudge costs tokens
//! on every prompt: "easy" suggests a cheap-model subagent; "hard" suggests the
//! premium model only when the current model is known and isn't already it;
//! "medium" stays silent.
use super::runtime::{KeywordRouter, RouteContext, Router};
use crate::decide::{self, Answer, DecideError, Outcome, Question, Response, shadow};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

const SITE: &str = "router";
const QUESTION_ID: &str = "difficulty";
const MIN_CONFIDENCE: f64 = 0.8;
const MAX_PROMPT_CHARS: usize = 2000;
/// Well inside the UserPromptSubmit hook's wall-clock budget.
const BUDGET: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tier {
    Easy,
    Medium,
    Hard,
}

impl Tier {
    fn from_choice(choice: &str) -> Option<Self> {
        match choice {
            "easy" => Some(Self::Easy),
            "medium" => Some(Self::Medium),
            "hard" => Some(Self::Hard),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
        }
    }
}

/// The model a nudge would send the user to, if any. This one value drives both
/// the nudge text and the shadow-log label, so the log compares what each
/// router would actually do (haiku / opus / none) and never a raw tier.
fn target(tier: Tier, current_model: Option<&str>) -> Option<&'static str> {
    match tier {
        Tier::Easy => Some("haiku"),
        Tier::Hard if current_model.is_some_and(|m| !m.to_lowercase().contains("opus")) => {
            Some("opus")
        }
        Tier::Medium | Tier::Hard => None,
    }
}

/// Options sort alphabetically on the wire (easy, hard, medium), and Jev leans
/// toward the first option, hence the confidence gate and the shadow log.
fn questions() -> BTreeMap<String, Question> {
    BTreeMap::from([(
        QUESTION_ID.to_string(),
        Question::choice(
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
        ),
    )])
}

fn tier_of(resp: &Response) -> Option<(Tier, Option<f64>)> {
    match resp.answers.get(QUESTION_ID)? {
        Answer::Choice {
            choice, confidence, ..
        } => Some((Tier::from_choice(choice)?, *confidence)),
        _ => None,
    }
}

fn nudge(tier: Tier, confidence: f64, current_model: Option<&str>) -> Option<String> {
    let model = target(tier, current_model)?;
    let rated = format!(
        "Jev rates this prompt {} (confidence {confidence:.2})",
        tier.label()
    );
    Some(match tier {
        Tier::Easy => format!(
            "{rated} — consider routing it to {model} (a cheap-model subagent) instead of running it inline."
        ),
        Tier::Medium | Tier::Hard => format!("{rated} — consider {model}."),
    })
}

pub struct JevRouter;

impl Router for JevRouter {
    fn route(&self, ctx: &RouteContext) -> Option<String> {
        route_with(
            ctx,
            &|prompt| decide::ask_within(&Value::from(prompt), &questions(), BUDGET),
            &shadow::record,
        )
    }
}

fn route_with(
    ctx: &RouteContext,
    ask: &dyn Fn(&str) -> Result<Outcome, DecideError>,
    record: &dyn Fn(&shadow::Row),
) -> Option<String> {
    let baseline = KeywordRouter.route(ctx);
    let prompt = ctx.prompt.trim();
    // Slash commands and empty prompts carry no task to rate: skip the network.
    if prompt.is_empty() || prompt.starts_with('/') {
        return baseline;
    }
    let prompt: String = prompt.chars().take(MAX_PROMPT_CHARS).collect();
    let baseline_label = if baseline.is_some() { "haiku" } else { "none" };
    let outcome = match ask(&prompt) {
        Ok(outcome) => outcome,
        // Not configured: behave exactly like the keyword router, silently.
        Err(DecideError::Disabled | DecideError::NoCredentials) => return baseline,
        Err(e) => {
            record(&shadow::Row::failed(
                SITE,
                &prompt,
                baseline_label,
                &e.to_string(),
            ));
            return baseline;
        }
    };
    let Some((tier, confidence)) = tier_of(&outcome.response) else {
        record(&shadow::Row::failed(
            SITE,
            &prompt,
            baseline_label,
            "unexpected answer shape",
        ));
        return baseline;
    };
    record(&shadow::Row::answered(
        SITE,
        &prompt,
        baseline_label,
        target(tier, ctx.current_model.as_deref()).unwrap_or("none"),
        confidence,
        outcome.elapsed_ms,
        outcome.response.usage.cost,
    ));
    if let Some(answer) = outcome.response.answers.get(QUESTION_ID) {
        decide::capture::record(decide::capture::Input {
            site: SITE,
            features: decide::capture::router_features(&prompt),
            norm_input: &prompt,
            label: decide::capture::label_of(answer),
            confidence,
            baseline: baseline_label,
            source_model: outcome.response.model.as_deref(),
        });
    }
    match confidence {
        Some(c) if c >= MIN_CONFIDENCE => nudge(tier, c, ctx.current_model.as_deref()),
        // Missing or low confidence means "don't rely on it": keep the heuristic.
        _ => baseline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::Usage;
    use std::cell::RefCell;

    fn ctx(prompt: &str, model: Option<&str>) -> RouteContext {
        RouteContext {
            prompt: prompt.to_string(),
            session_id: "s".to_string(),
            turn_count: 1,
            recent_tool_calls: vec![],
            current_model: model.map(str::to_string),
        }
    }

    fn answer(choice: &str, confidence: Option<f64>) -> Result<Outcome, DecideError> {
        Ok(Outcome {
            response: Response {
                answers: BTreeMap::from([(
                    QUESTION_ID.to_string(),
                    Answer::Choice {
                        choice: choice.to_string(),
                        confidence,
                        probabilities: BTreeMap::new(),
                    },
                )]),
                model: None,
                usage: Usage {
                    cost: Some(0.00001),
                    ..Usage::default()
                },
            },
            elapsed_ms: 700,
        })
    }

    /// Run `route_with`, returning the nudge and every shadow row recorded.
    fn run(
        c: &RouteContext,
        reply: impl Fn(&str) -> Result<Outcome, DecideError>,
    ) -> (Option<String>, Vec<shadow::Row>) {
        let rows = RefCell::new(vec![]);
        let nudge = route_with(c, &reply, &|r| rows.borrow_mut().push(r.clone()));
        (nudge, rows.into_inner())
    }

    #[test]
    fn confident_easy_suggests_haiku_and_logs_the_comparison() {
        let (nudge, rows) = run(&ctx("rename this variable", None), |_| {
            answer("easy", Some(0.95))
        });
        assert!(nudge.unwrap().contains("haiku"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].site, "router");
        assert_eq!(rows[0].jev.as_deref(), Some("haiku"));
        assert_eq!(rows[0].baseline, "none");
        assert_eq!(rows[0].confidence, Some(0.95));
    }

    #[test]
    fn hard_nudges_only_when_the_current_model_is_known_and_not_premium() {
        let hard = |_: &str| answer("hard", Some(0.9));
        assert_eq!(run(&ctx("redesign the scheduler", None), hard).0, None);
        assert!(
            run(
                &ctx("redesign the scheduler", Some("claude-sonnet-5-5")),
                hard
            )
            .0
            .unwrap()
            .contains("opus")
        );
        assert_eq!(
            run(
                &ctx("redesign the scheduler", Some("claude-opus-5-5")),
                hard
            )
            .0,
            None
        );
    }

    #[test]
    fn confident_medium_is_silent_even_when_the_keyword_router_would_speak() {
        let (nudge, rows) = run(&ctx("find the config loader and fix it", None), |_| {
            answer("medium", Some(0.9))
        });
        assert_eq!(nudge, None);
        assert_eq!(rows[0].baseline, "haiku"); // keyword router said haiku
        assert_eq!(rows[0].jev.as_deref(), Some("none")); // medium: no nudge, so "none"
    }

    #[test]
    fn low_or_missing_confidence_keeps_the_keyword_decision() {
        for conf in [Some(0.5), None] {
            let (nudge, rows) = run(&ctx("find the config file", None), |_| answer("hard", conf));
            assert!(nudge.unwrap().contains("cheap-model subagent"), "{conf:?}");
            assert_eq!(rows.len(), 1);
        }
    }

    #[test]
    fn not_configured_acts_like_the_keyword_router_and_logs_nothing() {
        for err in [DecideError::Disabled, DecideError::NoCredentials] {
            let (nudge, rows) = run(&ctx("find the config file", None), |_| Err(err_clone(&err)));
            assert!(nudge.is_some());
            assert!(rows.is_empty());
        }
    }

    fn err_clone(e: &DecideError) -> DecideError {
        match e {
            DecideError::Disabled => DecideError::Disabled,
            _ => DecideError::NoCredentials,
        }
    }

    #[test]
    fn backend_failure_falls_back_and_is_logged_as_a_fallback() {
        let (nudge, rows) = run(&ctx("rename a var", None), |_| {
            Err(DecideError::Status(402, "no credits".to_string()))
        });
        assert_eq!(nudge, None);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].error.as_deref().unwrap().contains("402"));
        assert_eq!(rows[0].jev, None);
    }

    #[test]
    fn unexpected_answer_shape_is_a_logged_fallback() {
        let (nudge, rows) = run(&ctx("rename a var", None), |_| {
            answer("impossible", Some(1.0))
        });
        assert_eq!(nudge, None);
        assert_eq!(rows[0].error.as_deref(), Some("unexpected answer shape"));
    }

    #[test]
    fn shadow_label_is_the_effective_target_not_the_raw_tier() {
        let label = |tier: &str, model: Option<&str>| {
            run(&ctx("redesign the scheduler", model), |_| {
                answer(tier, Some(0.9))
            })
            .1[0]
                .jev
                .clone()
        };
        assert_eq!(label("easy", None).as_deref(), Some("haiku"));
        assert_eq!(label("medium", None).as_deref(), Some("none"));
        assert_eq!(label("hard", None).as_deref(), Some("none")); // model unknown: no nudge
        assert_eq!(
            label("hard", Some("claude-sonnet-5")).as_deref(),
            Some("opus")
        );
        assert_eq!(
            label("hard", Some("claude-opus-5")).as_deref(),
            Some("none")
        );
    }

    #[test]
    fn slash_commands_and_empty_prompts_never_hit_the_network() {
        for p in ["/pm mode off", "   ", ""] {
            let (nudge, rows) = run(&ctx(p, None), |_| panic!("ask must not be called"));
            assert_eq!(nudge, None);
            assert!(rows.is_empty());
        }
    }

    #[test]
    fn long_prompts_are_truncated_before_leaving_the_machine() {
        let long = "x".repeat(MAX_PROMPT_CHARS * 3);
        let seen = RefCell::new(0);
        let _ = run(&ctx(&long, None), |p| {
            *seen.borrow_mut() = p.chars().count();
            answer("medium", Some(0.9))
        });
        assert_eq!(*seen.borrow(), MAX_PROMPT_CHARS);
    }
}
