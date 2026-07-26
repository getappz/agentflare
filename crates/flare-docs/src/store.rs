use agentflare_store::documents::{DocMatch, DocUpsertOpts, Document, DocumentSummary};
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};

/// Every row in the flare-docs store uses this fixed project_id. agentflare-store's
/// doc_* methods require project_id as a mandatory param; using one constant value
/// across every call is what makes this store logically global (fetched once,
/// reused by every project) rather than scoped per-project.
pub const PROJECT_ID: &str = "global";

/// Ceiling on results from a single [`DocsStore::search`] call.
///
/// Enforced here rather than at each caller so the MCP tool, the CLI, and any
/// future caller inherit it — an uncapped `limit` lets one request pull the
/// whole index into a response body. Mirrors the "max 50" the memory tool
/// already documents.
pub const MAX_SEARCH_LIMIT: usize = 50;

/// How many documents [`DocsStore::list_summaries`] returns when the caller
/// names no limit.
///
/// A default is not a nicety here. Per-item indexing means one crate
/// contributes hundreds of documents, so a real cache holds tens of
/// thousands; "return everything unless asked otherwise" makes the common
/// call the catastrophic one. Enumeration is also the wrong tool for finding
/// a specific page — that is what `search` is for — so a first page plus an
/// honest total serves the actual use.
pub const DEFAULT_LIST_LIMIT: usize = 100;

/// Ceiling on a single [`DocsStore::list_summaries`] page.
///
/// Higher than [`MAX_SEARCH_LIMIT`] because a summary is a fraction of the
/// size of a search hit (no snippet, no body), and walking a cache in pages
/// of 50 would be tedious. Paired with `offset`, so the cap bounds one
/// response without putting any document out of reach.
pub const MAX_LIST_LIMIT: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] agentflare_store::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub struct DocsStore {
    inner: agentflare_store::Store,
}

impl DocsStore {
    pub fn open_memory() -> Result<Self, Error> {
        Ok(Self {
            inner: agentflare_store::Store::open_memory()?,
        })
    }

    pub fn open_file(path: &Path) -> Result<Self, Error> {
        Ok(Self {
            inner: agentflare_store::Store::open_file(path)?,
        })
    }

