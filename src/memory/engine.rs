//! Optional ONNX embedding engine. Everything degrades to None: no
//! `semantic` feature, no downloaded model, or a failed call all mean
//! "no embeddings" — callers fall back to FTS-only behavior.

#[cfg(feature = "semantic")]
mod imp {
    use agentflare_store::embedding_pipeline::EmbeddingEngine;
    use std::sync::OnceLock;

    static ENGINE: OnceLock<Result<EmbeddingEngine, String>> = OnceLock::new();

    fn load() -> &'static Result<EmbeddingEngine, String> {
        ENGINE.get_or_init(|| match EmbeddingEngine::load_default() {
            Ok(e) => Ok(e),
            Err(err) => {
                // One log line per process, not per call.
                eprintln!("[memory] embedding engine unavailable: {err}");
                Err(err.to_string())
            }
        })
    }

    fn engine() -> Option<&'static EmbeddingEngine> {
        load().as_ref().ok()
    }

    pub fn unavailable_reason() -> Option<String> {
        load().as_ref().err().cloned()
    }

    pub fn embed_doc(text: &str) -> Option<Vec<f32>> {
        engine()?.embed(text).ok()
    }

    pub fn embed_query(text: &str) -> Option<Vec<f32>> {
        engine()?.embed_query(text).ok()
    }

    pub fn model_name() -> Option<String> {
        Some(engine()?.model_name().to_string())
    }
}

#[cfg(not(feature = "semantic"))]
mod imp {
    pub fn embed_doc(_: &str) -> Option<Vec<f32>> {
        None
    }

    pub fn embed_query(_: &str) -> Option<Vec<f32>> {
        None
    }

    pub fn model_name() -> Option<String> {
        None
    }

    pub fn unavailable_reason() -> Option<String> {
        Some("this build has no `semantic` feature".to_string())
    }
}

pub use imp::{embed_doc, embed_query, model_name, unavailable_reason};
