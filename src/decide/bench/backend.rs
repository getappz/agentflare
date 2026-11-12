//! Jev backends for the bench. The fake one never touches the network and
//! uses a *virtual* clock (latency is reported, not slept), so runs are fast
//! and exactly reproducible.
use super::Case;

#[derive(Debug, Clone)]
pub struct Reply {
    pub label: String,
    pub confidence: Option<f64>,
    pub latency_us: u64,
    pub cost: f64,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub latency_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureMode {
    Down,
    Timeout,
    Http402,
    LowConfidence,
}

impl FailureMode {
    pub const ALL: [Self; 4] = [
        Self::Down,
        Self::Timeout,
        Self::Http402,
        Self::LowConfidence,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Timeout => "timeout",
            Self::Http402 => "402",
            Self::LowConfidence => "low_confidence",
        }
    }
}

pub trait Backend {
    fn name(&self) -> String;
    /// True when numbers come from simulated answers/latency, so they
    /// measure the harness, not the sites (reports are stamped accordingly).
    fn synthetic(&self) -> bool {
        true
    }
    /// `nth` is the call index within the current set (0 = first).
    fn ask(&mut self, case: &Case, nth: usize) -> Result<Reply, Failure>;
    /// Force every following call into a failure mode (`None` = normal).
    fn force(&mut self, mode: Option<FailureMode>);
    /// Calls with `nth < cold_calls` count as cold for latency reporting.
    fn cold_calls(&self) -> usize;
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub cold_ms: u64,
    pub warm_ms: u64,
    /// Uniform extra latency in `0..=jitter_ms`, seeded.
    pub jitter_ms: u64,
    pub cold_calls: usize,
    pub timeout_ms: u64,
    pub cost_per_decision: f64,
    pub seed: u64,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            cold_ms: 1200,
            warm_ms: 350,
            jitter_ms: 100,
            cold_calls: 3,
            timeout_ms: 3000,
            cost_per_decision: 0.00001,
            seed: 7,
        }
    }
}

/// Replays each case's recorded Jev label with a seeded virtual latency.
pub struct FakeBackend {
    pub profile: Profile,
    forced: Option<FailureMode>,
}

impl FakeBackend {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            forced: None,
        }
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl Backend for FakeBackend {
    fn name(&self) -> String {
        format!("fake(seed={})", self.profile.seed)
    }

    fn ask(&mut self, case: &Case, nth: usize) -> Result<Reply, Failure> {
        let p = &self.profile;
        let base = if nth < p.cold_calls {
            p.cold_ms
        } else {
            p.warm_ms
        };
        let jitter = if p.jitter_ms == 0 {
            0
        } else {
            let site = case
                .site
                .bytes()
                .fold(0u64, |a, b| a.wrapping_mul(31) ^ u64::from(b));
            splitmix(p.seed ^ site ^ (nth as u64).wrapping_mul(0x1000_0001)) % (p.jitter_ms + 1)
        };
        let latency_us = (base + jitter) * 1000;
        match self.forced {
            Some(FailureMode::Down) => return Err(Failure { latency_us: 1000 }),
            Some(FailureMode::Timeout) => {
                return Err(Failure {
                    latency_us: p.timeout_ms * 1000,
                });
            }
            Some(FailureMode::Http402) => return Err(Failure { latency_us }),
            _ => {}
        }
        let Some(label) = case.recorded_label() else {
            return Err(Failure { latency_us: 1000 });
        };
        let confidence = if self.forced == Some(FailureMode::LowConfidence) {
            Some(0.05)
        } else {
            case.confidence
        };
        Ok(Reply {
            label,
            confidence,
            latency_us,
            cost: p.cost_per_decision,
        })
    }

    fn force(&mut self, mode: Option<FailureMode>) {
        self.forced = mode;
    }

    fn cold_calls(&self) -> usize {
        self.profile.cold_calls
    }
}
