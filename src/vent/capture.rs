use std::io::Write;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct VentLine {
    pub event_id: String,
    pub ts: String,
    #[serde(default)]
    pub session: Option<String>,
    pub severity: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub message: String,
}

pub fn append(
    log_path: &Path,
    session: Option<&str>,
    severity: &str,
    tags: &[String],
    message: &str,
) -> std::io::Result<String> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let id = crate::vent::event_id(message);
    let line = VentLine {
        event_id: id.clone(),
        ts: chrono::Utc::now().to_rfc3339(),
        session: session.map(str::to_string),
        severity: severity.to_string(),
        tags: tags.to_vec(),
        message: message.to_string(),
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(id)
}

pub const THROTTLE_WINDOW_SECS: i64 = 300;
pub const THROTTLE_MAX_PER_WINDOW: usize = 1;

fn recent_count_for_topic(
    log_path: &Path,
    topic_key: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> usize {
    let Ok(text) = std::fs::read_to_string(log_path) else {
        return 0;
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<VentLine>(l).ok())
        .filter(|v| crate::vent::classify::topic_key(&v.message) == topic_key)
        .filter(|v| {
            chrono::DateTime::parse_from_rfc3339(&v.ts)
                .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() < THROTTLE_WINDOW_SECS)
                .unwrap_or(false)
        })
        .count()
}

/// Classifies origin, picks the destination log (global agentflare-core vs.
/// per-repo), throttles repeat vents of the same topic within
/// `THROTTLE_WINDOW_SECS`, and appends if not throttled. Returns
/// `(event_id, suppressed)` — `suppressed = true` means nothing was written.
pub fn append_routed(
    session: Option<&str>,
    severity: &str,
    tags: &[String],
    message: &str,
) -> std::io::Result<(String, bool)> {
    let log_path = if crate::vent::classify::origin(message) == "agentflare-core" {
        crate::vent::paths::global_log_path()
    } else {
        crate::vent::paths::log_path()
    };
    let topic_key = crate::vent::classify::topic_key(message);
    let now = chrono::Utc::now();
    if recent_count_for_topic(&log_path, &topic_key, now) >= THROTTLE_MAX_PER_WINDOW {
        return Ok((crate::vent::event_id(message), true));
    }
    let id = append(&log_path, session, severity, tags, message)?;
    Ok((id, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_line_at(log: &std::path::Path, ts: &str, severity: &str, message: &str) {
        let line = VentLine {
            event_id: crate::vent::event_id(message),
            ts: ts.to_string(),
            session: None,
            severity: severity.to_string(),
            tags: vec![],
            message: message.to_string(),
        };
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        writeln!(f, "{}", serde_json::to_string(&line).unwrap()).unwrap();
    }

    #[test]
    fn recent_count_ignores_lines_outside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("v.jsonl");
        write_line_at(&log, "2020-01-01T00:00:00Z", "high", "an old repeated failure");
        let now = chrono::Utc::now();
        let key = crate::vent::classify::topic_key("an old repeated failure");
        assert_eq!(recent_count_for_topic(&log, &key, now), 0);
    }

    #[test]
    fn recent_count_includes_lines_inside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("v.jsonl");
        let now = chrono::Utc::now();
        write_line_at(&log, &now.to_rfc3339(), "high", "a fresh repeated failure");
        let key = crate::vent::classify::topic_key("a fresh repeated failure");
        assert_eq!(recent_count_for_topic(&log, &key, now), 1);
    }

    #[test]
    fn append_routed_suppresses_second_identical_call_within_window() {
        crate::paths::test_support::with_temp_home(|| {
            let (_, suppressed1) =
                append_routed(None, "high", &[], "the exact same af-guard bug").unwrap();
            let (_, suppressed2) =
                append_routed(None, "high", &[], "the exact same af-guard bug").unwrap();
            assert!(!suppressed1, "first occurrence must be captured");
            assert!(suppressed2, "second identical vent within the window must be throttled");
        });
    }

    #[test]
    fn append_routed_sends_agentflare_core_and_external_to_different_logs() {
        crate::paths::test_support::with_temp_home(|| {
            append_routed(None, "high", &[], "af-guard blocked a protected branch push").unwrap();
            append_routed(None, "high", &[], "a totally unrelated bash quoting bug").unwrap();
            let global_text = std::fs::read_to_string(crate::vent::paths::global_log_path())
                .unwrap_or_default();
            let repo_text = std::fs::read_to_string(crate::vent::paths::log_path())
                .unwrap_or_default();
            assert!(global_text.contains("af-guard"));
            assert!(!repo_text.contains("af-guard"));
            assert!(repo_text.contains("bash quoting bug"));
        });
    }

    #[test]
    fn append_writes_one_parseable_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("v.jsonl");
        let id1 = append(&log, Some("s1"), "high", &["dx".into()], "boom").unwrap();
        let _id2 = append(&log, None, "medium", &[], "again").unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: VentLine = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first.event_id, id1);
        assert_eq!(first.severity, "high");
        assert_eq!(first.message, "boom");
        assert_eq!(first.tags, vec!["dx".to_string()]);
    }
}
