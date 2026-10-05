//! Rerank the skills injected on each prompt with Jev. The BM25/embedding
//! ranking proposes up to `TOP_N` candidates; Jev answers one `noul` per
//! candidate ("is this skill relevant to the task?"), so irrelevant skills stop
//! costing tokens on every turn.
//!
//! Off unless `AGENTFLARE_JEV_SKILLS` is set (the prompt leaves the machine):
//! - `shadow`: inject the existing top picks unchanged, log Jev's picks beside
//!   them (site `skill_rerank`; see `agentflare decide report`);
//! - `apply`: inject Jev's picks.
//!
//! Where Jev is unsure (probability near 0.5) the existing ranking decides, and
//! any failure falls back to the existing picks.
use crate::decide::{self, Answer, DecideError, Outcome, Question, shadow};
use crate::skill_detect::RankedSkill;
use std::collections::BTreeMap;
use std::time::Duration;

const SITE: &str = "skill_rerank";
/// Candidates sent to Jev per prompt.
pub const TOP_N: usize = 8;
const KEEP_AT: f64 = 0.65;
const DROP_AT: f64 = 0.35;
const MAX_PROMPT_CHARS: usize = 1500;
const MAX_DESCRIPTION_CHARS: usize = 200;
/// Well inside the UserPromptSubmit hook's wall-clock budget.
const BUDGET: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Off,
    Shadow,
    Apply,
}

fn mode() -> Mode {
    match std::env::var("AGENTFLARE_JEV_SKILLS").as_deref() {
        Ok("shadow") => Mode::Shadow,
        Ok("apply") => Mode::Apply,
        _ => Mode::Off,
    }
}

/// How many candidates the caller should fetch: more than it will inject, but
/// only when reranking is on, so the default path is untouched.
pub fn fetch_limit(inject: usize) -> usize {
    if mode() == Mode::Off {
        inject
    } else {
        TOP_N.max(inject)
    }
}

/// The skills to inject: at most `keep` of `ranked` (best first).
pub fn pick(prompt: &str, ranked: Vec<RankedSkill>, keep: usize) -> Vec<RankedSkill> {
    let baseline: Vec<RankedSkill> = ranked.iter().take(keep).cloned().collect();
    pick_with_baseline(prompt, ranked, baseline, keep)
}

/// Like [`pick`], but the shadow baseline is the caller's original top-`keep`
/// set instead of the first `keep` of the expanded candidates. Hook callers
/// that fetched an expanded candidate set pass the separately fetched
/// unexpanded result here so shadow mode logs (and injects) the true default.
pub fn pick_with_baseline(
    prompt: &str,
    candidates: Vec<RankedSkill>,
    baseline: Vec<RankedSkill>,
    keep: usize,
) -> Vec<RankedSkill> {
    pick_with_baseline_inner(
        prompt,
        candidates,
        baseline,
        keep,
        mode(),
        &|state, qs| decide::ask_within(&serde_json::Value::from(state), qs, BUDGET),
        &shadow::record,
    )
}

/// The backend call, injectable for tests.
type AskFn<'a> = dyn Fn(&str, &BTreeMap<String, Question>) -> Result<Outcome, DecideError> + 'a;

fn question_for(skill: &RankedSkill) -> Question {
    let description: String = skill
        .description
        .chars()
        .take(MAX_DESCRIPTION_CHARS)
        .collect();
    Question::noul(&format!(
        "Would the skill \"{}\" ({description}) help with the task described in the state?",
        skill.name
    ))
}

