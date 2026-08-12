//! Host signals: CPU pressure, deployment mode.
//!
//! Sampled on a fixed cadence by [`crate::gate`]; this file just captures
//! one snapshot at a time.
//!
//! Battery/AC-power probing (openhuman's `scheduler_gate` also samples
//! those) is deliberately deferred: agentflare's dispatch model runs mostly
//! on always-on dev boxes/servers, and pulling in `starship-battery` adds a
//! real (if thin) FFI dependency chain on macOS specifically (mach2,
//! core-foundation, objc2-core-foundation, objc2-io-kit, nix). Add it later
//! behind a new `Signals` field + `PauseReason` variant if it turns out to
//! matter — no API break, `decide()`'s `match` already has to be touched
//! per new signal.

use std::path::Path;

use sysinfo::System;

#[derive(Debug, Clone, Copy)]
pub struct Signals {
    /// Recent global CPU usage, 0..100.
    pub cpu_usage_pct: f32,
    pub server_mode: bool,
}

impl Signals {
    /// Sample once. Not free — the CPU reading sleeps ~`MINIMUM_CPU_UPDATE_INTERVAL`
    /// to get a real delta — but cheap enough (ms-scale) to call from a
    /// background sampler thread on a multi-second cadence, never from a
    /// hot path.
    pub fn sample() -> Self {
        Self {
            cpu_usage_pct: sample_cpu(),
            server_mode: detect_server_mode(),
        }
    }
}

// ---- cpu -----------------------------------------------------------------

fn sample_cpu() -> f32 {
    // Build a *fresh* `System` every sample instead of reusing a long-lived
    // one. sysinfo's Linux CPU refresh builds a per-core Vec sized to the
    // `cpuN` lines in /proc/stat on its first refresh, then indexes that Vec
    // by line position on every later refresh. If the visible core count
    // later grows (CPU hotplug, or a host rebalancing vCPUs at runtime), a
    // process-wide System captured the boot-time core count and the next
    // refresh indexes past the Vec and panics. Building per call means both
    // refreshes below always see the current core count.
    //
    // Two refreshes spaced ~MINIMUM_CPU_UPDATE_INTERVAL apart give sysinfo a
    // real delta to compute usage from; we only read the global aggregate, so
    // not retaining per-core state across calls costs us nothing.
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL + std::time::Duration::from_millis(50));
    sys.refresh_cpu_usage();
    sys.global_cpu_usage()
}

// ---- deployment mode -----------------------------------------------------

fn detect_server_mode() -> bool {
    if let Ok(v) = std::env::var("AGENTFLARE_DEPLOYMENT") {
        if v.eq_ignore_ascii_case("server") {
            return true;
        }
        if matches!(v.to_ascii_lowercase().as_str(), "desktop" | "laptop") {
            return false;
        }
    }
    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
        return true;
    }
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    // Heuristic of last resort: a Linux box with no display server set is
    // likely headless (server or CI), though this alone is weaker than the
    // openhuman original (which also required "no battery present" —
    // deferred here along with the rest of battery probing).
    if cfg!(target_os = "linux")
        && std::env::var("DISPLAY").is_err()
        && std::env::var("WAYLAND_DISPLAY").is_err()
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sample_cpu` must always yield a finite percentage in `0..=100`, and
    /// must not panic — the regression guard for a class of bug where a
    /// long-lived `System` panicked with an out-of-bounds index after the
    /// host's visible core count grew. A fresh `System` per call keeps the
    /// per-core Vec sized to the current core count.
    #[test]
    fn sample_cpu_is_finite_and_bounded() {
        let pct = sample_cpu();
        assert!(pct.is_finite(), "cpu usage should be finite, got {pct}");
        assert!(
            (0.0..=100.0).contains(&pct),
            "cpu usage out of range: {pct}"
        );
    }

    /// Full snapshot smoke: `Signals::sample()` returns well-formed values and
    /// never panics through the CPU path.
    #[test]
    fn signals_sample_smoke() {
        let s = Signals::sample();
        assert!(s.cpu_usage_pct.is_finite());
        assert!((0.0..=100.0).contains(&s.cpu_usage_pct));
    }
}
