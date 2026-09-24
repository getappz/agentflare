//! Process-wide cached policy — a background thread samples signals on a
//! fixed cadence and stores the decision; callers get a cheap read instead
//! of paying the CPU-sample cost (which sleeps ~ms) on every dispatch
//! decision.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use crate::config::{GateConfig, GateMode};
use crate::policy::{self, Policy};
use crate::signals::Signals;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

static STATE: OnceLock<RwLock<Policy>> = OnceLock::new();

/// Serializes the sampler thread against [`force_resume`]/[`clear_force_resume`]
/// so a slower writer can't clobber a faster one's result (e.g. the 30s
/// sampler tick overwriting a just-applied `force_resume()` with a stale
/// decision it started computing beforehand). Deliberately separate from
/// `STATE`'s `RwLock`: `sample_policy` sleeps ~`MINIMUM_CPU_UPDATE_INTERVAL`
/// (~250ms) inside `Signals::sample()`, and `STATE`'s write guard must never
/// be held across that sleep or `current_policy()`'s `.read()` — meant to be
/// cheap on every dispatch decision — would block behind it too.
static WRITER_LOCK: Mutex<()> = Mutex::new(());

/// Set by [`force_resume`], cleared by [`clear_force_resume`]. Persists
/// across sampler ticks (unlike a one-shot `STATE` write) so a
/// `AGENTFLARE_DISPATCH_GATE_MODE=off` value baked into the daemon's
/// environment doesn't re-pause dispatch 30s after a forced resume — see
/// `sample_policy`.
static FORCE_RESUME: AtomicBool = AtomicBool::new(false);

/// Samples signals and decides the policy, applying the `force_resume`
/// override when active. The override only ever turns a `GateMode::Off`
/// misconfiguration into `GateMode::Auto` — it never bypasses genuine CPU
/// pressure (`PauseReason::CpuPressure`), which is exactly what the gate
/// exists to protect against.
fn sample_policy(cfg: &GateConfig) -> Policy {
    let mut effective = *cfg;
    if FORCE_RESUME.load(Ordering::SeqCst) && matches!(effective.mode, GateMode::Off) {
        effective.mode = GateMode::Auto;
    }
    policy::decide(&Signals::sample(), &effective)
}

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
    let initial = sample_policy(&cfg);
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
                let _writer = WRITER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                let decision = sample_policy(&cfg);
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

/// Force-unpause a gate stuck on `AGENTFLARE_DISPATCH_GATE_MODE=off` (item
/// #643) without a full daemon restart, which alone doesn't clear it since
/// `init_global` just re-reads the same stuck env var on every startup.
/// Applies immediately — doesn't wait for the next `SAMPLE_INTERVAL` tick —
/// and the override persists until [`clear_force_resume`] is called. A
/// no-op on a gate paused for `PauseReason::CpuPressure` instead: that
/// reason self-clears once CPU drops, and isn't what this override targets.
pub fn force_resume() {
    set_force_resume(true);
}

/// Restores normal `AGENTFLARE_DISPATCH_GATE_MODE` handling after
/// [`force_resume`]. Mostly for tests/symmetry today — there's no CLI path
/// that re-pauses a gate, so nothing currently calls this in production.
pub fn clear_force_resume() {
    set_force_resume(false);
}

/// Shared body for [`force_resume`]/[`clear_force_resume`]: takes the
/// `WRITER_LOCK` so a concurrent sampler tick can't race this update, flips
/// the flag, then re-samples and applies the result immediately rather than
/// waiting for the next `SAMPLE_INTERVAL` tick.
fn set_force_resume(active: bool) {
    let _writer = WRITER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    FORCE_RESUME.store(active, Ordering::SeqCst);
    if let Some(lock) = STATE.get() {
        let cfg = GateConfig::from_env();
        let decision = sample_policy(&cfg);
        *lock.write().unwrap_or_else(|e| e.into_inner()) = decision;
    }
}

/// Whether [`force_resume`]'s override is currently active.
pub fn force_resume_active() -> bool {
    FORCE_RESUME.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DEFAULT_CPU_BUSY_PCT, DEFAULT_CPU_SEVERE_PCT};

    fn cfg(mode: GateMode) -> GateConfig {
        GateConfig {
            mode,
            cpu_busy_threshold_pct: DEFAULT_CPU_BUSY_PCT,
            cpu_severe_pct: DEFAULT_CPU_SEVERE_PCT,
        }
    }

    /// One test, not three — `FORCE_RESUME` is a process-wide static, and
    /// `cargo test` runs test fns on separate threads by default, so
    /// splitting these across tests would race on the same flag.
    #[test]
    fn force_resume_flag_overrides_off_mode_until_cleared() {
        assert!(!force_resume_active());
        assert_eq!(
            sample_policy(&cfg(GateMode::Off)).pause_reason(),
            Some(crate::policy::PauseReason::UserDisabled)
        );

        FORCE_RESUME.store(true, Ordering::SeqCst);
        assert!(force_resume_active());
        assert_ne!(
            sample_policy(&cfg(GateMode::Off)).pause_reason(),
            Some(crate::policy::PauseReason::UserDisabled)
        );

        FORCE_RESUME.store(false, Ordering::SeqCst);
        assert!(!force_resume_active());
        assert_eq!(
            sample_policy(&cfg(GateMode::Off)).pause_reason(),
            Some(crate::policy::PauseReason::UserDisabled)
        );
    }
}