    pub fn default_db_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".agentflare")
            .join("flare-docs.db")
    }

    pub fn open_default() -> Result<Self, Error> {
        let path = Self::default_db_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::open_file(&path)
    }

    pub fn upsert(
        &self,
        id_path: &str,
        content: &str,
        opts: DocUpsertOpts,
    ) -> Result<Document, Error> {
        Ok(self
            .inner
            .doc_upsert_with_opts(PROJECT_ID, id_path, content, opts)?)
    }

    pub fn get(&self, id: &str) -> Result<Option<Document>, Error> {
        Ok(self.inner.doc_get(id)?)
    }

    pub fn get_by_path(&self, path: &str) -> Result<Option<Document>, Error> {
        Ok(self.inner.doc_get_by_path(PROJECT_ID, path)?)
    }

    /// `limit` is clamped to [`MAX_SEARCH_LIMIT`]; a larger request is
    /// silently capped rather than rejected, since asking for "everything" is
    /// a reasonable thing to want and a truncated answer still serves it.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<DocMatch>, Error> {
        let limit = limit.min(MAX_SEARCH_LIMIT);
        Ok(self.inner.doc_search(PROJECT_ID, query, limit)?)
    }

    /// Every cached document, bodies included.
    ///
    /// Kept for the CLI, which writes to a terminal a human can pipe through
    /// `jq`/`head`. Callers whose output goes into a context window — the MCP
    /// tool above all — want [`Self::list_summaries`] instead: a cache of a
    /// dozen packages is already several megabytes here.
    pub fn list(&self) -> Result<Vec<Document>, Error> {
        Ok(self.inner.doc_list(PROJECT_ID)?)
    }

    /// One page of cached documents, without their bodies.
    ///
    /// `limit` is clamped to [`MAX_LIST_LIMIT`] and `None` means
    /// [`DEFAULT_LIST_LIMIT`] — the same "cap, don't reject" treatment
    /// [`Self::search`] gives an oversized request, since asking for
    /// everything is reasonable and a truncated answer still serves it.
    pub fn list_summaries(
        &self,
        limit: Option<usize>,
        offset: usize,
    ) -> Result<Vec<DocumentSummary>, Error> {
        let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT);
        Ok(self.inner.doc_list_summaries(PROJECT_ID, limit, offset)?)
    }

    /// How many documents the cache holds, so a capped listing can say what
    /// it left out.
    pub fn count(&self) -> Result<usize, Error> {
        Ok(self.inner.doc_count(PROJECT_ID)?)
    }

    pub fn blob_store_raw(&self, data: &[u8]) -> Result<String, Error> {
        Ok(self.inner.blob_store(data)?)
    }

    /// Upserts many documents in a single transaction.
    ///
    /// A crate's rustdoc JSON can have thousands of items; upserting them
    /// one at a time through [`Self::upsert`] would open a separate
    /// `BEGIN IMMEDIATE` transaction (and FTS resync) per item, turning a
    /// single `get`/`refresh` into thousands of exclusive-lock commits.
    /// This batches the whole set into one transaction and never writes
    /// `store_doc_history` rows — cache-type per-item docs (rustdoc text is
    /// stable across refreshes) have no use for version history, and
    /// writing it unconditionally would be pure amplification at this
    /// volume.
    ///
    /// An item whose content is byte-identical to what's already stored is
    /// skipped entirely (no row write, no FTS resync, no version bump) — a
    /// crate whose docs haven't changed since the last fetch would otherwise
    /// still pay the full per-item write cost (and grow `version`
    /// unboundedly) on every refresh, the same amplification problem
    /// [`agentflare_store::Store::doc_upsert_with_opts`]'s content-hash
    /// short-circuit exists to avoid for the single-doc path.
    ///
    /// Returns the number of items actually written (inserted or changed),
    /// not the total number of items passed in.
    pub fn upsert_batch(&self, items: &[BatchItem]) -> Result<usize, Error> {
        let conn = self.inner.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let written = Self::write_batch_items(&tx, items)?;
        tx.commit()?;
        Ok(written)
    }

    /// Like [`Self::upsert_batch`], but also reconciles the store against
    /// the fresh fetch: any existing, non-deleted document whose path
    /// starts with `path_prefix` and is *not* in `items` gets soft-deleted
    /// (`deleted_at` set), in the same transaction as the batch upsert.
    ///
    /// This is for cache-refresh callers like rustdoc per-item indexing,
    /// where a refetch's item set is authoritative for everything under
    /// `path_prefix` — an item that disappeared from the new fetch (renamed,
    /// removed, made private) must stop being findable, not linger from the
    /// previous fetch forever.
    pub fn upsert_batch_reconciled(
        &self,
        path_prefix: &str,
        items: &[BatchItem],
    ) -> Result<usize, Error> {
        // Scoped so the connection guard is dropped before the blob unrefs
        // below, which take the same non-reentrant mutex.
        let (written, stale_blobs) = {
            let conn = self.inner.conn();
            let tx = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let written = Self::write_batch_items(&tx, items)?;

            let now = db_kit::ids::now();
            let fresh_paths: std::collections::HashSet<&str> =
                items.iter().map(|i| i.path.as_str()).collect();
            // SQLite's LIKE is case-insensitive for ASCII by default, but a path
            // prefix match must be exact -- re-check with a case-sensitive
            // `starts_with` in Rust so a path that only coincidentally matches
            // `path_prefix` case-insensitively (e.g. a different crate/version
            // segment differing only in case) is never treated as belonging to
            // this prefix.
            let like_pattern = format!("{}%", escape_like(path_prefix));
            let mut stmt = tx.prepare(
                "SELECT rowid, path, blob_hash FROM store_documents
                 WHERE project_id = ?1 AND path LIKE ?2 ESCAPE '\\' AND deleted_at IS NULL",
            )?;
            let stored: Vec<(i64, String, Option<String>)> = stmt
                .query_map(rusqlite::params![PROJECT_ID, like_pattern], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);

            let mut stale_blobs = Vec::new();
            for (rowid, stored_path, blob_hash) in stored {
                if stored_path.starts_with(path_prefix)
                    && !fresh_paths.contains(stored_path.as_str())
                {
                    tx.execute(
                        "UPDATE store_documents SET deleted_at = ?1 WHERE rowid = ?2",
                        rusqlite::params![now, rowid],
                    )?;
                    // Mirror write_batch_items's explicit FTS sync -- store_docs_fts
                    // is a manually-synced table, not a content=/external-content
                    // FTS5 table, so a soft-deleted row's stale entry would
                    // otherwise stay directly matchable via `store_docs_fts MATCH`
                    // (doc_search's own deleted_at filter still excludes it, but
                    // nothing else queries store_docs_fts through that guard).
                    tx.execute(
                        "DELETE FROM store_docs_fts WHERE rowid = ?1",
                        rusqlite::params![rowid],
                    )?;
                    if let Some(hash) = blob_hash {
                        stale_blobs.push(hash);
                    }
                }
            }

            tx.commit()?;
            (written, stale_blobs)
        };

        // After the commit, for the same reason doc_delete releases blobs last:
        // the row is the source of truth, so a failure here leaks a file rather
        // than stranding a live document with no content. Per-item docs store
        // their text inline, but the crate-overview rows hold the raw rustdoc
        // JSON as a blob -- without this, reconciling one away orphans that
        // file in <root>/blobs forever.
        // Every hash gets its attempt before the first failure is reported:
        // returning early would strand the rest of the batch as orphans, and
        // the rows are already committed so there is nothing to roll back. The
        // error still surfaces -- a failed unref means a leaked file, which is
        // exactly the condition this whole change exists to stop hiding.
        let mut first_err = None;
        for hash in stale_blobs {
            if let Err(e) = self.inner.blob_unref(&hash) {
                first_err = first_err.or(Some(e));
            }
        }
        if let Some(e) = first_err {
            return Err(e.into());
        }
        Ok(written)
    }

    /// Upserts `items` within an already-open transaction. Skips any item
    /// whose content is byte-identical to what's already stored (no row
    /// write, no FTS resync, no version bump) — a crate whose docs haven't
    /// changed since the last fetch would otherwise still pay the full
    /// per-item write cost (and grow `version` unboundedly) on every
    /// refresh, the same amplification problem
    /// [`agentflare_store::Store::doc_upsert_with_opts`]'s content-hash
    /// short-circuit exists to avoid for the single-doc path.
    ///
    /// Returns the number of items actually written (inserted or changed),
    /// not the total number of items passed in.
    fn write_batch_items(
        tx: &rusqlite::Transaction,
        items: &[BatchItem],
    ) -> rusqlite::Result<usize> {
        let now = db_kit::ids::now();
        let mut written = 0usize;

        // Prepared once and reused across every item (rusqlite's statement
        // cache would do this implicitly via prepare_cached, but a crate can
        // have thousands of items, so holding the four statements open for
        // the whole loop avoids thousands of cache lookups too).
        {
            let mut select_stmt = tx.prepare_cached(
                "SELECT content FROM store_documents WHERE project_id = ?1 AND path = ?2 AND deleted_at IS NULL",
            )?;
            let mut upsert_stmt = tx.prepare_cached(
                "INSERT INTO store_documents
                 (id, project_id, path, content, title, doc_type, blob_hash, mime, tags, session_id, source, metadata, size, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, '', ?7, NULL, ?8, '{}', 0, 1, ?9, ?9)
                 ON CONFLICT(project_id, path) DO UPDATE SET
                    content = excluded.content,
                    title = excluded.title,
                    doc_type = excluded.doc_type,
                    tags = excluded.tags,
                    source = excluded.source,
                    version = store_documents.version + 1,
                    updated_at = excluded.updated_at,
                    deleted_at = NULL
                 RETURNING rowid",
            )?;
            let mut fts_del_stmt =
                tx.prepare_cached("DELETE FROM store_docs_fts WHERE rowid = ?1")?;
            let mut fts_ins_stmt =
                tx.prepare_cached("INSERT INTO store_docs_fts(rowid, content) VALUES (?1, ?2)")?;

            for item in items {
                let existing_content: Option<String> = select_stmt
                    .query_row(rusqlite::params![PROJECT_ID, item.path], |row| row.get(0))
                    .optional()?;
                if existing_content.as_deref() == Some(item.content.as_str()) {
                    continue;
                }

                let tags_json = serde_json::to_string(&item.tags).unwrap_or_else(|_| "[]".into());
                let id = db_kit::ids::new_id();
                let rowid: i64 = upsert_stmt.query_row(
                    rusqlite::params![
                        id,
                        PROJECT_ID,
                        item.path,
                        item.content,
                        item.title,
                        item.doc_type,
                        tags_json,
                        item.source,
                        now,
                    ],
                    |row| row.get(0),
                )?;

                fts_del_stmt.execute(rusqlite::params![rowid])?;
                fts_ins_stmt.execute(rusqlite::params![rowid, item.content])?;
                written += 1;
            }
        } // statements dropped here, releasing their borrow of `tx` before commit

        Ok(written)
    }
}

