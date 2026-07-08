//! Caps oversized `gateway_execute` results so one chatty downstream tool
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
    let cut = find_safe_cut_point(&json, budget);
    serde_json::json!({
        "_truncated": true,
        "_data_is_fragment": true,
        "_original_chars": json.len(),
        "_shown_chars": cut,
        "data": &json[..cut],
    })
}

fn find_safe_cut_point(json: &str, max_pos: usize) -> usize {
    let limit = floor_char_boundary(json, max_pos);
    let region = &json[..limit];
    if let Some(pos) = region.rfind('\n') {
        if pos > limit / 2 {
            return pos;
        }
    }
    if let Some(pos) = region.rfind(',') {
        if pos > limit / 2 {
            return pos + 1;
        }
    }
    region.char_indices().last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0)
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
}
