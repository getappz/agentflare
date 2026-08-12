//! Gate configuration — env-var overrides today, a `[dispatch_gate]` block
//! in `~/.agentflare/config.toml` once epic #331's loader exists (don't
//! block this on that epic landing first).

/// Shared by [`GateConfig::from_env`] and by `policy::decide`'s
/// inverted-threshold fallback, so both recover to the same sane pair.
pub const DEFAULT_CPU_BUSY_PCT: f32 = 80.0;
pub const DEFAULT_CPU_SEVERE_PCT: f32 = 95.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateMode {
    /// Sample signals and decide a tier dynamically (the default).
    Auto,
    /// Bypass every throttle — always `Policy::Aggressive`.
    AlwaysOn,
    /// Never let host-gated work through — always `Policy::Paused(UserDisabled)`.
    Off,
}

#[derive(Debug, Clone, Copy)]
pub struct GateConfig {
    pub mode: GateMode,
    /// CPU usage at/above this throttles (0..100).
    pub cpu_busy_threshold_pct: f32,
    /// CPU usage at/above this pauses outright — the host is unusable, a
    /// throttled backoff isn't enough (0..100).
    pub cpu_severe_pct: f32,
}

impl GateConfig {
    pub fn from_env() -> Self {
        Self {
            mode: parse_mode(
                std::env::var("AGENTFLARE_DISPATCH_GATE_MODE")
                    .ok()
                    .as_deref(),
            ),
            cpu_busy_threshold_pct: parse_f32_env(
                "AGENTFLARE_DISPATCH_GATE_CPU_BUSY_PCT",
                DEFAULT_CPU_BUSY_PCT,
            ),
            cpu_severe_pct: parse_f32_env(
                "AGENTFLARE_DISPATCH_GATE_CPU_SEVERE_PCT",
                DEFAULT_CPU_SEVERE_PCT,
            ),
        }
        .normalized()
    }

    /// Force the thresholds into a usable shape: each into `0..=100`, and
    /// the pair into a real ordering.
    ///
    /// Range-clamping alone still admits an inverted pair (busy >= severe),
    /// which collapses the tier ladder — every reading at/above `severe`
    /// pauses before it can be judged merely busy, so `Throttled` becomes
    /// unreachable. Applied at the env boundary so `from_env` never returns
    /// a config that misrepresents what the gate will actually do, and
    /// again in `policy::decide` because these fields are public and a
    /// caller can hand-build a `GateConfig` that never passed through here.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.cpu_busy_threshold_pct = self.cpu_busy_threshold_pct.clamp(0.0, 100.0);
        self.cpu_severe_pct = self.cpu_severe_pct.clamp(0.0, 100.0);
        if self.cpu_busy_threshold_pct >= self.cpu_severe_pct {
            self.cpu_busy_threshold_pct = DEFAULT_CPU_BUSY_PCT;
            self.cpu_severe_pct = DEFAULT_CPU_SEVERE_PCT;
        }
        self
    }
}

fn parse_mode(raw: Option<&str>) -> GateMode {
    match raw.map(str::to_ascii_lowercase).as_deref() {
        Some("always_on") | Some("always-on") | Some("on") => GateMode::AlwaysOn,
        Some("off") => GateMode::Off,
        _ => GateMode::Auto,
    }
}

fn parse_f32_env(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_defaults_to_auto_on_garbage_or_absent() {
        assert_eq!(parse_mode(None), GateMode::Auto);
        assert_eq!(parse_mode(Some("garbage")), GateMode::Auto);
    }

    #[test]
    fn parse_mode_recognizes_both_spellings_of_always_on() {
        assert_eq!(parse_mode(Some("always_on")), GateMode::AlwaysOn);
        assert_eq!(parse_mode(Some("ALWAYS-ON")), GateMode::AlwaysOn);
        assert_eq!(parse_mode(Some("off")), GateMode::Off);
    }

    /// The env boundary must not hand back a config whose thresholds
    /// disagree with what the gate will actually enforce — `policy::decide`
    /// re-normalizes defensively, but anything else reading these fields
    /// (a log line, a future dashboard tile) would otherwise report an
    /// inverted pair as if it were live.
    #[test]
    fn normalized_recovers_defaults_from_an_inverted_or_equal_pair() {
        let inverted = GateConfig {
            mode: GateMode::Auto,
            cpu_busy_threshold_pct: 95.0,
            cpu_severe_pct: 80.0,
        }
        .normalized();
        assert_eq!(inverted.cpu_busy_threshold_pct, DEFAULT_CPU_BUSY_PCT);
        assert_eq!(inverted.cpu_severe_pct, DEFAULT_CPU_SEVERE_PCT);

        let equal = GateConfig {
            mode: GateMode::Auto,
            cpu_busy_threshold_pct: 60.0,
            cpu_severe_pct: 60.0,
        }
        .normalized();
        assert_eq!(equal.cpu_busy_threshold_pct, DEFAULT_CPU_BUSY_PCT);
        assert_eq!(equal.cpu_severe_pct, DEFAULT_CPU_SEVERE_PCT);
    }

    #[test]
    fn normalized_clamps_out_of_range_values_and_leaves_a_sane_pair_alone() {
        let clamped = GateConfig {
            mode: GateMode::Auto,
            cpu_busy_threshold_pct: -10.0,
            cpu_severe_pct: 200.0,
        }
        .normalized();
        assert_eq!(clamped.cpu_busy_threshold_pct, 0.0);
        assert_eq!(clamped.cpu_severe_pct, 100.0);

        let sane = GateConfig {
            mode: GateMode::Auto,
            cpu_busy_threshold_pct: 70.0,
            cpu_severe_pct: 90.0,
        };
        let after = sane.normalized();
        assert_eq!(after.cpu_busy_threshold_pct, 70.0);
        assert_eq!(after.cpu_severe_pct, 90.0);
        // Idempotent — `decide` re-normalizes every call.
        let twice = after.normalized();
        assert_eq!(twice.cpu_busy_threshold_pct, 70.0);
        assert_eq!(twice.cpu_severe_pct, 90.0);
    }

    #[test]
    fn parse_f32_env_falls_back_on_missing_or_unparseable() {
        assert_eq!(
            parse_f32_env("AGENTFLARE_TEST_DOES_NOT_EXIST_XYZ", 42.0),
            42.0
        );
    }
}