/// Escapes `%`, `_`, and `\` in a LIKE prefix so it matches only literal
/// text (used with `ESCAPE '\\'`) — crate names commonly contain `_`
/// (e.g. `serde_json`), which would otherwise match any single character.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// One document to write via [`DocsStore::upsert_batch`].
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub path: String,
    pub content: String,
    pub title: String,
    pub doc_type: String,
    pub tags: Vec<String>,
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_get_and_search_round_trip() {
        let store = DocsStore::open_memory().unwrap();

        let opts = DocUpsertOpts {
            title: Some("serde".to_string()),
            doc_type: Some("rust-crate".to_string()),
            source: Some("docsrs".to_string()),
            tags: Some(vec!["serde".to_string(), "rust".to_string()]),
            ..Default::default()
        };
        let doc = store
            .upsert(
                "docsrs/serde",
                "A generic serialization/deserialization framework",
                opts,
            )
            .unwrap();
        assert_eq!(doc.project_id, PROJECT_ID);
        assert_eq!(doc.path, "docsrs/serde");
        assert_eq!(doc.doc_type, "rust-crate");
        assert_eq!(doc.tags, vec!["serde", "rust"]);

        let fetched = store.get(&doc.id).unwrap().unwrap();
        assert_eq!(
            fetched.content,
            "A generic serialization/deserialization framework"
        );

        let hits = store.search("serialization", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, doc.id);

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, doc.id);
    }

    #[test]
    fn search_clamps_an_oversized_limit() {
        // Every caller (MCP tool, CLI) routes through here, so an unbounded
        // `--limit`/`limit:` can never pull more than the cap.
        let store = DocsStore::open_memory().unwrap();
        for i in 0..(MAX_SEARCH_LIMIT + 10) {
            store
                .upsert(
                    &format!("docsrs/crate{i}"),
                    "serialization framework",
                    DocUpsertOpts::default(),
                )
                .unwrap();
        }

        let hits = store.search("serialization", usize::MAX).unwrap();
        assert_eq!(hits.len(), MAX_SEARCH_LIMIT);

        // A limit under the cap is still honoured exactly.
        let few = store.search("serialization", 3).unwrap();
        assert_eq!(few.len(), 3);
    }

    #[test]
    fn upsert_batch_inserts_multiple_documents_in_one_call() {
        let store = DocsStore::open_memory().unwrap();
        let items = vec![
            BatchItem {
                path: "docsrs/axum/latest/axum::extract::State".into(),
                content: "State docs".into(),
                title: "State".into(),
                doc_type: "rust-item".into(),
                tags: vec!["axum".into(), "rust".into(), "struct".into()],
                source: "docsrs".into(),
            },
            BatchItem {
                path: "docsrs/axum/latest/axum::Router".into(),
                content: "Router docs".into(),
                title: "Router".into(),
                doc_type: "rust-item".into(),
                tags: vec!["axum".into(), "rust".into(), "struct".into()],
                source: "docsrs".into(),
            },
        ];

        let n = store.upsert_batch(&items).unwrap();
        assert_eq!(n, 2);

        let state_doc = store
            .get_by_path("docsrs/axum/latest/axum::extract::State")
            .unwrap()
            .unwrap();
        assert_eq!(state_doc.content, "State docs");
        assert_eq!(state_doc.title, "State");
        assert_eq!(state_doc.project_id, PROJECT_ID);

        let hits = store.search("State docs", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn upsert_batch_updates_existing_row_and_preserves_id() {
        let store = DocsStore::open_memory().unwrap();
        let make = |content: &str| {
            vec![BatchItem {
                path: "docsrs/foo/latest/foo::Bar".into(),
                content: content.into(),
                title: "Bar".into(),
                doc_type: "rust-item".into(),
                tags: vec![],
                source: "docsrs".into(),
            }]
        };

        store.upsert_batch(&make("v1")).unwrap();
        let original = store
            .get_by_path("docsrs/foo/latest/foo::Bar")
            .unwrap()
            .unwrap();

        store.upsert_batch(&make("v2")).unwrap();
        let updated = store
            .get_by_path("docsrs/foo/latest/foo::Bar")
            .unwrap()
            .unwrap();

        assert_eq!(updated.id, original.id);
        assert_eq!(updated.content, "v2");
        assert_eq!(updated.version, 2);
    }

    #[test]
    fn upsert_batch_skips_unchanged_items_entirely() {
        let store = DocsStore::open_memory().unwrap();
        let make = |content: &str| {
            vec![BatchItem {
                path: "docsrs/foo/latest/foo::Bar".into(),
                content: content.into(),
                title: "Bar".into(),
                doc_type: "rust-item".into(),
                tags: vec![],
                source: "docsrs".into(),
            }]
        };

        let n = store.upsert_batch(&make("same content")).unwrap();
        assert_eq!(n, 1, "first write is a real insert");
        let original = store
            .get_by_path("docsrs/foo/latest/foo::Bar")
            .unwrap()
            .unwrap();

        // Re-running the batch with identical content must not bump
        // version or count as a write -- this is the exact write
        // amplification the content-hash check exists to avoid.
        let n = store.upsert_batch(&make("same content")).unwrap();
        assert_eq!(n, 0, "unchanged content must not be counted as written");
        let unchanged = store
            .get_by_path("docsrs/foo/latest/foo::Bar")
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.version, original.version);

        // A real content change is still written normally.
        let n = store.upsert_batch(&make("different content")).unwrap();
        assert_eq!(n, 1);
        let changed = store
            .get_by_path("docsrs/foo/latest/foo::Bar")
            .unwrap()
            .unwrap();
        assert_eq!(changed.content, "different content");
        assert_eq!(changed.version, original.version + 1);
    }

    #[test]
    fn get_by_path_finds_an_existing_doc_and_none_for_a_missing_one() {
        let store = DocsStore::open_memory().unwrap();
        store
            .upsert("docsrs/serde", "docs", DocUpsertOpts::default())
            .unwrap();

        let found = store.get_by_path("docsrs/serde").unwrap().unwrap();
        assert_eq!(found.path, "docsrs/serde");

        assert!(store.get_by_path("docsrs/nope").unwrap().is_none());
    }

    #[test]
    fn upsert_batch_reconciled_removes_stale_items_and_their_fts_entries() {
        let store = DocsStore::open_memory().unwrap();
        let prefix = "docsrs/axum/latest/item/";
        let make = |path: &str, content: &str| BatchItem {
            path: format!("{prefix}{path}"),
            content: content.into(),
            title: path.into(),
            doc_type: "rust-item".into(),
            tags: vec![],
            source: "docsrs".into(),
        };

        store
            .upsert_batch_reconciled(
                prefix,
                &[
                    make("axum::Router", "router docs unique marker"),
                    make("axum::extract::State", "state docs"),
                ],
            )
            .unwrap();
        assert_eq!(store.search("unique marker", 10).unwrap().len(), 1);

        // Refetch with Router gone -- it must be soft-deleted...
        store
            .upsert_batch_reconciled(prefix, &[make("axum::extract::State", "state docs")])
            .unwrap();
        assert!(
            store
                .get_by_path(&format!("{prefix}axum::Router"))
                .unwrap()
                .is_none()
        );
        // ...and its FTS entry must be gone too, not just excluded by
        // doc_search's deleted_at filter -- searching for its old content
        // must return nothing.
        assert!(
            store.search("unique marker", 10).unwrap().is_empty(),
            "a soft-deleted item's FTS entry must not remain matchable"
        );
    }

    // File-backed on purpose: blob bytes only land on disk for file stores, so
    // an open_memory version of this would pass while real installs leaked.
    #[test]
    fn upsert_batch_reconciled_reclaims_a_stale_items_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open_file(&dir.path().join("flare-docs.db")).unwrap();
        let prefix = "docsrs/axum/latest/item/";

        let hash = store
            .blob_store_raw(b"raw rustdoc json for a stale item")
            .unwrap();
        store
            .upsert(
                &format!("{prefix}axum::Router"),
                "",
                DocUpsertOpts {
                    blob_hash: Some(hash.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Mirrors agentflare_store::blobs::blob_disk_path, which is private.
        let disk_path = dir.path().join("blobs").join(&hash[..2]).join(&hash);
        assert!(disk_path.exists(), "precondition: blob written to disk");

        // Refetch without the blob-backed item: reconciliation soft-deletes it.
        store
            .upsert_batch_reconciled(
                prefix,
                &[BatchItem {
                    path: format!("{prefix}axum::extract::State"),
                    content: "state docs".into(),
                    title: "State".into(),
                    doc_type: "rust-item".into(),
                    tags: vec![],
                    source: "docsrs".into(),
                }],
            )
            .unwrap();

        assert!(
            !disk_path.exists(),
            "reconciling away the last referrer must reclaim its blob file"
        );
    }

    #[test]
    fn upsert_batch_reconciled_prefix_match_is_case_sensitive() {
        let store = DocsStore::open_memory().unwrap();
        // A path that only matches the LIKE pattern case-insensitively
        // (different case on the crate segment) must not be treated as
        // belonging to this prefix and must survive reconciliation.
        store
            .upsert_batch(&[BatchItem {
                path: "docsrs/Axum/latest/item/axum::Other".into(),
                content: "other-case docs".into(),
                title: "Other".into(),
                doc_type: "rust-item".into(),
                tags: vec![],
                source: "docsrs".into(),
            }])
            .unwrap();

        store
            .upsert_batch_reconciled(
                "docsrs/axum/latest/item/",
                &[BatchItem {
                    path: "docsrs/axum/latest/item/axum::Router".into(),
                    content: "router docs".into(),
                    title: "Router".into(),
                    doc_type: "rust-item".into(),
                    tags: vec![],
                    source: "docsrs".into(),
                }],
            )
            .unwrap();

        assert!(
            store
                .get_by_path("docsrs/Axum/latest/item/axum::Other")
                .unwrap()
                .is_some(),
            "a differently-cased path must not be soft-deleted by a case-insensitive LIKE match"
        );
    }
}
