//! Live Jev backend (`decide bench --live`): sends each case's captured
//! question + features to the configured Jev provider (same credentials as
//! `decide ping`, needs `AGENTFLARE_JEV=1`). Costs real money, so it is never
//! used by CI or tests.
//!
//! Failure modes are *simulated* here, not provoked: forcing down / timeout /
//! 402 / low-confidence skips the network and returns the synthetic failure,
//! so the fail-open path is still exercised without spending anything.
use super::{Backend, Case, Failure, FailureMode, Reply};
use crate::decide::{self, Answer, Response};
use std::time::{Duration, Instant};

pub struct LiveBackend {
    timeout: Duration,
    forced: Option<FailureMode>,
}

impl LiveBackend {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            forced: None,
        }
    }
}

pub(super) fn live_reply(case: &Case, response: Response, latency_us: u64) -> Option<Reply> {
    let answers = &response.answers;
    let (label, confidence) =
        if serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&case.baseline)
            .is_ok()
        {
            (
                decide::tracking::summary(answers),
                answers
                    .values()
                    .filter_map(Answer::confidence)
                    .reduce(f64::min),
            )
        } else if case.site == "skill_rerank"
            && answers.values().all(|a| matches!(a, Answer::Noul { .. }))
        {
            let candidates = case.features["candidates"].as_array()?;
            let keep = usize::try_from(case.features["keep"].as_u64()?).ok()?;
            if candidates.len() != answers.len() {
                return None;
            }
            let mut picks = Vec::new();
            let mut certainty = 1.0_f64;
            for (i, candidate) in candidates.iter().enumerate() {
                let suffix = format!("s{i}");
                let name = candidate["name"].as_str()?;
                let (_, Answer::Noul { noul }) =
                    answers.iter().find(|(key, _)| key.ends_with(&suffix))?
                else {
                    return None;
                };
                certainty = certainty.min(2.0 * (noul - 0.5).abs());
                if picks.len() < keep
                    && crate::skill_rerank::keep_candidate(
                        *noul,
                        case.baseline.split(',').any(|b| b == name),
                    )
                {
                    picks.push(name);
                }
            }
            (
                if picks.is_empty() {
                    "(none)".into()
                } else {
                    picks.join(",")
                },
                Some(certainty),
            )
        } else {
            let answer = if case.site == "sdd_judge" {
                answers.get("action")
            } else if answers.len() == 1 {
                answers.values().next()
            } else {
                None
            }?;
            let label = decide::capture::label_of(answer);
            (
                case.decision_label(label["summary"].as_str()?)?,
                answer.confidence(),
            )
        };
    Some(Reply {
        label,
        confidence,
        latency_us,
        cost: response.usage.cost,
    })
}

impl Backend for LiveBackend {
    fn name(&self) -> String {
        "live(jev)".into()
    }

    fn synthetic(&self) -> bool {
        false
    }

    fn ask(&mut self, case: &Case, _nth: usize) -> Result<Reply, Failure> {
        let fail = |latency_us| Err(Failure { latency_us });
        match self.forced {
            Some(FailureMode::Down | FailureMode::Http402) => return fail(1000),
            Some(FailureMode::Timeout) => return fail(self.timeout.as_micros() as u64),
            Some(FailureMode::LowConfidence) => {
                return match case.recorded_label() {
                    Some(label) => Ok(Reply {
                        label,
                        confidence: Some(0.05),
                        latency_us: 1000,
                        cost: Some(0.0),
                    }),
                    None => fail(1000),
                };
            }
            None => {}
        }
        let Some(questions) = case.question.as_ref().filter(|q| !q.is_empty()) else {
            return fail(0);
        };
        let t = Instant::now();
        let res = decide::ask_within(&case.features, questions, self.timeout);
        let latency_us = t.elapsed().as_micros() as u64;
        let Ok(out) = res else {
            return fail(latency_us);
        };
        live_reply(case, out.response, latency_us).ok_or(Failure { latency_us })
    }

    fn force(&mut self, mode: Option<FailureMode>) {
        self.forced = mode;
    }

    fn cold_calls(&self) -> usize {
        3
    }
}
