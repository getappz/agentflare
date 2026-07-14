//! Caps oversized `tool_execute` results so one chatty downstream tool
//! can't blow out the LLM's context. Same motivation as forgemax's
//! `MAX_RESULT_CHARS` envelope, written fresh (no code shared — FSL).

use serde_json::Value;

pub const DEFAULT_MAX_CHARS: usize = 100_000;

pub fn truncate_if_needed(value: &Value, max_chars: usize) -> Value {
    let json = match serde_json::to_string_pretty(value) {
        Ok(s) => s,
        Err(_) => return value.clone(),
    };
    if json.len() <= max_chars {
        return value.clone();
    }
    let budget = max_chars.saturating_sub(300);
    let mut cut = find_safe_cut_point(&json, budget);
    let mut envelope = build_envelope(&json, cut);

    // `cut` was chosen against the RAW character length of the fragment,
    // but that fragment is embedded below as a JSON *string* value and
    // re-serialized — string serialization escapes quotes, backslashes, and
    // control characters (`"` -> `\"`, `\` -> `\\`, etc.), which can make
    // the ACTUAL serialized envelope larger than `cut` implied. Re-measure
    // the real serialized size and keep shrinking the cut point (reusing
    // `find_safe_cut_point` so the UTF-8 char-boundary guarantee is never
    // bypassed) until it actually fits, or there's nothing left to shrink.
    // Bounded iteration count so a pathological input can't loop forever;
    // in practice this converges in a handful of iterations since each
    // step shrinks by at least 10% (or 64 chars, whichever is larger).
    let mut attempts = 0;
    while envelope_len(&envelope) > max_chars && cut > 0 && attempts < 200 {
        attempts += 1;
        let step = (cut / 10).max(64);
        let shrink_to = cut.saturating_sub(step);
        cut = find_safe_cut_point(&json, shrink_to);
        envelope = build_envelope(&json, cut);
    }
    envelope
}

fn build_envelope(json: &str, cut: usize) -> Value {
    serde_json::json!({
        "_truncated": true,
        "_data_is_fragment": true,
        "_original_chars": json.len(),
        "_shown_chars": cut,
        "data": &json[..cut],
    })
}

/// The ACTUAL serialized size of a candidate envelope — matches
/// `tool_execute` (`src/mcp_server.rs`), which serializes the capped
/// value with `serde_json::to_string_pretty` before returning it.
fn envelope_len(envelope: &Value) -> usize {
    serde_json::to_string_pretty(envelope)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
}

fn find_safe_cut_point(json: &str, max_pos: usize) -> usize {
    let limit = floor_char_boundary(json, max_pos);
    let region = &json[..limit];
    if let Some(pos) = region.rfind('\n')
        && pos > limit / 2
    {
        return pos;
    }
    if let Some(pos) = region.rfind(',')
        && pos > limit / 2
    {
        return pos + 1;
    }
    region
        .char_indices()
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0)
}

fn floor_char_boundary(s: &str, max: usize) -> usize {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_pass_through_unchanged() {
        let v = serde_json::json!({"a": 1});
        assert_eq!(truncate_if_needed(&v, 100_000), v);
    }

    #[test]
    fn oversized_values_get_wrapped() {
        let big = "x".repeat(200);
        let v = serde_json::json!({"data": big});
        let wrapped = truncate_if_needed(&v, 100);
        assert_eq!(wrapped["_truncated"], serde_json::json!(true));
        assert!(wrapped["_shown_chars"].as_u64().unwrap() <= 100);
        assert!(wrapped["data"].as_str().unwrap().len() <= 100);
    }

    #[test]
    fn cut_point_never_splits_a_utf8_char() {
        let v = serde_json::json!({"data": "é".repeat(100)});
        let wrapped = truncate_if_needed(&v, 50);
        // Must not panic (String indexing on a non-boundary panics) and must
        // produce valid UTF-8 (guaranteed by &str slicing succeeding at all).
        assert!(wrapped["data"].as_str().is_some());
    }

    #[test]
    fn heavy_escaping_content_still_fits_within_max_chars_once_reserialized() {
        // Content that's almost entirely quotes/backslashes: each raw char
        // becomes a 2-character escape sequence (`\"` / `\\`) once embedded
        // as a JSON string value and re-serialized, so the RAW cut length
        // `find_safe_cut_point` measures understates the real serialized
        // size by roughly 2x. Before accounting for escaping overhead, this
        // could produce a final envelope well over `max_chars`.
        let nasty: String = "\"\\".repeat(5_000); // 10_000 raw chars, all escape-worthy
        let v = serde_json::json!({"data": nasty});
        let max_chars = 2_000;
        let wrapped = truncate_if_needed(&v, max_chars);
        let serialized = serde_json::to_string_pretty(&wrapped).unwrap();
        assert!(
            serialized.len() <= max_chars,
            "serialized envelope was {} chars, expected <= {max_chars}",
            serialized.len()
        );
    }
}