fn names(skills: &[RankedSkill]) -> String {
    if skills.is_empty() {
        return "(none)".to_string();
    }
    skills
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
fn pick_with(
    prompt: &str,
    ranked: Vec<RankedSkill>,
    keep: usize,
    mode: Mode,
    ask: &AskFn<'_>,
    record: &dyn Fn(&shadow::Row),
) -> Vec<RankedSkill> {
    let baseline: Vec<RankedSkill> = ranked.iter().take(keep).cloned().collect();
    pick_with_baseline_inner(prompt, ranked, baseline, keep, mode, ask, record)
}

fn pick_with_baseline_inner(
    prompt: &str,
    candidates: Vec<RankedSkill>,
    baseline: Vec<RankedSkill>,
    keep: usize,
    mode: Mode,
    ask: &AskFn<'_>,
    record: &dyn Fn(&shadow::Row),
) -> Vec<RankedSkill> {
    let prompt = prompt.trim();
    if mode == Mode::Off || candidates.is_empty() || prompt.is_empty() || prompt.starts_with('/') {
        return baseline.into_iter().take(keep).collect();
    }
    let prompt: String = prompt.chars().take(MAX_PROMPT_CHARS).collect();
    let candidates = &candidates[..candidates.len().min(TOP_N)];
    let questions: BTreeMap<String, Question> = candidates
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("s{i}"), question_for(s)))
        .collect();
    let baseline_names = names(&baseline);
    let outcome = match ask(&prompt, &questions) {
        Ok(outcome) => outcome,
        // Not configured: behave exactly as if reranking were off, silently.
        Err(DecideError::Disabled | DecideError::NoCredentials) => return baseline,
        Err(e) => {
            record(&shadow::Row::failed(
                SITE,
                &prompt,
                &baseline_names,
                &e.to_string(),
            ));
            return baseline;
        }
    };
    let probabilities: Option<Vec<f64>> = (0..candidates.len())
        .map(|i| match outcome.response.answers.get(&format!("s{i}")) {
            Some(Answer::Noul { noul }) => Some(*noul),
            _ => None,
        })
        .collect();
    let Some(probabilities) = probabilities else {
        record(&shadow::Row::failed(
            SITE,
            &prompt,
            &baseline_names,
            "incomplete answers",
        ));
        return baseline;
    };
    let in_baseline = |s: &RankedSkill| baseline.iter().any(|b| b.name == s.name);
    let picks: Vec<RankedSkill> = candidates
        .iter()
        .zip(&probabilities)
        .filter(|(s, p)| match **p {
            p if p >= KEEP_AT => true,
            p if p <= DROP_AT => false,
            _ => in_baseline(s), // unsure: the existing ranking decides
        })
        .map(|(s, _)| s.clone())
        .take(keep)
        .collect();
    // "Confidence" of the whole call = its least certain candidate.
    let certainty = probabilities
        .iter()
        .map(|p| 2.0 * (p - 0.5).abs())
        .fold(1.0, f64::min);
    record(&shadow::Row::answered(
        SITE,
        &prompt,
        &baseline_names,
        &names(&picks),
        Some(certainty),
        outcome.elapsed_ms,
        outcome.response.usage.cost,
    ));
    match mode {
        Mode::Apply => picks,
        Mode::Shadow | Mode::Off => baseline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::{Response, Usage};
    use std::cell::RefCell;

    fn skill(name: &str) -> RankedSkill {
        RankedSkill {
            est_tokens: 10,
            name: name.to_string(),
            source: "test".to_string(),
            description: format!("{name} description"),
            score: 1.0,
            match_reason: String::new(),
        }
    }

    fn ranked(n: usize) -> Vec<RankedSkill> {
        (0..n).map(|i| skill(&format!("skill{i}"))).collect()
    }

    fn reply(probs: &[f64]) -> Result<Outcome, DecideError> {
        Ok(Outcome {
            response: Response {
                answers: probs
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (format!("s{i}"), Answer::Noul { noul: *p }))
                    .collect(),
                model: None,
                usage: Usage {
                    cost: Some(0.00002),
                    ..Usage::default()
                },
            },
            elapsed_ms: 800,
        })
    }

    fn run(
        mode: Mode,
        skills: Vec<RankedSkill>,
        ask: impl Fn(&str, &BTreeMap<String, Question>) -> Result<Outcome, DecideError>,
    ) -> (Vec<String>, Vec<shadow::Row>) {
        let rows = RefCell::new(vec![]);
        let picked = pick_with("fix the login bug", skills, 3, mode, &ask, &|r| {
            rows.borrow_mut().push(r.clone())
        });
        (
            picked.into_iter().map(|s| s.name).collect(),
            rows.into_inner(),
        )
    }

    #[test]
    fn shadow_injects_the_baseline_and_logs_jevs_different_picks() {
        let (picked, rows) = run(Mode::Shadow, ranked(5), |_, _| {
            reply(&[0.1, 0.9, 0.95, 0.8, 0.2])
        });
        assert_eq!(picked, ["skill0", "skill1", "skill2"]); // unchanged
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].site, "skill_rerank");
        assert_eq!(rows[0].baseline, "skill0,skill1,skill2");
        assert_eq!(rows[0].jev.as_deref(), Some("skill1,skill2,skill3"));
    }

    #[test]
    fn apply_injects_jevs_picks_in_rank_order() {
        let (picked, _) = run(Mode::Apply, ranked(5), |_, _| {
            reply(&[0.1, 0.9, 0.95, 0.8, 0.2])
        });
        assert_eq!(picked, ["skill1", "skill2", "skill3"]);
    }

    #[test]
    fn explicit_baseline_survives_an_expanded_candidate_set() {
        // The expanded fetch reordered the top 3; shadow must still inject
        // and log the original 3-result set.
        let candidates = vec![
            skill("skill3"),
            skill("skill4"),
            skill("skill5"),
            skill("skill0"),
            skill("skill1"),
        ];
        let baseline = vec![skill("skill0"), skill("skill1"), skill("skill2")];
        let rows = RefCell::new(vec![]);
        let picked = pick_with_baseline_inner(
            "fix the login bug",
            candidates,
            baseline,
            3,
            Mode::Shadow,
            &|_, _| reply(&[0.9, 0.9, 0.9, 0.1, 0.1]),
            &|r| rows.borrow_mut().push(r.clone()),
        );
        assert_eq!(
            picked.into_iter().map(|s| s.name).collect::<Vec<_>>(),
            ["skill0", "skill1", "skill2"]
        );
        assert_eq!(rows.borrow().len(), 1);
        assert_eq!(rows.borrow()[0].baseline, "skill0,skill1,skill2");
    }

    #[test]
    fn apply_can_inject_nothing_when_nothing_is_relevant() {
        let (picked, _) = run(Mode::Apply, ranked(4), |_, _| reply(&[0.05, 0.1, 0.0, 0.2]));
        assert!(picked.is_empty());
    }

    #[test]
    fn unsure_answers_defer_to_the_existing_ranking() {
        let (picked, _) = run(Mode::Apply, ranked(5), |_, _| reply(&[0.5; 5]));
        assert_eq!(picked, ["skill0", "skill1", "skill2"]); // baseline members kept
    }

    #[test]
    fn one_question_per_candidate_capped_and_carrying_name_and_description() {
        let seen = RefCell::new(None);
        let _ = run(Mode::Shadow, ranked(12), |state, qs| {
            *seen.borrow_mut() = Some((state.to_string(), qs.len()));
            Err(DecideError::Disabled)
        });
        let (state, n) = seen.into_inner().unwrap();
        assert_eq!(state, "fix the login bug");
        assert_eq!(n, TOP_N);
        let q = serde_json::to_string(&question_for(&skill("zeta"))).unwrap();
        assert!(q.contains("zeta") && q.contains("zeta description") && q.contains("noul"));
    }

    #[test]
    fn long_descriptions_and_prompts_are_truncated() {
        let mut s = skill("big");
        s.description = "d".repeat(MAX_DESCRIPTION_CHARS * 4);
        assert!(
            serde_json::to_string(&question_for(&s)).unwrap().len() < MAX_DESCRIPTION_CHARS * 2
        );
        let seen = RefCell::new(0);
        let long = "p".repeat(MAX_PROMPT_CHARS * 3);
        let _ = pick_with(
            &long,
            ranked(2),
            3,
            Mode::Shadow,
            &|p, _| {
                *seen.borrow_mut() = p.chars().count();
                Err(DecideError::Disabled)
            },
            &|_| {},
        );
        assert_eq!(*seen.borrow(), MAX_PROMPT_CHARS);
    }

    #[test]
    fn off_slash_and_empty_never_hit_the_network() {
        let boom = |_: &str, _: &BTreeMap<String, Question>| -> Result<Outcome, DecideError> {
            panic!("ask must not be called")
        };
        for (mode, prompt) in [
            (Mode::Off, "fix it"),
            (Mode::Apply, "/pm"),
            (Mode::Apply, "  "),
        ] {
            let out = pick_with(prompt, ranked(5), 3, mode, &boom, &|_| {});
            assert_eq!(out.len(), 3);
        }
        assert!(pick_with("x", vec![], 3, Mode::Apply, &boom, &|_| {}).is_empty());
    }

    #[test]
    fn failures_fall_back_to_the_baseline_and_are_logged() {
        let (picked, rows) = run(Mode::Apply, ranked(5), |_, _| {
            Err(DecideError::Status(402, "no credits".to_string()))
        });
        assert_eq!(picked, ["skill0", "skill1", "skill2"]);
        assert!(rows[0].error.as_deref().unwrap().contains("402"));

        let (picked, rows) = run(Mode::Apply, ranked(5), |_, _| reply(&[0.9, 0.9])); // too few answers
        assert_eq!(picked, ["skill0", "skill1", "skill2"]);
        assert_eq!(rows[0].error.as_deref(), Some("incomplete answers"));
    }

    #[test]
    fn not_configured_is_silent() {
        for err in [DecideError::Disabled, DecideError::NoCredentials] {
            let (picked, rows) = run(Mode::Apply, ranked(5), |_, _| match &err {
                DecideError::Disabled => Err(DecideError::Disabled),
                _ => Err(DecideError::NoCredentials),
            });
            assert_eq!(picked.len(), 3);
            assert!(rows.is_empty());
        }
    }

    #[test]
    fn fetch_limit_never_shrinks_below_the_injected_count() {
        assert!(fetch_limit(3) >= 3);
        assert_eq!(fetch_limit(20), 20); // larger than TOP_N: unchanged in every mode
    }
}
