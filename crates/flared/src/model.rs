use serde::{Deserialize, Serialize};

/// Fingerprint recorded when a lease is taken. A PID alone is never trusted:
/// PIDs are recycled by every OS, so a kill is only valid while the live
/// process still matches this fingerprint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub exe_name: String,
    /// Process start time, seconds since the unix epoch.
    pub start_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    Agents,
    Browsers,
    Terminals,
    Desktop,
    Build,
    Services,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub cmd: String,
    pub start_time: u64,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub bucket: Bucket,
    pub protected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub pid: u32,
    pub class: String,
    /// Seconds since the unix epoch. Heartbeats reset this.
    pub created_at: u64,
    pub ttl_seconds: u64,
    pub identity: Identity,
    pub allow_kill: bool,
}

impl Lease {
    pub fn expires_at(&self) -> u64 {
        // Saturate: a huge client-supplied TTL must mean "never expires",
        // not wrap into an immediately-expired (killable) lease.
        self.created_at.saturating_add(self.ttl_seconds)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    KillTree,
    Review,
    PruneRegistryEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Owned lease, identity verified: eligible for automatic execution.
    Safe,
    /// Never executed automatically; surfaced for a human.
    ReviewOnly,
    /// Recorded but refused even with --execute.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub kind: ActionKind,
    pub target: String,
    pub reason: String,
    pub pid: Option<u32>,
    pub risk: Risk,
}

/// A heuristic orphan detection. Findings are report-only; they never become
/// automatic kills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub pid: u32,
    pub name: String,
    pub reason: String,
}

/// True when the live process still matches the recorded fingerprint.
/// Exe names compare case-insensitively (Windows reports mixed case), and
/// start times may drift by rounding, so `tolerance_secs` bounds the gap.
pub fn identity_matches(
    identity: &Identity,
    live_name: &str,
    live_start_time: u64,
    tolerance_secs: u64,
) -> bool {
    identity.exe_name.eq_ignore_ascii_case(live_name)
        && identity.start_time.abs_diff(live_start_time) <= tolerance_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str, start: u64) -> Identity {
        Identity { exe_name: name.into(), start_time: start }
    }

    #[test]
    fn matches_same_name_and_start_time() {
        assert!(identity_matches(&identity("lean-ctx.exe", 1000), "lean-ctx.exe", 1000, 2));
    }

    #[test]
    fn matches_case_insensitively() {
        assert!(identity_matches(&identity("Lean-Ctx.EXE", 1000), "lean-ctx.exe", 1000, 2));
    }

    #[test]
    fn matches_start_time_within_tolerance() {
        assert!(identity_matches(&identity("a.exe", 1000), "a.exe", 1002, 2));
        assert!(identity_matches(&identity("a.exe", 1002), "a.exe", 1000, 2));
    }

    #[test]
    fn rejects_start_time_beyond_tolerance() {
        assert!(!identity_matches(&identity("a.exe", 1000), "a.exe", 1003, 2));
    }

    #[test]
    fn huge_ttl_saturates_instead_of_wrapping() {
        let lease = Lease {
            id: "l".into(),
            pid: 1,
            class: "a".into(),
            created_at: 100,
            ttl_seconds: u64::MAX,
            identity: identity("a.exe", 1),
            allow_kill: true,
        };
        assert_eq!(lease.expires_at(), u64::MAX);
    }

    #[test]
    fn zero_tolerance_requires_exact_start_time() {
        assert!(identity_matches(&identity("a.exe", 1000), "a.exe", 1000, 0));
        assert!(!identity_matches(&identity("a.exe", 1000), "a.exe", 1001, 0));
    }

    #[test]
    fn rejects_different_exe_name_even_with_same_pid_slot() {
        // The PID-reuse case: same pid, different program now running there.
        assert!(!identity_matches(&identity("lean-ctx.exe", 1000), "spotify.exe", 1000, 2));
    }
}
