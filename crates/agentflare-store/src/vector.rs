//! sqlite-vec vec0 ANN — optional scale lane for >50k chunks.
//! Adopted from `asg017/sqlite-vec` (MIT/Apache, 383k dl/mo, 280 crates)
//! instead of hand-rolling HNSW. Pure Rust crate `sqlite-vec` statically
//! links C extension and registers via `sqlite3_auto_extension`.

#[cfg(feature = "vector")]
use std::sync::OnceLock;

#[cfg(feature = "vector")]
static VEC_INIT: OnceLock<bool> = OnceLock::new();

/// Register sqlite-vec extension globally (once). Safe to call multiple times.
#[cfg(feature = "vector")]
pub fn ensure_init() -> bool {
    *VEC_INIT.get_or_init(|| {
        unsafe {
            // SAFETY: sqlite3_vec_init is the extension entrypoint, transmuted to auto_extension sig.
            let rc = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
            rc == rusqlite::ffi::SQLITE_OK
        }
    })
}

/// Ensure vec0 virtual table exists for 384-d BGE embeddings.
/// Call after `ensure_init()` and after opening connection (inside Store::open).
/// Uses `IF NOT EXISTS` so re-running is no-op.
#[cfg(feature = "vector")]
pub fn ensure_vec_table(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    // 384 = BGE small, the default in fastembed.rs. If you use larger model, recreate table with new dim.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS store_chunk_vec0 USING vec0(embedding float[384]);",
    )?;
    Ok(())
}

#[cfg(not(feature = "vector"))]
pub fn ensure_init() -> bool {
    false
}
#[cfg(not(feature = "vector"))]
pub fn ensure_vec_table(_conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    Ok(())
}

/// Check if vec0 table exists (for fallback logic).
pub fn vec_table_exists(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='store_chunk_vec0'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}
