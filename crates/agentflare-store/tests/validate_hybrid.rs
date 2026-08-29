use agentflare_store::Store;

#[test]
fn validate_hybrid() {
    let store = Store::open_memory().unwrap();
    let long_md = format!(
        "# Title\n\n{}\n\n## Section 2\n\n{}\n\n## Section 3\n\n{}",
        "alpha ".repeat(300) + " UNIQUE_TERM_ONE",
        "beta ".repeat(300) + " UNIQUE_TERM_TWO",
        "gamma ".repeat(300) + " UNIQUE_TERM_THREE",
    );
    let doc = store.doc_upsert("proj1", "docs/long.md", &long_md).unwrap();
    let chunks = store.doc_chunks(&doc.id).unwrap();
    assert!(chunks.len() >= 3, "chunks {}", chunks.len());
    let hits = store.chunk_search("proj1", "UNIQUE_TERM_TWO", 5).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].id, doc.id);
    store.doc_set_meta(&doc.id, "lang", "rust").unwrap();
    let filtered = store.doc_search_filtered("proj1", "UNIQUE_TERM_TWO", 5, Some(&[("lang".to_string(),"rust".to_string())]), None).unwrap();
    assert_eq!(filtered.len(), 1);
    let filtered_zero = store.doc_search_filtered("proj1", "UNIQUE_TERM_TWO", 5, Some(&[("lang".to_string(),"py".to_string())]), None).unwrap();
    assert!(filtered_zero.is_empty());
    let glob_hit = store.doc_search_filtered("proj1", "UNIQUE_TERM_TWO", 5, None, Some("docs/*.md")).unwrap();
    assert_eq!(glob_hit.len(), 1);
    store.search_cache_put("UNIQUE_TERM_TWO", "proj1", &hits);
    let cached = store.search_cache_get("UNIQUE_TERM_TWO", "proj1").unwrap();
    assert_eq!(cached[0].id, doc.id);
    {
        let conn = store.conn();
        let id = "backfill_doc";
        conn.execute("INSERT INTO store_documents (id, project_id, path, content, created_at, updated_at, title, doc_type, mime, tags, source, version, metadata, size) VALUES (?1, ?2, ?3, ?4, 1, 1, '', 'file', '', '[]', '', 1, '{}', 0)", rusqlite::params![id, "proj1", "docs/backfill.md", "backfill unique term ".repeat(100)]).unwrap();
    }
    let n = store.backfill_chunks(10).unwrap();
    assert_eq!(n, 1);
    let after = store.doc_chunks("backfill_doc").unwrap().len();
    assert!(after > 0);
}
