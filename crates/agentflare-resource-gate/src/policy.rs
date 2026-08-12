//! Decision logic — turn raw [`Signals`] + config into a [`Policy`].

use crate::config::{DEFAULT_CPU_BUSY_PCT, DEFAULT_CPU_SEVERE_PCT, GateConfig, GateMode};
use crate::signals::Signals;

/// Why the gate is currently paused. Carried by [`Policy::Paused`] so
/// downstream consumers (logging, a future dashboard tile) can surface a
/// specific reason instead of a generic "paused" label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// User explicitly turned the gate off (`AGENTFLARE_DISPATCH_GATE_MODE=off`).
    UserDisabled,
    /// CPU pressure exceeded the gate's severe ceiling.
    CpuPressure,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserDisabled => "user_disabled",
            Self::CpuPressure => "cpu_pressure",
        }
    }
}

/// Dispatch-throttling tier. Independent of `auth_db`'s per-agent
/// rate-limit cooldown — both gates must pass before a dispatch proceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Aggressive,
    Normal,
    Throttled,
    Paused { reason: PauseReason },
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aggressive => "aggressive",
            Self::Normal => "normal",
            Self::Throttled => "throttled",
            Self::Paused { .. } => "paused",
        }
    }

    /// `Some(reason)` when paused, `None` otherwise.
    pub fn pause_reason(self) -> Option<PauseReason> {
        match self {
            Self::Paused { reason } => Some(reason),
            _ => None,
        }
    }

    /// Whether a dispatch decision point should hold off this tick.
    /// `Throttled` and `Paused` both skip for v1 — the discovery tick's own
    /// fixed interval is the retry mechanism, same as an `is_cooling_down`
    /// skip; no separate backoff/permit machinery needed.
    pub fn blocks_dispatch(self) -> bool {
        matches!(self, Self::Throttled | Self::Paused { .. })
    }
}

