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

#[cfg(test)]
mod tests {
    use super::*;

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
