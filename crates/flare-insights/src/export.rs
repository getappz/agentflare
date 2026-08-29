use crate::model::Session;

#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Json,
    Jsonl,
    Html,
    Deepeval,
    OpenAiEvals,
}

pub fn export_sessions(
    sessions: &[Session],
    turns: &[crate::model::Turn],
    format: ExportFormat,
) -> String {
    match format {
        ExportFormat::Json => serde_json::to_string_pretty(&(sessions, turns)).unwrap_or_default(),
        ExportFormat::Jsonl => turns
            .iter()
            .map(|t| serde_json::to_string(t).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n"),
        ExportFormat::Html => export_html(sessions, turns),
        ExportFormat::Deepeval => serde_json::to_string_pretty(&serde_json::json!({
            "sessions": sessions,
            "turns": turns,
            "exported_at": chrono::Utc::now().to_rfc3339(),
            "format": "deepeval"
        }))
        .unwrap_or_default(),
        ExportFormat::OpenAiEvals => turns
            .iter()
            .map(|t| serde_json::json!({"input": t.user_text, "ideal": t.assistant_text}))
            .map(|v| serde_json::to_string(&v).unwrap())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn export_html(sessions: &[Session], turns: &[crate::model::Turn]) -> String {
    let mut h = String::from("<html><body><h1>Agent Sessions Export</h1>");
    for s in sessions {
        h.push_str(&format!("<h2>{} ({})</h2>", s.id, s.source.as_str()));
        h.push_str(&format!(
            "<p>Project: {} | Model: {}</p>",
            s.project,
            s.model.as_deref().unwrap_or("-")
        ));
    }
    for t in turns {
        h.push_str(&format!(
            "<h3>Turn {}</h3><pre>{}</pre>",
            t.seq,
            t.assistant_text.as_deref().unwrap_or("")
        ));
    }
    h.push_str("</body></html>");
    h
}

#[derive(Debug, Clone)]
pub struct BundleMeta {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub session_count: usize,
    pub turn_count: usize,
}
