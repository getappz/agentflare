use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};
use std::path::Path;

/// Schema history, oldest first. `0001_initial` is today's full schema —
/// every `CREATE TABLE`/`INDEX` uses `IF NOT EXISTS`, so replaying it against
/// a pre-migration db (one whose schema was applied via the old one-shot
/// `execute_batch`, and so has `user_version` still at 0) is a harmless
/// no-op that just brings it under migration tracking. Future schema changes
/// are new `000N_*.sql` files appended here, never edits to this one.
const MIGRATION_LIST: &[M<'static>] = &[
    M::up(include_str!("migrations/0001_initial.sql")),
    M::up(include_str!("migrations/0002_schema_constraints.sql")),
    M::up(include_str!("migrations/0003_asset_versioning.sql")),
    M::up(include_str!("migrations/0004_item_comments.sql")),
    M::up(include_str!("migrations/0005_items_fts.sql")),
    M::up(include_str!("migrations/0006_vents.sql")),
    M::up(include_str!("migrations/0007_ask_events.sql")),
    M::up(include_str!("migrations/0008_bridge_repos.sql")),
    M::up(include_str!("migrations/0009_vent_escalation.sql")),
    M::up(include_str!("migrations/0010_project_dirs.sql")),
    M::up(include_str!("migrations/0011_item_assignment_events.sql")),
    M::up(include_str!("migrations/0012_item_relation_types.sql")),
    M::up(include_str!("migrations/0013_item_dates.sql")),
];
const MIGRATIONS: Migrations = Migrations::from_slice(MIGRATION_LIST);

