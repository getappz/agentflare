//! Memory is recalled into future prompts and synced to a shared branch, so a
//! pasted credential must never be stored. Reuses the handoff secret scanner.
use super::mcp::RememberInput;
use crate::mcp_server::secret_scan::{flagged_field, redact};

pub(super) const REDACTED_WARNING: &str = "secret-like text was redacted before storing";

/// `text` with secret-like text redacted, plus whether anything changed.
pub(super) fn redact_opt(text: Option<&str>) -> (Option<String>, bool) {
    let redacted = text.map(redact);
    let changed = redacted.as_deref() != text;
    (redacted, changed)
}

/// True when `text` is nothing but `[REDACTED:..]` placeholders (and whitespace):
/// storing it would replace real content with a marker.
pub(super) fn only_placeholders(text: &str) -> bool {
    let mut rest = text;
    let mut kept = String::new();
    while let Some(i) = rest.find("[REDACTED") {
        kept.push_str(&rest[..i]);
        match rest[i..].find(']') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                kept.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    kept.push_str(rest);
    kept.trim().is_empty()
}

/// Rejects a secret-like value in a field stored and synced as an identity key.
/// Redacting would change what the row is keyed on, so refuse instead; the error
/// names the field and pattern class, never the value.
pub(super) fn check_identity(input: &RememberInput) -> Result<(), String> {
    let fields = [
        ("topic_key", &input.topic_key),
        ("idempotency_key", &input.idempotency_key),
        ("project", &input.project),
    ]
    .into_iter()
    .filter_map(|(name, v)| v.as_deref().map(|v| (name, v)));
    match flagged_field(fields) {
        Some((field, pattern)) => Err(format!(
            "remember blocked: field '{field}' looks like it contains a {pattern}; it is \
             stored and synced as an identity key. Remove it and retry."
        )),
        None => Ok(()),
    }
}

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
    use super::super::{
        curate::{CurateInput, curate_in},
        mcp::remember_in,
        observations, schema,
    };
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

    // Built at runtime so no credential-shaped literal sits in the source.
    fn token() -> String {
        format!("ghp_{}", "a".repeat(36))
    }

    fn db() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    fn base() -> RememberInput {
        RememberInput {
            title: "a note".into(),
            content: "plain note".into(),
            r#type: "note".into(),
            session_id: None,
            project: Some("p".into()),
            topic_key: None,
            scope: None,
            idempotency_key: None,
        }
    }

    #[test]
    fn identity_fields_with_a_secret_are_rejected_not_redacted() {
        let conn = db();
        for field in ["topic_key", "idempotency_key", "project"] {
            let mut input = base();
            let slot = match field {
                "topic_key" => &mut input.topic_key,
                "idempotency_key" => &mut input.idempotency_key,
                _ => &mut input.project,
            };
            *slot = Some(token());
            let err = remember_in(&conn, input).unwrap_err();
            assert!(err.contains(field), "{err}");
            assert!(
                !err.contains(&token()),
                "the error must not echo the secret"
            );
        }
        let mut ok = base();
        ok.topic_key = Some("deploy/notes".into());
        assert!(remember_in(&conn, ok).is_ok());
    }

    #[test]
    fn curate_update_redacts_secrets_and_warns() {
        let conn = db();
        let saved: serde_json::Value =
            serde_json::from_str(&remember_in(&conn, base()).unwrap()).unwrap();
        let id = saved["id"].as_i64().unwrap();
        let update = |title: Option<String>, content: Option<String>| CurateInput {
            action: "update".into(),
            id,
            title,
            content,
            r#type: None,
            pinned: None,
        };
        let out = curate_in(&conn, update(None, Some(format!("key {}", token())))).unwrap();
        let obs = observations::get(&conn, id).unwrap().unwrap();
        assert!(!obs.content.contains(&token()), "{}", obs.content);
        assert!(out.contains("redacted"), "{out}");
        let out = curate_in(&conn, update(Some(format!("t {}", token())), None)).unwrap();
        assert!(
            !observations::get(&conn, id)
                .unwrap()
                .unwrap()
                .title
                .contains(&token())
        );
        assert!(out.contains("redacted"), "{out}");
        let out = curate_in(&conn, update(Some("clean title".into()), None)).unwrap();
        assert!(!out.contains("redacted"), "{out}");
    }

    #[test]
    fn curate_update_refuses_a_wholly_secret_value() {
        let conn = db();
        let saved: serde_json::Value =
            serde_json::from_str(&remember_in(&conn, base()).unwrap()).unwrap();
        let id = saved["id"].as_i64().unwrap();
        let err = curate_in(
            &conn,
            CurateInput {
                action: "update".into(),
                id,
                title: None,
                content: Some(token()),
                r#type: None,
                pinned: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("entirely secret-like"), "{err}");
        let obs = observations::get(&conn, id).unwrap().unwrap();
        assert_eq!(
            obs.content, "plain note",
            "stored content must be untouched"
        );
    }
}
