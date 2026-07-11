//! Shared circuit-breaker state for downstream backend connections
//! (`McpStdioBackend`, `McpHttpBackend`). The exact same logic was about to
//! be duplicated start-to-finish between the two backends' connect paths;
//! extracted once instead of pasted twice.

use crate::error::GatewayError;
use std::time::{Duration, Instant};

/// Consecutive discover()/call() failures before the circuit opens.
pub const CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
/// How long the circuit stays open before allowing one probe attempt
/// through.
pub const CIRCUIT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct CircuitState {
    consecutive_failures: u32,
    opened_until: Option<Instant>,
}

pub struct CircuitBreaker {
    state: tokio::sync::Mutex<CircuitState>,
    failure_threshold: u32,
    recovery_timeout: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, recovery_timeout: Duration) -> Self {
        Self {
            state: tokio::sync::Mutex::new(CircuitState::default()),
            failure_threshold,
            recovery_timeout,
        }
    }

    /// Fast-fails without attempting a connection if the circuit is open.
    /// `backend_name` is only used to identify the backend in the error
    /// message (the command string for stdio, the URL for HTTP) — the
    /// breaker itself doesn't know or care what kind of backend it guards.
    pub async fn check(&self, backend_name: &str) -> Result<(), GatewayError> {
        let state = self.state.lock().await;
        if let Some(until) = state.opened_until {
            let now = Instant::now();
            if now < until {
                return Err(GatewayError::CircuitOpen(format!(
                    "backend '{backend_name}' circuit open after {} consecutive failures — retry in {:?}",
                    state.consecutive_failures,
                    until.saturating_duration_since(now)
                )));
            }
        }
        Ok(())
    }

    pub async fn record_success(&self) {
        let mut state = self.state.lock().await;
        state.consecutive_failures = 0;
        state.opened_until = None;
    }

    pub async fn record_failure(&self) {
        let mut state = self.state.lock().await;
        state.consecutive_failures += 1;
        if state.consecutive_failures >= self.failure_threshold {
            state.opened_until = Some(Instant::now() + self.recovery_timeout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_calls_while_under_threshold() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));
        cb.check("x").await.unwrap();
        cb.record_failure().await;
        cb.record_failure().await;
        cb.check("x").await.unwrap();
    }

    #[tokio::test]
    async fn opens_after_threshold_consecutive_failures() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));
        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_failure().await;
        let err = cb.check("x").await.unwrap_err();
        assert!(matches!(err, GatewayError::CircuitOpen(_)));
    }

    #[tokio::test]
    async fn success_resets_the_failure_count() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));
        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_success().await;
        cb.record_failure().await;
        cb.record_failure().await;
        cb.check("x").await.unwrap();
    }

    #[tokio::test]
    async fn allows_a_probe_after_the_recovery_window_passes() {
        let cb = CircuitBreaker::new(3, Duration::from_millis(50));
        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_failure().await;
        cb.check("x").await.unwrap_err();
        tokio::time::sleep(Duration::from_millis(80)).await;
        cb.check("x").await.unwrap();
    }
}
