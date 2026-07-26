//! Runs the external-content FTS conversion against a copy of a real
//! `~/.agentflare/flare-docs.db`, if one exists. Skipped when it doesn't, so
//! CI and fresh checkouts stay green -- the point is to catch, locally,
//! anything the synthetic fixtures don't model about a database that has
//! actually been fetched into for months.

use std::path::PathBuf;

fn real_db() -> Option<PathBuf> {
    let p = dirs::home_dir()?.join(".agentflare").join("flare-docs.db");
    p.exists().then_some(p)
}

/// Sample rows on one side of the `deleted_at` split, as (rowid, first
/// alphanumeric word of their content) -- enough to ask the index directly
/// whether it knows about that specific row.
fn sample(conn: &rusqlite::Connection, deleted: bool, limit: i64) -> Vec<(i64, String)> {
    let sql = format!(
        "SELECT rowid, content FROM store_documents
          WHERE deleted_at IS {} NULL AND content != '' LIMIT ?1",
        if deleted { "NOT" } else { "" }
    );
    conn.prepare(&sql)
        .unwrap()
        .query_map([limit], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .filter_map(|row| {
            let (rowid, content) = row.unwrap();
            let word = content
                .split(|c: char| !c.is_alphanumeric())
                .find(|w| w.len() > 3)?
                .to_lowercase();
            Some((rowid, word))
        })
        .collect()
}

/// Whether `rowid` is reachable through the index. Deliberately not
/// `SELECT COUNT(*) FROM store_docs_fts`: on an external-content table a
/// bare scan is answered from the *content* table, so it reports every
/// document row whether or not it is indexed. Only a MATCH goes through the
/// index.
fn indexed(conn: &rusqlite::Connection, rowid: i64, word: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM store_docs_fts
          WHERE store_docs_fts MATCH ?1 AND rowid = ?2",
        rusqlite::params![format!("\"{word}\""), rowid],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

#[test]
fn converting_a_real_docs_cache_keeps_every_live_document_searchable() {
    let Some(src) = real_db() else {
        eprintln!("no ~/.agentflare/flare-docs.db -- skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("flare-docs.db");
    std::fs::copy(&src, &dst).unwrap();

    // Sampled before the migration so the assertions are against what the
    // database actually held, not against what the migration just wrote.
    let (live, dead) = {
        let conn = rusqlite::Connection::open(&dst).unwrap();
        (sample(&conn, false, 200), sample(&conn, true, 200))
    };
    assert!(!live.is_empty(), "fixture database has no live documents");

    // Opening applies the migration; dropped before reopening plainly so the
    // assertions below see a committed, closed database.
    drop(flare_docs::DocsStore::open_file(&dst).expect("migration must apply cleanly"));

    let conn = rusqlite::Connection::open(&dst).unwrap();
    // fts5's own verdict that the index and the content table agree -- the
    // check that actually covers all ~10k rows, not just the sample.
    conn.execute_batch("INSERT INTO store_docs_fts(store_docs_fts) VALUES('integrity-check');")
        .expect("converted index must pass fts5's own integrity check");

    for (rowid, word) in &live {
        assert!(
            indexed(&conn, *rowid, word),
            "live document {rowid} lost its index entry (searched {word:?})"
        );
    }
    for (rowid, word) in &dead {
        assert!(
            !indexed(&conn, *rowid, word),
            "soft-deleted document {rowid} must not be matchable (searched {word:?})"
        );
    }
}
