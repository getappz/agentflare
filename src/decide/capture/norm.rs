//! Normalization, redaction and size bounds for captured rows.
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Longest string kept in any feature field.
pub const MAX_STR_CHARS: usize = 500;
/// Serialized-size ceiling for one row's `features`.
pub const MAX_FEATURES_BYTES: usize = 4096;

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap()
});
static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-f]{7,64}\b").unwrap());
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Lowercase, strip volatile ids (uuids, hex hashes) and numbers, collapse
/// whitespace. Two inputs that differ only in those get the same text.
pub fn normalize(text: &str) -> String {
    let lower = text.to_lowercase();
    let s = UUID.replace_all(&lower, "");
    // A hex run only counts as an id when it has a digit (not "defaced").
    let s = HEX.replace_all(&s, |c: &regex::Captures| {
        if c[0].bytes().any(|b| b.is_ascii_digit()) {
            String::new()
        } else {
            c[0].to_string()
        }
    });
    let s = DIGITS.replace_all(&s, "");
    SPACE.replace_all(&s, " ").trim().to_string()
}

/// Short stable hash of the normalized text.
pub fn norm_key(text: &str) -> String {
    hex::encode(&Sha256::digest(normalize(text).as_bytes())[..8])
}

/// Redact secrets, then clip, every string in `v`; replace the whole value
/// with a marker if it is still over the size ceiling.
pub fn sanitize_features(v: Value) -> Value {
    let v = redact_value(v);
    let len = v.to_string().len();
    if len > MAX_FEATURES_BYTES {
        serde_json::json!({ "truncated": true, "bytes": len })
    } else {
        v
    }
}

/// Redact then clip every string value AND object key in `v`.
pub fn redact_value(v: Value) -> Value {
    let clean = |s: &str| -> String {
        let red = crate::mcp_server::secret_scan::redact(s);
        red.chars().take(MAX_STR_CHARS).collect()
    };
    match v {
        Value::String(s) => Value::String(clean(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(redact_value).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (clean(&k), redact_value(v)))
                .collect(),
        ),
        other => other,
    }
}
