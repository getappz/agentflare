//! Memory is recalled into future prompts and synced to a shared branch, so a
//! pasted credential must never be stored. Reuses the handoff secret scanner.
use super::mcp::RememberInput;
use crate::mcp_server::secret_scan::redact;

/// `input` with secret-like text in title/content redacted, plus whether
/// anything changed.
pub(super) fn redact_input(input: RememberInput) -> (RememberInput, bool) {
    let (title, content) = (redact(&input.title), redact(&input.content));
    let changed = title != input.title || content != input.content;
    (
        RememberInput {
            title,
            content,
            ..input
        },
        changed,
    )
}

#[cfg(test)]
mod tests {
    use super::super::{mcp::remember_in, observations, schema};
    use super::*;

    #[test]
    fn remember_redacts_secrets_before_storing_and_warns() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn).unwrap();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz012345";
        let input = |content: String| RememberInput {
            title: "deploy token gotcha".into(),
            content,
            r#type: "note".into(),
            session_id: None,
            project: Some("p".into()),
            topic_key: None,
            scope: None,
            idempotency_key: None,
        };
        let run = |content: String| -> serde_json::Value {
            serde_json::from_str(&remember_in(&conn, input(content)).unwrap()).unwrap()
        };
        let out = run(format!("push failed with {secret} in src/x.rs"));
        let obs = observations::get(&conn, out["id"].as_i64().unwrap())
            .unwrap()
            .unwrap();
        assert!(!obs.content.contains(secret), "{}", obs.content);
        assert!(obs.content.contains("[REDACTED]"));
        assert!(out["warnings"].to_string().contains("redacted"));

        let clean = run("plain note about src/x.rs".into());
        assert!(!clean["warnings"].to_string().contains("redacted"));
    }
}
