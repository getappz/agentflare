use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Vent {
    pub id: String,
    pub project_id: String,
    pub message: String,
    pub severity: String,
    pub tags: String,
    pub topic_key: String,
    pub seen_count: i64,
    pub actionable: bool,
    pub item_id: Option<String>,
    pub first_event_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct UpsertOutcome {
    pub id: String,
    pub seen_count: i64,
    pub existing_item_id: Option<String>,
    pub was_actionable: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn upsert(
    conn: &Connection,
    project_id: &str,
    message: &str,
    severity: &str,
    tags_json: &str,
    topic_key: &str,
    first_event_id: &str,
    seen_delta: i64,
    now: i64,
) -> Result<UpsertOutcome> {
    // IMMEDIATE takes the write lock up front so the read-merge-write of
    // tags below is atomic -- two concurrent upserts for the same
    // topic_key must not both read the same old_tags_json and have one
    // clobber the other's merged result.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;

    let existing = tx
        .query_row(
            "SELECT id, seen_count, item_id, actionable, tags FROM vents
             WHERE project_id = ?1 AND topic_key = ?2",
            params![project_id, topic_key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;

    if let Some((id, seen, item_id, actionable, old_tags_json)) = existing {
        let new_seen = seen + seen_delta;
        let merged_tags_json = merge_tags_json(&old_tags_json, tags_json);
        tx.execute(
            "UPDATE vents SET seen_count = ?2, message = ?3, severity = ?4,
                 tags = ?5, updated_at = ?6 WHERE id = ?1",
            params![id, new_seen, message, severity, merged_tags_json, now],
        )?;
        tx.commit()?;
        return Ok(UpsertOutcome {
            id,
            seen_count: new_seen,
            existing_item_id: item_id,
            was_actionable: actionable,
        });
    }

    let id = db_kit::ids::new_id();
    // Canonicalize (dedup) even on first insert -- a caller whose own tags
    // array has internal duplicates must not seed the row with them.
    let canonical_tags_json = merge_tags_json("[]", tags_json);
    tx.execute(
        "INSERT INTO vents (id, project_id, message, severity, tags, topic_key,
             seen_count, actionable, item_id, first_event_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, NULL, ?8, ?9, ?9)",
        params![
            id,
            project_id,
            message,
            severity,
            canonical_tags_json,
            topic_key,
            seen_delta,
            first_event_id,
            now
        ],
    )?;
    tx.commit()?;
    Ok(UpsertOutcome {
        id,
        seen_count: seen_delta,
        existing_item_id: None,
        was_actionable: false,
    })
}

/// Unions `new_json`'s tags into `old_json`'s (both JSON string arrays),
/// deduped and order-preserving. A later upsert with no tags (or a
/// different subset) must not erase tags a prior upsert already persisted
/// for the same topic_key.
fn merge_tags_json(old_json: &str, new_json: &str) -> String {
    let mut tags: Vec<String> = serde_json::from_str(old_json).unwrap_or_default();
    let new_tags: Vec<String> = serde_json::from_str(new_json).unwrap_or_default();
    for t in new_tags {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string())
}

pub fn link_item(conn: &Connection, vent_id: &str, item_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE vents SET item_id = ?2 WHERE id = ?1",
        params![vent_id, item_id],
    )?;
    Ok(())
}

pub fn set_actionable(conn: &Connection, vent_id: &str, actionable: bool) -> Result<()> {
    conn.execute(
        "UPDATE vents SET actionable = ?2 WHERE id = ?1",
        params![vent_id, i64::from(actionable)],
    )?;
    Ok(())
}

pub fn list(conn: &Connection, project_id: &str, actionable_only: bool) -> Result<Vec<Vent>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, message, severity, tags, topic_key, seen_count,
                actionable, item_id, first_event_id, created_at, updated_at
         FROM vents WHERE project_id = ?1 AND (?2 = 0 OR actionable = 1)
         ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map(params![project_id, i64::from(actionable_only)], |r| {
        Ok(Vent {
            id: r.get(0)?,
            project_id: r.get(1)?,
            message: r.get(2)?,
            severity: r.get(3)?,
            tags: r.get(4)?,
            topic_key: r.get(5)?,
            seen_count: r.get(6)?,
            actionable: r.get::<_, i64>(7)? != 0,
            item_id: r.get(8)?,
            first_event_id: r.get(9)?,
            created_at: r.get(10)?,
            updated_at: r.get(11)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn seed_project(conn: &rusqlite::Connection) -> String {
        conn.execute("INSERT INTO workspaces (id,name,slug,item_label,created_at,updated_at) VALUES ('w','W','w','Item',1,1)", []).unwrap();
        conn.execute("INSERT INTO projects (id,workspace_id,name,identifier,created_at,updated_at) VALUES ('p','w','P','P',1,1)", []).unwrap();
        "p".to_string()
    }

    #[test]
    fn upsert_dedups_by_topic_and_accumulates_seen_count() {
        let conn = open_in_memory().unwrap();
        let p = seed_project(&conn);
        let a = upsert(
            &conn,
            &p,
            "disk full",
            "medium",
            "[]",
            "disk full",
            "ev1",
            1,
            100,
        )
        .unwrap();
        assert_eq!(a.seen_count, 1);
        assert!(a.existing_item_id.is_none());
        let b = upsert(
            &conn,
            &p,
            "disk full",
            "high",
            "[]",
            "disk full",
            "ev2",
            5,
            200,
        )
        .unwrap();
        assert_eq!(a.id, b.id, "same topic → same row");
        assert_eq!(b.seen_count, 6, "1 + delta 5");
        assert_eq!(list(&conn, &p, false).unwrap().len(), 1);
    }

    #[test]
    fn upsert_unions_tags_instead_of_replacing() {
        let conn = open_in_memory().unwrap();
        let p = seed_project(&conn);
        upsert(
            &conn,
            &p,
            "disk full",
            "medium",
            r#"["dx"]"#,
            "disk full",
            "ev1",
            1,
            100,
        )
        .unwrap();
        // A later upsert with a different (or empty) tag set must not erase
        // "dx" -- and a fresh tag must still be added.
        upsert(
            &conn,
            &p,
            "disk full",
            "high",
            r#"["perf"]"#,
            "disk full",
            "ev2",
            1,
            200,
        )
        .unwrap();
        let stored = &list(&conn, &p, false).unwrap()[0].tags;
        let mut tags: Vec<String> = serde_json::from_str(stored).unwrap();
        tags.sort();
        assert_eq!(tags, vec!["dx".to_string(), "perf".to_string()]);

        // An untagged re-upsert must not erase what's already stored.
        upsert(
            &conn,
            &p,
            "disk full",
            "high",
            "[]",
            "disk full",
            "ev3",
            1,
            300,
        )
        .unwrap();
        let after_untagged = &list(&conn, &p, false).unwrap()[0].tags;
        let mut tags: Vec<String> = serde_json::from_str(after_untagged).unwrap();
        tags.sort();
        assert_eq!(
            tags,
            vec!["dx".to_string(), "perf".to_string()],
            "an untagged upsert must not erase previously-stored tags"
        );
    }

    #[test]
    fn duplicate_tags_on_first_upsert_are_deduped_in_order() {
        let conn = open_in_memory().unwrap();
        let p = seed_project(&conn);
        // Even with no existing row to merge against, a caller's own tags
        // array with internal duplicates must not seed the row with them.
        upsert(
            &conn,
            &p,
            "disk full",
            "medium",
            r#"["dx","dx","perf","dx"]"#,
            "disk full",
            "ev1",
            1,
            100,
        )
        .unwrap();
        let stored = &list(&conn, &p, false).unwrap()[0].tags;
        let tags: Vec<String> = serde_json::from_str(stored).unwrap();
        assert_eq!(
            tags,
            vec!["dx".to_string(), "perf".to_string()],
            "duplicates in the caller's own tags array must be deduped on insert too, \
             in first-seen order"
        );
    }

    #[test]
    fn link_item_and_actionable_filter() {
        let conn = open_in_memory().unwrap();
        let p = seed_project(&conn);
        conn.execute("INSERT INTO states (id,project_id,name,group_name,sequence,is_default,created_at,updated_at) VALUES ('s','p','Backlog','backlog',1.0,1,1,1)", []).unwrap();
        conn.execute("INSERT INTO items (id,project_id,state_id,name,created_at,updated_at) VALUES ('it','p','s','x',1,1)", []).unwrap();
        let v = upsert(&conn, &p, "m", "low", "[]", "m", "ev", 1, 1).unwrap();
        set_actionable(&conn, &v.id, true).unwrap();
        link_item(&conn, &v.id, "it").unwrap();
        let only = list(&conn, &p, true).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].item_id.as_deref(), Some("it"));
        assert!(only[0].actionable);
    }
}
