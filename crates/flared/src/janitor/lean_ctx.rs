//! Stale-entry check for lean-ctx's `agents/registry.json` — the source of
//! the "408 active agents" dashboard bug: MCP instances register on start and
//! are never reaped. An entry counts as live only when its PID exists AND the
//! process name matches AND the process start time agrees with the recorded
//! `started_at` (PID-reuse guard).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::ProcInfo;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaleEntry {
    pub agent_id: String,
    pub pid: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryReport {
    pub total: usize,
    pub live: usize,
    pub stale: Vec<StaleEntry>,
}

/// Parse the registry and classify every entry as live or stale.
/// `expected_exe` is a substring the live process name must contain
/// (case-insensitive), e.g. "lean-ctx".
pub fn check_registry(
    path: &Path,
    procs: &HashMap<u32, ProcInfo>,
    expected_exe: &str,
    tolerance_secs: u64,
) -> eyre::Result<RegistryReport> {
    let text = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let agents = value
        .get("agents")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();

    let expected = expected_exe.to_ascii_lowercase();
    let mut live = 0usize;
    let mut stale = Vec::new();
    for entry in &agents {
        let agent_id = entry
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>")
            .to_string();
        let raw_pid = entry.get("pid").and_then(|v| v.as_u64());
        let Some(pid) = raw_pid.and_then(|v| u32::try_from(v).ok()) else {
            stale.push(StaleEntry {
                agent_id,
                pid: 0,
                reason: format!("missing or out-of-range pid ({raw_pid:?})"),
            });
            continue;
        };
        let recorded_start = entry
            .get("started_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp().max(0) as u64);

        // Identity requires ALL of: pid alive, expected exe name, and a
        // recorded start time that agrees. Missing/unparseable started_at is
        // stale — falling through as "live" would bypass the PID-reuse guard.
        let stale_reason = match procs.get(&pid) {
            None => Some(format!("pid {pid} is not running")),
            Some(p) if !p.name.to_ascii_lowercase().contains(&expected) => {
                Some(format!("pid {pid} was reused by '{}'", p.name))
            }
            Some(p) => match recorded_start {
                None => Some("missing or unparseable started_at".to_string()),
                Some(t) if p.start_time.abs_diff(t) > tolerance_secs => Some(format!(
                    "pid {pid} start time {} does not match recorded started_at {t}",
                    p.start_time
                )),
                Some(_) => None,
            },
        };
        match stale_reason {
            Some(reason) => stale.push(StaleEntry { agent_id, pid, reason }),
            None => live += 1,
        }
    }
    Ok(RegistryReport { total: agents.len(), live, stale })
}

/// Remove the stale entries named in `report`, keeping everything else and
/// all unrelated top-level keys intact. Writes `<file>.bak-<unix-ts>` first
/// and returns the backup path.
pub fn prune_registry(path: &Path, report: &RegistryReport) -> eyre::Result<PathBuf> {
    let text = std::fs::read_to_string(path)?;
    let mut value: serde_json::Value = serde_json::from_str(&text)?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = PathBuf::from(format!("{}.bak-{ts}", path.display()));
    std::fs::write(&backup, &text)?;

    // Remove by (agent_id, pid) pair, not id alone: a fresh registration that
    // happened to reuse an id between check and prune keeps its entry unless
    // the pid also matches the stale record.
    let stale_keys: std::collections::HashSet<(&str, u64)> =
        report.stale.iter().map(|s| (s.agent_id.as_str(), s.pid as u64)).collect();
    if let Some(agents) = value.get_mut("agents").and_then(|a| a.as_array_mut()) {
        agents.retain(|entry| {
            let id = entry.get("agent_id").and_then(|v| v.as_str());
            let pid = entry.get("pid").and_then(|v| v.as_u64());
            match (id, pid) {
                (Some(id), Some(pid)) => !stale_keys.contains(&(id, pid)),
                // Entries with a broken pid were reported with pid 0.
                (Some(id), None) => !stale_keys.contains(&(id, 0)),
                _ => true,
            }
        });
    }

    let file_name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(&value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Bucket;
    use pretty_assertions::assert_eq;

    fn proc(pid: u32, name: &str, start: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid: Some(1),
            name: name.into(),
            cmd: name.into(),
            start_time: start,
            cpu_pct: 0.0,
            rss_bytes: 0,
            bucket: Bucket::Agents,
            protected: false,
        }
    }

    /// 2000-01-01T00:00:00Z = 946684800 unix.
    const T0: u64 = 946684800;

    fn registry_json() -> String {
        // Three entries: pid 100 live-and-matching, pid 200 gone,
        // pid 300 reused by an unrelated exe.
        format!(
            r#"{{
  "agents": [
    {{"agent_id": "mcp-100", "agent_type": "mcp", "role": "coder",
      "project_root": "C:/w/a", "started_at": "2000-01-01T00:00:00Z",
      "last_active": "2000-01-01T00:00:00Z", "pid": 100,
      "status": "Active", "status_message": null}},
    {{"agent_id": "mcp-200", "agent_type": "mcp", "role": "coder",
      "project_root": "C:/w/b", "started_at": "2000-01-01T00:00:00Z",
      "last_active": "2000-01-01T00:00:00Z", "pid": 200,
      "status": "Active", "status_message": null}},
    {{"agent_id": "mcp-300", "agent_type": "mcp", "role": "coder",
      "project_root": "C:/w/c", "started_at": "2000-01-01T00:00:00Z",
      "last_active": "2000-01-01T00:00:00Z", "pid": 300,
      "status": "Active", "status_message": null}}
  ],
  "scratchpad": [],
  "updated_at": "2000-01-02T00:00:00.000000000Z"
}}"#
        )
    }

    fn write_registry(dir: &tempfile::TempDir) -> PathBuf {
        let path = dir.path().join("registry.json");
        std::fs::write(&path, registry_json()).unwrap();
        path
    }

    fn live_procs() -> HashMap<u32, ProcInfo> {
        [
            (100, proc(100, "lean-ctx.exe", T0)),
            (300, proc(300, "spotify.exe", T0 + 90000)),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn classifies_live_dead_and_reused_pids() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_registry(&dir);
        let report = check_registry(&path, &live_procs(), "lean-ctx", 5).unwrap();
        assert_eq!(report.total, 3);
        assert_eq!(report.live, 1);
        let stale_ids: Vec<&str> = report.stale.iter().map(|s| s.agent_id.as_str()).collect();
        assert_eq!(stale_ids, vec!["mcp-200", "mcp-300"]);
    }

    #[test]
    fn start_time_mismatch_counts_as_stale() {
        // Right exe name, but the process started much later than the
        // registry claims -> recycled pid slot reused by a NEW lean-ctx.
        let dir = tempfile::tempdir().unwrap();
        let path = write_registry(&dir);
        let procs: HashMap<u32, ProcInfo> =
            [(100, proc(100, "lean-ctx.exe", T0 + 3600))].into_iter().collect();
        let report = check_registry(&path, &procs, "lean-ctx", 5).unwrap();
        assert_eq!(report.live, 0);
        assert_eq!(report.stale.len(), 3);
    }

    #[test]
    fn out_of_range_pid_and_missing_started_at_are_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        // Entry 1: pid exceeds u32 (would previously wrap). Entry 2: live pid
        // + right name, but no started_at (would previously pass as live).
        std::fs::write(
            &path,
            r#"{"agents": [
                {"agent_id": "a-big", "pid": 4294967297, "started_at": "2000-01-01T00:00:00Z", "status": "Active"},
                {"agent_id": "a-nostart", "pid": 100, "status": "Active"}
            ]}"#,
        )
        .unwrap();
        let procs: HashMap<u32, ProcInfo> =
            [(100, proc(100, "lean-ctx.exe", T0))].into_iter().collect();
        let report = check_registry(&path, &procs, "lean-ctx", 5).unwrap();
        assert_eq!(report.live, 0);
        assert_eq!(report.stale.len(), 2);
    }

    #[test]
    fn prune_removes_stale_keeps_live_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_registry(&dir);
        let report = check_registry(&path, &live_procs(), "lean-ctx", 5).unwrap();
        let backup = prune_registry(&path, &report).unwrap();

        assert!(backup.exists(), "backup must be written before mutation");
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let agents = after["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["agent_id"], "mcp-100");
        // Unrelated top-level keys survive.
        assert!(after.get("scratchpad").is_some());
        assert!(after.get("updated_at").is_some());

        let before: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
        assert_eq!(before["agents"].as_array().unwrap().len(), 3);
    }
}
