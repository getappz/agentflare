use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Observation {
    pub id: i64,
    pub session_id: Option<String>,
    pub r#type: String,
    pub title: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub project: Option<String>,
    pub scope: String,
    pub topic_key: Option<String>,
    pub normalized_hash: Option<String>,
    pub revision_count: i64,
    pub duplicate_count: i64,
    pub last_seen_at: Option<String>,
    pub review_after: Option<String>,
    pub pinned: i64,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Created(i64),
    Updated(i64),
    Duplicate(i64),
}

#[allow(clippy::too_many_arguments)]
pub fn save(
    conn: &Connection,
    session_id: Option<&str>,
    r#type: &str,
    title: &str,
    content: &str,
    tool_name: Option<&str>,
    project: Option<&str>,
    scope: Option<&str>,
    topic_key: Option<&str>,
) -> rusqlite::Result<SaveOutcome> {
    let hash = hash_normalized(title, content);
    let now = now_iso();
    let scope = scope.unwrap_or("project");

    if let Some((id, _)) = find_duplicate(conn, &hash, project)? {
        conn.execute(
            "UPDATE observations SET duplicate_count = duplicate_count + 1, last_seen_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        return Ok(SaveOutcome::Duplicate(id));
    }

    if let Some(tk) = topic_key.filter(|t| !t.is_empty())
        && let Some(existing) = conn
            .query_row(
                "SELECT id, revision_count FROM observations
                 WHERE topic_key = ?1 AND deleted_at IS NULL AND (?2 IS NULL OR project = ?2) LIMIT 1",
                params![tk, project],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?
        {
            let (id, rev) = existing;
            conn.execute(
                "UPDATE observations SET content = ?2, title = ?3, revision_count = ?4, normalized_hash = ?5, updated_at = ?6, last_seen_at = ?6 WHERE id = ?1",
                params![id, content, title, rev + 1, &hash, now],
            )?;
            return Ok(SaveOutcome::Updated(id));
        }

    conn.execute(
        "INSERT INTO observations
            (session_id, type, title, content, tool_name, project, scope,
             topic_key, normalized_hash, created_at, updated_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)",
        params![
            session_id, r#type, title, content, tool_name, project, scope, topic_key, &hash, now
        ],
    )?;
    let id = conn.last_insert_rowid();
    Ok(SaveOutcome::Created(id))
}

/// Upsert used by cross-machine sync import. Unlike `save`, the caller
/// supplies `created_at`/`updated_at` from the source machine's clock (not
/// `now_iso()`), and a topic_key match is only overwritten when the
/// incoming row is newer than the local one, or ties it with different
/// content (the deterministic winner `sync::merge` already picked) — a
/// pulled entry that is strictly stale relative to a fresher local edit
/// must not clobber it. ISO8601 timestamps in this fixed format compare
/// correctly as strings.
#[allow(clippy::too_many_arguments)]
pub fn upsert_synced(
    conn: &Connection,
    r#type: &str,
    title: &str,
    content: &str,
    project: Option<&str>,
    scope: &str,
    topic_key: Option<&str>,
    normalized_hash: &str,
    created_at: &str,
    updated_at: &str,
) -> rusqlite::Result<SaveOutcome> {
    if let Some(tk) = topic_key.filter(|t| !t.is_empty()) {
        if let Some((id, existing_updated_at, existing_content, existing_title)) = conn
            .query_row(
                "SELECT id, updated_at, content, title FROM observations
                 WHERE topic_key = ?1 AND deleted_at IS NULL AND project IS ?2 LIMIT 1",
                params![tk, project],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        {
            // A tie on `updated_at` with identical content is a true no-op
            // (skip it rather than bump revision_count for nothing); a tie
            // with different content must still converge to whichever entry
            // `sync::merge` picked as the winner, or the local db silently
            // disagrees with the file it just wrote to the remote branch.
            let is_stale = updated_at < existing_updated_at.as_str()
                || (updated_at == existing_updated_at.as_str()
                    && content == existing_content
                    && title == existing_title);
            if is_stale {
                return Ok(SaveOutcome::Duplicate(id));
            }
            conn.execute(
                "UPDATE observations SET content = ?2, title = ?3, normalized_hash = ?4,
                 type = ?5, scope = ?6, created_at = ?7,
                 revision_count = revision_count + 1, updated_at = ?8, last_seen_at = ?8
                 WHERE id = ?1",
                params![
                    id,
                    content,
                    title,
                    normalized_hash,
                    r#type,
                    scope,
                    created_at,
                    updated_at
                ],
            )?;
            return Ok(SaveOutcome::Updated(id));
        }
        conn.execute(
            "INSERT INTO observations
                (type, title, content, project, scope, topic_key, normalized_hash,
                 created_at, updated_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                r#type,
                title,
                content,
                project,
                scope,
                tk,
                normalized_hash,
                created_at,
                updated_at
            ],
        )?;
        return Ok(SaveOutcome::Created(conn.last_insert_rowid()));
    }

    if let Some((id, _)) = find_duplicate(conn, normalized_hash, project)? {
        return Ok(SaveOutcome::Duplicate(id));
    }
    conn.execute(
        "INSERT INTO observations
            (type, title, content, project, scope, normalized_hash, created_at, updated_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![r#type, title, content, project, scope, normalized_hash, created_at, updated_at],
    )?;
    Ok(SaveOutcome::Created(conn.last_insert_rowid()))
}

pub fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Observation>> {
    conn.query_row(
        "SELECT id, session_id, type, title, content, tool_name, project, scope,
                topic_key, normalized_hash, revision_count, duplicate_count,
                last_seen_at, review_after, pinned, created_at, updated_at, deleted_at
         FROM observations WHERE id = ?1",
        params![id],
        map_observation,
    )
    .optional()
}

pub fn update(
    conn: &Connection,
    id: i64,
    title: Option<&str>,
    content: Option<&str>,
    r#type: Option<&str>,
    pinned: Option<bool>,
) -> rusqlite::Result<bool> {
    let now = now_iso();
    let mut parts = Vec::new();
    let mut vals: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(t) = title {
        parts.push("title = ?");
        vals.push(Box::new(t.to_string()));
    }
    if let Some(c) = content {
        parts.push("content = ?");
        vals.push(Box::new(c.to_string()));
    }
    if let Some(t) = r#type {
        parts.push("type = ?");
        vals.push(Box::new(t.to_string()));
    }
    if let Some(p) = pinned {
        parts.push("pinned = ?");
        vals.push(Box::new(if p { 1i64 } else { 0i64 }));
    }
    if parts.is_empty() {
        return Ok(false);
    }
    parts.push("updated_at = ?");
    vals.push(Box::new(now.clone()));

    let sql = format!(
        "UPDATE observations SET {} WHERE id = ? AND deleted_at IS NULL",
        parts.join(", "),
    );
    vals.push(Box::new(id));
    let params: Vec<&dyn rusqlite::types::ToSql> = vals.iter().map(|v| v.as_ref()).collect();
    let n = conn.execute(&sql, params.as_slice())?;
    Ok(n > 0)
}

pub fn soft_delete(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let now = now_iso();
    let n = conn.execute(
        "UPDATE observations SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
        params![id, now],
    )?;
    Ok(n > 0)
}

pub fn pin(conn: &Connection, id: i64, pinned: bool) -> rusqlite::Result<bool> {
    let now = now_iso();
    let n = conn.execute(
        "UPDATE observations SET pinned = ?2, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
        params![id, if pinned { 1i64 } else { 0i64 }, now],
    )?;
    Ok(n > 0)
}

pub fn list_recent(
    conn: &Connection,
    project: Option<&str>,
    r#type: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<Observation>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, type, title, content, tool_name, project, scope,
                topic_key, normalized_hash, revision_count, duplicate_count,
                last_seen_at, review_after, pinned, created_at, updated_at, deleted_at
         FROM observations
         WHERE deleted_at IS NULL
           AND (?1 IS NULL OR project = ?1)
           AND (?2 IS NULL OR type = ?2)
         ORDER BY created_at DESC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![project, r#type, limit as i64], map_observation)?;
    rows.collect()
}

/// All non-deleted observations across every project, unpaged. Used by
/// memory::sync, which needs the whole set to merge against a remote copy
/// -- `list_recent`'s `limit` doesn't fit that (and `usize::MAX as i64`
/// wraps negative on a 64-bit build).
pub fn list_all(conn: &Connection) -> rusqlite::Result<Vec<Observation>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, type, title, content, tool_name, project, scope,
                topic_key, normalized_hash, revision_count, duplicate_count,
                last_seen_at, review_after, pinned, created_at, updated_at, deleted_at
         FROM observations
         WHERE deleted_at IS NULL
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], map_observation)?;
    rows.collect()
}

fn find_duplicate(
    conn: &Connection,
    hash: &str,
    project: Option<&str>,
) -> rusqlite::Result<Option<(i64, String)>> {
    conn.query_row(
        "SELECT id, created_at FROM observations
         WHERE normalized_hash = ?1 AND deleted_at IS NULL AND (?2 IS NULL OR project = ?2)
         ORDER BY created_at DESC LIMIT 1",
        params![hash, project],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

pub fn hash_normalized(title: &str, content: &str) -> String {
    let normal = |s: &str| -> String {
        s.split_whitespace()
            .map(|w| w.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let combined = format!("{} | {}", normal(title), normal(content));
    hex::encode(Sha256::digest(combined.as_bytes()))
}

fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn map_observation(r: &rusqlite::Row<'_>) -> rusqlite::Result<Observation> {
    Ok(Observation {
        id: r.get(0)?,
        session_id: r.get(1)?,
        r#type: r.get(2)?,
        title: r.get(3)?,
        content: r.get(4)?,
        tool_name: r.get(5)?,
        project: r.get(6)?,
        scope: r.get(7)?,
        topic_key: r.get(8)?,
        normalized_hash: r.get(9)?,
        revision_count: r.get(10)?,
        duplicate_count: r.get(11)?,
        last_seen_at: r.get(12)?,
        review_after: r.get(13)?,
        pinned: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
        deleted_at: r.get(17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema;

    fn new_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    // Regression test: identical title/content in two different projects
    // used to collide on the (unscoped) normalized_hash dedup check and
    // report the second save as a Duplicate of the first.
    #[test]
    fn cross_project_observations_with_identical_content_do_not_collide() {
        let conn = new_db();
        let outcome_a = save(
            &conn,
            None,
            "note",
            "same title",
            "same content",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();
        let outcome_b = save(
            &conn,
            None,
            "note",
            "same title",
            "same content",
            None,
            Some("proj-b"),
            None,
            None,
        )
        .unwrap();

        assert!(matches!(outcome_a, SaveOutcome::Created(_)));
        assert!(matches!(outcome_b, SaveOutcome::Created(_)));

        // Saving the same content again *within* proj-a should still be
        // detected as a duplicate (scoping must not break same-project dedup).
        let outcome_a2 = save(
            &conn,
            None,
            "note",
            "same title",
            "same content",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();
        assert!(matches!(outcome_a2, SaveOutcome::Duplicate(_)));
    }

    // Regression test: revising a topic_key observation used to leave
    // normalized_hash stale, so a later genuine duplicate of the *revised*
    // content wouldn't be recognized as a duplicate.
    #[test]
    fn topic_key_revision_refreshes_normalized_hash() {
        let conn = new_db();
        let outcome1 = save(
            &conn,
            None,
            "decision",
            "v1 title",
            "v1 content",
            None,
            Some("proj-a"),
            None,
            Some("topic-x"),
        )
        .unwrap();
        let id = match outcome1 {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };

        let outcome2 = save(
            &conn,
            None,
            "decision",
            "v2 title",
            "v2 content",
            None,
            Some("proj-a"),
            None,
            Some("topic-x"),
        )
        .unwrap();
        assert_eq!(outcome2, SaveOutcome::Updated(id));

        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(
            obs.normalized_hash.as_deref(),
            Some(hash_normalized("v2 title", "v2 content").as_str())
        );

        // Now a fresh save with content matching the *revised* version
        // (different topic_key so it goes through the dedup path, not the
        // topic_key path) should be recognized as a duplicate of `id`.
        let outcome3 = save(
            &conn,
            None,
            "decision",
            "v2 title",
            "v2 content",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(outcome3, SaveOutcome::Duplicate(id));
    }

    #[test]
    fn save_get_update_pin_soft_delete_roundtrip() {
        let conn = new_db();
        // observations.session_id has a REFERENCES sessions(id) constraint;
        // this build's bundled SQLite enforces foreign keys by default, so
        // create the referenced session first.
        crate::memory::sessions::create(&conn, "sess-1", Some("proj-a"), None).unwrap();
        let outcome = save(
            &conn,
            Some("sess-1"),
            "learning",
            "title",
            "content",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();
        let id = match outcome {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };

        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(obs.title, "title");
        assert_eq!(obs.pinned, 0);

        assert!(update(&conn, id, Some("new title"), None, None, Some(true)).unwrap());
        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(obs.title, "new title");
        assert_eq!(obs.pinned, 1);

        assert!(pin(&conn, id, false).unwrap());
        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(obs.pinned, 0);

        assert!(soft_delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().unwrap().deleted_at.is_some());

        // Soft-deleted rows are excluded from list_recent.
        let recent = list_recent(&conn, Some("proj-a"), None, 10).unwrap();
        assert!(recent.iter().all(|o| o.id != id));
    }

    #[test]
    fn upsert_synced_creates_when_topic_key_is_new() {
        let conn = new_db();
        let outcome = upsert_synced(
            &conn,
            "decision",
            "t",
            "c",
            Some("proj-a"),
            "project",
            Some("topic-x"),
            &hash_normalized("t", "c"),
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
        assert!(matches!(outcome, SaveOutcome::Created(_)));
    }

    // The whole point of upsert_synced over save: a remote row that is
    // OLDER than the local one must not overwrite it.
    #[test]
    fn upsert_synced_skips_a_stale_topic_key_update() {
        let conn = new_db();
        let id = match save(
            &conn,
            None,
            "decision",
            "fresh title",
            "fresh content",
            None,
            Some("proj-a"),
            None,
            Some("topic-x"),
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        conn.execute(
            "UPDATE observations SET updated_at = '2026-06-01T00:00:00.000Z' WHERE id = ?1",
            params![id],
        )
        .unwrap();

        let outcome = upsert_synced(
            &conn,
            "decision",
            "stale title",
            "stale content",
            Some("proj-a"),
            "project",
            Some("topic-x"),
            &hash_normalized("stale title", "stale content"),
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
        assert_eq!(outcome, SaveOutcome::Duplicate(id));
        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(obs.title, "fresh title");
    }

    #[test]
    fn upsert_synced_applies_a_newer_topic_key_update() {
        let conn = new_db();
        let id = match save(
            &conn,
            None,
            "decision",
            "old title",
            "old content",
            None,
            Some("proj-a"),
            None,
            Some("topic-x"),
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };

        let outcome = upsert_synced(
            &conn,
            "decision",
            "new title",
            "new content",
            Some("proj-a"),
            "project",
            Some("topic-x"),
            &hash_normalized("new title", "new content"),
            "2026-01-01T00:00:00.000Z",
            "2027-01-01T00:00:00.000Z",
        )
        .unwrap();
        assert_eq!(outcome, SaveOutcome::Updated(id));
        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(obs.title, "new title");
    }

    #[test]
    fn upsert_synced_dedupes_non_topic_key_entries_by_hash() {
        let conn = new_db();
        let hash = hash_normalized("t", "c");
        let first = upsert_synced(
            &conn,
            "note",
            "t",
            "c",
            Some("proj-a"),
            "project",
            None,
            &hash,
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
        let id = match first {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        let second = upsert_synced(
            &conn,
            "note",
            "t",
            "c",
            Some("proj-a"),
            "project",
            None,
            &hash,
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
        assert_eq!(second, SaveOutcome::Duplicate(id));
    }

    #[test]
    fn upsert_synced_applies_the_merge_winner_on_an_equal_timestamp_tie_with_different_content() {
        let conn = new_db();
        let id = match save(
            &conn,
            None,
            "decision",
            "remote-newer title",
            "remote-newer content",
            None,
            Some("proj-a"),
            None,
            Some("topic-x"),
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        conn.execute(
            "UPDATE observations SET updated_at = '2026-06-01T00:00:00.000Z' WHERE id = ?1",
            params![id],
        )
        .unwrap();

        // Same tie-break `sync::merge` uses on an exact-timestamp collision:
        // the entry chosen there must still land here, not get skipped as
        // "not strictly newer".
        let outcome = upsert_synced(
            &conn,
            "decision",
            "local-older title",
            "local-older content",
            Some("proj-a"),
            "project",
            Some("topic-x"),
            &hash_normalized("local-older title", "local-older content"),
            "2026-06-01T00:00:00.000Z",
            "2026-06-01T00:00:00.000Z",
        )
        .unwrap();
        assert_eq!(outcome, SaveOutcome::Updated(id));
        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(obs.title, "local-older title");
    }

    #[test]
    fn upsert_synced_is_a_true_noop_on_an_equal_timestamp_tie_with_identical_content() {
        let conn = new_db();
        let id = match save(
            &conn,
            None,
            "decision",
            "t",
            "c",
            None,
            Some("proj-a"),
            None,
            Some("topic-x"),
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        let before = get(&conn, id).unwrap().unwrap();

        let outcome = upsert_synced(
            &conn,
            "decision",
            "t",
            "c",
            Some("proj-a"),
            "project",
            Some("topic-x"),
            &hash_normalized("t", "c"),
            &before.created_at,
            &before.updated_at,
        )
        .unwrap();
        assert_eq!(outcome, SaveOutcome::Duplicate(id));
        let after = get(&conn, id).unwrap().unwrap();
        assert_eq!(
            after.revision_count, before.revision_count,
            "an identical tie must not bump revision_count"
        );
    }

    #[test]
    fn upsert_synced_updates_type_scope_and_created_at_on_a_newer_entry() {
        let conn = new_db();
        let id = match save(
            &conn,
            None,
            "note",
            "t",
            "c",
            None,
            Some("proj-a"),
            Some("project"),
            Some("topic-x"),
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };

        upsert_synced(
            &conn,
            "decision",
            "t2",
            "c2",
            Some("proj-a"),
            "global",
            Some("topic-x"),
            &hash_normalized("t2", "c2"),
            "2025-01-01T00:00:00.000Z",
            "2027-01-01T00:00:00.000Z",
        )
        .unwrap();

        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(obs.r#type, "decision");
        assert_eq!(obs.scope, "global");
        assert_eq!(obs.created_at, "2025-01-01T00:00:00.000Z");
    }

    #[test]
    fn upsert_synced_does_not_let_a_global_entry_clobber_a_project_scoped_one() {
        let conn = new_db();
        let id = match save(
            &conn,
            None,
            "decision",
            "project fact",
            "project content",
            None,
            Some("proj-a"),
            None,
            Some("topic-x"),
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };

        // No project (a global entry) sharing the same topic_key must not
        // match the project-scoped row -- `project IS ?2`, not
        // `?2 IS NULL OR project = ?2`, which would match every project.
        let outcome = upsert_synced(
            &conn,
            "decision",
            "global fact",
            "global content",
            None,
            "project",
            Some("topic-x"),
            &hash_normalized("global fact", "global content"),
            "2027-01-01T00:00:00.000Z",
            "2027-01-01T00:00:00.000Z",
        )
        .unwrap();
        assert!(matches!(outcome, SaveOutcome::Created(_)));

        let obs = get(&conn, id).unwrap().unwrap();
        assert_eq!(
            obs.title, "project fact",
            "the project-scoped row must be untouched"
        );
    }

    #[test]
    fn list_all_returns_every_project_and_excludes_soft_deleted() {
        let conn = new_db();
        let keep_a = match save(
            &conn,
            None,
            "note",
            "a",
            "a",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        let keep_b = match save(
            &conn,
            None,
            "note",
            "b",
            "b",
            None,
            Some("proj-b"),
            None,
            None,
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        let deleted = match save(
            &conn,
            None,
            "note",
            "d",
            "d",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap()
        {
            SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        soft_delete(&conn, deleted).unwrap();

        let all = list_all(&conn).unwrap();
        let ids: Vec<i64> = all.iter().map(|o| o.id).collect();
        assert!(ids.contains(&keep_a));
        assert!(ids.contains(&keep_b));
        assert!(!ids.contains(&deleted));
    }
}
