//! Retry policy execution: backoff strategies with optional jitter.
//!
//! Ported from SMG `wfaas` engine.rs backoff helpers (Apache-2.0), extended
//! with a jitter factor and retryability predicates. All strategies are
//! lightweight local implementations — randomization is handled uniformly
//! by `apply_jitter` rather than a strategy-specific crate.

use std::time::Duration;

use rand::Rng;

use crate::types::{BackoffStrategy, WorkflowError};

/// Multiplier applied to the exponential strategy's interval on each step.
const EXPONENTIAL_MULTIPLIER: f64 = 1.5;

/// Whether an error should be retried, given the policy.
pub trait Retryable {
    fn is_retryable(&self) -> bool;
}

impl Retryable for WorkflowError {
    fn is_retryable(&self) -> bool {
        !matches!(
            self,
            WorkflowError::ShuttingDown | WorkflowError::NotFound(_)
        )
    }
}

/// A backoff sequence. Call `next()` for each delay; `reset()` starts over.
#[derive(Debug, Clone)]
pub enum Backoff {
    Fixed {
        delay: Duration,
    },
    Exponential {
        current: Duration,
        max: Duration,
    },
    Linear {
        current: Duration,
        increment: Duration,
        max: Duration,
    },
    /// An already-computed delay (used when a strategy resolves to a constant).
    Ready(Duration),
}

impl Backoff {
    pub fn from_strategy(strategy: &BackoffStrategy) -> Self {
        match strategy {
            BackoffStrategy::Fixed(delay) => Backoff::Fixed { delay: *delay },
            BackoffStrategy::Exponential { base, max } => Backoff::Exponential {
                current: *base,
                max: *max,
            },
            BackoffStrategy::Linear { increment, max } => Backoff::Linear {
                current: *increment,
                increment: *increment,
                max: *max,
            },
        }
    }

    /// Next delay, applying an optional jitter factor (0.0-1.0).
    pub fn next(&mut self, jitter: f64) -> Option<Duration> {
        let base = match self {
            Backoff::Fixed { delay } => Some(*delay),
            Backoff::Exponential { current, max } => {
                let next = *current;
                let scaled = current.mul_f64(EXPONENTIAL_MULTIPLIER);
                *current = scaled.min(*max);
                Some(next)
            }
            Backoff::Linear {
                current,
                increment,
                max,
            } => {
                let next = *current;
                *current = (*current + *increment).min(*max);
                Some(next)
            }
            Backoff::Ready(delay) => Some(*delay),
        };
        base.map(|d| apply_jitter(d, jitter))
    }
}

/// Jitter a delay by up to `factor` (0.0 = none, 1.0 = up to ±100%).
pub fn apply_jitter(delay: Duration, factor: f64) -> Duration {
    if factor <= 0.0 {
        return delay;
    }
    let factor = factor.clamp(0.0, 1.0);
    let nanos = delay.as_nanos() as f64;
    let delta = nanos * factor * rand::thread_rng().gen_range(-1.0f64..1.0f64);
    let jittered = (nanos + delta).max(0.0) as u64;
    Duration::from_nanos(jittered)
}

/// Maximum attempts honoring `RetryIndefinitely` (`None` max = unbounded).
pub fn effective_max_attempts(policy_max: u32, retry_indefinitely: bool) -> u32 {
    if retry_indefinitely {
        u32::MAX
    } else {
        policy_max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_returns_constant() {
        let mut b = Backoff::from_strategy(&BackoffStrategy::Fixed(Duration::from_secs(2)));
        assert_eq!(b.next(0.0), Some(Duration::from_secs(2)));
        assert_eq!(b.next(0.0), Some(Duration::from_secs(2)));
    }

    #[test]
    fn linear_caps_at_max() {
        let mut b = Backoff::from_strategy(&BackoffStrategy::Linear {
            increment: Duration::from_secs(1),
            max: Duration::from_secs(3),
        });
        assert_eq!(b.next(0.0), Some(Duration::from_secs(1)));
        assert_eq!(b.next(0.0), Some(Duration::from_secs(2)));
        assert_eq!(b.next(0.0), Some(Duration::from_secs(3)));
        assert_eq!(b.next(0.0), Some(Duration::from_secs(3)));
    }

    #[test]
    fn exponential_grows() {
        let mut b = Backoff::from_strategy(&BackoffStrategy::Exponential {
            base: Duration::from_secs(1),
            max: Duration::from_secs(64),
        });
        assert_eq!(b.next(0.0), Some(Duration::from_secs(1)));
        assert_eq!(b.next(0.0), Some(Duration::from_millis(1_500)));
        assert_eq!(b.next(0.0), Some(Duration::from_millis(2_250)));
        // Never exceeds `max`, even after many iterations.
        for _ in 0..20 {
            let d = b.next(0.0).unwrap();
            assert!(d <= Duration::from_secs(64));
        }
    }

    #[test]
    fn jitter_bounds_within_factor() {
        let delay = Duration::from_secs(10);
        for _ in 0..100 {
            let j = apply_jitter(delay, 0.5);
            assert!(j <= Duration::from_secs(15));
            assert!(j >= Duration::from_secs(5));
        }
    }

    #[test]
    fn zero_jitter_identity() {
        assert_eq!(
            apply_jitter(Duration::from_secs(10), 0.0),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn retryable_errors_exclude_terminal() {
        assert!(
            WorkflowError::StepFailed {
                step_id: crate::StepId::new("s"),
                message: "x".into()
            }
            .is_retryable()
        );
        assert!(!WorkflowError::ShuttingDown.is_retryable());
    }
}
