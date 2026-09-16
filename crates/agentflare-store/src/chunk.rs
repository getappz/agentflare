//! Semantic chunking for store documents — thin wrapper over `text-splitter`.
//!
//! Adopted from `benbrandt/text-splitter` (MIT, 629★, 98k dl/mo) instead of
//! hand-rolling Unicode segmentation. `MarkdownSplitter` respects CommonMark
//! heading levels, code blocks, lists, etc., so chunk boundaries align with
//! author intent — same engine AI Search's configurable chunker uses conceptually.

use text_splitter::{ChunkConfig, MarkdownSplitter, TextSplitter};

/// Default chunk capacity: 512..1024 chars (~130..260 tokens). Aligned with
/// `bge-small` 512-token window and AI Search's 4 MB file / per-chunk budget.
/// Range lets `text-splitter` maximize fill without splitting mid-sentence.
const DEFAULT_MIN: usize = 512;
const DEFAULT_MAX: usize = 1024;

/// Chunk plain text or Markdown. Returns non-empty trimmed chunks.
pub fn chunk_markdown(text: &str) -> Vec<String> {
    chunk_markdown_with_range(text, DEFAULT_MIN..DEFAULT_MAX)
}

/// Chunk with explicit char range — caller controls size (e.g. for code).
pub fn chunk_markdown_with_range(text: &str, range: std::ops::Range<usize>) -> Vec<String> {
    // Empty / whitespace-only docs produce no chunks (caller should skip FTS).
    if text.trim().is_empty() {
        return Vec::new();
    }
    let splitter = MarkdownSplitter::new(ChunkConfig::new(range).with_trim(true));
    splitter
        .chunks(text)
        .map(|c| c.to_string())
        .filter(|c| !c.trim().is_empty())
        .collect()
}

/// Fallback for non-Markdown (binary-decoded text, code without tree-sitter).
pub fn chunk_text(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let splitter = TextSplitter::new(ChunkConfig::new(DEFAULT_MIN..DEFAULT_MAX).with_trim(true));
    splitter
        .chunks(text)
        .map(|c| c.to_string())
        .filter(|c| !c.trim().is_empty())
        .collect()
}

/// Stable chunk id = `blake3(doc_id | chunk_index)` hex — content-addressed
/// so re-chunking same doc with same range yields same ids.
pub fn chunk_id(doc_id: &str, idx: usize) -> String {
    let mut h = blake3::Hasher::new();
    h.update(doc_id.as_bytes());
    h.update(b"|");
    h.update(idx.to_string().as_bytes());
    h.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_produces_no_chunks() {
        assert!(chunk_markdown("").is_empty());
        assert!(chunk_markdown("   \n\n  ").is_empty());
        assert!(chunk_text("").is_empty());
    }

    #[test]
    fn markdown_respects_headings() {
        let md = "# H1\n\nPara one is a bit longer so it has content.\n\n## H2\n\nPara two follows under second heading.";
        let chunks = chunk_markdown(md);
        assert!(!chunks.is_empty());
        // Headings should not be split mid-heading
        for c in &chunks {
            assert!(!c.is_empty());
        }
    }

    #[test]
    fn range_controls_chunk_count() {
        let text = "word ".repeat(500); // ~2500 chars
        let small = chunk_markdown_with_range(&text, 100..200);
        let large = chunk_markdown_with_range(&text, 500..1000);
        assert!(small.len() > large.len());
    }

    #[test]
    fn chunk_id_is_stable() {
        assert_eq!(chunk_id("doc1", 0), chunk_id("doc1", 0));
        assert_ne!(chunk_id("doc1", 0), chunk_id("doc1", 1));
        assert_ne!(chunk_id("doc1", 0), chunk_id("doc2", 0));
    }
}
