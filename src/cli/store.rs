use clap::{Args, Subcommand};

#[derive(Args)]
pub struct StoreArgs {
    #[command(subcommand)]
    pub command: StoreCommands,
}

#[derive(Subcommand)]
pub enum StoreCommands {
    /// Backfill text-splitter chunks for existing docs that have none yet (one-time after enabling hybrid search)
    Backfill {
        #[arg(long, default_value_t = 1000)]
        limit: usize,
    },
    /// Show chunk/vector counts and scale warning status
    Stats,
    /// Rebuild FTS indexes (doc + chunks) — needed after VACUUM
    Rebuild,
}

impl StoreArgs {
    pub fn run(self) {
        match self.command {
            StoreCommands::Backfill { limit } => {
                let store = match crate::store::open() {
                    Ok(s) => s,
                    Err(e) => return crate::ui::error(&e.to_string()),
                };
                match store.backfill_chunks(limit) {
                    Ok(n) => crate::ui::success(&format!("backfilled {n} docs (limit {limit})")),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
                // Show post-backfill stats
                match store.chunk_count(None) {
                    Ok(c) => println!("total chunks: {c}"),
                    Err(e) => crate::ui::warning(&e.to_string()),
                }
            }
            StoreCommands::Stats => {
                let store = match crate::store::open() {
                    Ok(s) => s,
                    Err(e) => return crate::ui::error(&e.to_string()),
                };
                let total_docs: i64 = store
                    .conn()
                    .query_row("SELECT COUNT(*) FROM store_documents WHERE deleted_at IS NULL", [], |r| r.get(0))
                    .unwrap_or(0);
                let total_chunks = store.chunk_count(None).unwrap_or(0);
                let total_vecs: i64 = store
                    .conn()
                    .query_row("SELECT COUNT(*) FROM store_chunk_vec", [], |r| r.get(0))
                    .unwrap_or(0);
                let total_meta: i64 = store
                    .conn()
                    .query_row("SELECT COUNT(*) FROM store_doc_meta", [], |r| r.get(0))
                    .unwrap_or(0);
                println!("docs: {total_docs}");
                println!("chunks: {total_chunks}");
                println!("chunk vectors: {total_vecs}");
                println!("meta rows: {total_meta}");
                if total_chunks > 50_000 {
                    crate::ui::warning(&format!(
                        "scale: {total_chunks} chunks >50k — vector search will be >100ms without --features vector (sqlite-vec ANN)"
                    ));
                } else {
                    println!("scale: ok (<50k chunks, brute-force vector fine)");
                }
                // Also test a dummy query time
                let start = std::time::Instant::now();
                let _ = store.chunk_search("test", "hello", 5);
                let ms = start.elapsed().as_millis();
                println!("probe chunk_search: {ms}ms");
                if let Some(w) = store.scale_warning("test", ms) {
                    crate::ui::warning(&w);
                }
            }
            StoreCommands::Rebuild => {
                let store = match crate::store::open() {
                    Ok(s) => s,
                    Err(e) => return crate::ui::error(&e.to_string()),
                };
                match store.doc_fts_rebuild() {
                    Ok(()) => crate::ui::success("doc FTS rebuilt"),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
                // Chunks FTS rebuild (same SQL as doc but for chunks)
                match store.conn().execute_batch(
                    "INSERT INTO store_chunks_fts(store_chunks_fts) VALUES('delete-all');
                     INSERT INTO store_chunks_fts(rowid, content) SELECT rowid, content FROM store_doc_chunks;",
                ) {
                    Ok(()) => crate::ui::success("chunk FTS rebuilt"),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
            }
        }
    }
}