/// Compute the current [`Policy`] from sampled signals + config.
///
/// Order of evaluation matters — explicit user override wins first, then
/// deployment mode, then the CPU severity ceiling, then the busy threshold.
pub fn decide(signals: &Signals, cfg: &GateConfig) -> Policy {
    match cfg.mode {
        GateMode::Off => {
            return Policy::Paused {
                reason: PauseReason::UserDisabled,
            };
        }
        GateMode::AlwaysOn => return Policy::Aggressive,
        GateMode::Auto => {}
    }

    if signals.server_mode {
        return Policy::Aggressive;
    }

    // Clamp config-supplied thresholds so a malformed override can't
    // silently disable or force-throttle dispatch.
    let mut cpu_threshold = cfg.cpu_busy_threshold_pct.clamp(0.0, 100.0);
    let mut cpu_severe = cfg.cpu_severe_pct.clamp(0.0, 100.0);
    // Range-clamping each value independently still admits an inverted pair
    // (busy >= severe), which collapses the tier ladder: every CPU reading
    // above `severe` pauses before it can ever be judged merely busy, so
    // `Throttled` becomes unreachable. `GateConfig`'s fields are public, so
    // this has to be enforced here rather than only in `from_env`.
    if cpu_threshold >= cpu_severe {
        cpu_threshold = DEFAULT_CPU_BUSY_PCT;
        cpu_severe = DEFAULT_CPU_SEVERE_PCT;
    }

    if signals.cpu_usage_pct >= cpu_severe {
        return Policy::Paused {
            reason: PauseReason::CpuPressure,
        };
    }

    if signals.cpu_usage_pct <= cpu_threshold {
        Policy::Normal
    } else {
        Policy::Throttled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: GateMode) -> GateConfig {
        GateConfig {
            mode,
            cpu_busy_threshold_pct: 70.0,
            cpu_severe_pct: 95.0,
        }
    }

    fn signals(cpu: f32, server: bool) -> Signals {
        Signals {
            cpu_usage_pct: cpu,
            server_mode: server,
        }
    }

    #[test]
    fn off_mode_pauses() {
        let p = decide(&signals(5.0, false), &cfg(GateMode::Off));
        assert_eq!(
            p,
            Policy::Paused {
                reason: PauseReason::UserDisabled
            }
        );
    }

    #[test]
    fn pause_reason_helper_returns_none_for_non_paused() {
        assert_eq!(Policy::Aggressive.pause_reason(), None);
        assert_eq!(Policy::Normal.pause_reason(), None);
        assert_eq!(Policy::Throttled.pause_reason(), None);
    }

    #[test]
    fn pause_reason_as_str_round_trips_each_variant() {
        assert_eq!(PauseReason::UserDisabled.as_str(), "user_disabled");
        assert_eq!(PauseReason::CpuPressure.as_str(), "cpu_pressure");
    }

    #[test]
    fn always_on_overrides_signals() {
        // CPU pegged, desktop — still Aggressive.
        let p = decide(&signals(99.0, false), &cfg(GateMode::AlwaysOn));
        assert_eq!(p, Policy::Aggressive);
    }

    #[test]
    fn server_mode_is_aggressive() {
        let p = decide(&signals(50.0, true), &cfg(GateMode::Auto));
        assert_eq!(p, Policy::Aggressive);
    }

    #[test]
    fn idle_is_normal() {
        let p = decide(&signals(20.0, false), &cfg(GateMode::Auto));
        assert_eq!(p, Policy::Normal);
    }

    #[test]
    fn busy_cpu_throttles() {
        let p = decide(&signals(90.0, false), &cfg(GateMode::Auto));
        assert_eq!(p, Policy::Throttled);
    }

    #[test]
    fn cpu_severe_pauses_on_pressure() {
        let mut c = cfg(GateMode::Auto);
        c.cpu_severe_pct = 90.0;
        let p = decide(&signals(96.0, false), &c);
        assert_eq!(
            p,
            Policy::Paused {
                reason: PauseReason::CpuPressure
            }
        );
    }

    #[test]
    fn cpu_just_below_severe_throttles_not_pauses() {
        let mut c = cfg(GateMode::Auto);
        c.cpu_busy_threshold_pct = 70.0;
        c.cpu_severe_pct = 95.0;
        let p = decide(&signals(80.0, false), &c);
        assert_eq!(p, Policy::Throttled);
    }

    #[test]
    fn cpu_severe_recovers_to_normal() {
        let mut c = cfg(GateMode::Auto);
        c.cpu_severe_pct = 90.0;
        assert!(matches!(
            decide(&signals(99.0, false), &c),
            Policy::Paused {
                reason: PauseReason::CpuPressure
            }
        ));
        assert_eq!(decide(&signals(5.0, false), &c), Policy::Normal);
    }

    #[test]
    fn out_of_range_cpu_severe_pct_is_clamped() {
        // 200.0 clamped to 100.0 — only true 100% CPU triggers pause.
        let mut c = cfg(GateMode::Auto);
        c.cpu_severe_pct = 200.0;
        let p = decide(&signals(99.9, false), &c);
        assert_eq!(p, Policy::Throttled);
        // A negative severe threshold clamps to 0.0, which puts it at or
        // below the busy threshold — an inverted pair. Rather than let that
        // pause dispatch forever on any CPU above zero (the exact
        // force-throttle failure the clamping exists to prevent), the
        // ordering guard recovers the default pair.
        c.cpu_severe_pct = -10.0;
        assert_eq!(decide(&signals(0.5, false), &c), Policy::Normal);
    }

    /// An inverted pair (busy >= severe) would otherwise collapse the tier
    /// ladder — everything above `severe` pauses before it can be judged
    /// merely busy, making `Throttled` unreachable. Both fields are public,
    /// so `decide` must defend itself rather than trust `from_env`.
    #[test]
    fn inverted_thresholds_fall_back_to_defaults_and_keep_all_three_tiers() {
        let mut c = cfg(GateMode::Auto);
        c.cpu_busy_threshold_pct = 95.0;
        c.cpu_severe_pct = 80.0;

        // Below the default busy threshold — Normal, not Paused.
        assert_eq!(decide(&signals(10.0, false), &c), Policy::Normal);
        // Between the default busy and severe thresholds — Throttled stays
        // reachable, which is the whole point of the fallback.
        assert_eq!(decide(&signals(85.0, false), &c), Policy::Throttled);
        // At/above the default severe threshold — Paused.
        assert_eq!(
            decide(&signals(99.0, false), &c),
            Policy::Paused {
                reason: PauseReason::CpuPressure
            }
        );
    }

    #[test]
    fn equal_thresholds_also_fall_back_rather_than_erasing_throttled() {
        let mut c = cfg(GateMode::Auto);
        c.cpu_busy_threshold_pct = 60.0;
        c.cpu_severe_pct = 60.0;
        // Without the fallback this would pause; with it, 85 lands in the
        // default Throttled band.
        assert_eq!(decide(&signals(85.0, false), &c), Policy::Throttled);
        assert_eq!(
            decide(&signals(DEFAULT_CPU_SEVERE_PCT, false), &c),
            Policy::Paused {
                reason: PauseReason::CpuPressure
            }
        );
        assert_eq!(
            decide(&signals(DEFAULT_CPU_BUSY_PCT - 1.0, false), &c),
            Policy::Normal
        );
    }

    #[test]
    fn server_mode_overrides_pause_signals() {
        let mut c = cfg(GateMode::Auto);
        c.cpu_severe_pct = 50.0;
        let p = decide(&signals(99.0, true), &c);
        assert_eq!(p, Policy::Aggressive);
    }

    #[test]
    fn blocks_dispatch_matches_throttled_and_paused_only() {
        assert!(!Policy::Aggressive.blocks_dispatch());
        assert!(!Policy::Normal.blocks_dispatch());
        assert!(Policy::Throttled.blocks_dispatch());
        assert!(
            Policy::Paused {
                reason: PauseReason::CpuPressure
            }
            .blocks_dispatch()
        );
    }
}