pub fn open_db(path: &Path) -> Result<Connection, db_kit::open::Error> {
    let conn = db_kit::open_file(path, &MIGRATIONS)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection, db_kit::open::Error> {
    let conn = db_kit::open_memory(&MIGRATIONS)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_safe_against_a_pre_migration_db() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("backend.db");

        // Simulate a db from before this crate adopted rusqlite_migration:
        // schema applied via one-shot execute_batch, user_version left at 0.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(include_str!("migrations/0001_initial.sql"))
                .unwrap();
            conn.execute(
                "INSERT INTO workspaces (id, name, slug, item_label, created_at, updated_at)
                 VALUES ('w1', 'Test', 'test', 'Item', 1, 1)",
                [],
            )
            .unwrap();
        }

        let conn = open_db(&path).unwrap();
        let name: String = conn
            .query_row("SELECT name FROM workspaces WHERE id = 'w1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Test");
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, MIGRATION_LIST.len() as i64);
    }

    #[test]
    fn open_db_sets_wal_journal_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("backend.db")).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn migration_0002_preserves_data_and_drops_bad_self_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("backend.db");

        // Simulate a db at the state right after PR #166 (only migration
        // 0001 applied): pre-existing rows, including a degenerate
        // self-referencing dependency that predates 0002's CHECK constraint.
        {
            let v1 = Migrations::new(vec![M::up(include_str!("migrations/0001_initial.sql"))]);
            let mut conn = Connection::open(&path).unwrap();
            v1.to_latest(&mut conn).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "INSERT INTO workspaces (id, name, slug, item_label, created_at, updated_at)
                   VALUES ('w1', 'W', 'w', 'Item', 1, 1);
                 INSERT INTO projects (id, workspace_id, name, identifier, created_at, updated_at)
                   VALUES ('p1', 'w1', 'P', 'P', 1, 1);
                 INSERT INTO states (id, project_id, name, group_name, sequence, created_at, updated_at)
                   VALUES ('s1', 'p1', 'Backlog', 'backlog', 1.0, 1, 1);
                 INSERT INTO items (id, project_id, state_id, name, created_at, updated_at)
                   VALUES ('i1', 'p1', 's1', 'I1', 1, 1);
                 INSERT INTO items (id, project_id, state_id, name, created_at, updated_at)
                   VALUES ('i2', 'p1', 's1', 'I2', 1, 1);
                 INSERT INTO items (id, project_id, state_id, name, created_at, updated_at)
                   VALUES ('bad', 'p1', 's1', 'Bad', 1, 1);
                 INSERT INTO item_dependencies (item_id, depends_on_item_id) VALUES ('i1', 'i2');
                 INSERT INTO item_dependencies (item_id, depends_on_item_id) VALUES ('bad', 'bad');
                 INSERT INTO labels (id, workspace_id, project_id, name, created_at, updated_at)
                   VALUES ('l1', 'w1', 'p1', 'Label', 1, 1);
                 INSERT INTO webhooks (id, workspace_id, url, secret_key, created_at, updated_at)
                   VALUES ('wh1', 'w1', 'https://example.com', 'secret', 1, 1);
                 INSERT INTO webhook_logs (id, workspace_id, webhook_id, created_at)
                   VALUES ('log1', 'w1', 'wh1', 1);",
            )
            .unwrap();
        }

        let conn = open_db(&path).unwrap();

        let dep_count: i64 = conn
            .query_row("SELECT count(*) FROM item_dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            dep_count, 1,
            "the self-referencing row should have been dropped"
        );

        let label_name: String = conn
            .query_row("SELECT name FROM labels WHERE id = 'l1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label_name, "Label");

        let log_id: String = conn
            .query_row("SELECT id FROM webhook_logs WHERE id = 'log1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(log_id, "log1");

        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, MIGRATION_LIST.len() as i64);
    }

    fn is_constraint_violation(err: &rusqlite::Error) -> bool {
        matches!(
            err,
            rusqlite::Error::SqliteFailure(e, _)
                if e.code == rusqlite::ErrorCode::ConstraintViolation
        )
    }

    #[test]
    fn migration_0002_enforces_new_constraints() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO workspaces (id, name, slug, item_label, created_at, updated_at)
             VALUES ('w1', 'W', 'w', 'Item', 1, 1)",
            [],
        )
        .unwrap();

        let err = conn
            .execute(
                "INSERT INTO item_dependencies (item_id, depends_on_item_id) VALUES ('x', 'x')",
                [],
            )
            .unwrap_err();
        assert!(is_constraint_violation(&err), "{err}");

        let err = conn
            .execute(
                "INSERT INTO labels (id, workspace_id, project_id, name, created_at, updated_at)
                 VALUES ('l', 'w1', 'missing-project', 'Label', 1, 1)",
                [],
            )
            .unwrap_err();
        assert!(is_constraint_violation(&err), "{err}");

        let err = conn
            .execute(
                "INSERT INTO webhook_logs (id, workspace_id, webhook_id, created_at)
                 VALUES ('log', 'w1', 'missing-webhook', 1)",
                [],
            )
            .unwrap_err();
        assert!(is_constraint_violation(&err), "{err}");
    }

    #[test]
    fn migration_0012_backfills_relation_type_and_widens_pk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("backend.db");

        // Simulate a db at the state right before 0012: pre-existing
        // item_dependencies rows with no relation_type column at all.
        {
            let pre_0012 = Migrations::new(
                MIGRATION_LIST[..MIGRATION_LIST.len() - 2].to_vec(),
            );
            let mut conn = Connection::open(&path).unwrap();
            pre_0012.to_latest(&mut conn).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "INSERT INTO workspaces (id, name, slug, item_label, created_at, updated_at)
                   VALUES ('w1', 'W', 'w', 'Item', 1, 1);
                 INSERT INTO projects (id, workspace_id, name, identifier, created_at, updated_at)
                   VALUES ('p1', 'w1', 'P', 'P', 1, 1);
                 INSERT INTO states (id, project_id, name, group_name, sequence, created_at, updated_at)
                   VALUES ('s1', 'p1', 'Backlog', 'backlog', 1.0, 1, 1);
                 INSERT INTO items (id, project_id, state_id, name, created_at, updated_at)
                   VALUES ('i1', 'p1', 's1', 'I1', 1, 1);
                 INSERT INTO items (id, project_id, state_id, name, created_at, updated_at)
                   VALUES ('i2', 'p1', 's1', 'I2', 1, 1);
                 INSERT INTO item_dependencies (item_id, depends_on_item_id) VALUES ('i1', 'i2');",
            )
            .unwrap();
        }

        let conn = open_db(&path).unwrap();

        let (item_id, depends_on, relation_type): (String, String, String) = conn
            .query_row(
                "SELECT item_id, depends_on_item_id, relation_type FROM item_dependencies",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(item_id, "i1");
        assert_eq!(depends_on, "i2");
        assert_eq!(
            relation_type, "blocks",
            "pre-existing rows must backfill to relation_type='blocks'"
        );

        // The widened PK now allows the same pair to also hold a 'duplicate'
        // relation without clobbering the 'blocks' row.
        conn.execute(
            "INSERT INTO item_dependencies (item_id, depends_on_item_id, relation_type) VALUES ('i1', 'i2', 'duplicate')",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM item_dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // An invalid relation_type is rejected.
        let err = conn
            .execute(
                "INSERT INTO item_dependencies (item_id, depends_on_item_id, relation_type) VALUES ('i1', 'i2', 'bogus')",
                [],
            )
            .unwrap_err();
        assert!(is_constraint_violation(&err), "{err}");

        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, MIGRATION_LIST.len() as i64);
    }

    #[test]
    fn migration_0013_adds_nullable_date_columns_without_data_loss() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("backend.db");

        // Simulate a db at 0011 (pre-0013), with an existing item row that
        // must survive the ALTER TABLE untouched.
        {
            let pre = Migrations::new(vec![
                M::up(include_str!("migrations/0001_initial.sql")),
                M::up(include_str!("migrations/0002_schema_constraints.sql")),
                M::up(include_str!("migrations/0003_asset_versioning.sql")),
                M::up(include_str!("migrations/0004_item_comments.sql")),
                M::up(include_str!("migrations/0005_items_fts.sql")),
                M::up(include_str!("migrations/0006_vents.sql")),
                M::up(include_str!("migrations/0007_ask_events.sql")),
                M::up(include_str!("migrations/0008_bridge_repos.sql")),
                M::up(include_str!("migrations/0009_vent_escalation.sql")),
                M::up(include_str!("migrations/0010_project_dirs.sql")),
                M::up(include_str!("migrations/0011_item_assignment_events.sql")),
            ]);
            let mut conn = Connection::open(&path).unwrap();
            pre.to_latest(&mut conn).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "INSERT INTO workspaces (id, name, slug, item_label, created_at, updated_at)
                   VALUES ('w1', 'W', 'w', 'Item', 1, 1);
                 INSERT INTO projects (id, workspace_id, name, identifier, created_at, updated_at)
                   VALUES ('p1', 'w1', 'P', 'P', 1, 1);
                 INSERT INTO states (id, project_id, name, group_name, sequence, created_at, updated_at)
                   VALUES ('s1', 'p1', 'Backlog', 'backlog', 1.0, 1, 1);
                 INSERT INTO items (id, project_id, state_id, name, created_at, updated_at)
                   VALUES ('i1', 'p1', 's1', 'I1', 1, 1);",
            )
            .unwrap();
        }

        let conn = open_db(&path).unwrap();

        let name: String = conn
            .query_row("SELECT name FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "I1", "pre-existing item row must survive the ALTER TABLE");

        let (start_date, due_date): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT start_date, due_date FROM items WHERE id = 'i1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(start_date, None, "new column must default to NULL");
        assert_eq!(due_date, None, "new column must default to NULL");

        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, MIGRATION_LIST.len() as i64);
    }
}
