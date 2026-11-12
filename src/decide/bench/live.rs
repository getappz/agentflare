//! Live Jev backend (`decide bench --live`): sends each case's captured
//! question + features to the configured Jev provider (same credentials as
//! `decide ping`, needs `AGENTFLARE_JEV=1`). Costs real money, so it is never
//! used by CI or tests.
//!
//! Failure modes are *simulated* here, not provoked: forcing down / timeout /
//! 402 / low-confidence skips the network and returns the synthetic failure,
//! so the fail-open path is still exercised without spending anything.
use super::{Backend, Case, Failure, FailureMode, Reply};
use crate::decide::{self, Answer, Question};
use std::collections::BTreeMap;
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

fn label_of(a: &Answer) -> String {
    match a {
        Answer::Choice { choice, .. } => choice.clone(),
        other => other.summary(),
    }
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
                        cost: 0.0,
                    }),
                    None => fail(1000),
                };
            }
            None => {}
        }
        // A case without its typed question cannot be replayed: counts as a
        // failure (baseline wins), so captured rows from before the question
        // was stored lower the Jev-applied share rather than error out.
        let Some(questions) = case
            .question
            .clone()
            .and_then(|q| serde_json::from_value::<BTreeMap<String, Question>>(q).ok())
        else {
            return fail(0);
        };
        let t = Instant::now();
        let res = decide::ask_within(&case.features, &questions, self.timeout);
        let latency_us = t.elapsed().as_micros() as u64;
        let Ok(out) = res else {
            return fail(latency_us);
        };
        let Some(answer) = out.response.answers.values().next() else {
            return fail(latency_us);
        };
        Ok(Reply {
            label: label_of(answer),
            confidence: answer.confidence(),
            latency_us,
            cost: out.response.usage.cost.unwrap_or(0.0),
        })
    }

    fn force(&mut self, mode: Option<FailureMode>) {
        self.forced = mode;
    }

    fn cold_calls(&self) -> usize {
        3
    }
}
