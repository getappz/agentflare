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
    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    let mut h = String::from("<html><body><h1>Agent Sessions Export</h1>");
    for s in sessions {
        h.push_str(&format!(
            "<h2>{} ({})</h2>",
            escape(&s.id),
            escape(s.source.as_str())
        ));
        h.push_str(&format!(
            "<p>Project: {} | Model: {}</p>",
            escape(&s.project),
            escape(s.model.as_deref().unwrap_or("-"))
        ));
    }
    for t in turns {
        h.push_str(&format!(
            "<h3>Turn {}</h3><pre>{}</pre>",
            t.seq,
            escape(t.assistant_text.as_deref().unwrap_or(""))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escapes_session_fields_and_assistant_text() {
        let payload = "</pre><script>alert('x' & \"y\")</script>";
        let session: Session = serde_json::from_value(serde_json::json!({
            "id": payload, "source": {"unknown": payload}, "project": payload,
            "model": payload, "status": "completed", "updated_at": "2026-01-01T00:00:00Z",
            "tokens": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "reasoning": 0},
            "turn_count": 1, "tool_call_count": 0, "subagent_count": 0,
            "tags": [], "starred": false
        }))
        .unwrap();
        let turn = crate::model::Turn {
            id: "t".into(),
            session_id: payload.into(),
            seq: 1,
            user_text: None,
            assistant_text: Some(payload.into()),
            started_at: None,
            ended_at: None,
            tokens: None,
            cost_usd: None,
        };
        let html = export_sessions(&[session], &[turn], ExportFormat::Html);
        assert!(!html.contains(payload));
        assert_eq!(
            html.matches("&lt;/pre&gt;&lt;script&gt;alert('x' &amp; \"y\")&lt;/script&gt;")
                .count(),
            5
        );
    }
}
