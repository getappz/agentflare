//! Fastembed wrapper — local ONNX embeddings + reranking via `ort`.
//! Adopted from `Anush008/fastembed-rs` (Apache-2.0, 999★) instead of hand-rolling
//! ONNX plumbing. Reuses the `ort =2.0.0-rc.13` you already ship.

#[cfg(feature = "embeddings")]
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

#[cfg(feature = "embeddings")]
use std::sync::OnceLock;

/// Singleton text embedding model (BGESmallEN15: 384d, 512 tokens,  ~100 MB).
/// Lazy — first call downloads to `FASTEMBED_CACHE_DIR`/HF cache if missing,
/// then offline. Failures return `None` so search falls back to BM25.
#[cfg(feature = "embeddings")]
static TEXT_MODEL: OnceLock<std::sync::Mutex<Option<TextEmbedding>>> = OnceLock::new();

#[cfg(feature = "embeddings")]
fn get_or_init_model() -> Option<std::sync::MutexGuard<'static, Option<TextEmbedding>>> {
    let cell = TEXT_MODEL.get_or_init(|| std::sync::Mutex::new(None));
    // Try init once; on failure keep None so subsequent calls don't retry download.
    // Caller can call `try_embed` again after fixing network/cache.
    let mut guard = cell.lock().ok()?;
    if guard.is_none() {
        match TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::BGESmallENV15)) {
            Ok(m) => *guard = Some(m),
            Err(_) => return None,
        }
    }
    // We need to return guard with Some — but we already checked.
    // Re-lock dance: keep guard alive, caller uses it.
    drop(guard);
    cell.lock().ok().filter(|g| g.is_some())
}

/// Embed a single query/text. Returns normalized vec or None (no model / offline).
#[cfg(feature = "embeddings")]
pub fn try_embed(text: &str) -> Option<Vec<f32>> {
    let mut guard = get_or_init_model()?;
    let model = guard.as_mut()?;
    match model.embed(vec![text], None) {
        Ok(v) if !v.is_empty() => Some(v.into_iter().next().unwrap()),
        _ => None,
    }
}

/// Embed batch of texts (chunks). Returns Vec<Vec<f32>> in same order; on error None.
#[cfg(feature = "embeddings")]
pub fn try_embed_batch(texts: &[String]) -> Option<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Some(vec![]);
    }
    let mut guard = get_or_init_model()?;
    let model = guard.as_mut()?;
    // fastembed batch size default 256 — pass None
    match model.embed(texts.to_vec(), None) {
        Ok(v) => Some(v),
        Err(_) => None,
    }
}

#[cfg(not(feature = "embeddings"))]
pub fn try_embed(_text: &str) -> Option<Vec<f32>> {
    None
}
#[cfg(not(feature = "embeddings"))]
pub fn try_embed_batch(_texts: &[String]) -> Option<Vec<Vec<f32>>> {
    None
}

/// Reranker wrapper — same once-lock pattern, model `BGERerankerBase`.
#[cfg(feature = "embeddings")]
use fastembed::{RerankerModel, RerankInitOptions, TextRerank};

#[cfg(feature = "embeddings")]
static RERANK_MODEL: OnceLock<std::sync::Mutex<Option<TextRerank>>> = OnceLock::new();

#[cfg(feature = "embeddings")]
pub fn try_rerank(query: &str, docs: Vec<String>) -> Option<Vec<(String, f32)>> {
    if docs.is_empty() {
        return Some(vec![]);
    }
    let cell = RERANK_MODEL.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cell.lock().ok()?;
    if guard.is_none() {
        match TextRerank::try_new(RerankInitOptions::new(RerankerModel::BGERerankerBase)) {
            Ok(m) => *guard = Some(m),
            Err(_) => return None,
        }
    }
    let model = guard.as_mut()?;
    // S must be same for query and docs — use owned String for both
    let q = query.to_string();
    match model.rerank(q, docs.clone(), true, None) {
        Ok(results) => {
            // results are RerankResult { document: Option<String>, score, index }
            let mut out: Vec<(String, f32)> = results
                .into_iter()
                .filter_map(|r| r.document.map(|d| (d, r.score)))
                .collect();
            // Ensure sorted best-first (fastembed already does, but guarantee)
            out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            Some(out)
        }
        Err(_) => None,
    }
}

#[cfg(not(feature = "embeddings"))]
pub fn try_rerank(_query: &str, _docs: Vec<String>) -> Option<Vec<(String, f32)>> {
    None
}

/// Local query rewriting (AI Search § query rewriting, local variant).
/// Without LLM, we do lightweight normalization: trim, lowercased variant,
/// and for sparse model available, SPLADE expansion (top lexical tokens).
/// Returns `Some(expanded)` if rewriting adds value, else `None` → keep original.
pub fn try_rewrite_query(query: &str) -> Option<String> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    // Rule-based: if query contains uppercase, add lowercased variant for case-insensitive recall
    let lower = q.to_lowercase();
    if lower != q {
        // Keep original plus lowercased (FTS is case-insensitive but this helps vector side)
        return Some(format!("{q} {lower}"));
    }
    // Sparse SPLADE expansion when embeddings feature + model cached
    #[cfg(feature = "embeddings")]
    {
        if let Some(expanded) = try_sparse_expand(q) {
            if expanded != q {
                return Some(expanded);
            }
        }
    }
    None
}

#[cfg(feature = "embeddings")]
fn try_sparse_expand(query: &str) -> Option<String> {
    use fastembed::{SparseInitOptions, SparseModel, SparseTextEmbedding};
    use std::sync::OnceLock;
    static SPARSE_MODEL: OnceLock<std::sync::Mutex<Option<SparseTextEmbedding>>> = OnceLock::new();
    let cell = SPARSE_MODEL.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cell.lock().ok()?;
    if guard.is_none() {
        match SparseTextEmbedding::try_new(SparseInitOptions::new(SparseModel::SPLADEPPV1)) {
            Ok(m) => *guard = Some(m),
            Err(_) => return None,
        }
    }
    let model = guard.as_mut()?;
    // Embed query, get sparse indices → map to tokens via tokenizer vocab? For now, just return original
    // SPLADE indices are vocab ids; detokenizing requires vocab lookup which fastembed doesn't expose directly.
    // Keep as no-op until we vendor vocab map — fallback to rule-based above.
    let _ = model.embed(vec![query], None).ok()?;
    None
}
