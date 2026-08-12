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

    #[test]
    fn parse_f32_env_falls_back_on_missing_or_unparseable() {
        assert_eq!(
            parse_f32_env("AGENTFLARE_TEST_DOES_NOT_EXIST_XYZ", 42.0),
            42.0
        );
    }
}
