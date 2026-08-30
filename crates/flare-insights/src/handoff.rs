use crate::model::Session;

#[derive(Debug, Clone, Copy)]
pub enum Verbosity {
    Minimal,
    Standard,
    Verbose,
    Full,
}

impl Verbosity {
    pub fn max_turns(self) -> usize {
        match self {
            Self::Minimal => 3,
            Self::Standard => 10,
            Self::Verbose => 20,
            Self::Full => 50,
        }
    }
}

pub fn handoff_doc(
    session: &Session,
    turns: &[crate::model::Turn],
    target: &str,
    verbosity: Verbosity,
) -> String {
    let n = verbosity.max_turns().min(turns.len());
    let slice = &turns[turns.len().saturating_sub(n)..];
    let mut out = String::new();
    out.push_str(&format!("# Handoff: {} → {}\n\n", session.id, target));
    out.push_str(&format!(
        "Source: {} | Project: {} | Model: {}\n\n",
        session.source.as_str(),
        session.project,
        session.model.as_deref().unwrap_or("unknown")
    ));
    out.push_str(&format!(
        "Turns: {} (showing last {})\n\n",
        turns.len(),
        slice.len()
    ));
    for t in slice {
        out.push_str(&format!("## Turn {}\n\n", t.seq));
        if let Some(u) = &t.user_text {
            out.push_str(&format!("**User:** {}\n\n", truncate(u, 4000)));
        }
        if let Some(a) = &t.assistant_text {
            out.push_str(&format!("**Assistant:** {}\n\n", truncate(a, 4000)));
        }
    }
    out.push_str(&format!(
        "\n---\nContinue this session in `{}` by pasting this context.\n",
        target
    ));
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{}…", t)
    }
}

pub fn convert_session_json(
    session: &Session,
    turns: &[crate::model::Turn],
    from: &str,
    to: &str,
) -> serde_json::Value {
    serde_json::json!({
        "converted_from": from,
        "converted_to": to,
        "session": session,
        "turns": turns,
        "converted_at": chrono::Utc::now().to_rfc3339(),
    })
}
