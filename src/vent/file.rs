use crate::vent::capture::VentLine;
use crate::vent::classify::{classify, severity_rank, topic_key};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingGroup {
    pub topic_key: String,
    pub message: String,
    pub severity: String,
    pub seen_count: i64,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct FiledEntry {
    issue_url: String,
    filed_at: i64,
}

/// How long a filed topic stays excluded from `pending_batch` before it's
/// eligible to resurface. Without this, a bug that regresses after its issue
/// is closed (or never actually gets fixed) would be hidden forever.
pub const FILED_TTL_SECS: i64 = 30 * 24 * 60 * 60;

fn read_all_lines(log_path: &Path) -> Vec<VentLine> {
    let Ok(text) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<VentLine>(l).ok())
        .collect()
}

fn load_filed(filed_path: &Path) -> BTreeMap<String, FiledEntry> {
    let Ok(text) = std::fs::read_to_string(filed_path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Writes via a sibling temp file + rename so a process killed mid-write
/// can never leave `filed_path` holding a truncated/corrupt JSON document.
fn save_filed(filed_path: &Path, filed: &BTreeMap<String, FiledEntry>) -> std::io::Result<()> {
    if let Some(parent) = filed_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = filed_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(filed)?)?;
    std::fs::rename(&tmp_path, filed_path)
}

/// Groups the global agentflare-core log by topic, applies the same
/// actionability rule the per-project pipeline uses, and excludes anything
/// recorded in `filed_path` less than `FILED_TTL_SECS` ago — a topic filed
/// longer than that resurfaces so a recurring or unfixed bug isn't hidden
/// forever behind a single old issue.
pub fn pending_batch(log_path: &Path, filed_path: &Path, now: i64) -> Vec<PendingGroup> {
    let lines = read_all_lines(log_path);
    let filed = load_filed(filed_path);
    let mut groups: BTreeMap<String, PendingGroup> = BTreeMap::new();
    for l in &lines {
        let key = topic_key(&l.message);
        let g = groups.entry(key.clone()).or_insert_with(|| PendingGroup {
            topic_key: key.clone(),
            message: l.message.clone(),
            severity: l.severity.clone(),
            seen_count: 0,
        });
        g.seen_count += 1;
        g.message = l.message.clone();
        if severity_rank(&l.severity) > severity_rank(&g.severity) {
            g.severity = l.severity.clone();
        }
    }
    groups
        .into_values()
        .filter(|g| classify(&g.severity, g.seen_count, &g.message))
        .filter(|g| match filed.get(&g.topic_key) {
            Some(entry) => now - entry.filed_at >= FILED_TTL_SECS,
            None => true,
        })
        .collect()
}

/// Records every topic in `topic_keys` as filed under `issue_url`. All-or-
/// nothing at the call-site level: callers only invoke this after a confirmed
/// issue URL comes back from `create_github_issue`.
pub fn mark_filed(
    filed_path: &Path,
    topic_keys: &[String],
    issue_url: &str,
    filed_at: i64,
) -> std::io::Result<()> {
    let mut filed = load_filed(filed_path);
    for k in topic_keys {
        filed.insert(
            k.clone(),
            FiledEntry {
                issue_url: issue_url.to_string(),
                filed_at,
            },
        );
    }
    save_filed(filed_path, &filed)
}

const GH_ISSUE_CREATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Shells out to `gh issue create`. Not unit tested (matches this repo's existing
/// convention for subprocess calls) — exercised at the integration/manual level.
/// Bounded by `GH_ISSUE_CREATE_TIMEOUT` so a hung `gh` (network stall, stuck
/// auth prompt) can't block the calling MCP tool call/CLI invocation forever.
pub fn create_github_issue(title: &str, body: &str) -> std::io::Result<String> {
    let title = title.to_string();
    let body = body.to_string();
    let output = crate::ipc::process::run_with_timeout(
        move || {
            flare_process::command("gh")
                .args([
                    "issue",
                    "create",
                    "--repo",
                    "getappz/agentflare",
                    "--title",
                    &title,
                    "--body",
                    &body,
                ])
                .output()
        },
        GH_ISSUE_CREATE_TIMEOUT,
    )
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("gh issue create did not finish within {GH_ISSUE_CREATE_TIMEOUT:?}"),
        )
    })??;
    if !output.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vent::capture::VentLine;
    use std::io::Write;

    fn write_lines(log: &Path, msgs: &[(&str, &str)]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        for (sev, m) in msgs {
            let line = VentLine {
                event_id: crate::vent::event_id(m),
                ts: "2026-01-01T00:00:00Z".into(),
                session: None,
                severity: (*sev).to_string(),
                tags: vec![],
                message: (*m).to_string(),
            };
            writeln!(f, "{}", serde_json::to_string(&line).unwrap()).unwrap();
        }
    }

    #[test]
    fn pending_batch_groups_by_topic_and_tracks_seen_count() {
        let dir = tempfile::tempdir().unwrap();
        let (log, filed) = (dir.path().join("v.jsonl"), dir.path().join("v.filed.json"));
        write_lines(
            &log,
            &[
                ("high", "af-guard blocked something"),
                ("high", "af-guard blocked something"),
                ("high", "a totally different bug"),
            ],
        );
        let batch = pending_batch(&log, &filed, 0);
        assert_eq!(batch.len(), 2, "two distinct topics");
        let guard_group = batch
            .iter()
            .find(|g| g.message.contains("af-guard"))
            .unwrap();
        assert_eq!(guard_group.seen_count, 2);
    }

    #[test]
    fn pending_batch_excludes_non_actionable_noise() {
        let dir = tempfile::tempdir().unwrap();
        let (log, filed) = (dir.path().join("v.jsonl"), dir.path().join("v.filed.json"));
        write_lines(&log, &[("low", "just a calm note")]);
        assert!(pending_batch(&log, &filed, 0).is_empty());
    }

    #[test]
    fn mark_filed_removes_group_from_next_pending_batch() {
        let dir = tempfile::tempdir().unwrap();
        let (log, filed) = (dir.path().join("v.jsonl"), dir.path().join("v.filed.json"));
        write_lines(&log, &[("high", "af-guard blocked something")]);
        let now = 1_700_000_000;
        let batch = pending_batch(&log, &filed, now);
        assert_eq!(batch.len(), 1);
        let keys: Vec<String> = batch.iter().map(|g| g.topic_key.clone()).collect();
        mark_filed(
            &filed,
            &keys,
            "https://github.com/getappz/agentflare/issues/1",
            now,
        )
        .unwrap();
        assert!(
            pending_batch(&log, &filed, now + 60).is_empty(),
            "already-filed topic must not reappear within the TTL window"
        );
    }

    #[test]
    fn pending_batch_allows_refiling_after_ttl_expires() {
        let dir = tempfile::tempdir().unwrap();
        let (log, filed) = (dir.path().join("v.jsonl"), dir.path().join("v.filed.json"));
        write_lines(&log, &[("high", "af-guard blocked something")]);
        let filed_at = 1_700_000_000;
        let keys = vec![topic_key("af-guard blocked something")];
        mark_filed(
            &filed,
            &keys,
            "https://github.com/getappz/agentflare/issues/1",
            filed_at,
        )
        .unwrap();
        assert!(
            pending_batch(&log, &filed, filed_at + FILED_TTL_SECS - 1).is_empty(),
            "still within the TTL window"
        );
        let batch = pending_batch(&log, &filed, filed_at + FILED_TTL_SECS + 1);
        assert_eq!(batch.len(), 1, "must resurface once the TTL has elapsed");
    }
}
