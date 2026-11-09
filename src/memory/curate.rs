//! `memory curate`: update, delete, pin and unpin one observation. Split out of
//! `mcp.rs` (LOC gate); re-exported there so existing paths keep working.
use serde::Deserialize;
use serde_json::json;

use super::mcp::{invalidate_project_of, open_db};
use super::observations;
use super::promotion::LESSON_TYPE;

#[derive(Debug, Deserialize)]
pub struct CurateInput {
    pub action: String,
    pub id: i64,
    pub title: Option<String>,
    pub content: Option<String>,
    pub r#type: Option<String>,
    pub pinned: Option<bool>,
}

pub fn handle_curate(input: CurateInput) -> Result<String, String> {
    curate_in(&open_db()?, input)
}

pub(super) fn curate_in(conn: &rusqlite::Connection, input: CurateInput) -> Result<String, String> {
    if input.action == "update"
        && input
            .r#type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case(LESSON_TYPE))
    {
        return Err(
            "type 'lesson' is reserved: use action=promote with evidence_ids and confidence".into(),
        );
    }
    match input.action.as_str() {
        "update" => {
            use super::remember_redact::{REDACTED_WARNING, redact_opt};
            // Same redaction as `remember`: an update must not be a way around it.
            let (title, t) = redact_opt(input.title.as_deref());
            let (content, c) = redact_opt(input.content.as_deref());
            let ok = observations::update(
                conn,
                input.id,
                title.as_deref(),
                content.as_deref(),
                input.r#type.as_deref(),
                input.pinned,
            )
            .map_err(|e| format!("update: {e}"))?;
            if ok {
                invalidate_project_of(conn, input.id);
            }
            let mut out = json!({"status": if ok { "updated" } else { "not_found" }});
            if t || c {
                out["warnings"] = json!([REDACTED_WARNING]);
            }
            Ok(out.to_string())
        }
        "delete" => {
            let ok =
                observations::soft_delete(conn, input.id).map_err(|e| format!("delete: {e}"))?;
            if ok {
                invalidate_project_of(conn, input.id);
            }
            Ok(json!({"status": if ok { "deleted" } else { "not_found" }}).to_string())
        }
        "pin" => {
            let ok = observations::pin(conn, input.id, true).map_err(|e| format!("pin: {e}"))?;
            if ok {
                invalidate_project_of(conn, input.id);
            }
            Ok(json!({"status": if ok { "pinned" } else { "not_found" }}).to_string())
        }
        "unpin" => {
            let ok = observations::pin(conn, input.id, false).map_err(|e| format!("unpin: {e}"))?;
            if ok {
                invalidate_project_of(conn, input.id);
            }
            Ok(json!({"status": if ok { "unpinned" } else { "not_found" }}).to_string())
        }
        other => Err(format!(
            "unknown action '{other}'; use update, delete, pin, or unpin"
        )),
    }
}
