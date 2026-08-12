//! Process-wide cached policy — a background thread samples signals on a
//! fixed cadence and stores the decision; callers get a cheap read instead
//! of paying the CPU-sample cost (which sleeps ~ms) on every dispatch
//! decision.

use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use crate::config::GateConfig;
use crate::policy::{self, Policy};
use crate::signals::Signals;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

static STATE: OnceLock<RwLock<Policy>> = OnceLock::new();

/// Starts the background sampler thread. Idempotent — safe to call more
/// than once (e.g. from multiple test setups in the same process); only
/// the first call spawns a thread. Does a synchronous first sample before
/// returning, so `current_policy()` is meaningful immediately rather than
/// defaulting to a guess for the first `SAMPLE_INTERVAL`.
pub fn init_global() {
    if STATE.get().is_some() {
        return;
    }
    let cfg = GateConfig::from_env();
    let initial = policy::decide(&Signals::sample(), &cfg);
    if STATE.set(RwLock::new(initial)).is_err() {
        // Lost an initialization race with another thread — that thread's
        // sampler is already running, nothing more to do.
        return;
    }
    std::thread::Builder::new()
        .name("agentflare-resource-gate-sampler".into())
        .spawn(move || {
            loop {
                std::thread::sleep(SAMPLE_INTERVAL);
                let decision = policy::decide(&Signals::sample(), &cfg);
                if let Some(lock) = STATE.get() {
                    *lock.write().unwrap_or_else(|e| e.into_inner()) = decision;
                }
            }
        })
        .expect("failed to spawn agentflare-resource-gate sampler thread");
}

/// Cheap read of the last-sampled policy. Returns `Policy::Normal` if
/// `init_global()` was never called — callers that skip initialization get
/// the least-surprising default (dispatch proceeds) rather than a silent
/// permanent throttle.
pub fn current_policy() -> Policy {
    STATE
        .get()
        .map(|lock| *lock.read().unwrap_or_else(|e| e.into_inner()))
        .unwrap_or(Policy::Normal)
}
